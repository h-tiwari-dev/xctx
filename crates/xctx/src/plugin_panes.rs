//! The plugin panes in the window: the API client, logs, database, browser and files panes drawn with
//! `xctx-plugin-view`, each with its own inputs and selections.
//!
//! Each non-terminal plugin pane of an open space gets a [`PaneUi`] (its prompt inputs, what is selected, its
//! scroll position and the view last built from the workspace's pane data), made the first time the pane is
//! drawn and dropped when the pane closes, is unmounted, or its space's tab closes ([`pane_state_changes`]).
//! The views are rebuilt from the pane data only when it changed ([`PluginPanes::changed`], bumped by every
//! plugin job and the logs ticker) or once a second, so drawing a frame reads no plugin folders. The mapping
//! from the workspace's pane data to the view types is plain functions, tested below. A files pane keeps its own
//! data instead: the folders it read and the git status, read on the background executor when it is first drawn,
//! when a folder opens and on Refresh, with its view rebuilt as each read lands.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, Context, Entity, FocusHandle, Focusable, IntoElement, ParentElement,
    ScrollHandle, SharedString, Styled, Subscription, WeakEntity, Window, div, prelude::*,
};
use xctx_browser::{ConsoleEntry, NetworkError, typed_url};
use xctx_core::{Redactor, SpaceId};
use xctx_http::{Exchange, ExchangeDiff, HeaderChange, Outcome, Request};
use xctx_layout::{PaneId, PaneKind};
use xctx_plugin_view::{
    BrowserPaneView, BrowserView, ConsoleRow, FileMark, FileRow, FileRowKind, FilesPaneView,
    FilesPreview, FilesView, HttpDiffView, HttpPaneView, HttpResponseView, HttpRow, HttpView,
    LOG_LINE_CHARS, LOG_LINE_LIMIT, LogLevel, LogLine, LogsPaneView, LogsView, NetworkErrorRow,
    PreviewBody, SQL_ROW_LIMIT, SqlPaneView, SqlResultView, SqlView, detect_level, level_passes,
    markdown_blocks,
};
use xctx_ui::{PromptEvent, PromptInput, Theme, cut_line};
use xctx_web_view::{PageStatus, WebView};

use crate::plugin_backend::TailStatus;
use crate::plugin_browser::page_area;
use crate::plugin_host::{
    FILES_PREVIEW_BYTES, FILES_PREVIEW_LINES, FilePreview, FileTree, GitChange, HttpPaneData,
    LogsPaneData, PreviewText, SqlPaneData, TreeRow, TreeRowKind, WebState, file_diff, git_status,
    list_folder, mask_console, mask_network, read_preview,
};
use crate::workspace::Workspace;

/// A plugin pane of an open space.
pub(crate) type PaneKey = (SpaceId, PaneId);

/// How long a pane's view is drawn from its last build before it is rebuilt even when no change was signalled.
pub const VIEW_MAX_AGE: Duration = Duration::from_secs(1);

/// What a logs pane says when it tails nothing (it could not start, or was mounted with no command or file).
pub const LOGS_NOT_TAILING: &str = "This pane tails nothing: close it (⌘W) and open Logs from the tool dock with a command or a log file.";

/// What a plugin pane says when it has no mount or its plugin cannot be used.
pub const NO_MOUNT: &str = "This tool pane has no tool behind it, or the tool is not installed or trusted. Close it (⌘W) and open the tool again from the dock on the right.";

/// What a space's second browser pane says: the space has one live page, shown in its first browser pane.
pub const ONE_PAGE_PER_SPACE: &str =
    "This space's live page is shown in its other browser pane. Close this one (⌘W) to keep one.";

/// The kinds of plugin pane drawn by a first-party renderer (terminal plugins run in a terminal).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RendererKind {
    /// The API client.
    Http,
    /// The logs tail.
    Logs,
    /// The read-only database.
    Sql,
    /// The browser.
    Web,
    /// The worktree's file explorer.
    Files,
}

impl RendererKind {
    /// The renderer of the manifest pane kind `kind` (`http`, `logs`, `sql`, `web` or `files`); `None` for
    /// `terminal` and anything else.
    pub(crate) fn from_name(kind: &str) -> Option<RendererKind> {
        match kind {
            "http" => Some(RendererKind::Http),
            "logs" => Some(RendererKind::Logs),
            "sql" => Some(RendererKind::Sql),
            "web" => Some(RendererKind::Web),
            "files" => Some(RendererKind::Files),
            _ => None,
        }
    }
}

/// Which plugin pane states to make and which to drop, given the panes that have one (`existing`) and the
/// non-terminal plugin panes of the open tabs (`live`): every live pane without a state is made, and every
/// state whose pane is no longer live (closed, unmounted, reshaped away, or its tab closed) is dropped.
pub(crate) fn pane_state_changes<'a>(
    existing: impl IntoIterator<Item = &'a PaneKey>,
    live: &[PaneKey],
) -> (Vec<PaneKey>, Vec<PaneKey>) {
    let existing: HashSet<&PaneKey> = existing.into_iter().collect();
    let live_set: HashSet<&PaneKey> = live.iter().collect();
    let create = live
        .iter()
        .filter(|key| !existing.contains(key))
        .cloned()
        .collect();
    let mut drop: Vec<PaneKey> = existing
        .into_iter()
        .filter(|key| !live_set.contains(key))
        .cloned()
        .collect();
    drop.sort_by_key(|(space, pane)| (space.as_str().to_owned(), pane.0));
    (create, drop)
}

// -------------------------------------------------------------------------------------------------------------
// Pane data to view types

/// The API client view of `data` (masked already), without what the pane itself holds (method, inputs,
/// compare, headers): the history newest first, each URL shown relative to the pane's base URL when under it,
/// the selected exchange's response (or its failure), and the diff.
pub(crate) fn http_view(data: &HttpPaneData) -> HttpView {
    let base = data.base_url.as_deref().map(|b| b.trim_end_matches('/'));
    let history = data
        .history
        .iter()
        .rev()
        .map(|e| HttpRow {
            id: e.id,
            method: e.request.method.to_uppercase().into(),
            path: shown_url(&e.request.url, base).into(),
            status: e.status(),
            elapsed_ms: e.elapsed_ms(),
        })
        .collect();
    let response = data
        .selected
        .and_then(|id| data.history.iter().find(|e| e.id == id))
        .map(response_view);
    HttpView {
        history,
        selected: data.selected,
        response,
        diff: data.diff.as_ref().map(diff_view),
        busy: data.busy,
        error: data.error.clone().map(SharedString::from),
        ..HttpView::default()
    }
}

/// `url` as the history shows it: the part after `base` when it is under it, else whole.
fn shown_url(url: &str, base: Option<&str>) -> String {
    match base.and_then(|b| url.strip_prefix(b)) {
        Some("") => "/".to_owned(),
        Some(rest) if rest.starts_with('/') || rest.starts_with('?') => rest.to_owned(),
        _ => url.to_owned(),
    }
}

fn response_view(e: &Exchange) -> HttpResponseView {
    match &e.outcome {
        Outcome::Response(r) => HttpResponseView {
            id: e.id,
            status: Some(r.status),
            headers: r
                .headers
                .iter()
                .map(|(k, v)| (k.clone().into(), v.clone().into()))
                .collect(),
            body: r.body.clone().into(),
            truncated: r.truncated,
            elapsed_ms: r.elapsed_ms,
        },
        Outcome::Failed(f) => HttpResponseView {
            id: e.id,
            status: None,
            headers: Vec::new(),
            body: f.message.clone().into(),
            truncated: false,
            elapsed_ms: f.elapsed_ms,
        },
    }
}

/// One line of the diff's header changes: `+ name: value`, `- name: value` or `~ name: from → to`.
pub(crate) fn header_change_line(change: &HeaderChange) -> String {
    match change {
        HeaderChange::Added { name, value } => format!("+ {name}: {value}"),
        HeaderChange::Removed { name, value } => format!("- {name}: {value}"),
        HeaderChange::Changed { name, from, to } => format!("~ {name}: {from} → {to}"),
    }
}

fn diff_view(diff: &ExchangeDiff) -> HttpDiffView {
    HttpDiffView {
        a: diff.a,
        b: diff.b,
        a_status: diff.a_status,
        b_status: diff.b_status,
        headers: diff
            .headers
            .iter()
            .map(|c| header_change_line(c).into())
            .collect(),
        body: diff.body.iter().map(|l| l.clone().into()).collect(),
    }
}

/// The request Send sends from the API client: `method` upper-cased, `url` (a path joined to the pane's base
/// URL, or a full URL; `/` when empty), and `body` when it is not blank, marked as JSON when it looks like
/// JSON.
pub(crate) fn http_request(method: &str, url: &str, body: &str) -> Request {
    let url = url.trim();
    let mut request = Request::new(
        method.to_uppercase(),
        if url.is_empty() { "/" } else { url },
    );
    if !body.trim().is_empty() {
        let start = body.trim_start();
        if start.starts_with('{') || start.starts_with('[') {
            request
                .headers
                .push(("content-type".to_owned(), "application/json".to_owned()));
        }
        request.body = Some(body.to_owned());
    }
    request
}

/// The logs view of `data` (masked already; `None` when the pane tails nothing) with the text filter `filter`
/// (case-insensitive, blank for none) and the level filter `level` applied: the last [`LOG_LINE_LIMIT`]
/// lines that pass, each cut to [`LOG_LINE_CHARS`] with its level detected, the count of all that pass, what is
/// tailed and whether the tail ended or failed. Follow and pause are the pane's own.
pub(crate) fn logs_view(
    data: Option<&LogsPaneData>,
    filter: &str,
    level: Option<LogLevel>,
) -> LogsView {
    let Some(data) = data else {
        return LogsView {
            filter: filter.to_owned().into(),
            level,
            error: Some(LOGS_NOT_TAILING.into()),
            ..LogsView::default()
        };
    };
    let needle = filter.trim().to_lowercase();
    let passing: Vec<(&str, Option<LogLevel>)> = data
        .lines
        .iter()
        .map(|l| (l.text.as_str(), detect_level(&l.text)))
        .filter(|(text, lvl)| {
            level_passes(*lvl, level)
                && (needle.is_empty() || text.to_lowercase().contains(&needle))
        })
        .collect();
    let total = passing.len();
    let lines = passing[total.saturating_sub(LOG_LINE_LIMIT)..]
        .iter()
        .map(|(text, lvl)| LogLine {
            text: cut_line(text, LOG_LINE_CHARS),
            level: *lvl,
        })
        .collect();
    let (source, error) = match &data.status {
        TailStatus::Running => (data.source.clone(), None),
        TailStatus::Exited(Some(code)) => (format!("{} — exited ({code})", data.source), None),
        TailStatus::Exited(None) => (format!("{} — stopped", data.source), None),
        TailStatus::Failed(why) => (data.source.clone(), Some(why.clone().into())),
    };
    LogsView {
        source: source.into(),
        lines,
        total,
        filter: filter.to_owned().into(),
        level,
        error,
        ..LogsView::default()
    }
}

/// The database view of `data` (masked already), without the query (the pane's input): the last result's
/// columns and its first [`SQL_ROW_LIMIT`] rows with the count of all, the error, and whether a query runs or
/// a write waits in the Inbox.
pub(crate) fn sql_view(data: &SqlPaneData) -> SqlView {
    SqlView {
        engine: data.engine.clone().into(),
        database: data.database.clone().into(),
        query: SharedString::default(),
        result: data.result.as_ref().map(|table| SqlResultView {
            columns: table.columns.iter().map(|c| c.clone().into()).collect(),
            rows: table
                .rows
                .iter()
                .take(SQL_ROW_LIMIT)
                .map(|row| row.iter().map(|c| c.clone().into()).collect())
                .collect(),
            total_rows: table.row_count.max(table.rows.len()),
            elapsed_ms: None,
        }),
        busy: data.busy,
        write_pending: data.write_pending,
        error: data.error.clone().map(SharedString::from),
    }
}

/// The level of a console message from its console method (`error`, `warning`, ...), or `exception`.
pub(crate) fn console_level(method: &str) -> Option<LogLevel> {
    match method {
        "error" | "exception" | "assert" => Some(LogLevel::Error),
        "warning" | "warn" => Some(LogLevel::Warn),
        "log" | "info" => Some(LogLevel::Info),
        "debug" | "trace" | "verbose" => Some(LogLevel::Debug),
        _ => None,
    }
}

/// What the live page shows: its status, console and failed requests, read from the space's browser.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PageShown {
    pub(crate) status: PageStatus,
    pub(crate) console: Vec<ConsoleEntry>,
    pub(crate) network_errors: Vec<NetworkError>,
}

/// The browser view of the live page `page` and the pane's `web` state, everything masked with `redactor`:
/// the URL and title, loading and history, why the last load failed, the agent's activity and last error,
/// the console (with where each message came from) and the failed requests.
pub(crate) fn browser_view(
    page: &PageShown,
    web: &WebState,
    console_open: bool,
    redactor: &Redactor,
) -> BrowserView {
    let status = &page.status;
    let masked = |text: &str| SharedString::from(redactor.redact(text).into_owned());
    BrowserView {
        url: status
            .url
            .as_deref()
            .map(|u| xctx_http::mask_url(u, redactor))
            .unwrap_or_default()
            .into(),
        title: status.title.as_deref().map(masked),
        loading: status.loading.then_some(status.progress),
        can_go_back: status.can_go_back,
        can_go_forward: status.can_go_forward,
        has_page: status.url.is_some(),
        load_error: status.error.as_deref().map(masked),
        agent_busy: web.busy > 0,
        error: web.error.as_deref().map(masked),
        console: page
            .console
            .iter()
            .map(|c| {
                let c = mask_console(c.clone(), redactor);
                ConsoleRow {
                    level: console_level(&c.level),
                    text: match &c.location {
                        Some(at) => format!("{} ({at})", c.text).into(),
                        None => c.text.into(),
                    },
                }
            })
            .collect(),
        network_errors: page
            .network_errors
            .iter()
            .map(|n| {
                let n = mask_network(n.clone(), redactor);
                NetworkErrorRow {
                    url: n.url.into(),
                    detail: match n.status {
                        Some(code) if n.reason.is_empty() => code.to_string().into(),
                        Some(code) => format!("{code} {}", n.reason).into(),
                        None => n.reason.into(),
                    },
                }
            })
            .collect(),
        console_open,
    }
}

/// What a files pane says of a deleted file it cannot read.
pub const FILES_DELETED: &str = "Deleted in the worktree. Turn on Diff to see what it held.";

/// A file's git change as the files pane draws it.
pub(crate) fn file_mark(change: GitChange) -> FileMark {
    match change {
        GitChange::Modified => FileMark::Modified,
        GitChange::Added => FileMark::Added,
        GitChange::Deleted => FileMark::Deleted,
        GitChange::Renamed => FileMark::Renamed,
        GitChange::Untracked => FileMark::Untracked,
        GitChange::Conflicted => FileMark::Conflicted,
    }
}

/// A tree row as the files pane draws it.
pub(crate) fn file_row(row: TreeRow) -> FileRow {
    FileRow {
        path: row.path.into(),
        name: row.name.into(),
        depth: row.depth,
        kind: match row.kind {
            TreeRowKind::Folder {
                open,
                changed,
                heavy,
            } => FileRowKind::Folder {
                open,
                changed,
                heavy,
            },
            TreeRowKind::File(change) => FileRowKind::File {
                mark: change.map(file_mark),
            },
            TreeRowKind::More(n) => FileRowKind::More(n),
            TreeRowKind::Loading => FileRowKind::Loading,
            TreeRowKind::Refused(why) => FileRowKind::Refused(why.into()),
        },
    }
}

/// The preview of a file's (or a diff's) text, and the note saying why it is cut when it is.
fn text_body(text: &PreviewText, markdown: bool) -> (PreviewBody, Option<SharedString>) {
    match text {
        PreviewText::Binary => (PreviewBody::Binary, None),
        PreviewText::Text { text, cut } => {
            let body = if markdown {
                PreviewBody::Markdown(markdown_blocks(text))
            } else {
                PreviewBody::Lines(text.lines().map(|line| line.to_owned().into()).collect())
            };
            let lines = text.lines().count();
            let note = cut.then(|| {
                if lines >= FILES_PREVIEW_LINES {
                    format!("Cut: showing the first {FILES_PREVIEW_LINES} lines").into()
                } else {
                    format!("Cut: showing the first {} KiB", FILES_PREVIEW_BYTES / 1024).into()
                }
            });
            (body, note)
        }
    }
}

/// The files view of `ui`: the tree's rows (only changed paths when "Changed only" is on) and the selected file's
/// text, or its diff when Diff is on and it has a change. Everything in it was masked when it was read.
pub(crate) fn files_view(ui: &FilesUi) -> FilesView {
    let change = ui.selected.as_deref().and_then(|p| ui.tree.change_of(p));
    let preview = match &ui.selected {
        None => FilesPreview::default(),
        Some(path) => {
            let diff = ui.diff && change.is_some();
            let markdown = std::path::Path::new(path).extension().is_some_and(|ext| {
                ext.eq_ignore_ascii_case("md")
                    || ext.eq_ignore_ascii_case("markdown")
                    || ext.eq_ignore_ascii_case("mdx")
            });
            let document = !diff
                && std::path::Path::new(path)
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
                && matches!(ui.preview, Some(Ok(_)));
            let (body, cut) = if document {
                (PreviewBody::Document, None)
            } else if diff {
                match &ui.diff_text {
                    None => (PreviewBody::Loading, None),
                    Some(Err(why)) => (PreviewBody::Message(why.clone().into()), None),
                    Some(Ok(text)) => text_body(text, false),
                }
            } else {
                match &ui.preview {
                    None => (PreviewBody::Loading, None),
                    Some(Err(_)) if change == Some(GitChange::Deleted) => {
                        (PreviewBody::Message(FILES_DELETED.into()), None)
                    }
                    Some(Err(why)) => (PreviewBody::Message(why.clone().into()), None),
                    Some(Ok(file)) => text_body(&file.body, markdown),
                }
            };
            FilesPreview {
                path: path.clone().into(),
                size: match &ui.preview {
                    Some(Ok(file)) => Some(file.size),
                    _ => None,
                },
                body,
                diff,
                can_diff: change.is_some(),
                cut,
            }
        }
    };
    let loading_file = ui.selected.is_some()
        && (ui.preview.is_none() || (ui.diff && change.is_some() && ui.diff_text.is_none()));
    FilesView {
        root: ui.root_shown.clone(),
        rows: ui
            .tree
            .rows(ui.changed_only)
            .into_iter()
            .map(file_row)
            .collect(),
        changed_only: ui.changed_only,
        busy: ui.tree.is_reading() || ui.status_reading || loading_file,
        status_error: ui.tree.status_error().map(|e| e.to_owned().into()),
        preview,
    }
}

// -------------------------------------------------------------------------------------------------------------
// Per-pane state

/// The API client pane's own state.
pub(crate) struct HttpUi {
    url: Entity<PromptInput>,
    body: Entity<PromptInput>,
    method: &'static str,
    compare: Option<u64>,
    headers_open: bool,
    view: HttpView,
}

/// The logs pane's own state.
pub(crate) struct LogsUi {
    filter: Entity<PromptInput>,
    level: Option<LogLevel>,
    follow: bool,
    paused: bool,
    scroll: ScrollHandle,
    data: Option<LogsPaneData>,
    /// The filter and level the view was built with, and whether it was built from the latest data.
    built: Option<(String, Option<LogLevel>)>,
    view: LogsView,
}

/// The database pane's own state.
pub(crate) struct SqlUi {
    query: Entity<PromptInput>,
    view: SqlView,
}

/// The browser pane's own state.
pub(crate) struct WebUi {
    url: Entity<PromptInput>,
    /// The pane's page (`http://localhost:{PORT}` unless mounted with another), unmasked: what a path typed in
    /// the URL bar is relative to.
    base: String,
    /// The page URL the URL bar was last set to, so it follows the page as it navigates.
    shown_url: Option<String>,
    console_open: bool,
    view: BrowserView,
}

/// The files pane's own state: what it read of its worktree, and the selected file.
pub(crate) struct FilesUi {
    tree: FileTree,
    /// The worktree as the header draws it, masked.
    root_shown: SharedString,
    changed_only: bool,
    diff: bool,
    /// The selected file's path relative to the worktree.
    selected: Option<String>,
    /// The selected file once read: its size and text, or why it cannot be shown (masked).
    preview: Option<Result<FilePreview, String>>,
    /// The selected file's diff once read, when Diff is on.
    diff_text: Option<Result<PreviewText, String>>,
    /// True while the selected file's diff is read.
    diff_reading: bool,
    /// True while git status runs.
    status_reading: bool,
    /// Counts file selections and refreshes: a file read that lands after a newer one started is dropped.
    generation: u64,
    view: FilesView,
}

impl FilesUi {
    fn new(root: std::path::PathBuf, root_shown: String) -> FilesUi {
        let mut ui = FilesUi {
            tree: FileTree::new(root),
            root_shown: root_shown.into(),
            changed_only: false,
            diff: false,
            selected: None,
            preview: None,
            diff_text: None,
            diff_reading: false,
            status_reading: false,
            generation: 0,
            view: FilesView::default(),
        };
        ui.rebuild();
        ui
    }

    fn rebuild(&mut self) {
        let view = files_view(self);
        self.view = view;
    }
}

/// What a plugin pane holds, by renderer.
pub(crate) enum PaneState {
    Http(HttpUi),
    Logs(LogsUi),
    Sql(SqlUi),
    Web(WebUi),
    Files(FilesUi),
    /// The pane has no mount, or its plugin is not usable: why, drawn in its place.
    Missing(String),
}

/// One plugin pane's state.
pub(crate) struct PaneUi {
    pub(crate) state: PaneState,
    /// The change count and time its view was last built at; `None` before the first build.
    built: Option<(u64, Instant)>,
    _subscriptions: Vec<Subscription>,
}

impl PaneUi {
    /// The input that takes the keyboard when the pane is focused.
    fn primary_input(&self) -> Option<&Entity<PromptInput>> {
        match &self.state {
            PaneState::Http(ui) => Some(&ui.url),
            PaneState::Logs(ui) => Some(&ui.filter),
            PaneState::Sql(ui) => Some(&ui.query),
            PaneState::Web(ui) => Some(&ui.url),
            PaneState::Files(_) => None,
            PaneState::Missing(_) => None,
        }
    }
}

/// The state of every drawn plugin pane, and a count of changes to the plugin data they show.
#[derive(Default)]
pub(crate) struct PluginPanes {
    panes: HashMap<PaneKey, PaneUi>,
    version: u64,
    /// Counts new log lines only, so a busy tail does not rebuild the other panes' views.
    logs_version: u64,
}

impl PluginPanes {
    /// The plugin data changed (a job started or finished, a tail got lines, a gate settled): rebuild the views.
    pub(crate) fn changed(&mut self) {
        self.version = self.version.wrapping_add(1);
    }

    /// A logs tail got new lines: rebuild the logs views.
    pub(crate) fn logs_changed(&mut self) {
        self.logs_version = self.logs_version.wrapping_add(1);
    }

    /// Drop the state of `pane` of `space`, as it closes.
    pub(crate) fn forget(&mut self, space: &SpaceId, pane: PaneId) {
        self.panes.remove(&(space.clone(), pane));
    }

    /// Drop the state of every pane of `space`, as its tab closes.
    pub(crate) fn forget_space(&mut self, space: &SpaceId) {
        self.panes.retain(|(s, _), _| s != space);
    }

    #[cfg(test)]
    fn keys(&self) -> Vec<PaneKey> {
        self.panes.keys().cloned().collect()
    }
}

/// Whether a view built at `built` must be rebuilt now that the change count is `version`.
pub(crate) fn view_is_stale(built: Option<(u64, Instant)>, version: u64, now: Instant) -> bool {
    match built {
        None => true,
        Some((seen, at)) => seen != version || now.duration_since(at) >= VIEW_MAX_AGE,
    }
}

fn input(
    cx: &mut Context<Workspace>,
    placeholder: String,
    value: Option<&str>,
) -> Entity<PromptInput> {
    cx.new(|cx| {
        let mut input = PromptInput::new(cx).placeholder(placeholder);
        if let Some(value) = value {
            input.set_value(value, cx);
        }
        input
    })
}

impl Workspace {
    /// Make the state of the non-terminal plugin panes of the space shown that have none, and drop the state
    /// of panes that are gone. Called before every frame.
    pub(crate) fn sync_plugin_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.active_space_id();
        let mut live = Vec::new();
        let mut kept = Vec::new();
        for (id, tab) in &self.tabs {
            for pane in tab.layout.panes() {
                if pane.role.kind != PaneKind::Plugin || tab.terminals.contains_key(&pane.id) {
                    continue;
                }
                let key = (id.clone(), pane.id);
                if Some(id) == shown.as_ref() {
                    live.push(key);
                } else if self.plugin_panes.panes.contains_key(&key) {
                    // Panes of hidden tabs keep their state; it is made only when drawn.
                    kept.push(key);
                }
            }
        }
        live.extend(kept);
        let (create, drop) = pane_state_changes(self.plugin_panes.panes.keys(), &live);
        for key in drop {
            self.plugin_panes.panes.remove(&key);
            self.browsers.close_document(&key.0, key.1);
        }
        for key in create {
            let ui = self.new_pane_ui(&key, window, cx);
            let files = matches!(ui.state, PaneState::Files(_));
            self.plugin_panes.panes.insert(key.clone(), ui);
            if files {
                // The first read: the worktree's own entries and the git status.
                self.files_refresh(&key, cx);
            }
        }
    }

    fn new_pane_ui(
        &mut self,
        key: &PaneKey,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PaneUi {
        let (id, pane) = key;
        let missing = |why: String| PaneUi {
            state: PaneState::Missing(why),
            built: None,
            _subscriptions: Vec::new(),
        };
        let Some(space) = self.tabs.get(id).map(|tab| tab.space.clone()) else {
            return missing(format!("space {id} is not open"));
        };
        let Some((kind, settings)) = self.plugins.mounted_pane(&space, *pane, &self.redactor)
        else {
            return missing(NO_MOUNT.to_owned());
        };
        let Some(renderer) = RendererKind::from_name(kind) else {
            return missing(format!("a {kind} tool pane has no renderer"));
        };
        let settings = settings.unwrap_or_default();
        // URLs with their credentials (user info, token-like query values) masked too.
        let mask = |text: &str| xctx_http::mask_url(text, &self.redactor);
        let mut inputs = Vec::new();
        let state = match renderer {
            RendererKind::Http => {
                let base = settings.base_url.as_deref().map(mask);
                let placeholder = match &base {
                    Some(base) => format!("Path on {base}, or a full local URL"),
                    None => "A full local URL, e.g. http://localhost:3000/health".to_owned(),
                };
                let url = input(cx, placeholder, None);
                let body = input(cx, "Body (optional)".to_owned(), None);
                inputs.push(url.clone());
                inputs.push(body.clone());
                PaneState::Http(HttpUi {
                    url,
                    body,
                    method: xctx_plugin_view::HTTP_METHODS[0],
                    compare: None,
                    headers_open: false,
                    view: HttpView::default(),
                })
            }
            RendererKind::Logs => {
                let filter = input(cx, "Filter lines".to_owned(), None);
                inputs.push(filter.clone());
                PaneState::Logs(LogsUi {
                    filter,
                    level: None,
                    follow: true,
                    paused: false,
                    scroll: ScrollHandle::new(),
                    data: None,
                    built: None,
                    view: LogsView::default(),
                })
            }
            RendererKind::Sql => {
                let query = input(cx, "SELECT … (read-only)".to_owned(), None);
                inputs.push(query.clone());
                PaneState::Sql(SqlUi {
                    query,
                    view: SqlView::default(),
                })
            }
            RendererKind::Web => {
                let base = settings
                    .url
                    .clone()
                    .unwrap_or_else(|| format!("http://localhost:{}", space.port));
                let url = input(
                    cx,
                    "A URL such as google.com, or a path of the app such as /login".to_owned(),
                    Some(&mask(&base)),
                );
                inputs.push(url.clone());
                PaneState::Web(WebUi {
                    url,
                    base,
                    shown_url: None,
                    console_open: false,
                    view: BrowserView::default(),
                })
            }
            RendererKind::Files => {
                let Some(root) = settings.files_root.clone() else {
                    return missing("this files pane has no worktree to show".to_owned());
                };
                let shown = self
                    .redactor
                    .redact(&root.display().to_string())
                    .into_owned();
                PaneState::Files(FilesUi::new(root, shown))
            }
        };
        if let PaneState::Web(ui) = &state {
            // The space's live browser, opening the pane's page the first time.
            let start = ui.base.clone();
            if let Err(e) = self.space_browser(id, Some(&start), cx) {
                self.plugins.runtime(id).web.error = Some(self.redactor.redact(&e).into_owned());
            }
        }
        let subscriptions = inputs
            .iter()
            .map(|entity| {
                let key = key.clone();
                cx.subscribe_in(
                    entity,
                    window,
                    move |this, _, event: &PromptEvent, window, cx| match event {
                        PromptEvent::Submit(_) => this.plugin_pane_submit(&key, window, cx),
                        // Esc leaves the input; the pane's keys go back to the window.
                        PromptEvent::Cancel => {
                            this.focus_handle.focus(window, cx);
                            cx.notify();
                        }
                    },
                )
            })
            .collect();
        PaneUi {
            state,
            built: None,
            _subscriptions: subscriptions,
        }
    }

    /// The input of plugin pane `key` that takes the keyboard when the pane is focused, if it has one.
    pub(crate) fn plugin_pane_focus(&self, key: &PaneKey, cx: &App) -> Option<FocusHandle> {
        self.plugin_panes
            .panes
            .get(key)
            .and_then(PaneUi::primary_input)
            .map(|input| input.focus_handle(cx))
    }

    /// Rebuild the views of the shown space's plugin panes whose data changed (see [`view_is_stale`]), and the
    /// logs views whose filters changed. Called before every frame.
    pub(crate) fn refresh_plugin_panes(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active_space_id() else {
            return;
        };
        let now = Instant::now();
        let (base_version, logs_version) =
            (self.plugin_panes.version, self.plugin_panes.logs_version);
        let keys: Vec<PaneKey> = self
            .plugin_panes
            .panes
            .keys()
            .filter(|(space, _)| space == &id)
            .cloned()
            .collect();
        for key in keys {
            let kind = match self.plugin_panes.panes.get(&key).map(|ui| &ui.state) {
                Some(PaneState::Http(_)) => RendererKind::Http,
                Some(PaneState::Logs(_)) => RendererKind::Logs,
                Some(PaneState::Sql(_)) => RendererKind::Sql,
                Some(PaneState::Web(_)) => RendererKind::Web,
                // A files pane keeps its own data and rebuilds its view as each read lands.
                Some(PaneState::Files(_) | PaneState::Missing(_)) | None => continue,
            };
            let version = match kind {
                RendererKind::Logs => base_version.wrapping_add(logs_version.wrapping_mul(1 << 32)),
                _ => base_version,
            };
            let stale = self
                .plugin_panes
                .panes
                .get(&key)
                .is_some_and(|ui| view_is_stale(ui.built, version, now));
            match kind {
                RendererKind::Http if stale => {
                    let view = http_view(&self.plugin_http(&id));
                    if let Some(PaneState::Http(ui)) = self.pane_state_mut(&key) {
                        ui.view = view;
                    }
                }
                RendererKind::Sql if stale => {
                    let view = sql_view(&self.plugin_sql(&id, key.1));
                    if let Some(PaneState::Sql(ui)) = self.pane_state_mut(&key) {
                        ui.view = view;
                    }
                }
                RendererKind::Web if stale => {
                    let page = self
                        .browsers
                        .view(&id)
                        .map(|view| PageShown {
                            status: view.status(),
                            console: view.console(),
                            network_errors: view.network_errors(),
                        })
                        .unwrap_or_default();
                    let web = self.plugins.web_state(&id);
                    let redactor = self.redactor.clone();
                    if let Some(PaneState::Web(ui)) = self.pane_state_mut(&key) {
                        ui.view = browser_view(&page, &web, ui.console_open, &redactor);
                        // The URL bar follows the page as it navigates.
                        if page.status.url.is_some() && page.status.url != ui.shown_url {
                            ui.shown_url = page.status.url.clone();
                            let shown = ui.view.url.to_string();
                            ui.url.update(cx, |input, cx| input.set_value(&shown, cx));
                        }
                    }
                }
                RendererKind::Logs => {
                    let paused = matches!(
                        self.plugin_panes.panes.get(&key).map(|ui| &ui.state),
                        Some(PaneState::Logs(ui)) if ui.paused
                    );
                    let fresh = (stale && !paused).then(|| self.plugin_logs(&id, key.1));
                    if let Some(PaneState::Logs(ui)) = self.pane_state_mut(&key) {
                        let filter = ui.filter.read(cx).value().to_owned();
                        let wanted = (filter, ui.level);
                        let had_data = fresh.is_some();
                        if let Some(data) = fresh {
                            ui.data = data;
                        }
                        if had_data || ui.built.as_ref() != Some(&wanted) {
                            ui.view = logs_view(ui.data.as_ref(), &wanted.0, wanted.1);
                            ui.built = Some(wanted);
                        }
                    }
                }
                _ => {}
            }
            if stale && let Some(ui) = self.plugin_panes.panes.get_mut(&key) {
                ui.built = Some((version, now));
            }
        }
    }

    fn pane_state_mut(&mut self, key: &PaneKey) -> Option<&mut PaneState> {
        self.plugin_panes.panes.get_mut(key).map(|ui| &mut ui.state)
    }

    /// Change plugin pane `key`'s own state and redraw.
    fn change_pane(
        &mut self,
        key: &PaneKey,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut PaneState),
    ) {
        if let Some(state) = self.pane_state_mut(key) {
            change(state);
            cx.notify();
        }
    }

    /// Enter in one of plugin pane `key`'s inputs: send, run, open; a logs filter applies as it is typed.
    fn plugin_pane_submit(&mut self, key: &PaneKey, _window: &mut Window, cx: &mut Context<Self>) {
        match self.pane_state_mut(key) {
            Some(PaneState::Http(_)) => self.http_pane_send(key, cx),
            Some(PaneState::Sql(_)) => self.sql_pane_run(key, false, cx),
            Some(PaneState::Web(_)) => self.web_pane_open(key, cx),
            _ => {}
        }
    }

    fn http_pane_send(&mut self, key: &PaneKey, cx: &mut Context<Self>) {
        let Some(PaneState::Http(ui)) = self.pane_state_mut(key) else {
            return;
        };
        let (method, url, body) = (ui.method, ui.url.clone(), ui.body.clone());
        let request = http_request(method, url.read(cx).value(), body.read(cx).value());
        self.http_send(&key.0, request, cx);
    }

    fn sql_pane_run(&mut self, key: &PaneKey, write: bool, cx: &mut Context<Self>) {
        let Some(PaneState::Sql(ui)) = self.pane_state_mut(key) else {
            return;
        };
        let query = ui.query.read(cx).value().trim().to_owned();
        if query.is_empty() {
            self.info("Type a query first", cx);
            return;
        }
        if write {
            self.sql_request_write(&key.0, Some(key.1), &query, cx);
        } else {
            self.sql_run(&key.0, Some(key.1), &query, cx);
        }
    }

    /// Open what the URL bar of browser pane `key` holds: any web page the user types (a bare host gets
    /// `https://`, a path is on the app). The user's browsing is not limited to the app.
    fn web_pane_open(&mut self, key: &PaneKey, cx: &mut Context<Self>) {
        let Some(PaneState::Web(ui)) = self.pane_state_mut(key) else {
            return;
        };
        let url = typed_url(ui.url.read(cx).value(), &ui.base);
        ui.shown_url = None;
        let opened = self
            .space_browser(&key.0, None, cx)
            .and_then(|view| view.load(&url).map_err(|e| e.to_string()));
        let error = opened.err().map(|e| self.redactor.redact(&e).into_owned());
        self.plugins.runtime(&key.0).web.error = error;
        self.plugin_panes.changed();
        cx.notify();
    }

    /// Back, Forward or Reload in browser pane `key`.
    fn web_pane_go(&mut self, key: &PaneKey, go: fn(&WebView), cx: &mut Context<Self>) {
        if let Some(view) = self.browsers.view(&key.0) {
            go(&view);
        }
        self.plugin_panes.changed();
        cx.notify();
    }

    /// Open or close browser pane `key`'s console strip.
    fn web_pane_toggle_console(&mut self, key: &PaneKey, cx: &mut Context<Self>) {
        self.change_pane(key, cx, |state| {
            if let PaneState::Web(ui) = state {
                ui.console_open = !ui.console_open;
                ui.view.console_open = ui.console_open;
            }
        });
    }

    fn files_ui(&mut self, key: &PaneKey) -> Option<&mut FilesUi> {
        match self.pane_state_mut(key) {
            Some(PaneState::Files(ui)) => Some(ui),
            _ => None,
        }
    }

    /// Run `work` on the background executor, then `land` with its result on the UI thread if files pane `key`
    /// is still there: its view is rebuilt, a diff that is now wanted is read, and the window redraws.
    fn files_job<T: Send + 'static>(
        &mut self,
        key: &PaneKey,
        work: impl FnOnce() -> T + Send + 'static,
        land: impl FnOnce(&mut FilesUi, T) + 'static,
        cx: &mut Context<Self>,
    ) {
        let key = key.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let result = executor.spawn(async move { work() }).await;
            let _ = this.update(cx, |this, cx| {
                if let Some(ui) = this.files_ui(&key) {
                    land(ui, result);
                    ui.rebuild();
                    this.files_read_diff(&key, cx);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Read files pane `key`'s open folders, its git status and its selected file again.
    fn files_refresh(&mut self, key: &PaneKey, cx: &mut Context<Self>) {
        let redactor = self.redactor.clone();
        let Some(ui) = self.files_ui(key) else {
            return;
        };
        let folders = ui.tree.refresh();
        let root = ui.tree.root().to_path_buf();
        ui.status_reading = true;
        ui.rebuild();
        for rel in folders {
            self.files_read_folder(key, rel, cx);
        }
        self.files_job(
            key,
            move || git_status(&root).map_err(|e| redactor.redact(&e).into_owned()),
            |ui, status| {
                ui.status_reading = false;
                ui.tree.set_status(status);
            },
            cx,
        );
        self.files_read_selected(key, cx);
    }

    /// List folder `rel` of files pane `key` in the background.
    fn files_read_folder(&mut self, key: &PaneKey, rel: String, cx: &mut Context<Self>) {
        let redactor = self.redactor.clone();
        let Some(ui) = self.files_ui(key) else {
            return;
        };
        let root = ui.tree.root().to_path_buf();
        let at = rel.clone();
        self.files_job(
            key,
            move || list_folder(&root, &at).map_err(|e| redactor.redact(&e).into_owned()),
            move |ui, listing| ui.tree.set_listing(&rel, listing),
            cx,
        );
    }

    /// Read files pane `key`'s selected file again (and its diff when Diff is on), dropping what it showed.
    fn files_read_selected(&mut self, key: &PaneKey, cx: &mut Context<Self>) {
        let redactor = self.redactor.clone();
        let Some(ui) = self.files_ui(key) else {
            return;
        };
        let Some(path) = ui.selected.clone() else {
            return;
        };
        ui.generation = ui.generation.wrapping_add(1);
        let generation = ui.generation;
        ui.preview = None;
        ui.diff_text = None;
        ui.diff_reading = false;
        let root = ui.tree.root().to_path_buf();
        ui.rebuild();
        self.files_job(
            key,
            move || {
                read_preview(&root, &path, &redactor).map_err(|e| redactor.redact(&e).into_owned())
            },
            move |ui, preview| {
                if ui.generation == generation {
                    ui.preview = Some(preview);
                }
            },
            cx,
        );
        self.files_read_diff(key, cx);
    }

    /// Read the diff of files pane `key`'s selected file when Diff is on, the file has a change, and its diff is
    /// neither read nor being read.
    fn files_read_diff(&mut self, key: &PaneKey, cx: &mut Context<Self>) {
        let redactor = self.redactor.clone();
        let Some(ui) = self.files_ui(key) else {
            return;
        };
        let Some(path) = ui.selected.clone() else {
            return;
        };
        let Some(change) = ui.tree.change_of(&path) else {
            return;
        };
        if !ui.diff || ui.diff_text.is_some() || ui.diff_reading {
            return;
        }
        ui.diff_reading = true;
        let generation = ui.generation;
        let root = ui.tree.root().to_path_buf();
        ui.rebuild();
        self.files_job(
            key,
            move || {
                file_diff(&root, &path, change, &redactor)
                    .map_err(|e| redactor.redact(&e).into_owned())
            },
            move |ui, diff| {
                if ui.generation == generation {
                    ui.diff_reading = false;
                    ui.diff_text = Some(diff);
                }
            },
            cx,
        );
    }

    /// A folder row of files pane `key` was clicked: open it (reading it the first time) or close it.
    fn files_toggle(&mut self, key: &PaneKey, path: &str, cx: &mut Context<Self>) {
        let Some(ui) = self.files_ui(key) else {
            return;
        };
        let read = ui.tree.toggle(path);
        ui.rebuild();
        if read {
            self.files_read_folder(key, path.to_owned(), cx);
        }
        cx.notify();
    }

    /// A file row of files pane `key` was clicked: show it.
    fn files_select(&mut self, key: &PaneKey, path: &str, cx: &mut Context<Self>) {
        let Some(ui) = self.files_ui(key) else {
            return;
        };
        ui.selected = Some(path.to_owned());
        self.files_read_selected(key, cx);
        cx.notify();
    }

    /// Change files pane `key`'s toggles, reading the selected file's diff if it is now wanted.
    fn files_set(
        &mut self,
        key: &PaneKey,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut FilesUi),
    ) {
        if let Some(ui) = self.files_ui(key) {
            change(ui);
            ui.rebuild();
        }
        self.files_read_diff(key, cx);
        cx.notify();
    }

    /// Plugin pane `pane` of `tab`'s space drawn by its renderer, if it has state (see
    /// [`Workspace::sync_plugin_panes`]).
    pub(crate) fn render_plugin_pane(
        &self,
        id: &SpaceId,
        pane: PaneId,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let key: PaneKey = (id.clone(), pane);
        let ui = self.plugin_panes.panes.get(&key)?;
        let ws = cx.entity().downgrade();
        let content: AnyElement = match &ui.state {
            PaneState::Http(ui) => render_http(ui, &ws, &key).into_any_element(),
            PaneState::Logs(ui) => render_logs(ui, &ws, &key).into_any_element(),
            PaneState::Sql(ui) => render_sql(ui, &ws, &key).into_any_element(),
            PaneState::Web(ui) => {
                // One live page per space: it shows in the space's first browser pane; the others say so.
                let first = self
                    .plugin_panes
                    .panes
                    .iter()
                    .filter(|((s, _), p)| s == id && matches!(p.state, PaneState::Web(_)))
                    .map(|((_, p), _)| *p)
                    .min_by_key(|p| p.0);
                let page = if first == Some(pane) {
                    // A dialog or a tool dock popover draws over the pane, and the page would draw over it: hide it
                    // then.
                    self.browsers
                        .view(id)
                        .map(|view| page_area(view, self.pages_may_show()))
                } else {
                    let theme = Theme::of(cx);
                    Some(
                        div()
                            .m_auto()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_subtle)
                            .child(ONE_PAGE_PER_SPACE)
                            .into_any_element(),
                    )
                };
                render_web(ui, page, &ws, &key).into_any_element()
            }
            PaneState::Files(ui) => {
                let document = matches!(ui.view.preview.body, PreviewBody::Document)
                    .then(|| {
                        let relative = ui.selected.as_deref()?;
                        let root = ui.tree.root();
                        let path = root.join(relative);
                        self.browsers
                            .document(id, pane, &path, root)
                            .ok()
                            .map(|view| page_area(view, self.pages_may_show()))
                    })
                    .flatten();
                render_files(ui, document, &ws, &key).into_any_element()
            }
            PaneState::Missing(why) => {
                let theme = Theme::of(cx);
                div()
                    .p(Theme::SPACE_3)
                    .text_size(Theme::TEXT_SM)
                    .text_color(theme.danger)
                    .child(why.clone())
                    .into_any_element()
            }
        };
        Some(
            div()
                .id(SharedString::from(format!("plugin-pane-{}", pane.0)))
                .size_full()
                .min_h_0()
                .child(content)
                .into_any_element(),
        )
    }
}

/// A handler calling `run` on the workspace for plugin pane `key`.
fn on_ws(
    ws: &WeakEntity<Workspace>,
    key: &PaneKey,
    run: impl Fn(&mut Workspace, &PaneKey, &mut Window, &mut Context<Workspace>) + 'static,
) -> impl Fn(&mut Window, &mut App) + 'static {
    let (ws, key) = (ws.clone(), key.clone());
    move |window, cx| {
        let _ = ws.update(cx, |this, cx| run(this, &key, window, cx));
    }
}

fn render_http(ui: &HttpUi, ws: &WeakEntity<Workspace>, key: &PaneKey) -> HttpPaneView {
    let mut view = ui.view.clone();
    view.method = ui.method.into();
    view.compare = ui.compare.filter(|c| Some(*c) != view.selected);
    view.headers_open = ui.headers_open;
    let (w, k) = (ws.clone(), key.clone());
    let method = move |next: &'static str, _: &mut Window, cx: &mut App| {
        let _ = w.update(cx, |this, cx| {
            this.change_pane(&k, cx, |state| {
                if let PaneState::Http(ui) = state {
                    ui.method = next;
                }
            })
        });
    };
    let (w, k) = (ws.clone(), key.clone());
    let replay = move |exchange: u64, times: u32, _: &mut Window, cx: &mut App| {
        let _ = w.update(cx, |this, cx| this.http_replay(&k.0, exchange, times, cx));
    };
    let (w, k) = (ws.clone(), key.clone());
    let select = move |exchange: u64, _: &mut Window, cx: &mut App| {
        let _ = w.update(cx, |this, cx| {
            this.http_select(&k.0, Some(exchange), cx);
        });
    };
    let (w, k) = (ws.clone(), key.clone());
    let compare = move |exchange: u64, _: &mut Window, cx: &mut App| {
        let _ = w.update(cx, |this, cx| {
            this.change_pane(&k, cx, |state| {
                if let PaneState::Http(ui) = state {
                    ui.compare = (ui.compare != Some(exchange)).then_some(exchange);
                }
            })
        });
    };
    let (w, k) = (ws.clone(), key.clone());
    let diff = move |a: u64, b: u64, _: &mut Window, cx: &mut App| {
        let _ = w.update(cx, |this, cx| this.http_diff(&k.0, a, b, cx));
    };
    let (w, k) = (ws.clone(), key.clone());
    let headers = move |open: bool, _: &mut Window, cx: &mut App| {
        let _ = w.update(cx, |this, cx| {
            this.change_pane(&k, cx, |state| {
                if let PaneState::Http(ui) = state {
                    ui.headers_open = open;
                }
            })
        });
    };
    HttpPaneView::new(view)
        .url_input(ui.url.clone())
        .body_input(ui.body.clone())
        .on_method(method)
        .on_send(on_ws(ws, key, |this, key, _, cx| {
            this.http_pane_send(key, cx)
        }))
        .on_replay(replay)
        .on_select(select)
        .on_compare(compare)
        .on_diff(diff)
        .on_headers(headers)
}

fn render_logs(ui: &LogsUi, ws: &WeakEntity<Workspace>, key: &PaneKey) -> LogsPaneView {
    let mut view = ui.view.clone();
    view.follow = ui.follow;
    view.paused = ui.paused;
    let (w, k) = (ws.clone(), key.clone());
    let level = move |level: Option<LogLevel>, _: &mut Window, cx: &mut App| {
        let _ = w.update(cx, |this, cx| {
            this.change_pane(&k, cx, |state| {
                if let PaneState::Logs(ui) = state {
                    ui.level = level;
                }
            })
        });
    };
    let (w, k) = (ws.clone(), key.clone());
    let follow = move |follow: bool, _: &mut Window, cx: &mut App| {
        let _ = w.update(cx, |this, cx| {
            this.change_pane(&k, cx, |state| {
                if let PaneState::Logs(ui) = state {
                    ui.follow = follow;
                }
            })
        });
    };
    let (w, k) = (ws.clone(), key.clone());
    let pause = move |paused: bool, _: &mut Window, cx: &mut App| {
        let _ = w.update(cx, |this, cx| {
            this.change_pane(&k, cx, |state| {
                if let PaneState::Logs(ui) = state {
                    ui.paused = paused;
                }
            });
            // Resuming shows what came meanwhile.
            this.plugin_panes.changed();
        });
    };
    LogsPaneView::new(view)
        .filter_input(ui.filter.clone())
        .scroll_handle(ui.scroll.clone())
        .on_level(level)
        .on_follow(follow)
        .on_pause(pause)
        .on_clear(on_ws(ws, key, |this, key, _, cx| {
            this.logs_clear(&key.0, key.1, cx);
        }))
}

fn render_sql(ui: &SqlUi, ws: &WeakEntity<Workspace>, key: &PaneKey) -> SqlPaneView {
    SqlPaneView::new(ui.view.clone())
        .query_input(ui.query.clone())
        .on_run(on_ws(ws, key, |this, key, _, cx| {
            this.sql_pane_run(key, false, cx)
        }))
        .on_request_write(on_ws(ws, key, |this, key, _, cx| {
            this.sql_pane_run(key, true, cx)
        }))
}

fn render_web(
    ui: &WebUi,
    page: Option<AnyElement>,
    ws: &WeakEntity<Workspace>,
    key: &PaneKey,
) -> BrowserPaneView {
    let mut pane = BrowserPaneView::new(ui.view.clone())
        .url_input(ui.url.clone())
        .on_back(on_ws(ws, key, |this, key, _, cx| {
            this.web_pane_go(key, WebView::go_back, cx)
        }))
        .on_forward(on_ws(ws, key, |this, key, _, cx| {
            this.web_pane_go(key, WebView::go_forward, cx)
        }))
        .on_reload(on_ws(ws, key, |this, key, _, cx| {
            this.web_pane_go(
                key,
                |view| {
                    view.reload();
                },
                cx,
            )
        }))
        .on_open(on_ws(ws, key, |this, key, _, cx| {
            this.web_pane_open(key, cx)
        }))
        .on_toggle_console(on_ws(ws, key, |this, key, _, cx| {
            this.web_pane_toggle_console(key, cx)
        }));
    if let Some(page) = page {
        pane = pane.page(page);
    }
    pane
}

fn render_files(
    ui: &FilesUi,
    document: Option<AnyElement>,
    ws: &WeakEntity<Workspace>,
    key: &PaneKey,
) -> FilesPaneView {
    let (w, k) = (ws.clone(), key.clone());
    let folder = move |path: SharedString, _: &mut Window, cx: &mut App| {
        let _ = w.update(cx, |this, cx| this.files_toggle(&k, &path, cx));
    };
    let (w, k) = (ws.clone(), key.clone());
    let file = move |path: SharedString, _: &mut Window, cx: &mut App| {
        let _ = w.update(cx, |this, cx| this.files_select(&k, &path, cx));
    };
    let (w, k) = (ws.clone(), key.clone());
    let changed_only = move |on: bool, _: &mut Window, cx: &mut App| {
        let _ = w.update(cx, |this, cx| {
            this.files_set(&k, cx, |ui| ui.changed_only = on)
        });
    };
    let (w, k) = (ws.clone(), key.clone());
    let diff = move |on: bool, _: &mut Window, cx: &mut App| {
        let _ = w.update(cx, |this, cx| this.files_set(&k, cx, |ui| ui.diff = on));
    };
    let mut pane = FilesPaneView::new(ui.view.clone())
        .on_folder(folder)
        .on_file(file)
        .on_refresh(on_ws(ws, key, |this, key, _, cx| {
            this.files_refresh(key, cx);
            cx.notify();
        }))
        .on_changed_only(changed_only)
        .on_diff(diff);
    if let Some(document) = document {
        pane = pane.document(document);
    }
    pane
}

#[cfg(test)]
mod tests {
    use xctx_http::{Failure, FailureKind, Response};

    use super::*;
    use crate::plugin_host::SqlTable;

    fn exchange(id: u64, url: &str, outcome: Outcome) -> Exchange {
        Exchange {
            id,
            request: Request::new("get", url),
            outcome,
            at: 0,
        }
    }

    fn ok(status: u16, body: &str) -> Outcome {
        Outcome::Response(Response {
            status,
            headers: vec![("x-cache".into(), "hit".into())],
            body: body.into(),
            truncated: false,
            elapsed_ms: 12,
        })
    }

    #[test]
    fn the_http_view_lists_newest_first_and_shows_the_selected_response() {
        let failed = Outcome::Failed(Failure {
            kind: FailureKind::Connect,
            message: "connection refused".into(),
            elapsed_ms: 3,
        });
        let data = HttpPaneData {
            base_url: Some("http://localhost:4011/".into()),
            history: vec![
                exchange(1, "http://localhost:4011/health", ok(200, "ok")),
                exchange(2, "http://localhost:4011", failed),
                exchange(3, "http://127.0.0.1:9/other", ok(503, "down")),
            ],
            selected: Some(2),
            diff: Some(ExchangeDiff {
                a: 1,
                b: 3,
                a_status: Some(200),
                b_status: Some(503),
                status_changed: true,
                headers: vec![
                    HeaderChange::Added {
                        name: "retry-after".into(),
                        value: "5".into(),
                    },
                    HeaderChange::Changed {
                        name: "server".into(),
                        from: "a".into(),
                        to: "b".into(),
                    },
                ],
                body: vec!["-ok".into(), "+down".into()],
                a_elapsed_ms: 1,
                b_elapsed_ms: 2,
            }),
            busy: true,
            error: Some("boom".into()),
        };
        let view = http_view(&data);
        let ids: Vec<u64> = view.history.iter().map(|r| r.id).collect();
        assert_eq!(ids, [3, 2, 1]);
        let paths: Vec<&str> = view.history.iter().map(|r| r.path.as_ref()).collect();
        assert_eq!(paths, ["http://127.0.0.1:9/other", "/", "/health"]);
        assert_eq!(view.history[0].method.as_ref(), "GET");
        assert_eq!(view.history[1].status, None);
        let response = view.response.as_ref().ok_or("no response");
        assert_eq!(
            response.map(|r| (r.status, r.body.as_ref())),
            Ok((None, "connection refused"))
        );
        let diff = view
            .diff
            .as_ref()
            .map(|d| d.headers.clone())
            .unwrap_or_default();
        assert_eq!(diff, ["+ retry-after: 5", "~ server: a → b"]);
        assert!(view.busy);
        assert_eq!(view.error.as_ref().map(|e| e.as_ref()), Some("boom"));
        assert_eq!(view.compare, None);
        // Nothing selected: no response.
        let view = http_view(&HttpPaneData {
            selected: None,
            ..data
        });
        assert!(view.response.is_none());
    }

    #[test]
    fn send_builds_a_request_from_the_inputs() {
        let r = http_request("post", " /checkout ", "{\"qty\": 1}");
        assert_eq!((r.method.as_str(), r.url.as_str()), ("POST", "/checkout"));
        assert_eq!(r.body.as_deref(), Some("{\"qty\": 1}"));
        assert_eq!(
            r.headers,
            [("content-type".to_owned(), "application/json".to_owned())]
        );
        let r = http_request("GET", "", "   ");
        assert_eq!((r.url.as_str(), r.body, r.headers.len()), ("/", None, 0));
        let r = http_request("PUT", "http://localhost:1/a", "plain");
        assert_eq!((r.body.as_deref(), r.headers.len()), (Some("plain"), 0));
    }

    fn logs(lines: &[&str], status: TailStatus) -> LogsPaneData {
        LogsPaneData {
            source: "tail -f app.log".into(),
            lines: lines
                .iter()
                .map(|t| xctx_connectors::LogLine {
                    at_ms: None,
                    text: (*t).to_owned(),
                })
                .collect(),
            status,
        }
    }

    #[test]
    fn the_logs_view_filters_by_text_and_level() {
        let data = logs(
            &[
                "INFO started",
                "WARN slow query",
                "ERROR checkout failed",
                "plain line",
            ],
            TailStatus::Running,
        );
        let all = logs_view(Some(&data), "", None);
        assert_eq!((all.lines.len(), all.total), (4, 4));
        assert_eq!(all.lines[2].level, Some(LogLevel::Error));
        assert_eq!(all.source.as_ref(), "tail -f app.log");
        let warn = logs_view(Some(&data), "", Some(LogLevel::Warn));
        let texts: Vec<&str> = warn.lines.iter().map(|l| l.text.as_ref()).collect();
        assert_eq!(texts, ["WARN slow query", "ERROR checkout failed"]);
        let text = logs_view(Some(&data), " CHECKOUT ", None);
        assert_eq!(text.lines.len(), 1);
        assert_eq!(text.filter.as_ref(), " CHECKOUT ");
        let none = logs_view(None, "", None);
        assert_eq!(
            none.error.as_ref().map(|e| e.as_ref()),
            Some(LOGS_NOT_TAILING)
        );
        let failed = logs_view(
            Some(&logs(&[], TailStatus::Failed("no file".into()))),
            "",
            None,
        );
        assert_eq!(failed.error.as_ref().map(|e| e.as_ref()), Some("no file"));
        let exited = logs_view(Some(&logs(&[], TailStatus::Exited(Some(2)))), "", None);
        assert_eq!(exited.source.as_ref(), "tail -f app.log — exited (2)");
    }

    #[test]
    fn the_logs_view_keeps_the_last_lines_and_counts_all() {
        let many: Vec<String> = (0..LOG_LINE_LIMIT + 10)
            .map(|i| format!("line {i}"))
            .collect();
        let refs: Vec<&str> = many.iter().map(String::as_str).collect();
        let view = logs_view(Some(&logs(&refs, TailStatus::Running)), "", None);
        assert_eq!(view.lines.len(), LOG_LINE_LIMIT);
        assert_eq!(view.total, LOG_LINE_LIMIT + 10);
        assert_eq!(view.lines[0].text.as_ref(), "line 10");
    }

    #[test]
    fn the_sql_view_caps_rows_and_keeps_the_count() {
        let rows: Vec<Vec<String>> = (0..SQL_ROW_LIMIT + 5)
            .map(|i| vec![i.to_string()])
            .collect();
        let data = SqlPaneData {
            engine: "sqlite".into(),
            database: "app.db".into(),
            result: Some(SqlTable {
                columns: vec!["id".into()],
                rows,
                row_count: SQL_ROW_LIMIT + 5,
                truncated: false,
            }),
            error: None,
            busy: false,
            write_pending: true,
        };
        let view = sql_view(&data);
        let result = view.result.clone().unwrap_or_default();
        assert_eq!(result.rows.len(), SQL_ROW_LIMIT);
        assert_eq!(result.row_count(), SQL_ROW_LIMIT + 5);
        assert_eq!(result.columns, [SharedString::from("id")]);
        assert_eq!(
            (view.engine.as_ref(), view.database.as_ref()),
            ("sqlite", "app.db")
        );
        assert!(view.write_pending && !view.busy);
        assert!(sql_view(&SqlPaneData::default()).result.is_none());
    }

    #[test]
    fn the_browser_view_maps_the_live_page_masked() {
        let redactor = Redactor::new([("TOKEN".to_owned(), "hunter22".to_owned())]);
        let page = PageShown {
            status: PageStatus {
                url: Some("http://localhost:4011/login?token=hunter22".into()),
                title: Some("Sign in hunter22".into()),
                loading: true,
                progress: 0.25,
                can_go_back: true,
                can_go_forward: false,
                error: Some("http://localhost:4011/x could not be loaded: hunter22".into()),
            },
            console: vec![
                ConsoleEntry {
                    level: "exception".into(),
                    text: "TypeError hunter22".into(),
                    location: Some("app.js:1:2".into()),
                },
                ConsoleEntry {
                    level: "log".into(),
                    text: "hello".into(),
                    location: None,
                },
            ],
            network_errors: vec![
                NetworkError {
                    url: "http://localhost:4011/api?key=hunter22".into(),
                    status: Some(404),
                    reason: "Not Found".into(),
                    resource_type: Some("fetch".into()),
                },
                NetworkError {
                    url: "http://localhost:4011/ws".into(),
                    status: None,
                    reason: "Could not connect to the server.".into(),
                    resource_type: None,
                },
            ],
        };
        let web = WebState {
            busy: 1,
            error: Some("#go: no element matches".into()),
        };
        let view = browser_view(&page, &web, true, &redactor);
        let everything = format!("{view:?}");
        assert!(!everything.contains("hunter22"), "{everything}");
        assert!(view.url.starts_with("http://localhost:4011/login"));
        assert_eq!(view.loading, Some(0.25));
        assert!(view.can_go_back && !view.can_go_forward && view.has_page);
        assert!(view.agent_busy && view.console_open);
        assert!(view.load_error.is_some());
        assert_eq!(view.error.as_deref(), Some("#go: no element matches"));
        assert_eq!(view.console[0].level, Some(LogLevel::Error));
        assert!(view.console[0].text.ends_with("(app.js:1:2)"));
        assert_eq!(view.console[1].level, Some(LogLevel::Info));
        let details: Vec<&str> = view
            .network_errors
            .iter()
            .map(|n| n.detail.as_ref())
            .collect();
        assert_eq!(
            details,
            ["404 Not Found", "Could not connect to the server."]
        );
        assert_eq!(console_level("warning"), Some(LogLevel::Warn));
        assert_eq!(console_level("dir"), None);

        let blank = browser_view(
            &PageShown::default(),
            &WebState::default(),
            false,
            &redactor,
        );
        assert!(!blank.has_page && blank.loading.is_none() && !blank.agent_busy);
    }

    #[test]
    fn pane_states_are_made_for_new_panes_and_dropped_for_gone_ones() {
        let s = SpaceId::new("fix-retry");
        let t = SpaceId::new("other");
        let existing = [
            (s.clone(), PaneId(1)),
            (s.clone(), PaneId(2)),
            (t.clone(), PaneId(1)),
        ];
        // Pane 2 was closed, pane 3 mounted, and the other space's tab closed.
        let live = [(s.clone(), PaneId(1)), (s.clone(), PaneId(3))];
        let (create, drop) = pane_state_changes(existing.iter(), &live);
        assert_eq!(create, [(s.clone(), PaneId(3))]);
        assert_eq!(drop, [(s.clone(), PaneId(2)), (t.clone(), PaneId(1))]);
        // Nothing changes once in step.
        let (create, drop) = pane_state_changes(live.iter(), &live);
        assert!(create.is_empty() && drop.is_empty());
        // Closing a space forgets its panes.
        let mut panes = PluginPanes::default();
        for key in existing {
            panes.panes.insert(
                key,
                PaneUi {
                    state: PaneState::Missing(String::new()),
                    built: None,
                    _subscriptions: Vec::new(),
                },
            );
        }
        panes.forget(&s, PaneId(2));
        panes.forget_space(&t);
        assert_eq!(panes.keys(), [(s, PaneId(1))]);
    }

    #[test]
    fn views_are_rebuilt_on_a_change_or_once_a_second() {
        let now = Instant::now();
        assert!(view_is_stale(None, 0, now));
        assert!(!view_is_stale(Some((3, now)), 3, now));
        assert!(view_is_stale(Some((3, now)), 4, now));
        assert!(view_is_stale(Some((3, now)), 3, now + VIEW_MAX_AGE));
    }

    #[test]
    fn the_files_view_shows_the_tree_and_the_selected_files_text_or_diff() {
        use crate::plugin_host::{FolderEntry, FolderListing, GitStatus, parse_porcelain_z};
        let mut ui = FilesUi::new("/w".into(), "/w".into());
        ui.tree.refresh();
        ui.tree.set_listing(
            "",
            Ok(FolderListing {
                entries: vec![
                    FolderEntry {
                        name: "a.rs".into(),
                        is_dir: false,
                    },
                    FolderEntry {
                        name: "b.rs".into(),
                        is_dir: false,
                    },
                ],
                more: 0,
            }),
        );
        ui.tree.set_status(Ok(GitStatus::new(
            parse_porcelain_z(" M a.rs\0 D c.rs\0"),
            "",
        )));
        let view = files_view(&ui);
        assert_eq!(view.root.as_ref(), "/w");
        let rows: Vec<(&str, &FileRowKind)> = view
            .rows
            .iter()
            .map(|r| (r.path.as_ref(), &r.kind))
            .collect();
        assert_eq!(
            rows,
            [
                (
                    "a.rs",
                    &FileRowKind::File {
                        mark: Some(FileMark::Modified)
                    }
                ),
                ("b.rs", &FileRowKind::File { mark: None }),
                (
                    "c.rs",
                    &FileRowKind::File {
                        mark: Some(FileMark::Deleted)
                    }
                ),
            ]
        );
        assert_eq!(view.preview, FilesPreview::default());
        assert!(!view.busy, "the root and the git status landed");

        ui.selected = Some("a.rs".into());
        ui.preview = Some(Ok(FilePreview {
            size: 12,
            body: PreviewText::Text {
                text: "one\ntwo\n".into(),
                cut: true,
            },
        }));
        let view = files_view(&ui);
        assert_eq!(view.preview.size, Some(12));
        assert!(view.preview.can_diff && !view.preview.diff);
        assert_eq!(
            view.preview.body,
            PreviewBody::Lines(vec!["one".into(), "two".into()])
        );
        assert!(view.preview.cut.is_some_and(|c| c.contains("KiB")));

        ui.diff = true;
        assert_eq!(files_view(&ui).preview.body, PreviewBody::Loading);
        ui.diff_text = Some(Ok(PreviewText::Binary));
        let view = files_view(&ui);
        assert!(view.preview.diff);
        assert_eq!(view.preview.body, PreviewBody::Binary);

        ui.diff = false;
        ui.selected = Some("c.rs".into());
        ui.preview = Some(Err("cannot open c.rs: not found".into()));
        assert_eq!(
            files_view(&ui).preview.body,
            PreviewBody::Message(FILES_DELETED.into())
        );

        ui.selected = Some("guide.md".into());
        ui.preview = Some(Ok(FilePreview {
            size: 9,
            body: PreviewText::Text {
                text: "# Guide\n".into(),
                cut: false,
            },
        }));
        assert!(matches!(
            files_view(&ui).preview.body,
            PreviewBody::Markdown(_)
        ));

        ui.selected = Some("design.pdf".into());
        ui.preview = Some(Ok(FilePreview {
            size: 1024,
            body: PreviewText::Binary,
        }));
        assert_eq!(files_view(&ui).preview.body, PreviewBody::Document);
    }
}
