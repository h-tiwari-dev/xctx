//! [`OmpView`]: the native omp pane. It draws a read-only [`SessionModel`] snapshot (session bar, Changes/Diff
//! panel, HUD, transcript, subagent tray, side questions, todo drawer, banner, composer) and emits [`OmpIntent`]s;
//! it never talks to omp itself.

use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    AnyElement, App, ClipboardItem, Context, Div, Entity, EventEmitter, ExternalPaths, FocusHandle,
    Focusable, FontWeight, Hsla, KeyDownEvent, MouseButton, SharedString, Window, div, prelude::*,
    px, relative,
};
use xctx_omp::{Entry, Phase, SessionModel};
use xctx_ui::{
    Badge, Button, ButtonSize, ButtonVariant, Icon, IconName, IconSize, Pill, PromptEvent,
    PromptInput, Spinner, Theme, Tone, Tooltip,
};

use crate::banner::{self, Banner, Exit, banners_for};
use crate::changes::{self, ChangesPanelProps, ChangesRefresher, CopyCallback, SessionChanges};
use crate::composer::{Composer, ComposerOutput, FileCompleter, PaneEscape, mode_border};
use crate::draft::WhileRunning;
use crate::gate::GateState;
use crate::hud::{
    ModelChoice, TurnMeter, WorkingClock, compact_tokens, context_is_high, cost_label, rate_label,
    todo_progress,
};
use crate::intent::OmpIntent;
use crate::panels::{
    self, IntentCallback, SessionEntry, SessionInfoProps, SessionsDrawerProps, SessionsRefresher,
    SplitAgentHandler, SubagentTrayProps,
};
use crate::selectable::{LinkHandler, SelectContext, TextSelection};
use crate::tool_card::OpenFileHandler;
use crate::transcript::{Transcript, TranscriptOptions};

/// How long a Copy button shows its "Copied" confirmation before returning to "Copy".
const COPY_FEEDBACK_MS: u64 = 1500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HudMenu {
    Model,
    Effort,
}

/// The native omp pane.
///
/// Create it with `cx.new(|cx| OmpView::new(session, cx))`, subscribe to its [`OmpIntent`]s (send each one's
/// [`OmpIntent::command`] to the `OmpClient`) and to [`PaneEscape`] (Esc the pane did not use), and call
/// [`OmpView::set_session`] with a fresh snapshot whenever the model changed. Tell it about the client with
/// [`OmpView::client_exited`] and [`OmpView::malformed_frame`], push worktree git diffs via
/// [`OmpView::set_changes`] (and [`OmpView::set_changes_refresher`]), and fill the model menu with
/// [`OmpView::set_model_choices`] once the host has the answer to [`OmpIntent::ListModels`].
pub struct OmpView {
    pub(crate) session: Arc<SessionModel>,
    /// The request omp waits on, while one is open (see [`crate::gate`]).
    pub(crate) gate: Option<GateState>,
    transcript: Transcript,
    composer: Entity<Composer>,
    spinner: Entity<Spinner>,
    clock: Entity<WorkingClock>,
    meter: TurnMeter,
    /// The folder the pane runs in and its git branch, shown in the footer.
    location: Option<(SharedString, Option<SharedString>)>,
    models: Option<Vec<ModelChoice>>,
    model_filter: Entity<PromptInput>,
    menu: Option<HudMenu>,
    exit: Option<Exit>,
    malformed: Option<String>,
    dismissed: Option<Banner>,
    /// Worktree git changes snapshot for the in-session Changes panel.
    changes: SessionChanges,
    changes_refresher: Option<ChangesRefresher>,
    changes_open: bool,
    selected_change: usize,
    /// Persisted sessions available to view or switch to in this pane.
    sessions: Vec<SessionEntry>,
    sessions_refresher: Option<SessionsRefresher>,
    split_agent: Option<SplitAgentHandler>,
    sessions_open: bool,
    session_filter: Entity<PromptInput>,
    /// Collapsible drawers inspired by Tern's native omp UI.
    info_open: bool,
    tray_open: bool,
    selected_subagent: Option<String>,
    steer_input: Entity<PromptInput>,
    btw_open: bool,
    todos_open: bool,
    /// Key of the Copy control currently showing its brief "Copied" state.
    copied: Option<String>,
    copied_gen: u64,
    /// Shared mouse-drag text selection across transcript rows and diffs.
    selection: TextSelection,
    /// Host callback to open a referenced file in xctx's side file viewer.
    open_file: Option<OpenFileHandler>,
    /// Host callback to open a clicked markdown link (external URL or local file).
    on_link: Option<LinkHandler>,
    /// Inline transcript search (`Cmd+F`).
    search_open: bool,
    search_input: Entity<PromptInput>,
    search_matches: Vec<usize>,
    search_index: usize,
    /// Inline session rename input in the Session info drawer.
    rename_input: Entity<PromptInput>,
    /// Pane-level focus handle so mouse selection in the transcript receives `Cmd+C`/`Cmd+F`/`Esc`.
    focus_handle: FocusHandle,
}

/// The most todo dots the rail draws; more become a `+N`.
const TODO_DOTS: usize = 12;

/// The colour of a todo's dot: done (completed or abandoned) in the accent, the one in progress in warning, the
/// rest in the border colour.
fn todo_dot(status: &str, theme: &Theme) -> Hsla {
    match status {
        "completed" | "abandoned" => theme.accent,
        "in_progress" => theme.warning,
        _ => theme.border,
    }
}

/// A short label in the dock. The selector is for tests.
fn readout(selector: &'static str, text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .debug_selector(move || selector.to_owned())
        .text_size(Theme::TEXT_XS)
        .text_color(color)
        .child(text.into())
}

fn clip_title(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
        None => text.to_owned(),
    }
}

/// Intensity ramp colour for omp's thinking effort level (`docs/m6-theme.md`).
fn effort_color(level: Option<&str>, theme: &Theme) -> Hsla {
    match level.unwrap_or("") {
        "off" | "none" | "minimal" => theme.text_subtle,
        "low" => theme.text_muted,
        "medium" => theme.text,
        "high" => theme.accent,
        "xhigh" | "max" => theme.meta,
        _ => theme.text_muted,
    }
}

impl EventEmitter<OmpIntent> for OmpView {}
impl EventEmitter<PaneEscape> for OmpView {}

impl Focusable for OmpView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

impl OmpView {
    /// A pane showing `session`.
    pub fn new(session: Arc<SessionModel>, cx: &mut Context<Self>) -> Self {
        let view = cx.weak_entity();
        let composer = cx.new(|cx| {
            Composer::new(
                Rc::new(move |output, cx| {
                    view.update(cx, |_, cx| match output {
                        ComposerOutput::Intent(intent) => cx.emit(intent),
                        ComposerOutput::Escape(escape) => cx.emit(escape),
                    })
                    .ok();
                }),
                cx,
            )
        });
        // The footer's Send depends on the draft text, which the composer's input holds.
        let input = composer.read(cx).input().clone();
        cx.observe(&input, |_, _, cx| cx.notify()).detach();

        let model_filter = cx.new(|cx| {
            PromptInput::new(cx)
                .placeholder("Filter models…")
                .borderless(true)
                .compact(true)
        });
        cx.observe(&model_filter, |_, _, cx| cx.notify()).detach();

        let steer_input = cx.new(|cx| {
            PromptInput::new(cx)
                .placeholder("Steer this subagent…")
                .borderless(true)
                .compact(true)
        });
        cx.observe(&steer_input, |_, _, cx| cx.notify()).detach();

        let session_filter = cx.new(|cx| {
            PromptInput::new(cx)
                .placeholder("Filter sessions…")
                .borderless(true)
                .compact(true)
        });
        cx.observe(&session_filter, |_, _, cx| cx.notify()).detach();

        let search_input = cx.new(|cx| {
            PromptInput::new(cx)
                .placeholder("Find in session…")
                .borderless(true)
                .compact(true)
        });
        cx.observe(&search_input, |this: &mut Self, _, cx| {
            this.update_search(true, cx);
        })
        .detach();
        cx.subscribe(
            &search_input,
            |this: &mut Self, _, event: &PromptEvent, cx| match event {
                PromptEvent::Submit(_) => this.step_search(true, cx),
                PromptEvent::Cancel => {
                    this.search_open = false;
                    this.search_matches.clear();
                    cx.notify();
                }
            },
        )
        .detach();

        let rename_input = cx.new(|cx| {
            PromptInput::new(cx)
                .placeholder("Session title…")
                .borderless(true)
                .compact(true)
        });
        cx.observe(&rename_input, |_, _, cx| cx.notify()).detach();
        cx.subscribe(
            &rename_input,
            |_this: &mut Self, _, event: &PromptEvent, cx| {
                if let PromptEvent::Submit(name) = event {
                    let trimmed = name.trim();
                    if !trimmed.is_empty() {
                        cx.emit(OmpIntent::RenameSession {
                            name: trimmed.to_owned(),
                        });
                    }
                }
            },
        )
        .detach();

        let mut this = OmpView {
            session: Arc::new(SessionModel::new()),
            gate: None,
            transcript: Transcript::new(),
            composer,
            spinner: cx.new(|_| Spinner::new().size(IconSize::Sm)),
            clock: cx.new(|_| WorkingClock::new()),
            meter: TurnMeter::default(),
            location: None,
            models: None,
            model_filter,
            menu: None,
            exit: None,
            malformed: None,
            dismissed: None,
            changes: SessionChanges::default(),
            changes_refresher: None,
            changes_open: false,
            selected_change: 0,
            sessions: Vec::new(),
            sessions_refresher: None,
            split_agent: None,
            sessions_open: false,
            session_filter,
            info_open: false,
            tray_open: false,
            selected_subagent: None,
            steer_input,
            btw_open: false,
            todos_open: false,
            copied: None,
            copied_gen: 0,
            selection: TextSelection::new(),
            open_file: None,
            on_link: None,
            search_open: false,
            search_input,
            search_matches: Vec::new(),
            search_index: 0,
            rename_input,
            focus_handle: cx.focus_handle(),
        };
        this.set_session(session, cx);
        this
    }

    /// Show a new snapshot of the session.
    pub fn set_session(&mut self, session: Arc<SessionModel>, cx: &mut Context<Self>) {
        let prev_btw = self.session.btw.len();
        let prev_subagents = self.session.subagents.len();
        let session_changed =
            self.session.session.id != session.session.id && self.session.session.id.is_some();
        if session_changed {
            self.selection.reset();
        }
        if session.session.name != self.session.session.name
            && let Some(name) = &session.session.name
        {
            self.rename_input
                .update(cx, |input, cx| input.set_value(name, cx));
        }

        self.meter.observe(&session, Instant::now());
        let started = self.meter.started();
        self.clock
            .update(cx, |clock, cx| clock.set_started(started, cx));
        let running = session.is_running();
        self.spinner
            .update(cx, |spinner, cx| spinner.set_active(running, cx));
        self.transcript.set_model(session.clone());
        self.composer
            .update(cx, |composer, cx| composer.set_session(session.clone(), cx));
        self.gate = match session.next_gate().cloned() {
            Some(request) if self.gate.as_ref().map(|g| &g.request_id) != Some(&request.id) => {
                Some(GateState::new(&request, cx))
            }
            Some(_) => self.gate.take(),
            None => None,
        };
        if session.btw.len() > prev_btw {
            self.btw_open = true;
        }
        if session.btw.is_empty() {
            self.btw_open = false;
        }
        if session.subagents.len() > prev_subagents {
            self.tray_open = true;
        }
        if session.subagents.is_empty() {
            self.tray_open = false;
            self.selected_subagent = None;
        } else if let Some(selected) = &self.selected_subagent
            && !session.subagents.contains_key(selected)
        {
            self.selected_subagent = None;
        }
        if session.todos.is_empty() {
            self.todos_open = false;
        }
        self.session = session;
        if self.search_open {
            self.update_search(false, cx);
        }
        cx.notify();
    }

    /// Push a fresh snapshot of uncommitted worktree changes for the in-session Changes panel.
    pub fn set_changes(&mut self, changes: SessionChanges, cx: &mut Context<Self>) {
        let count = changes.files.len();
        if count == 0 {
            self.selected_change = 0;
        } else {
            self.selected_change = self.selected_change.min(count - 1);
        }
        self.changes = changes;
        cx.notify();
    }

    /// Register the host callback used when the user clicks Refresh or opens the Changes panel.
    pub fn set_changes_refresher(&mut self, refresher: ChangesRefresher) {
        self.changes_refresher = Some(refresher);
    }

    /// Open or close the in-session Changes panel.
    pub fn set_changes_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.changes_open = open;
        if open && let Some(refresher) = &self.changes_refresher {
            refresher(cx);
        }
        cx.notify();
    }

    /// The latest worktree git changes snapshot shown by the Changes panel.
    pub fn changes(&self) -> &SessionChanges {
        &self.changes
    }

    /// Update the list of persisted sessions shown by the session strip and switcher drawer.
    pub fn set_sessions(&mut self, sessions: Vec<SessionEntry>, cx: &mut Context<Self>) {
        self.sessions = sessions;
        cx.notify();
    }

    /// Register the host callback used to re-scan persisted sessions on disk.
    pub fn set_sessions_refresher(&mut self, refresher: SessionsRefresher) {
        self.sessions_refresher = Some(refresher);
    }

    /// Register the host callback used to open another independent agent session side-by-side.
    pub fn set_split_agent_handler(&mut self, handler: SplitAgentHandler) {
        self.split_agent = Some(handler);
    }

    /// Open or close the Sessions switcher drawer.
    pub fn set_sessions_open(&mut self, open: bool, cx: &mut Context<Self>) {
        self.sessions_open = open;
        if open && let Some(refresher) = &self.sessions_refresher {
            refresher(cx);
        }
        cx.notify();
    }

    /// Register the host callback invoked when the user clicks to open a file from a tool card or Changes panel.
    pub fn set_open_file_handler(&mut self, handler: OpenFileHandler) {
        self.open_file = Some(handler);
    }

    /// Register the host callback invoked when the user clicks a markdown link in the transcript.
    pub fn set_link_handler(&mut self, handler: LinkHandler) {
        self.on_link = Some(handler);
    }

    /// Active text selection handle for the transcript and diff viewer.
    pub fn selection(&self) -> &TextSelection {
        &self.selection
    }

    /// Copy any active text selection in the transcript/diffs to the clipboard. Returns `true` if something was copied.
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(text) = self.selection.selected_text() else {
            return false;
        };
        if text.is_empty() {
            return false;
        }
        self.copy_text("selection".to_owned(), text, cx);
        true
    }

    /// Open or close the inline transcript search bar (`Cmd+F`).
    pub fn set_search_open(&mut self, open: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.search_open = open;
        if open {
            self.search_input.focus_handle(cx).focus(window, cx);
            self.update_search(true, cx);
        } else {
            self.search_matches.clear();
            self.search_index = 0;
            self.composer.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }

    fn update_search(&mut self, jump: bool, cx: &mut Context<Self>) {
        let query = self.search_input.read(cx).value().to_owned();
        self.search_matches = self.transcript.search_rows(&query);
        if self.search_matches.is_empty() {
            self.search_index = 0;
        } else {
            self.search_index = self.search_index.min(self.search_matches.len() - 1);
            if jump {
                self.transcript
                    .scroll_to_row(self.search_matches[self.search_index]);
            }
        }
        cx.notify();
    }

    fn step_search(&mut self, forward: bool, cx: &mut Context<Self>) {
        let len = self.search_matches.len();
        if len == 0 {
            return;
        }
        self.search_index = if forward {
            (self.search_index + 1) % len
        } else {
            (self.search_index + len - 1) % len
        };
        self.transcript
            .scroll_to_row(self.search_matches[self.search_index]);
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &event.keystroke;
        let m = k.modifiers;
        if m.platform && !m.control && !m.alt && !m.shift {
            match k.key.as_str() {
                "c" => {
                    if self.copy_selection(cx) {
                        cx.stop_propagation();
                    }
                    return;
                }
                "f" => {
                    let next = !self.search_open;
                    self.set_search_open(next, window, cx);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
        }
        if k.key == "escape" && !m.platform && !m.control && !m.alt {
            if self.search_open {
                self.set_search_open(false, window, cx);
                cx.stop_propagation();
            } else if self.selection.has_selection() {
                self.selection.clear();
                cx.stop_propagation();
                cx.notify();
            } else if self.session.is_running() && self.focus_handle.is_focused(window) {
                cx.emit(OmpIntent::Abort);
                cx.stop_propagation();
            }
        }
    }

    /// Write `text` to the system clipboard and flash "Copied" on the control identified by `key`.
    fn copy_text(&mut self, key: String, text: String, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.copied = Some(key);
        self.copied_gen = self.copied_gen.wrapping_add(1);
        let copy_gen = self.copied_gen;
        cx.notify();
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(COPY_FEEDBACK_MS))
                .await;
            this.update(cx, |this, cx| {
                if this.copied_gen == copy_gen {
                    this.copied = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn copy_callback(&self, cx: &Context<Self>) -> CopyCallback {
        let view = cx.weak_entity();
        Rc::new(move |key, text, cx| {
            view.update(cx, |this, cx| this.copy_text(key, text, cx))
                .ok();
        })
    }

    fn intent_callback(&self, cx: &Context<Self>) -> IntentCallback {
        let view = cx.weak_entity();
        Rc::new(move |intent, cx| {
            let closes_sessions = matches!(
                intent,
                OmpIntent::SwitchSession { .. } | OmpIntent::NewSession | OmpIntent::Fork
            );
            view.update(cx, |this, cx| {
                if closes_sessions {
                    this.sessions_open = false;
                }
                cx.emit(intent);
            })
            .ok();
        })
    }

    /// Show where the pane runs: the folder (its last component is shown) and the git branch checked out there.
    pub fn set_location(&mut self, folder: &str, branch: Option<&str>, cx: &mut Context<Self>) {
        let name = std::path::Path::new(folder)
            .file_name()
            .map_or_else(|| folder.to_owned(), |n| n.to_string_lossy().into_owned());
        self.location = Some((
            name.into(),
            branch.map(|b| SharedString::from(b.to_owned())),
        ));
        cx.notify();
    }

    /// Answer `@file` completion with `completer` (RPC has none of its own).
    pub fn set_file_completer(&mut self, completer: FileCompleter, cx: &mut Context<Self>) {
        self.composer
            .update(cx, |composer, _| composer.set_file_completer(completer));
    }

    /// Fill the model menu, from [`ModelChoice::from_picker`] on the `get_model_picker` response.
    pub fn set_model_choices(&mut self, choices: Vec<ModelChoice>, cx: &mut Context<Self>) {
        self.models = Some(choices);
        cx.notify();
    }

    /// The omp child exited (the client's `Incoming::Exited`): show it on the banner.
    pub fn client_exited(&mut self, code: Option<i32>, stderr: &str, cx: &mut Context<Self>) {
        self.exit = Some(Exit {
            code,
            stderr: stderr.to_owned(),
        });
        cx.notify();
    }

    /// omp wrote something the client could not decode (`Incoming::Malformed`): show it on the banner.
    pub fn malformed_frame(&mut self, detail: &str, cx: &mut Context<Self>) {
        self.malformed = Some(detail.to_owned());
        cx.notify();
    }

    /// The composer, to focus it or read the draft.
    pub fn composer(&self) -> &Entity<Composer> {
        &self.composer
    }

    /// The transcript rows on screen, in order.
    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    /// The banner shown now, if any: the most serious current problem except the one last dismissed.
    pub fn banner(&self) -> Option<Banner> {
        banners_for(&self.session, self.exit.as_ref(), self.malformed.as_deref())
            .into_iter()
            .find(|b| self.dismissed.as_ref() != Some(b))
    }

    fn toggle_menu(&mut self, menu: HudMenu, cx: &mut Context<Self>) {
        if self.menu == Some(menu) {
            self.menu = None;
        } else {
            self.menu = Some(menu);
            if menu == HudMenu::Model {
                self.model_filter.update(cx, |input, cx| input.clear(cx));
                if self.models.is_none() {
                    self.models = Some(Vec::new());
                    cx.emit(OmpIntent::ListModels);
                }
            }
        }
        cx.notify();
    }

    /// Derive the best human-readable title for the active session: explicit session name, else active persisted
    /// session title, else the first user message's opening line, else `"New session"`.
    fn session_display_title(&self) -> SharedString {
        if let Some(name) = self
            .session
            .session
            .name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            return SharedString::from(clip_title(name, 48));
        }
        if let Some(active) = self
            .sessions
            .iter()
            .find(|s| s.active && !s.title.trim().is_empty())
        {
            return SharedString::from(clip_title(active.title.trim(), 48));
        }
        for entry in &self.session.entries {
            if let Entry::User {
                text,
                from_agent: false,
                ..
            } = entry
                && let Some(line) = text.lines().find(|l| !l.trim().is_empty())
            {
                return SharedString::from(clip_title(line.trim(), 48));
            }
        }
        "New session".into()
    }

    /// Compact session top bar: real session title & switcher, worktree git diffstat, subagents/btw when present,
    /// and a quiet utility icon cluster (search, split, expand tools, copy reply, session menu).
    fn render_session_bar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let session = &self.session;
        let title = self.session_display_title();
        let goal = session.goal.as_ref().and_then(panels::goal_summary);

        let change_count = self.changes.files.len();
        let (total_add, total_del) = self
            .changes
            .files
            .iter()
            .fold((0usize, 0usize), |(a, d), f| {
                (a + f.additions, d + f.deletions)
            });

        let subagent_count = session.subagents.len();
        let subagent_running = session
            .subagents
            .values()
            .filter(|s| panels::subagent_is_active(s))
            .count();
        let btw_count = session.btw.len();

        let has_tools = !session.tools.is_empty();
        let all_open = self.transcript.all_tools_open();

        let last_reply = self.transcript.last_reply();
        let copied_last = self.copied.as_deref() == Some("last-reply");
        let session_count = self.sessions.len();
        let switcher_label: SharedString = if session_count > 1 {
            format!("{title} ({session_count})").into()
        } else {
            title
        };
        let hover = theme.hover;
        let changes_selected = self.changes_open;

        let changes_chip = div()
            .id("omp-changes-toggle")
            .debug_selector(|| "omp-changes-toggle".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(5.0))
            .h(px(24.0))
            .px(Theme::SPACE_2)
            .rounded(Theme::RADIUS_SM)
            .border_1()
            .border_color(if changes_selected {
                theme.accent.alpha(0.55)
            } else if change_count > 0 {
                theme.border_subtle
            } else {
                gpui::transparent_black()
            })
            .bg(if changes_selected {
                theme.selection
            } else if change_count > 0 {
                theme.raised.alpha(0.55)
            } else {
                gpui::transparent_black()
            })
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .tooltip(|_, cx| Tooltip::view("View and copy worktree git changes", cx))
            .on_click(cx.listener(|this, _, _, cx| {
                let next = !this.changes_open;
                this.set_changes_open(next, cx);
            }))
            .child(Icon::new(IconName::Git).size(IconSize::Xs).color(
                if changes_selected || change_count > 0 {
                    theme.text
                } else {
                    theme.text_muted
                },
            ))
            .child(
                div()
                    .text_size(Theme::TEXT_XS)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if changes_selected || change_count > 0 {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .child(if change_count > 0 {
                        format!("Changes {change_count}")
                    } else {
                        "Changes".to_owned()
                    }),
            )
            .when(total_add > 0 || total_del > 0, |row| {
                row.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(3.0))
                        .font_family("Menlo")
                        .text_size(Theme::TEXT_XS)
                        .when(total_add > 0, |r| {
                            r.child(
                                div()
                                    .text_color(theme.success)
                                    .child(format!("+{total_add}")),
                            )
                        })
                        .when(total_del > 0, |r| {
                            r.child(
                                div()
                                    .text_color(theme.danger)
                                    .child(format!("−{total_del}")),
                            )
                        }),
                )
            });

        div()
            .debug_selector(|| "omp-session-bar".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_1)
            .w_full()
            .h(px(34.0))
            .px(Theme::SPACE_3)
            .border_b_1()
            .border_color(theme.border_subtle.alpha(0.75))
            .bg(theme.surface)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .child(
                        div()
                            .id("omp-sessions-toggle")
                            .debug_selector(|| "omp-sessions-toggle".to_owned())
                            .flex()
                            .min_w_0()
                            .items_center()
                            .gap(px(5.0))
                            .h(px(24.0))
                            .px(Theme::SPACE_2)
                            .rounded(Theme::RADIUS_SM)
                            .when(self.sessions_open, |d| d.bg(theme.selection))
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover))
                            .tooltip(|_, cx| {
                                Tooltip::view("Switch, fork, start, or split agent sessions", cx)
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                let next = !this.sessions_open;
                                this.set_sessions_open(next, cx);
                            }))
                            .child(
                                Icon::new(if self.sessions_open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .size(IconSize::Xs)
                                .color(theme.text_subtle),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(Theme::TEXT_SM)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(switcher_label),
                            ),
                    )
                    .child(
                        Button::new("omp-bar-new", "")
                            .icon(IconName::Plus)
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .tooltip("Start a new session")
                            .selector("omp-bar-new")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(OmpIntent::NewSession))),
                    )
                    .children(goal.map(|g| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .pl(Theme::SPACE_1)
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_subtle)
                            .child(g)
                    }))
                    .children(
                        session
                            .status_segments
                            .values()
                            .take(2)
                            .map(|seg| Badge::new(seg.clone()).tone(Tone::Neutral)),
                    ),
            )
            .child(changes_chip)
            .when(subagent_count > 0, |row| {
                let label: SharedString = if subagent_running > 0 {
                    format!("Agents ({subagent_running}/{subagent_count})").into()
                } else {
                    format!("Agents ({subagent_count})").into()
                };
                row.child(
                    Button::new("omp-tray-toggle", label)
                        .icon(IconName::Orchestrator)
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Compact)
                        .selected(self.tray_open)
                        .tooltip("Show or hide subagent tray")
                        .selector("omp-tray-toggle")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.tray_open = !this.tray_open;
                            cx.notify();
                        })),
                )
            })
            .when(btw_count > 0, |row| {
                row.child(
                    Button::new("omp-btw-toggle", format!("Btw ({btw_count})"))
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Compact)
                        .selected(self.btw_open)
                        .tooltip("Show or hide side question answers")
                        .selector("omp-btw-toggle")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.btw_open = !this.btw_open;
                            cx.notify();
                        })),
                )
            })
            .child(
                div()
                    .w(px(1.0))
                    .h(px(14.0))
                    .mx(px(2.0))
                    .bg(theme.border_subtle),
            )
            .child(
                Button::new("omp-search-toggle", "")
                    .icon(IconName::Search)
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .selected(self.search_open)
                    .tooltip("Find in session (⌘F)")
                    .selector("omp-search-toggle")
                    .on_click(cx.listener(|this, _, window, cx| {
                        let next = !this.search_open;
                        this.set_search_open(next, window, cx);
                    })),
            )
            .children(self.split_agent.clone().map(|split| {
                Button::new("omp-split-agent", "")
                    .icon(IconName::SplitH)
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .tooltip("Open another agent session side-by-side")
                    .selector("omp-split-agent")
                    .on_click(move |_, window, cx| split(window, cx))
            }))
            .when(has_tools, |row| {
                row.child(
                    Button::new("omp-tools-toggle", "")
                        .icon(IconName::Layers)
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Compact)
                        .selected(all_open)
                        .tooltip(if all_open {
                            "Collapse all tool cards"
                        } else {
                            "Expand all tool cards"
                        })
                        .selector("omp-tools-toggle")
                        .on_click(cx.listener(|this, _, _, cx| {
                            if this.transcript.all_tools_open() {
                                this.transcript.collapse_all_tools();
                            } else {
                                this.transcript.expand_all_tools();
                            }
                            cx.notify();
                        })),
                )
            })
            .children(last_reply.map(|reply| {
                Button::new("omp-copy-last", if copied_last { "Copied" } else { "" })
                    .icon(if copied_last {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .tooltip("Copy the assistant's latest reply")
                    .selector("omp-copy-last")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.copy_text("last-reply".to_owned(), reply.clone(), cx);
                    }))
            }))
            .child(
                Button::new("omp-session-info", "")
                    .icon(IconName::More)
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .selected(self.info_open)
                    .tooltip("Session info, rename, export HTML, copy transcript, compact, fork")
                    .selector("omp-session-info")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.info_open = !this.info_open;
                        cx.notify();
                    })),
            )
    }

    /// Inline find bar shown when `Cmd+F` or the search icon is toggled.
    fn render_search_bar(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.search_open {
            return None;
        }
        let query = self.search_input.read(cx).value();
        let total = self.search_matches.len();
        let count_label: SharedString = if query.trim().is_empty() {
            "".into()
        } else if total == 0 {
            "No matches".into()
        } else {
            format!("{} / {total}", self.search_index + 1).into()
        };
        Some(
            div()
                .debug_selector(|| "omp-search-bar".to_owned())
                .flex()
                .flex_none()
                .items_center()
                .gap(Theme::SPACE_1)
                .w_full()
                .px(Theme::SPACE_3)
                .py(px(4.0))
                .border_b_1()
                .border_color(theme.border_subtle)
                .bg(theme.raised)
                .child(
                    Icon::new(IconName::Search)
                        .size(IconSize::Xs)
                        .color(theme.text_subtle),
                )
                .child(div().flex_1().min_w_0().child(self.search_input.clone()))
                .when(!count_label.is_empty(), |row| {
                    row.child(
                        div()
                            .debug_selector(|| "omp-search-count".to_owned())
                            .font_family("Menlo")
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_subtle)
                            .child(count_label),
                    )
                })
                .child(
                    Button::new("omp-search-prev", "Prev")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Compact)
                        .disabled(total == 0)
                        .selector("omp-search-prev")
                        .on_click(cx.listener(|this, _, _, cx| this.step_search(false, cx))),
                )
                .child(
                    Button::new("omp-search-next", "Next")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Compact)
                        .disabled(total == 0)
                        .selector("omp-search-next")
                        .on_click(cx.listener(|this, _, _, cx| this.step_search(true, cx))),
                )
                .child(
                    Button::new("omp-search-close", "")
                        .icon(IconName::Close)
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Compact)
                        .tooltip("Close search (Esc)")
                        .selector("omp-search-close")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.set_search_open(false, window, cx);
                        })),
                )
                .into_any_element(),
        )
    }

    /// Whether the dock's top status rail has live content to show (working, needs-you, todos, or high context).
    fn rail_visible(&self) -> bool {
        let s = &self.session;
        s.is_running()
            || s.phase() == Phase::NeedsYou
            || todo_progress(s).is_some()
            || context_is_high(s)
    }

    /// The dock's top rail: only rendered when active (working + Stop, needs-you badge, clickable todos, or high
    /// context warning). When idle, the context readout lives cleanly in the footer instead of leaving a blank row.
    fn render_rail(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.rail_visible() {
            return None;
        }
        let session = &self.session;
        let running = session.is_running();
        let hover = theme.hover;
        let working = running.then(|| {
            div()
                .debug_selector(|| "omp-working".to_owned())
                .flex()
                .items_center()
                .gap(Theme::SPACE_1)
                .child(self.spinner.clone())
                .child(
                    div()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_muted)
                        .child(if session.compacting {
                            "Compacting"
                        } else {
                            "Working"
                        }),
                )
                .child(self.clock.clone())
                .child(
                    Button::new("omp-stop", "Stop")
                        .variant(ButtonVariant::Danger)
                        .size(ButtonSize::Compact)
                        .tooltip("Stop the turn (Esc)")
                        .selector("omp-stop")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(OmpIntent::Abort))),
                )
        });
        let needs_you = (session.phase() == Phase::NeedsYou)
            .then(|| Badge::new("needs you").tone(Tone::Warning));
        let todos = todo_progress(session).map(|(done, total)| {
            let statuses: Vec<&str> = session
                .todos
                .iter()
                .flat_map(|phase| &phase.tasks)
                .map(|task| task.status.as_str())
                .collect();
            div()
                .id("omp-todos-toggle")
                .debug_selector(|| "omp-todos-toggle".to_owned())
                .flex()
                .items_center()
                .gap(px(3.0))
                .px(Theme::SPACE_1)
                .py(px(2.0))
                .rounded(Theme::RADIUS_SM)
                .cursor_pointer()
                .when(self.todos_open, |d| d.bg(theme.selection))
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.todos_open = !this.todos_open;
                    cx.notify();
                }))
                .children(statuses.iter().take(TODO_DOTS).map(|status| {
                    div()
                        .size(px(6.0))
                        .rounded_full()
                        .bg(todo_dot(status, theme))
                }))
                .children((statuses.len() > TODO_DOTS).then(|| {
                    readout(
                        "omp-hud-todos-more",
                        format!("+{}", statuses.len() - TODO_DOTS),
                        theme.text_subtle,
                    )
                }))
                .child(readout(
                    "omp-hud-todos",
                    format!("{done}/{total}"),
                    theme.text_muted,
                ))
        });
        Some(
            div()
                .debug_selector(|| "omp-rail".to_owned())
                .flex()
                .flex_none()
                .items_center()
                .gap(Theme::SPACE_2)
                .w_full()
                .h(px(30.0))
                .px(Theme::SPACE_3)
                .border_b_1()
                .border_color(theme.border_subtle)
                .children(working)
                .children(needs_you)
                .children(todos)
                .child(div().flex_1())
                .children(self.render_context(theme, cx)),
        )
    }

    /// The context meter: how full the model's window is, with the window's size. Warns from 80% and offers a
    /// one-click Compact action when high.
    fn render_context(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let usage = self.session.context?;
        let percent = usage.percent.clamp(0.0, 100.0);
        let high = context_is_high(&self.session);
        let color = if high { theme.warning } else { theme.accent };
        let window_label = compact_tokens(usage.window);
        let tip: SharedString = format!("Context window: {percent:.0}% of {window_label}").into();
        Some(
            div()
                .id("omp-hud-context")
                .debug_selector(|| "omp-hud-context".to_owned())
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.0))
                .tooltip(move |_, cx| Tooltip::view(tip.clone(), cx))
                .child(
                    div()
                        .relative()
                        .w(px(44.0))
                        .h(px(5.0))
                        .rounded_full()
                        .bg(theme.border_strong.alpha(0.45))
                        .overflow_hidden()
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .h_full()
                                .w(relative(percent as f32 / 100.0))
                                .rounded_full()
                                .bg(color),
                        ),
                )
                .child(
                    readout(
                        "omp-hud-context-percent",
                        format!("{percent:.0}%"),
                        if high {
                            theme.warning
                        } else {
                            theme.text_muted
                        },
                    )
                    .font_family("Menlo"),
                )
                .child(
                    readout("omp-hud-context-window", window_label, theme.text_subtle)
                        .font_family("Menlo"),
                )
                .when(high && !self.session.compacting, |row| {
                    row.child(
                        Button::new("omp-hud-compact", "Compact")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .tooltip("Compact conversation context")
                            .selector("omp-hud-compact")
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(OmpIntent::Compact { instructions: None });
                            })),
                    )
                })
                .into_any_element(),
        )
    }

    /// The Send buttons: Steer and Follow up while the agent runs, Send when it is idle.
    fn render_send(&self, cx: &mut Context<Self>) -> AnyElement {
        let sendable = self.composer.read(cx).can_send(cx);
        if self.session.is_running() {
            let composer_follow = self.composer.clone();
            let composer_steer = self.composer.clone();
            div()
                .flex()
                .flex_none()
                .gap(Theme::SPACE_1)
                .child(
                    Button::new("omp-send-follow-up", "Follow up")
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Compact)
                        .rounded_full()
                        .tooltip("Queue for when the turn ends (Alt+Enter)")
                        .disabled(!sendable)
                        .selector("omp-send-follow-up")
                        .on_click(move |_, _, cx| {
                            composer_follow
                                .update(cx, |c, cx| c.submit(WhileRunning::FollowUp, cx));
                        }),
                )
                .child(
                    Button::new("omp-send-steer", "Steer")
                        .icon(IconName::Send)
                        .variant(ButtonVariant::Primary)
                        .size(ButtonSize::Compact)
                        .rounded_full()
                        .tooltip("Interrupt at the next step (Enter)")
                        .disabled(!sendable)
                        .selector("omp-send-steer")
                        .on_click(move |_, _, cx| {
                            composer_steer.update(cx, |c, cx| c.submit(WhileRunning::Steer, cx));
                        }),
                )
                .into_any_element()
        } else {
            let composer = self.composer.clone();
            div()
                .flex()
                .flex_none()
                .child(
                    Button::new("omp-send", "Send")
                        .icon(IconName::Send)
                        .variant(ButtonVariant::Primary)
                        .size(ButtonSize::Compact)
                        .rounded_full()
                        .tooltip("Send (Enter)")
                        .disabled(!sendable)
                        .selector("omp-send")
                        .on_click(move |_, _, cx| {
                            composer.update(cx, |c, cx| c.submit(WhileRunning::Steer, cx));
                        }),
                )
                .into_any_element()
        }
    }

    /// The dock's footer: sleek model/effort/mode selectors on the left, clean breathing space in the middle,
    /// context meter/cost/rate and primary Send pill anchored on the right.
    fn render_footer(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let session = &self.session;
        let model_label: SharedString = session
            .model
            .as_ref()
            .map_or_else(|| "Model".into(), |m| m.label().to_owned().into());
        let effort_raw = session.thinking.as_deref();
        let effort_label: SharedString = effort_raw
            .map(|e| e.to_owned().into())
            .unwrap_or_else(|| "Effort".into());
        let effort_fg = effort_color(effort_raw, theme);
        let inline_context = (!self.rail_visible())
            .then(|| self.render_context(theme, cx))
            .flatten();
        let inline_context_present = inline_context.is_some();
        let fast_active = session.fast_mode;
        let show_slow = session.slow_mode_supported;
        let slow_active = session.slow_mode;
        let model_open = self.menu == Some(HudMenu::Model);
        let effort_open = self.menu == Some(HudMenu::Effort);
        let effort_disabled = session.thinking_levels.is_empty();
        let hover = theme.hover;
        let cost_zero = session.stats.cost <= 0.0;

        let model_picker = div()
            .id("omp-hud-model")
            .debug_selector(|| "omp-hud-model".to_owned())
            .flex()
            .flex_shrink(1.0)
            .min_w(px(64.0))
            .max_w(px(210.0))
            .items_center()
            .gap(px(5.0))
            .h(px(24.0))
            .px(Theme::SPACE_2)
            .rounded(px(6.0))
            .border_1()
            .border_color(if model_open {
                theme.accent.alpha(0.6)
            } else {
                theme.border_subtle
            })
            .bg(if model_open {
                theme.selection
            } else {
                theme.surface.alpha(0.55)
            })
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .tooltip(|_, cx| Tooltip::view("Choose model", cx))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(HudMenu::Model, cx)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(Theme::TEXT_XS)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .child(model_label),
            )
            .child(
                Icon::new(IconName::ChevronDown)
                    .size(IconSize::Xs)
                    .color(theme.text_subtle),
            );

        let effort_picker = {
            let mut el = div()
                .id("omp-hud-effort")
                .debug_selector(|| "omp-hud-effort".to_owned())
                .flex()
                .flex_none()
                .items_center()
                .gap(px(4.0))
                .h(px(24.0))
                .px(Theme::SPACE_2)
                .rounded(px(6.0))
                .border_1()
                .border_color(if effort_open {
                    theme.accent.alpha(0.6)
                } else {
                    theme.border_subtle
                })
                .bg(if effort_open {
                    theme.selection
                } else {
                    theme.surface.alpha(0.55)
                })
                .tooltip(|_, cx| Tooltip::view("Thinking effort", cx))
                .child(
                    div()
                        .text_size(Theme::TEXT_XS)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(effort_fg)
                        .child(effort_label),
                )
                .child(
                    Icon::new(IconName::ChevronDown)
                        .size(IconSize::Xs)
                        .color(theme.text_subtle),
                );
            if effort_disabled {
                el = el.opacity(0.45);
            } else {
                el = el
                    .cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(HudMenu::Effort, cx)));
            }
            el
        };

        div()
            .debug_selector(|| "omp-footer".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.0))
            .w_full()
            .h(px(38.0))
            .px(px(10.0))
            .border_t_1()
            .border_color(theme.border_subtle.alpha(0.6))
            .child(model_picker)
            .child(effort_picker)
            .child(
                Button::new("omp-hud-fast", "Fast")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .selected(fast_active)
                    .tooltip("Toggle fast mode")
                    .selector("omp-hud-fast")
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(OmpIntent::SetFastMode {
                            enabled: !fast_active,
                        });
                    })),
            )
            .when(show_slow, |row| {
                row.child(
                    Button::new("omp-hud-slow", "Slow")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Compact)
                        .selected(slow_active)
                        .tooltip("Toggle slow mode")
                        .selector("omp-hud-slow")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(OmpIntent::SetSlowMode {
                                enabled: !slow_active,
                            });
                        })),
                )
            })
            .child(div().flex_1().min_w_0())
            .children(inline_context)
            .when(inline_context_present, |row| {
                row.child(
                    div()
                        .w(px(1.0))
                        .h(px(12.0))
                        .mx(px(2.0))
                        .bg(theme.border_subtle),
                )
            })
            .child(
                readout(
                    "omp-hud-cost",
                    cost_label(session),
                    if cost_zero {
                        theme.text_subtle
                    } else {
                        theme.meta
                    },
                )
                .font_family("Menlo"),
            )
            .children(self.meter.rate().map(|rate| {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(5.0))
                    .child(
                        div()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_subtle)
                            .child("·"),
                    )
                    .child(
                        readout("omp-hud-rate", rate_label(rate), theme.meta).font_family("Menlo"),
                    )
            }))
            .child(div().pl(px(4.0)).child(self.render_send(cx)))
    }

    fn render_menu(&self, menu: HudMenu, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let inner: AnyElement = match menu {
            HudMenu::Effort => {
                let current = self.session.thinking.as_deref();
                div()
                    .debug_selector(|| "omp-effort-menu".to_owned())
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .w_full()
                    .px(Theme::SPACE_3)
                    .py(Theme::SPACE_2)
                    .child(
                        div()
                            .pr(Theme::SPACE_1)
                            .text_size(Theme::TEXT_XS)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_subtle)
                            .child("Thinking effort"),
                    )
                    .children(self.session.thinking_levels.iter().map(|level| {
                        let pick = level.clone();
                        Pill::new(
                            SharedString::from(format!("omp-effort-{level}")),
                            level.clone(),
                        )
                        .selected(current == Some(level.as_str()))
                        .selector(format!("omp-effort-{level}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.menu = None;
                            cx.emit(OmpIntent::SetThinkingLevel {
                                level: pick.clone(),
                            });
                            cx.notify();
                        }))
                        .into_any_element()
                    }))
                    .into_any_element()
            }
            HudMenu::Model => {
                let choices = self.models.as_deref().unwrap_or_default();
                let query = self.model_filter.read(cx).value().trim().to_lowercase();
                let current = self.session.model.as_ref();

                let body: AnyElement = if choices.is_empty() {
                    div()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_subtle)
                        .child("Loading models…")
                        .into_any_element()
                } else {
                    let filtered: Vec<(usize, &ModelChoice)> = choices
                        .iter()
                        .enumerate()
                        .filter(|(_, c)| {
                            query.is_empty()
                                || c.label().to_lowercase().contains(&query)
                                || c.id.to_lowercase().contains(&query)
                                || c.provider.to_lowercase().contains(&query)
                        })
                        .collect();
                    if filtered.is_empty() {
                        div()
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.text_subtle)
                            .child("No models match.")
                            .into_any_element()
                    } else {
                        div()
                            .id("omp-model-list")
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(Theme::SPACE_1)
                            .w_full()
                            .max_h(px(180.0))
                            .overflow_y_scroll()
                            .children(filtered.into_iter().map(|(i, choice)| {
                                let selected = current.is_some_and(|m| {
                                    m.provider == choice.provider && m.id == choice.id
                                });
                                let (provider, id) = (choice.provider.clone(), choice.id.clone());
                                Pill::new(("omp-model", i), choice.label().to_owned())
                                    .selected(selected)
                                    .selector(format!("omp-model-{i}"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.menu = None;
                                        cx.emit(OmpIntent::SetModel {
                                            provider: provider.clone(),
                                            model_id: id.clone(),
                                        });
                                        cx.notify();
                                    }))
                                    .into_any_element()
                            }))
                            .into_any_element()
                    }
                };

                div()
                    .debug_selector(|| "omp-model-menu".to_owned())
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_2)
                    .w_full()
                    .px(Theme::SPACE_3)
                    .py(Theme::SPACE_2)
                    .when(!choices.is_empty(), |col| {
                        col.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(Theme::SPACE_1)
                                .px(Theme::SPACE_2)
                                .py(px(3.0))
                                .rounded(Theme::RADIUS_SM)
                                .bg(theme.surface)
                                .border_1()
                                .border_color(theme.border_subtle)
                                .child(
                                    Icon::new(IconName::Search)
                                        .size(IconSize::Xs)
                                        .color(theme.text_subtle),
                                )
                                .child(div().flex_1().min_w_0().child(self.model_filter.clone())),
                        )
                    })
                    .child(body)
                    .into_any_element()
            }
        };
        div()
            .flex_none()
            .w_full()
            .px(Theme::SPACE_4)
            .pb(px(6.0))
            .child(
                div()
                    .w_full()
                    .max_w(px(880.0))
                    .mx_auto()
                    .rounded(px(10.0))
                    .border_1()
                    .border_color(theme.border_strong)
                    .bg(theme.overlay)
                    .shadow_md()
                    .overflow_hidden()
                    .child(inner),
            )
            .into_any_element()
    }

    fn render_empty(&self, theme: &Theme) -> impl IntoElement {
        let hint = |keys: &'static str, what: &'static str| {
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .min_w(px(28.0))
                        .h(px(20.0))
                        .px(px(6.0))
                        .rounded(px(5.0))
                        .bg(theme.raised)
                        .border_1()
                        .border_color(theme.border)
                        .font_family("Menlo")
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.meta)
                        .child(keys),
                )
                .child(
                    div()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_muted)
                        .child(what),
                )
        };
        let location_pill = self.location.as_ref().map(|(folder, branch)| {
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(3.0))
                .rounded_full()
                .bg(theme.raised.alpha(0.7))
                .border_1()
                .border_color(theme.border_subtle)
                .text_size(Theme::TEXT_XS)
                .child(
                    Icon::new(IconName::Folder)
                        .size(IconSize::Xs)
                        .color(theme.text_subtle),
                )
                .child(
                    div()
                        .font_family("Menlo")
                        .text_color(theme.text_muted)
                        .child(folder.clone()),
                )
                .children(branch.clone().map(|b| {
                    div()
                        .flex()
                        .items_center()
                        .gap(px(4.0))
                        .pl(px(4.0))
                        .border_l_1()
                        .border_color(theme.border_subtle)
                        .font_family("Menlo")
                        .text_color(theme.meta)
                        .child(format!("⎇ {b}"))
                }))
        });
        div()
            .debug_selector(|| "omp-empty".to_owned())
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .size_full()
            .px(Theme::SPACE_4)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(36.0))
                    .rounded(px(10.0))
                    .bg(theme.tone(Tone::Accent).bg)
                    .border_1()
                    .border_color(theme.tone(Tone::Accent).border)
                    .child(
                        Icon::new(IconName::Orchestrator)
                            .size(IconSize::Md)
                            .color(theme.accent),
                    ),
            )
            .child(
                div()
                    .text_size(Theme::TEXT_XL)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child("What should we work on?"),
            )
            .children(location_pill)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.0))
                    .mt(px(4.0))
                    .px(px(16.0))
                    .py(px(10.0))
                    .rounded(px(10.0))
                    .bg(theme.raised.alpha(0.35))
                    .border_1()
                    .border_color(theme.border_subtle.alpha(0.75))
                    .child(hint("/", "Run slash commands"))
                    .child(hint("@", "Reference a file in the workspace"))
                    .child(hint(
                        "!",
                        "Run a shell command (!! keeps output out of context)",
                    ))
                    .child(hint("?", "Ask a quick side question"))
                    .child(hint("⌥↵", "Queue a follow-up while the agent works")),
            )
    }
}

impl Render for OmpView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let on_copy = self.copy_callback(cx);
        let on_intent = self.intent_callback(cx);
        let search_query: SharedString = if self.search_open {
            self.search_input.read(cx).value().to_owned().into()
        } else {
            SharedString::default()
        };
        let select_ctx = SelectContext {
            selection: self.selection.clone(),
            view: cx.entity_id(),
            search_query,
            on_copy: Some(on_copy.clone()),
            on_link: self.on_link.clone(),
        };
        let view_edit = cx.weak_entity();
        let on_edit_prompt = Rc::new(move |text: String, cx: &mut App| {
            view_edit
                .update(cx, |this, cx| {
                    this.composer
                        .update(cx, |composer, cx| composer.set_text(&text, cx));
                    cx.notify();
                })
                .ok();
        });

        let body = if self.transcript.is_empty() {
            self.render_empty(&theme).into_any_element()
        } else {
            self.transcript.render_interactive(
                cx.entity_id(),
                TranscriptOptions {
                    copied: self.copied.clone(),
                    on_copy: Some(on_copy.clone()),
                    select: Some(select_ctx.clone()),
                    on_open_file: self.open_file.clone(),
                    on_edit_prompt: Some(on_edit_prompt),
                },
            )
        };

        let show_jump = !self.transcript.is_empty() && !self.transcript.is_scrolled_to_end();
        let has_selection = self.selection.has_selection();
        let copied_sel = self.copied.as_deref() == Some("selection");

        let changes_panel = self.changes_open.then(|| {
            let view = cx.weak_entity();
            let view_refresh = cx.weak_entity();
            let view_close = cx.weak_entity();
            changes::render_changes_panel(
                ChangesPanelProps {
                    changes: &self.changes,
                    selected: self.selected_change,
                    copied: self.copied.as_deref(),
                    has_refresher: self.changes_refresher.is_some(),
                    on_select: Rc::new(move |ix, cx| {
                        view.update(cx, |this, cx| {
                            this.selected_change = ix;
                            cx.notify();
                        })
                        .ok();
                    }),
                    on_copy: on_copy.clone(),
                    on_open_file: self.open_file.clone(),
                    select: Some(select_ctx),
                    on_refresh: Rc::new(move |cx| {
                        view_refresh
                            .update(cx, |this, cx| {
                                if let Some(refresher) = &this.changes_refresher {
                                    refresher(cx);
                                }
                            })
                            .ok();
                    }),
                    on_close: Rc::new(move |cx| {
                        view_close
                            .update(cx, |this, cx| {
                                this.changes_open = false;
                                cx.notify();
                            })
                            .ok();
                    }),
                },
                &theme,
            )
        });

        let sessions_strip = (!self.sessions_open && self.sessions.len() > 1).then(|| {
            let view_open = cx.weak_entity();
            panels::render_sessions_strip(
                &self.sessions,
                on_intent.clone(),
                self.split_agent.clone(),
                Rc::new(move |cx| {
                    view_open
                        .update(cx, |this, cx| this.set_sessions_open(true, cx))
                        .ok();
                }),
                &theme,
            )
        });

        let sessions_drawer = self.sessions_open.then(|| {
            let view_close = cx.weak_entity();
            let filter_text = self.session_filter.read(cx).value().to_owned();
            panels::render_sessions_drawer(
                SessionsDrawerProps {
                    sessions: &self.sessions,
                    filter_input: self.session_filter.clone(),
                    filter_text: &filter_text,
                    on_intent: on_intent.clone(),
                    on_refresh: self.sessions_refresher.clone(),
                    on_split: self.split_agent.clone(),
                    on_close: Rc::new(move |cx| {
                        view_close
                            .update(cx, |this, cx| {
                                this.sessions_open = false;
                                cx.notify();
                            })
                            .ok();
                    }),
                },
                &theme,
            )
        });

        let info_panel = self.info_open.then(|| {
            let view_close = cx.weak_entity();
            let rename_value = self.rename_input.read(cx).value().to_owned();
            panels::render_session_info(
                SessionInfoProps {
                    session: &self.session,
                    transcript_text: self.transcript.format_markdown(),
                    last_reply: self.transcript.last_reply(),
                    rename_input: self.rename_input.clone(),
                    rename_value: &rename_value,
                    copied: self.copied.as_deref(),
                    on_copy: on_copy.clone(),
                    on_intent: on_intent.clone(),
                    on_close: Rc::new(move |cx| {
                        view_close
                            .update(cx, |this, cx| {
                                this.info_open = false;
                                cx.notify();
                            })
                            .ok();
                    }),
                },
                &theme,
            )
        });

        let subagent_tray = (self.tray_open && !self.session.subagents.is_empty()).then(|| {
            let view_sel = cx.weak_entity();
            let view_close = cx.weak_entity();
            panels::render_subagent_tray(
                SubagentTrayProps {
                    session: &self.session,
                    selected: self.selected_subagent.as_deref(),
                    steer_input: self.steer_input.clone(),
                    copied: self.copied.as_deref(),
                    on_select: Rc::new(move |id, cx| {
                        view_sel
                            .update(cx, |this, cx| {
                                this.selected_subagent = id;
                                cx.notify();
                            })
                            .ok();
                    }),
                    on_intent: on_intent.clone(),
                    on_copy: on_copy.clone(),
                    on_close: Rc::new(move |cx| {
                        view_close
                            .update(cx, |this, cx| {
                                this.tray_open = false;
                                cx.notify();
                            })
                            .ok();
                    }),
                },
                &theme,
            )
        });

        let btw_panel = (self.btw_open && !self.session.btw.is_empty()).then(|| {
            let view_close = cx.weak_entity();
            panels::render_btw_panel(
                &self.session.btw,
                self.copied.as_deref(),
                on_copy.clone(),
                Rc::new(move |cx| {
                    view_close
                        .update(cx, |this, cx| {
                            this.btw_open = false;
                            cx.notify();
                        })
                        .ok();
                }),
                &theme,
            )
        });

        let todo_drawer = (self.todos_open && !self.session.todos.is_empty()).then(|| {
            let view_close = cx.weak_entity();
            panels::render_todo_drawer(
                &self.session.todos,
                self.copied.as_deref(),
                on_copy,
                Rc::new(move |cx| {
                    view_close
                        .update(cx, |this, cx| {
                            this.todos_open = false;
                            cx.notify();
                        })
                        .ok();
                }),
                &theme,
            )
        });

        let banner = self.banner().map(|b| {
            let shown = b.clone();
            banner::render(
                &b,
                Box::new(cx.listener(move |this, _, _, cx| {
                    this.dismissed = Some(shown.clone());
                    cx.notify();
                })),
                &theme,
            )
        });
        let accent = theme.accent;
        let (mode, focused) = {
            let composer = self.composer.read(cx);
            (composer.mode(cx), composer.is_focused(window, cx))
        };
        let dock = div()
            .debug_selector(|| "omp-dock".to_owned())
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .max_w(px(880.0))
            .mx_auto()
            .rounded(px(12.0))
            .border_1()
            .when(mode.label().is_some(), |dock| dock.border_2())
            .border_color(mode_border(mode, focused, &theme))
            .bg(theme.raised)
            .shadow_md()
            .overflow_hidden()
            .children(self.render_rail(&theme, cx))
            .child(
                div()
                    .px(px(14.0))
                    .pt(px(8.0))
                    .pb(px(4.0))
                    .child(self.composer.clone()),
            )
            .child(self.render_footer(&theme, cx));
        let dock_wrap = div()
            .flex_none()
            .w_full()
            .px(Theme::SPACE_4)
            .pt(px(4.0))
            .pb(px(14.0))
            .child(dock);
        div()
            .debug_selector(|| "omp-view".to_owned())
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.surface)
            .text_color(theme.text)
            .border_1()
            .border_color(gpui::transparent_black())
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.border_color(accent))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.composer
                    .update(cx, |composer, cx| composer.attach_paths(paths.paths(), cx));
            }))
            .child(self.render_session_bar(&theme, cx))
            .children(self.render_search_bar(&theme, cx))
            .children(sessions_strip)
            .children(sessions_drawer)
            .children(info_panel)
            .children(changes_panel)
            .child(
                div()
                    .debug_selector(|| "omp-transcript".to_owned())
                    .track_focus(&self.focus_handle)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, window, cx| {
                            this.focus_handle.focus(window, cx);
                        }),
                    )
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .py(Theme::SPACE_2)
                    .child(body)
                    .when(show_jump || has_selection, |wrap| {
                        wrap.child(
                            div()
                                .absolute()
                                .bottom(Theme::SPACE_2)
                                .right(Theme::SPACE_4)
                                .flex()
                                .items_center()
                                .gap(Theme::SPACE_1)
                                .when(has_selection, |row| {
                                    row.child(
                                        Button::new(
                                            "omp-copy-selection",
                                            if copied_sel {
                                                "Copied"
                                            } else {
                                                "Copy selection ⌘C"
                                            },
                                        )
                                        .icon(if copied_sel {
                                            IconName::Check
                                        } else {
                                            IconName::Copy
                                        })
                                        .variant(ButtonVariant::Primary)
                                        .size(ButtonSize::Compact)
                                        .selector("omp-copy-selection")
                                        .on_click(
                                            cx.listener(|this, _, _, cx| {
                                                this.copy_selection(cx);
                                            }),
                                        ),
                                    )
                                    .child(
                                        Button::new("omp-clear-selection", "")
                                            .icon(IconName::Close)
                                            .variant(ButtonVariant::Secondary)
                                            .size(ButtonSize::Compact)
                                            .tooltip("Clear selection (Esc)")
                                            .selector("omp-clear-selection")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.selection.clear();
                                                cx.notify();
                                            })),
                                    )
                                })
                                .when(show_jump, |row| {
                                    row.child(
                                        Button::new("omp-scroll-bottom", "Jump to bottom")
                                            .icon(IconName::ChevronDown)
                                            .variant(ButtonVariant::Secondary)
                                            .size(ButtonSize::Compact)
                                            .selector("omp-scroll-bottom")
                                            .on_click(cx.listener(|this, _, _, cx| {
                                                this.transcript.scroll_to_end();
                                                cx.notify();
                                            })),
                                    )
                                }),
                        )
                    }),
            )
            .children(todo_drawer)
            .children(btw_panel)
            .children(subagent_tray)
            .children(self.render_gate(&theme, cx))
            .children(banner)
            .children(self.menu.map(|menu| self.render_menu(menu, &theme, cx)))
            .child(dock_wrap)
    }
}
