//! Delinked side file tree (shown in the Inspector's `Files` tab) and standalone file viewer panels
//! (`PaneKind::File` split panes), plus Cmd+Click link/path resolution from terminals.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use gpui::{AnyElement, ClipboardItem, Context, SharedString, Window, div, prelude::*};
use xctx_core::SpaceId;
use xctx_layout::{PaneId, PaneKind, PaneRole};
use xctx_plugin_view::{
    FileMark, FilePanelAction, FilePanelData, FilePanelView, PreviewBody, markdown_blocks,
};
use xctx_term::TerminalLinkKind;
use xctx_ui::inspector::{
    FileGitMark, InspectorFiles, InspectorTab, SidebarFileRow, SidebarFileRowKind,
};

use crate::plugin_browser::page_area;
use crate::plugin_host::{
    FILES_PREVIEW_BYTES, FILES_PREVIEW_LINES, FilePreview, FileTree, GitChange, PreviewText,
    TreeRowKind, file_diff, git_status, list_folder, read_preview,
};
use crate::plugin_panes::PaneKey;
use crate::workspace::Workspace;

/// Message shown when a deleted file is opened without Diff turned on.
const FILE_DELETED_MESSAGE: &str = "This file was deleted in the worktree. Turn on Diff to see it.";

/// Per-(space, repo) state of the delinked side file explorer.
struct SidebarTreeUi {
    tree: FileTree,
    root_shown: SharedString,
    changed_only: bool,
    status_reading: bool,
    initialized: bool,
}

impl SidebarTreeUi {
    fn new(root: PathBuf, root_shown: String) -> Self {
        Self {
            tree: FileTree::new(root),
            root_shown: root_shown.into(),
            changed_only: false,
            status_reading: false,
            initialized: false,
        }
    }
}

/// Per-pane state of a standalone `PaneKind::File` viewer panel.
struct FilePanelUi {
    /// Worktree root directory for worktree-relative files, or the file's parent directory for external files.
    root: PathBuf,
    /// Path relative to `root` (as passed to `xctx_files`), or basename for external files.
    relative_path: String,
    /// Path displayed in the panel toolbar and stored in `PaneRole::file`.
    display_path: String,
    /// Repo name from `PaneRole::repo`.
    repo: String,
    /// Whether this file lives inside the space's repo worktree (enabling git diff & reveal).
    in_worktree: bool,
    diff: bool,
    raw_markdown: bool,
    target_line: Option<usize>,
    change: Option<GitChange>,
    preview: Option<Result<FilePreview, String>>,
    diff_text: Option<Result<PreviewText, String>>,
    diff_reading: bool,
    generation: u64,
}

impl FilePanelUi {
    fn new(
        root: PathBuf,
        relative_path: String,
        display_path: String,
        repo: String,
        in_worktree: bool,
        target_line: Option<usize>,
    ) -> Self {
        Self {
            root,
            relative_path,
            display_path,
            repo,
            in_worktree,
            diff: false,
            raw_markdown: false,
            target_line,
            change: None,
            preview: None,
            diff_text: None,
            diff_reading: false,
            generation: 0,
        }
    }

    fn is_markdown(&self) -> bool {
        Path::new(&self.relative_path)
            .extension()
            .is_some_and(|ext| {
                ext.eq_ignore_ascii_case("md")
                    || ext.eq_ignore_ascii_case("markdown")
                    || ext.eq_ignore_ascii_case("mdx")
            })
    }

    fn is_pdf(&self) -> bool {
        Path::new(&self.relative_path)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
    }

    fn full_path(&self) -> PathBuf {
        self.root.join(&self.relative_path)
    }

    fn data(&self) -> FilePanelData {
        let can_diff = self.in_worktree && self.change.is_some();
        let diff = self.diff && can_diff;
        let is_markdown = self.is_markdown();
        let document = !diff && self.is_pdf() && matches!(self.preview, Some(Ok(_)));

        let (body, cut) = if document {
            (PreviewBody::Document, None)
        } else if diff {
            match &self.diff_text {
                None => (PreviewBody::Loading, None),
                Some(Err(why)) => (PreviewBody::Message(why.clone().into()), None),
                Some(Ok(text)) => panel_text_body(text, false),
            }
        } else {
            match &self.preview {
                None => (PreviewBody::Loading, None),
                Some(Err(_)) if self.change == Some(GitChange::Deleted) => {
                    (PreviewBody::Message(FILE_DELETED_MESSAGE.into()), None)
                }
                Some(Err(why)) => (PreviewBody::Message(why.clone().into()), None),
                Some(Ok(file)) => panel_text_body(&file.body, is_markdown && !self.raw_markdown),
            }
        };

        FilePanelData {
            path: self.display_path.clone().into(),
            size: match &self.preview {
                Some(Ok(file)) => Some(file.size),
                _ => None,
            },
            mark: self.change.map(to_file_mark),
            cut,
            can_diff,
            diff,
            is_markdown,
            raw_markdown: self.raw_markdown,
            target_line: self.target_line,
            body,
        }
    }

    fn copyable_text(&self) -> Option<String> {
        let diff = self.diff && self.in_worktree && self.change.is_some();
        if diff {
            match &self.diff_text {
                Some(Ok(PreviewText::Text { text, .. })) => Some(text.clone()),
                _ => None,
            }
        } else {
            match &self.preview {
                Some(Ok(FilePreview {
                    body: PreviewText::Text { text, .. },
                    ..
                })) => Some(text.clone()),
                _ => None,
            }
        }
    }
}

/// All delinked file-explorer sidebars and standalone file viewer panels across open spaces.
#[derive(Default)]
pub(crate) struct FilePanesState {
    sidebars: HashMap<(SpaceId, String), SidebarTreeUi>,
    active_repo: HashMap<SpaceId, String>,
    panels: HashMap<PaneKey, FilePanelUi>,
}

impl FilePanesState {
    pub(crate) fn close_space(&mut self, id: &SpaceId) {
        self.sidebars.retain(|(space, _), _| space != id);
        self.active_repo.remove(id);
        self.panels.retain(|(space, _), _| space != id);
    }

    pub(crate) fn close_pane(&mut self, key: &PaneKey) {
        self.panels.remove(key);
    }
}

fn to_file_mark(change: GitChange) -> FileMark {
    match change {
        GitChange::Modified => FileMark::Modified,
        GitChange::Added => FileMark::Added,
        GitChange::Deleted => FileMark::Deleted,
        GitChange::Renamed => FileMark::Renamed,
        GitChange::Untracked => FileMark::Untracked,
        GitChange::Conflicted => FileMark::Conflicted,
    }
}

fn to_sidebar_mark(change: GitChange) -> FileGitMark {
    match change {
        GitChange::Modified => FileGitMark::Modified,
        GitChange::Added => FileGitMark::Added,
        GitChange::Deleted => FileGitMark::Deleted,
        GitChange::Renamed => FileGitMark::Renamed,
        GitChange::Untracked => FileGitMark::Untracked,
        GitChange::Conflicted => FileGitMark::Conflicted,
    }
}

fn panel_text_body(text: &PreviewText, as_markdown: bool) -> (PreviewBody, Option<SharedString>) {
    match text {
        PreviewText::Binary => (PreviewBody::Binary, None),
        PreviewText::Text { text, cut } => {
            let body = if as_markdown {
                PreviewBody::Markdown(markdown_blocks(text))
            } else {
                PreviewBody::Lines(text.lines().map(|line| line.to_owned().into()).collect())
            };
            let lines = text.lines().count();
            let note = cut.then(|| {
                if lines >= FILES_PREVIEW_LINES {
                    format!("Showing first {FILES_PREVIEW_LINES} lines").into()
                } else {
                    format!("Showing first {} KiB", FILES_PREVIEW_BYTES / 1024).into()
                }
            });
            (body, note)
        }
    }
}

impl Workspace {
    /// Ensure every persisted or newly added `PaneKind::File` pane in open space `id` has a
    /// `FilePanelUi`, and drop state for closed file panes.
    pub(crate) fn sync_file_panes(&mut self, id: &SpaceId, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(id) else {
            return;
        };
        let space = tab.space.clone();
        let live_panes: Vec<(PaneId, PaneRole)> = tab
            .layout
            .panes()
            .into_iter()
            .filter(|p| p.role.kind == PaneKind::File)
            .map(|p| (p.id, p.role))
            .collect();
        let live_ids: HashSet<PaneId> = live_panes.iter().map(|(pid, _)| *pid).collect();
        self.file_panes
            .panels
            .retain(|(s, pid), _| s != id || live_ids.contains(pid));

        for (pane_id, role) in live_panes {
            let key = (id.clone(), pane_id);
            if self.file_panes.panels.contains_key(&key) {
                continue;
            }
            let Some(file_spec) = role.file else {
                continue;
            };
            if let Some(panel) = self.build_file_panel(&space, &role.repo, &file_spec, None) {
                self.file_panes.panels.insert(key.clone(), panel);
                self.file_panel_reload(&key, cx);
            }
        }
    }

    fn build_file_panel(
        &self,
        space: &xctx_core::Space,
        repo_name: &str,
        file_spec: &str,
        target_line: Option<usize>,
    ) -> Option<FilePanelUi> {
        let spec_path = Path::new(file_spec);
        if spec_path.is_absolute() {
            let parent = spec_path.parent()?.to_path_buf();
            let name = spec_path.file_name()?.to_string_lossy().into_owned();
            let display = self.redactor.redact(file_spec).into_owned();
            return Some(FilePanelUi::new(
                parent,
                name,
                display,
                repo_name.to_owned(),
                false,
                target_line,
            ));
        }
        let repo = space
            .repos
            .iter()
            .find(|r| r.name == repo_name)
            .or_else(|| space.repos.first())?;
        let mut ui = FilePanelUi::new(
            repo.worktree.clone(),
            file_spec.to_owned(),
            file_spec.to_owned(),
            repo.name.clone(),
            true,
            target_line,
        );
        if let Some(sidebar) = self
            .file_panes
            .sidebars
            .get(&(space.id.clone(), repo.name.clone()))
        {
            ui.change = sidebar.tree.change_of(file_spec);
        }
        Some(ui)
    }

    /// Open the side Files explorer for open space `id`, optionally selecting `repo`.
    pub(crate) fn show_files_sidebar(
        &mut self,
        id: &SpaceId,
        repo: Option<&str>,
        cx: &mut Context<Self>,
    ) {
        if let Some(repo) = repo {
            self.file_panes
                .active_repo
                .insert(id.clone(), repo.to_owned());
        }
        self.show_inspector_tab(InspectorTab::Files, cx);
        self.ensure_sidebar_loaded(id, cx);
    }

    /// Toggle the side Files explorer (Cmd+Shift+E): close Inspector if already showing Files,
    /// otherwise open Inspector on the Files tab.
    pub(crate) fn toggle_files_sidebar(&mut self, cx: &mut Context<Self>) {
        if self.prefs.inspector_open && self.inspector_tab == InspectorTab::Files {
            self.set_inspector_open(false, cx);
        } else if let Some(id) = self.active_space_id() {
            self.show_files_sidebar(&id, None, cx);
        } else {
            self.show_inspector_tab(InspectorTab::Files, cx);
        }
    }

    /// Reveal and expand ancestor folders of `relative_path` in `repo` in the side Files explorer.
    pub(crate) fn reveal_in_files_sidebar(
        &mut self,
        id: &SpaceId,
        repo: &str,
        relative_path: &str,
        cx: &mut Context<Self>,
    ) {
        self.file_panes
            .active_repo
            .insert(id.clone(), repo.to_owned());
        self.show_inspector_tab(InspectorTab::Files, cx);
        self.ensure_sidebar_loaded(id, cx);

        let key = (id.clone(), repo.to_owned());
        let mut to_read = Vec::new();
        if let Some(ui) = self.file_panes.sidebars.get_mut(&key) {
            let mut prefix = String::new();
            let parts: Vec<&str> = relative_path
                .trim_matches('/')
                .split('/')
                .filter(|p| !p.is_empty())
                .collect();
            // Expand every ancestor folder (and if the path itself is a directory, expand it too).
            let is_dir = ui.tree.root().join(relative_path).is_dir();
            let count = if is_dir {
                parts.len()
            } else {
                parts.len().saturating_sub(1)
            };
            for part in parts.into_iter().take(count) {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                if ui.tree.open_folder(&prefix) {
                    to_read.push(prefix.clone());
                }
            }
        }
        for rel in to_read {
            self.sidebar_read_folder(&key, rel, cx);
        }
        cx.notify();
    }

    /// Ensure the active repo's side file tree for space `id` exists and has kicked off its initial load.
    pub(crate) fn ensure_sidebar_loaded(&mut self, id: &SpaceId, cx: &mut Context<Self>) {
        let Some((repo_name, root)) = self.active_sidebar_repo(id) else {
            return;
        };
        let key = (id.clone(), repo_name);
        let needs_init = match self.file_panes.sidebars.get_mut(&key) {
            Some(ui) if ui.initialized => false,
            Some(ui) => {
                ui.initialized = true;
                true
            }
            None => {
                let shown = self
                    .redactor
                    .redact(&root.display().to_string())
                    .into_owned();
                let mut ui = SidebarTreeUi::new(root, shown);
                ui.initialized = true;
                self.file_panes.sidebars.insert(key.clone(), ui);
                true
            }
        };
        if needs_init {
            self.sidebar_refresh(&key, cx);
        }
    }

    fn active_sidebar_repo(&self, id: &SpaceId) -> Option<(String, PathBuf)> {
        let tab = self.tabs.get(id)?;
        let preferred = self
            .file_panes
            .active_repo
            .get(id)
            .map(String::as_str)
            .or_else(|| {
                tab.layout
                    .role(tab.layout.focused())
                    .map(|r| r.repo.as_str())
            });
        let repo = preferred
            .and_then(|name| tab.space.repos.iter().find(|r| r.name == name))
            .or_else(|| tab.space.repos.first())?;
        Some((repo.name.clone(), repo.worktree.clone()))
    }

    /// Build [`InspectorFiles`] for space `id` to render in the Inspector's `Files` tab.
    pub(crate) fn inspector_files_data(
        &mut self,
        id: &SpaceId,
        cx: &mut Context<Self>,
    ) -> InspectorFiles {
        if self.inspector_tab == InspectorTab::Files {
            self.ensure_sidebar_loaded(id, cx);
        }
        let Some(tab) = self.tabs.get(id) else {
            return InspectorFiles::default();
        };
        let repos: Vec<SharedString> = tab
            .space
            .repos
            .iter()
            .map(|r| r.name.clone().into())
            .collect();
        let Some((active_repo, _)) = self.active_sidebar_repo(id) else {
            return InspectorFiles::default();
        };

        let focused_pane = tab.layout.focused();
        let active_file: Option<SharedString> = tab.layout.role(focused_pane).and_then(|role| {
            (role.kind == PaneKind::File && role.repo == active_repo)
                .then(|| role.file.clone().map(Into::into))
                .flatten()
        });
        let open_files: HashSet<String> = tab
            .layout
            .panes()
            .into_iter()
            .filter(|p| p.role.kind == PaneKind::File && p.role.repo == active_repo)
            .filter_map(|p| p.role.file)
            .collect();

        let key = (id.clone(), active_repo.clone());
        let Some(ui) = self.file_panes.sidebars.get(&key) else {
            return InspectorFiles {
                repos,
                active_repo: active_repo.into(),
                busy: true,
                ..InspectorFiles::default()
            };
        };
        let rows = ui
            .tree
            .rows(ui.changed_only)
            .into_iter()
            .map(|row| SidebarFileRow {
                kind: match row.kind {
                    TreeRowKind::Folder {
                        open,
                        changed,
                        heavy,
                    } => SidebarFileRowKind::Folder {
                        open,
                        changed,
                        heavy,
                    },
                    TreeRowKind::File(change) => SidebarFileRowKind::File {
                        mark: change.map(to_sidebar_mark),
                        open_in_pane: open_files.contains(&row.path),
                    },
                    TreeRowKind::More(n) => SidebarFileRowKind::More(n),
                    TreeRowKind::Loading => SidebarFileRowKind::Loading,
                    TreeRowKind::Refused(why) => SidebarFileRowKind::Refused(why.into()),
                },
                path: row.path.into(),
                name: row.name.into(),
                depth: row.depth,
            })
            .collect();

        InspectorFiles {
            repos,
            active_repo: active_repo.into(),
            root_label: ui.root_shown.clone(),
            rows,
            changed_only: ui.changed_only,
            busy: ui.tree.is_reading() || ui.status_reading,
            status_error: ui.tree.status_error().map(|e| e.to_owned().into()),
            active_file,
        }
    }

    pub(crate) fn sidebar_select_repo(
        &mut self,
        id: &SpaceId,
        repo: String,
        cx: &mut Context<Self>,
    ) {
        self.file_panes.active_repo.insert(id.clone(), repo);
        self.ensure_sidebar_loaded(id, cx);
        cx.notify();
    }

    pub(crate) fn sidebar_toggle_folder(
        &mut self,
        id: &SpaceId,
        path: &str,
        cx: &mut Context<Self>,
    ) {
        let Some((repo, _)) = self.active_sidebar_repo(id) else {
            return;
        };
        let key = (id.clone(), repo);
        let should_read = self
            .file_panes
            .sidebars
            .get_mut(&key)
            .is_some_and(|ui| ui.tree.toggle(path));
        if should_read {
            self.sidebar_read_folder(&key, path.to_owned(), cx);
        }
        cx.notify();
    }

    pub(crate) fn sidebar_set_changed_only(
        &mut self,
        id: &SpaceId,
        on: bool,
        cx: &mut Context<Self>,
    ) {
        let Some((repo, _)) = self.active_sidebar_repo(id) else {
            return;
        };
        let key = (id.clone(), repo);
        if let Some(ui) = self.file_panes.sidebars.get_mut(&key) {
            ui.changed_only = on;
        }
        cx.notify();
    }

    pub(crate) fn sidebar_refresh_active(&mut self, id: &SpaceId, cx: &mut Context<Self>) {
        self.ensure_sidebar_loaded(id, cx);
        let Some((repo, _)) = self.active_sidebar_repo(id) else {
            return;
        };
        let key = (id.clone(), repo);
        self.sidebar_refresh(&key, cx);
    }

    fn sidebar_refresh(&mut self, key: &(SpaceId, String), cx: &mut Context<Self>) {
        let redactor = self.redactor.clone();
        let Some(ui) = self.file_panes.sidebars.get_mut(key) else {
            return;
        };
        let folders = ui.tree.refresh();
        let root = ui.tree.root().to_path_buf();
        ui.status_reading = true;
        for rel in folders {
            self.sidebar_read_folder(key, rel, cx);
        }
        let key_owned = key.clone();
        let background = cx.background_executor().clone();
        cx.spawn(async move |ws, cx| {
            let status = background
                .spawn(
                    async move { git_status(&root).map_err(|e| redactor.redact(&e).into_owned()) },
                )
                .await;
            let _ = ws.update(cx, |this, cx| {
                if let Some(ui) = this.file_panes.sidebars.get_mut(&key_owned) {
                    ui.status_reading = false;
                    ui.tree.set_status(status);
                }
                // Propagate fresh git marks to open file panels in this repo.
                let (space_id, repo_name) = &key_owned;
                let changes: HashMap<String, Option<GitChange>> = this
                    .file_panes
                    .sidebars
                    .get(&key_owned)
                    .map(|ui| {
                        this.file_panes
                            .panels
                            .iter()
                            .filter(|((s, _), p)| {
                                s == space_id && &p.repo == repo_name && p.in_worktree
                            })
                            .map(|(_, p)| {
                                (p.relative_path.clone(), ui.tree.change_of(&p.relative_path))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                for ((s, _), panel) in &mut this.file_panes.panels {
                    if s == space_id
                        && &panel.repo == repo_name
                        && panel.in_worktree
                        && let Some(change) = changes.get(&panel.relative_path)
                    {
                        panel.change = *change;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn sidebar_read_folder(
        &mut self,
        key: &(SpaceId, String),
        rel: String,
        cx: &mut Context<Self>,
    ) {
        let redactor = self.redactor.clone();
        let Some(ui) = self.file_panes.sidebars.get(key) else {
            return;
        };
        let root = ui.tree.root().to_path_buf();
        let key_owned = key.clone();
        let at = rel.clone();
        let background = cx.background_executor().clone();
        cx.spawn(async move |ws, cx| {
            let listing = background
                .spawn(async move {
                    list_folder(&root, &at).map_err(|e| redactor.redact(&e).into_owned())
                })
                .await;
            let _ = ws.update(cx, |this, cx| {
                if let Some(ui) = this.file_panes.sidebars.get_mut(&key_owned) {
                    ui.tree.set_listing(&rel, listing);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Open `file_spec` (relative to `repo`'s worktree, or an absolute path) as a standalone
    /// `PaneKind::File` split panel in space `id`. If that file is already open in a panel, focus it
    /// and jump to `target_line`.
    pub(crate) fn open_file_pane(
        &mut self,
        id: &SpaceId,
        repo: &str,
        file_spec: &str,
        target_line: Option<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(id) else {
            return;
        };
        let space = tab.space.clone();
        // Reuse an already-open panel for the same file.
        if let Some(existing) = tab
            .layout
            .panes()
            .into_iter()
            .find(|p| {
                p.role.kind == PaneKind::File
                    && p.role.repo == repo
                    && p.role.file.as_deref() == Some(file_spec)
            })
            .map(|p| p.id)
        {
            let key = (id.clone(), existing);
            if let Some(panel) = self.file_panes.panels.get_mut(&key)
                && target_line.is_some()
            {
                panel.target_line = target_line;
            }
            let _ = self.focus_pane_in(id, existing, window, cx);
            cx.notify();
            return;
        }

        let Some(panel) = self.build_file_panel(&space, repo, file_spec, target_line) else {
            self.error(format!("Could not open {file_spec}"), cx);
            return;
        };
        let role = PaneRole::for_file(file_spec, repo);
        let added = match self.tabs.get_mut(id) {
            Some(tab) => tab.layout.add_file_pane(role),
            None => return,
        };
        let pane = match added {
            Ok(pane) => pane,
            Err(e) => {
                self.error(format!("Could not open panel: {e}"), cx);
                return;
            }
        };
        let key = (id.clone(), pane.id);
        self.file_panes.panels.insert(key.clone(), panel);
        self.file_panel_reload(&key, cx);
        self.layout_changed(id, window, cx);
    }

    fn file_panel_reload(&mut self, key: &PaneKey, cx: &mut Context<Self>) {
        let redactor = self.redactor.clone();
        let Some(panel) = self.file_panes.panels.get_mut(key) else {
            return;
        };
        panel.generation = panel.generation.wrapping_add(1);
        let generation = panel.generation;
        panel.preview = None;
        panel.diff_text = None;
        panel.diff_reading = false;
        let root = panel.root.clone();
        let rel = panel.relative_path.clone();
        let in_worktree = panel.in_worktree;
        let key_owned = key.clone();
        let background = cx.background_executor().clone();
        cx.spawn(async move |ws, cx| {
            let (preview, status) = background
                .spawn(async move {
                    let preview = read_preview(&root, &rel, &redactor)
                        .map_err(|e| redactor.redact(&e).into_owned());
                    let status = if in_worktree {
                        git_status(&root).ok().and_then(|map| map.of(&rel))
                    } else {
                        None
                    };
                    (preview, status)
                })
                .await;
            let _ = ws.update(cx, |this, cx| {
                let mut should_diff = false;
                if let Some(panel) = this.file_panes.panels.get_mut(&key_owned)
                    && panel.generation == generation
                {
                    panel.preview = Some(preview);
                    if in_worktree {
                        panel.change = status;
                    }
                    should_diff = panel.diff && panel.change.is_some();
                }
                if should_diff {
                    this.file_panel_read_diff(&key_owned, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn file_panel_read_diff(&mut self, key: &PaneKey, cx: &mut Context<Self>) {
        let redactor = self.redactor.clone();
        let Some(panel) = self.file_panes.panels.get_mut(key) else {
            return;
        };
        let Some(change) = panel.change else {
            return;
        };
        if !panel.in_worktree || !panel.diff || panel.diff_text.is_some() || panel.diff_reading {
            return;
        }
        panel.diff_reading = true;
        let generation = panel.generation;
        let root = panel.root.clone();
        let rel = panel.relative_path.clone();
        let key_owned = key.clone();
        let background = cx.background_executor().clone();
        cx.spawn(async move |ws, cx| {
            let diff = background
                .spawn(async move {
                    file_diff(&root, &rel, change, &redactor)
                        .map_err(|e| redactor.redact(&e).into_owned())
                })
                .await;
            let _ = ws.update(cx, |this, cx| {
                if let Some(panel) = this.file_panes.panels.get_mut(&key_owned)
                    && panel.generation == generation
                {
                    panel.diff_reading = false;
                    panel.diff_text = Some(diff);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn on_file_panel_action(
        &mut self,
        key: &PaneKey,
        action: FilePanelAction,
        cx: &mut Context<Self>,
    ) {
        match action {
            FilePanelAction::ToggleDiff(on) => {
                if let Some(panel) = self.file_panes.panels.get_mut(key) {
                    panel.diff = on;
                }
                if on {
                    self.file_panel_read_diff(key, cx);
                }
                cx.notify();
            }
            FilePanelAction::ToggleRawMarkdown(raw) => {
                if let Some(panel) = self.file_panes.panels.get_mut(key) {
                    panel.raw_markdown = raw;
                }
                cx.notify();
            }
            FilePanelAction::RevealInSidebar => {
                let target = self
                    .file_panes
                    .panels
                    .get(key)
                    .filter(|p| p.in_worktree)
                    .map(|p| (p.repo.clone(), p.relative_path.clone()));
                if let Some((repo, rel)) = target {
                    self.reveal_in_files_sidebar(&key.0, &repo, &rel, cx);
                } else {
                    self.info("This file is outside the space's worktree", cx);
                }
            }
            FilePanelAction::CopyPath => {
                if let Some(panel) = self.file_panes.panels.get(key) {
                    let path = panel.display_path.clone();
                    cx.write_to_clipboard(ClipboardItem::new_string(path));
                    self.info("Copied file path", cx);
                }
            }
            FilePanelAction::CopyContent => {
                if let Some(text) = self
                    .file_panes
                    .panels
                    .get(key)
                    .and_then(FilePanelUi::copyable_text)
                {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                    self.info("Copied file contents", cx);
                }
            }
            FilePanelAction::Reload => {
                self.file_panel_reload(key, cx);
                cx.notify();
            }
        }
    }

    /// Render a standalone `PaneKind::File` viewer panel, if hydrated.
    pub(crate) fn render_file_pane(
        &self,
        id: &SpaceId,
        pane: PaneId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let key: PaneKey = (id.clone(), pane);
        let panel = self.file_panes.panels.get(&key)?;
        let data = panel.data();
        let document = matches!(data.body, PreviewBody::Document)
            .then(|| {
                let path = panel.full_path();
                self.browsers
                    .document(id, pane, &path, &panel.root)
                    .ok()
                    .map(|view| page_area(view, self.pages_may_show()))
            })
            .flatten();
        let ws = cx.entity().downgrade();
        let action_key = key.clone();
        Some(
            div()
                .id(SharedString::from(format!("file-pane-{}", pane.0)))
                .size_full()
                .min_h_0()
                .child(FilePanelView::new(data).document(document).on_action(
                    move |action, _window, cx| {
                        let _ = ws.update(cx, |this, cx| {
                            this.on_file_panel_action(&action_key, action, cx);
                        });
                    },
                ))
                .into_any_element(),
        )
    }

    /// Handle a Cmd+Click link/file event emitted by a terminal pane in space `id`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn handle_terminal_open_link(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        target: &str,
        kind: &TerminalLinkKind,
        working_dir: Option<&Path>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match kind {
            TerminalLinkKind::Url | TerminalLinkKind::Osc8 => {
                if target.starts_with("http://") || target.starts_with("https://") {
                    cx.open_url(target);
                }
            }
            TerminalLinkKind::Path { path, line, .. } => {
                let Some(tab) = self.tabs.get(id) else {
                    return;
                };
                let space = tab.space.clone();
                let pane_repo_name = tab
                    .layout
                    .role(pane)
                    .map(|r| r.repo.clone())
                    .or_else(|| space.repos.first().map(|r| r.name.clone()))
                    .unwrap_or_default();
                let pane_worktree = space
                    .repos
                    .iter()
                    .find(|r| r.name == pane_repo_name)
                    .map(|r| r.worktree.as_path());

                let Some(resolved) =
                    resolve_terminal_path(path, working_dir, pane_worktree, &space.repos)
                else {
                    self.info(format!("Not found: {path}"), cx);
                    return;
                };

                // Check if the resolved path belongs to any repo worktree in this space.
                let matched_repo = space.repos.iter().find_map(|repo| {
                    let canon_root = repo
                        .worktree
                        .canonicalize()
                        .unwrap_or_else(|_| repo.worktree.clone());
                    let rel = resolved.strip_prefix(&canon_root).ok()?;
                    let rel_str = rel.to_string_lossy().replace('\\', "/");
                    Some((repo.name.clone(), rel_str))
                });

                if resolved.is_dir() {
                    if let Some((repo_name, rel)) = matched_repo {
                        self.reveal_in_files_sidebar(id, &repo_name, &rel, cx);
                    } else {
                        self.info(
                            format!("Directory outside space: {}", resolved.display()),
                            cx,
                        );
                    }
                    return;
                }

                if resolved.is_file() {
                    match matched_repo {
                        Some((repo_name, rel)) if !rel.is_empty() => {
                            self.open_file_pane(id, &repo_name, &rel, *line, window, cx);
                        }
                        _ => {
                            let abs = resolved.to_string_lossy().into_owned();
                            self.open_file_pane(id, &pane_repo_name, &abs, *line, window, cx);
                        }
                    }
                }
            }
        }
    }
}

/// Resolve a clicked terminal path (relative, `./`, `~/`, bare `ls` filename, or absolute) against
/// the terminal's live working directory, the pane's repo worktree, and all space repos.
pub(crate) fn resolve_terminal_path(
    raw: &str,
    working_dir: Option<&Path>,
    pane_worktree: Option<&Path>,
    repos: &[xctx_core::SpaceRepo],
) -> Option<PathBuf> {
    let expanded = if let Some(rest) = raw.strip_prefix("~/") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(rest))
    } else {
        None
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(p) = expanded {
        candidates.push(p);
    } else {
        let path = Path::new(raw);
        if path.is_absolute() {
            candidates.push(path.to_path_buf());
        } else {
            if let Some(cwd) = working_dir {
                candidates.push(cwd.join(path));
            }
            if let Some(wt) = pane_worktree {
                candidates.push(wt.join(path));
            }
            for repo in repos {
                candidates.push(repo.worktree.join(path));
            }
        }
    }

    for candidate in candidates {
        if candidate.exists() {
            return Some(candidate.canonicalize().unwrap_or(candidate));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space_layout::SpaceLayout;
    use xctx_layout::Shape;

    #[test]
    fn resolve_terminal_path_prefers_live_cwd_then_worktree() {
        let dir = tempfile::tempdir().expect("tempdir");
        let worktree = dir.path().join("repo");
        let sub = worktree.join("crates/app");
        std::fs::create_dir_all(&sub).expect("mkdir");
        let root_file = worktree.join("Cargo.toml");
        let sub_file = sub.join("Cargo.toml");
        std::fs::write(&root_file, "[workspace]\n").expect("write root");
        std::fs::write(&sub_file, "[package]\n").expect("write sub");

        let repos = vec![xctx_core::SpaceRepo {
            name: "app".into(),
            repo: worktree.clone(),
            worktree: worktree.clone(),
            branch: "main".into(),
            base_branch: "main".into(),
            base_commit: "0000000".into(),
            branch_created: false,
            reason: String::new(),
        }];

        // When the terminal cd'd into `crates/app` and ran `ls`, Cmd+clicking `Cargo.toml`
        // resolves to `crates/app/Cargo.toml`, not the workspace root `Cargo.toml`.
        let resolved =
            resolve_terminal_path("Cargo.toml", Some(&sub), Some(&worktree), &repos).unwrap();
        assert_eq!(resolved, sub_file.canonicalize().unwrap());

        // Relative path from repo root still resolves even if cwd is elsewhere.
        let resolved_rel =
            resolve_terminal_path("crates/app/Cargo.toml", Some(&sub), Some(&worktree), &repos)
                .unwrap();
        assert_eq!(resolved_rel, sub_file.canonicalize().unwrap());

        assert!(
            resolve_terminal_path("does-not-exist.rs", Some(&sub), Some(&worktree), &repos)
                .is_none()
        );
    }

    #[test]
    fn file_panes_open_beside_workspace_and_survive_reshape() {
        let (mut layout, _) = SpaceLayout::launch(Shape::Dev, &["api".into()]).expect("launch");
        let file_pane = layout
            .add_file_pane(PaneRole::for_file("src/main.rs", "api"))
            .expect("add file pane");
        assert_eq!(layout.focused(), file_pane.id);
        assert_eq!(
            layout.role(file_pane.id),
            Some(&PaneRole::for_file("src/main.rs", "api"))
        );

        let reshaped = layout
            .reshape(Shape::Review, &["api".into()])
            .expect("reshape");
        assert!(
            reshaped
                .panes
                .iter()
                .any(|p| p.id == file_pane.id && p.role.kind == PaneKind::File)
        );
        assert!(!reshaped.removed.iter().any(|p| p.id == file_pane.id));
    }
}
