//! Home: the task prompt, the draft it makes and its launch, the living spaces, and the indexed folders.

use std::path::{Path, PathBuf};

use gpui::{
    AnyElement, Context, Div, Focusable, FontWeight, IntoElement, ParentElement, Styled, Window,
    div, prelude::*,
};
use xctx_core::{DraftRepo, Space, SpaceDraft};
use xctx_ui::{Badge, Button, ButtonSize, ButtonVariant, Theme, Tone, shape_label};

use crate::lifecycle::{
    ADDED_BY_HAND, addable_repos, folder_to_index, home_dir, launch_space, repo_for_draft,
};
use crate::space_layout::layout_shape;
use crate::workspace::Workspace;

/// How many indexed repos the draft offers to add with one click.
const ADDABLE_REPOS_SHOWN: usize = 8;

/// The draft shown on Home, with an id that changes whenever a new draft replaces it or it is thrown away, so a
/// launch that finishes after the user started another draft leaves that one alone.
#[derive(Debug, Default)]
pub(crate) struct HomeDraft {
    draft: Option<SpaceDraft>,
    id: u64,
}

impl HomeDraft {
    /// The draft, if there is one.
    pub(crate) fn get(&self) -> Option<&SpaceDraft> {
        self.draft.as_ref()
    }

    /// The draft, to add or drop repos; editing it keeps its id.
    pub(crate) fn get_mut(&mut self) -> Option<&mut SpaceDraft> {
        self.draft.as_mut()
    }

    /// The id of the draft shown now.
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    /// Show `draft` instead (or none), under a new id.
    pub(crate) fn replace(&mut self, draft: Option<SpaceDraft>) {
        self.id += 1;
        self.draft = draft;
    }

    /// Throw the draft away if it is still the one that had `id`; returns whether it was.
    pub(crate) fn clear_if_current(&mut self, id: u64) -> bool {
        if self.id != id {
            return false;
        }
        self.replace(None);
        true
    }
}

impl Workspace {
    /// Draft a space for `task` from the repo index. While the index is being built, the task waits for it.
    pub(crate) fn submit_task(&mut self, task: String, cx: &mut Context<Self>) {
        let task = task.trim().to_owned();
        if task.is_empty() {
            return;
        }
        if self.scanning {
            self.pending_task = Some(task);
            cx.notify();
            return;
        }
        let draft = self.index.draft(&task);
        if draft.repos.is_empty() {
            let hint = if self.index.roots().is_empty() {
                "no folders are indexed yet; add one below, or add a repo path to the draft"
            } else {
                "no indexed repo matched; add a repo to the draft"
            };
            self.info(hint, cx);
        }
        self.draft.replace(Some(draft));
        cx.notify();
    }

    /// Throw the draft away and clear the prompt.
    pub(crate) fn cancel_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.draft.replace(None);
        self.pending_task = None;
        self.task_prompt.update(cx, |prompt, cx| prompt.clear(cx));
        self.repo_input.update(cx, |input, cx| input.clear(cx));
        let focus = self.task_prompt.focus_handle(cx);
        focus.focus(window, cx);
        cx.notify();
    }

    /// Index the folder the user typed, and rescan.
    pub(crate) fn add_root(&mut self, input: String, cx: &mut Context<Self>) {
        let folder = match folder_to_index(&input, home_dir().as_deref()) {
            Ok(folder) => folder,
            Err(e) => {
                self.error(e, cx);
                return;
            }
        };
        let Some(store) = self.store.as_mut() else {
            self.error("the store is busy or unavailable; try again", cx);
            return;
        };
        match store.add_root(folder.clone()) {
            Ok(true) => {
                self.root_input.update(cx, |input, cx| input.clear(cx));
                self.info(format!("indexing {}", folder.display()), cx);
                self.rescan(cx);
            }
            Ok(false) => self.info(format!("{} is already indexed", folder.display()), cx),
            Err(e) => self.error(format!("could not add {}: {e}", folder.display()), cx),
        }
    }

    fn remove_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        let Some(store) = self.store.as_mut() else {
            self.error("the store is busy or unavailable; try again", cx);
            return;
        };
        match store.remove_root(&root) {
            Ok(_) => {
                self.info(format!("stopped indexing {}", root.display()), cx);
                self.rescan(cx);
            }
            Err(e) => self.error(format!("could not remove {}: {e}", root.display()), cx),
        }
    }

    /// Add the repo at the path the user typed to the draft.
    pub(crate) fn add_repo_path(&mut self, input: String, cx: &mut Context<Self>) {
        if self.draft.get().is_none() {
            self.info("type a task first; repos are added to its draft", cx);
            return;
        }
        match repo_for_draft(&input, home_dir().as_deref()) {
            Ok(repo) => {
                self.repo_input.update(cx, |input, cx| input.clear(cx));
                self.add_to_draft(repo, cx);
            }
            Err(e) => self.error(e, cx),
        }
    }

    fn add_to_draft(&mut self, repo: DraftRepo, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.get_mut() else {
            return;
        };
        let name = repo.name.clone();
        if !draft.add_repo(repo) {
            self.info(format!("{name} is already in the draft"), cx);
        }
        cx.notify();
    }

    fn drop_from_draft(&mut self, path: &Path, cx: &mut Context<Self>) {
        if let Some(draft) = self.draft.get_mut() {
            draft.drop_repo(path);
            cx.notify();
        }
    }

    /// Launch the draft in the background: worktrees, mise files and the space; then open its tab with the
    /// agents given the task. The draft and prompt are cleared then, unless the user has started another draft.
    pub(crate) fn launch_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(draft) = self.draft.get().cloned() else {
            return;
        };
        let draft_id = self.draft.id();
        if draft.repos.is_empty() {
            self.error("add at least one repo to the draft before launching", cx);
            return;
        }
        let Some(mut store) = self.take_store(&format!("launching {}", draft.title), cx) else {
            return;
        };
        let executor = cx.background_executor().clone();
        cx.spawn_in(window, async move |this, cx| {
            let (store, result) = executor
                .spawn(async move {
                    let result = launch_space(&mut store, draft);
                    (store, result)
                })
                .await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.return_store(store, cx);
                match result {
                    Ok(launched) => {
                        let id = launched.space.id.clone();
                        let port = launched.space.port;
                        if this.draft.clear_if_current(draft_id) {
                            this.task_prompt.update(cx, |prompt, cx| prompt.clear(cx));
                        }
                        this.refresh_spaces();
                        this.open_launched_space(
                            launched.space,
                            launched.layout,
                            launched.panes,
                            window,
                            cx,
                        );
                        this.show_space(&id, window, cx);
                        match launched.warning {
                            Some(warning) => this.error(warning, cx),
                            None => this.info(format!("launched {id} on PORT {port}"), cx),
                        }
                    }
                    Err(e) => this.error(e, cx),
                }
            });
        })
        .detach();
    }

    /// Home's content.
    pub(crate) fn render_home(&self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx);
        let mut column = div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_6)
            .w_full()
            .max_w(gpui::px(880.0))
            .px(Theme::SPACE_6)
            .py(Theme::SPACE_6)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_2)
                    .child(
                        div()
                            .text_size(Theme::TEXT_XL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("What are you working on?"),
                    )
                    .child(self.task_prompt.clone())
                    .child(
                        div()
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.text_subtle)
                            .child("Enter drafts a space from your indexed repos. Nothing is created until you launch it."),
                    ),
            );
        if let Some(error) = &self.store_error {
            column = column.child(notice(error.clone(), theme.danger, &theme));
        }
        if let Some(busy) = &self.busy {
            column = column.child(notice(format!("{busy}…"), theme.text_muted, &theme));
        }
        if let Some(draft) = self.draft.get() {
            column = column.child(self.render_draft(draft, cx));
        } else if let Some(task) = &self.pending_task {
            column = column.child(notice(
                format!("indexing your folders before drafting “{task}”…"),
                theme.text_muted,
                &theme,
            ));
        }
        column = column
            .child(self.render_spaces(cx))
            .child(self.render_roots(cx));
        div()
            .id("home")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .justify_center()
            .child(column)
            .into_any_element()
    }

    fn render_draft(&self, draft: &SpaceDraft, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx);
        let slug = self
            .store
            .as_ref()
            .map(|store| store.unique_slug(&draft.task));
        let mut repos = div().flex().flex_col().gap(Theme::SPACE_1);
        if draft.repos.is_empty() {
            repos = repos.child(
                div()
                    .text_color(theme.text_muted)
                    .child("No repos yet. Add one below."),
            );
        }
        for (index, repo) in draft.repos.iter().enumerate() {
            let path = repo.path.clone();
            repos = repos.child(
                row(&theme)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .gap(Theme::SPACE_2)
                                    .items_center()
                                    .child(
                                        div()
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(repo.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(Theme::TEXT_SM)
                                            .text_color(theme.text_subtle)
                                            .truncate()
                                            .child(repo.path.display().to_string()),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(Theme::TEXT_SM)
                                    .text_color(theme.text_muted)
                                    .child(if repo.reason.is_empty() {
                                        "matched".to_owned()
                                    } else {
                                        repo.reason.clone()
                                    }),
                            ),
                    )
                    .child(
                        Button::new(("drop-repo", index), "Drop")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.drop_from_draft(&path, cx)),
                            ),
                    ),
            );
        }
        let mut addable = div().flex().flex_wrap().gap(Theme::SPACE_1);
        let candidates = addable_repos(&self.index, draft);
        for (index, repo) in candidates.iter().take(ADDABLE_REPOS_SHOWN).enumerate() {
            let candidate = DraftRepo::new(repo.path.clone(), ADDED_BY_HAND);
            addable = addable.child(
                Button::new(("add-repo", index), format!("+ {}", repo.name))
                    .variant(ButtonVariant::Secondary)
                    .size(ButtonSize::Compact)
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.add_to_draft(candidate.clone(), cx)),
                    ),
            );
        }
        let branch = slug
            .as_ref()
            .map_or_else(|| "space/<slug>".to_owned(), |slug| format!("space/{slug}"));
        card(&theme)
            .border_color(theme.accent)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(
                        div()
                            .text_size(Theme::TEXT_LG)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(format!("Draft: {}", draft.title)),
                    )
                    .child(Badge::new(shape_label(layout_shape(draft.shape))).tone(Tone::Accent))
                    .child(
                        div()
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.text_subtle)
                            .child(format!("branch {branch}")),
                    ),
            )
            .child(
                div()
                    .text_size(Theme::TEXT_SM)
                    .text_color(theme.text_muted)
                    .child(format!(
                        "Launching makes a worktree on {branch} in each repo, a git-ignored mise file with its own PORT, and starts pi with your task."
                    )),
            )
            .child(repos)
            .when(!candidates.is_empty(), |card| {
                card.child(section_label("Add an indexed repo", &theme))
                    .child(addable)
            })
            .child(section_label("Add a repo by path", &theme))
            .child(self.repo_input.clone())
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(Theme::SPACE_2)
                    .child(
                        Button::new("cancel-draft", "Cancel")
                            .variant(ButtonVariant::Ghost)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.cancel_draft(window, cx)
                            })),
                    )
                    .child(
                        Button::new("launch-draft", "Launch  ⌘⏎")
                            .variant(ButtonVariant::Primary)
                            .disabled(draft.repos.is_empty() || self.busy.is_some())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.launch_draft(window, cx)
                            })),
                    ),
            )
    }

    fn render_spaces(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx);
        let mut list = div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_2)
            .child(section_title("Living spaces", &theme));
        if self.spaces.is_empty() {
            list = list.child(
                div()
                    .text_color(theme.text_muted)
                    .child("No spaces yet. Type a task above to start one."),
            );
        }
        for (index, space) in self.spaces.iter().enumerate() {
            let id = space.id.clone();
            let running = self
                .tabs
                .get(&space.id)
                .is_some_and(|tab| tab.agent_running(cx));
            list = list.child(
                card(&theme)
                    .id(("space-card", index))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.surface_hover))
                    .on_click(
                        cx.listener(move |this, _, window, cx| this.show_space(&id, window, cx)),
                    )
                    .child(space_summary(space, running, index, &theme)),
            );
        }
        list
    }

    fn render_roots(&self, cx: &mut Context<Self>) -> Div {
        let theme = Theme::of(cx);
        let roots = self.roots.clone();
        let status = if self.scanning {
            "indexing…".to_owned()
        } else {
            let count = self.index.repos().len();
            if count == 1 {
                "1 repo indexed".to_owned()
            } else {
                format!("{count} repos indexed")
            }
        };
        let mut list = div().flex().flex_col().gap(Theme::SPACE_2).child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(section_title("Indexed folders", &theme))
                .child(
                    div()
                        .flex_1()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_subtle)
                        .child(status),
                )
                .child(
                    Button::new("rescan", "Rescan")
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Compact)
                        .disabled(self.scanning)
                        .on_click(cx.listener(|this, _, _, cx| this.rescan(cx))),
                ),
        );
        if roots.is_empty() {
            list = list.child(notice(
                "xctx finds repos in the folders you index. Add the folder that holds your repos, such as ~/code or ~/Desktop.".to_owned(),
                theme.warning,
                &theme,
            ));
        }
        for (index, root) in roots.into_iter().enumerate() {
            let label = root.display().to_string();
            list = list.child(
                row(&theme)
                    .child(div().flex_1().truncate().child(label))
                    .child(
                        Button::new(("remove-root", index), "Remove")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.remove_root(root.clone(), cx)
                            })),
                    ),
            );
        }
        list.child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(div().flex_1().child(self.root_input.clone()))
                .child(
                    Button::new("add-root", "Add folder")
                        .variant(ButtonVariant::Secondary)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let input = this.root_input.read(cx).value().to_owned();
                            this.add_root(input, cx);
                        })),
                ),
        )
    }
}

/// A living space's card body: title, slug, shape, port, branch and repos.
fn space_summary(space: &Space, running: bool, index: usize, theme: &Theme) -> Div {
    let repos = space
        .repos
        .iter()
        .map(|r| r.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let shortcut = (index < 9).then(|| format!("⌘{}", index + 1));
    div()
        .flex()
        .flex_col()
        .gap(Theme::SPACE_1)
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(
                    div()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_size(Theme::TEXT_LG)
                        .child(space.title.clone()),
                )
                .child(Badge::new(shape_label(layout_shape(space.shape))).tone(Tone::Accent))
                .when(running, |row| {
                    row.child(Badge::new("running").tone(Tone::Success))
                })
                .child(div().flex_1())
                .children(shortcut.map(|keys| {
                    div()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_subtle)
                        .child(keys)
                })),
        )
        .child(
            div()
                .text_size(Theme::TEXT_SM)
                .text_color(theme.text_muted)
                .child(format!(
                    "{}  ·  {}  ·  PORT {}  ·  {}",
                    space.id,
                    space.branch(),
                    space.port,
                    repos
                )),
        )
}

fn card(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(Theme::SPACE_2)
        .p(Theme::SPACE_4)
        .rounded(Theme::RADIUS_LG)
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface)
}

fn row(theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .gap(Theme::SPACE_2)
        .px(Theme::SPACE_3)
        .py(Theme::SPACE_2)
        .rounded(Theme::RADIUS_MD)
        .bg(theme.elevated)
}

fn notice(message: String, color: gpui::Hsla, theme: &Theme) -> Div {
    div()
        .px(Theme::SPACE_3)
        .py(Theme::SPACE_2)
        .rounded(Theme::RADIUS_MD)
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface)
        .text_size(Theme::TEXT_SM)
        .text_color(color)
        .child(message)
}

fn section_title(title: &'static str, theme: &Theme) -> Div {
    div()
        .text_size(Theme::TEXT_LG)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text)
        .child(title)
}

fn section_label(label: &'static str, theme: &Theme) -> Div {
    div()
        .text_size(Theme::TEXT_SM)
        .text_color(theme.text_subtle)
        .child(label)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_clears_only_the_draft_it_launched() {
        let mut slot = HomeDraft::default();
        slot.replace(Some(SpaceDraft::new("add rate limiting")));
        let launched = slot.id();
        slot.replace(Some(SpaceDraft::new("fix the flaky test")));
        assert!(!slot.clear_if_current(launched));
        assert_eq!(
            slot.get().map(|d| d.task.as_str()),
            Some("fix the flaky test")
        );
        let current = slot.id();
        assert!(slot.clear_if_current(current));
        assert!(slot.get().is_none());
    }

    #[test]
    fn editing_a_draft_keeps_its_id_and_cancelling_changes_it() {
        let mut slot = HomeDraft::default();
        slot.replace(Some(SpaceDraft::new("add rate limiting")));
        let launched = slot.id();
        if let Some(draft) = slot.get_mut() {
            draft.add_repo(DraftRepo::new(PathBuf::from("/code/api"), ADDED_BY_HAND));
        }
        assert_eq!(slot.id(), launched);
        slot.replace(None);
        slot.replace(Some(SpaceDraft::new("add rate limiting")));
        assert!(!slot.clear_if_current(launched));
        assert!(slot.get().is_some());
    }
}
