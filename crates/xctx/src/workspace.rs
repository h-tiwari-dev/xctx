//! The main window's root view: the title bar row over the Roster and Home or a space, over the status bar; the
//! command palette over all of it; and the state they share (the store, the repo index, the open spaces, the
//! status banner).

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use gpui::{
    Context, Entity, FocusHandle, Focusable, Hsla, IntoElement, ParentElement, Render,
    SharedString, Styled, Subscription, Task, Window, div, prelude::*,
};
use xctx_core::{Redactor, RepoIndex, Space, SpaceId, Store};
use xctx_inbox::{Inbox, ItemId};
use xctx_layout::{FocusDirection, SplitAxis};
use xctx_plugin::RunnerRegistry;
use xctx_term_view::{TerminalFont, TerminalTheme};
use xctx_ui::command_palette::{CommandPalette, PaletteEvent};
use xctx_ui::{BannerKind, PromptEvent, PromptInput, StatusBanner, StatusBar, Theme, ToastStack};

use crate::agent_session::SessionStorage;
use crate::attempt_ui::shown_worktree;
use crate::attempts::SpaceAttempts;
use crate::control::{Control, PendingDraft, PendingMaskedRead};
use crate::dialogs::Dialog;
use crate::inbox_view::{EnvPrompt, PlanLaunches};
use crate::inspector::{self, InspectorPlacement};
use crate::keymap::{
    ActivateTab, ClosePane, CloseWindow, DismissDialog, FocusDown, FocusLeft, FocusNext, FocusPrev,
    FocusRight, FocusUp, ForkAttempts, LaunchDraft, Quit, ResizeDown, ResizeLeft, ResizeRight,
    ResizeUp, ReviewAttempts, ShowFiles, ShowHome, ShowInspectorPlugins, SplitDown, SplitRight,
    ToggleDock, ToggleFlow, ToggleInbox, ToggleInspector, TogglePalette, ToggleRoster,
    ToggleTranslucency, ToggleZoom, WORKSPACE_CONTEXT, palette_hint,
};
use crate::lifecycle::scan_index;
use crate::parent_agent::ParentAgent;
use crate::plugin_host::{PluginHost, load_runners, user_plugin_dir, user_runner_dir};
use crate::shell::theme_for;
use crate::space_graph::SpaceGraphs;
use crate::space_layout::layout_shape;
use crate::space_tab::SpaceTab;
use crate::spawn::ProgramSearch;
use crate::ui_prefs::UiPrefs;

/// What the window shows to the right of the Roster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum View {
    /// Home: the prompt, the draft, the living spaces and the indexed folders.
    Home,
    /// A space's panes.
    Space(SpaceId),
}

/// The root view of the main window.
pub(crate) struct Workspace {
    pub(crate) focus_handle: FocusHandle,
    /// The store; `None` while a background job (launch, ship, discard) has it, or when it could not be opened.
    pub(crate) store: Option<Store>,
    /// Why the store could not be opened.
    pub(crate) store_error: Option<String>,
    /// What a background job holding the store is doing, for the UI.
    pub(crate) busy: Option<SharedString>,
    /// The active spaces, as last read from the store.
    pub(crate) spaces: Vec<Space>,
    /// Spaces whose layout changed while the store was busy, saved once it is back.
    pub(crate) pending_persist: HashSet<SpaceId>,
    /// Spaces whose divider drags are waiting for [`Workspace::persist_task`] to save them.
    pub(crate) pending_drag_saves: HashSet<SpaceId>,
    /// Spaces whose Ship or Discard job is running: they cannot be opened, shown, shipped or discarded until it
    /// ends.
    pub(crate) in_flight_spaces: HashSet<SpaceId>,
    /// The indexed folders, as last read from the store; kept here so Home lists them while a job has the store.
    pub(crate) roots: Vec<PathBuf>,
    pub(crate) index: RepoIndex,
    pub(crate) scanning: bool,
    scan_generation: u64,
    pub(crate) root_input: Entity<PromptInput>,
    /// The task composer input on Home that sends prompts to the Parent Agent.
    pub(crate) task_input: Entity<PromptInput>,
    /// Whether the Parent Agent's terminal card is shown on Home.
    pub(crate) home_agent_open: bool,
    /// Whether the indexed-folders panel is shown on Home.
    pub(crate) home_roots_open: bool,
    pub(crate) view: View,
    /// Open space tabs, by space; a space's terminals start when it is first shown.
    pub(crate) tabs: HashMap<SpaceId, SpaceTab>,
    pub(crate) dialog: Option<Dialog>,
    pub(crate) dialog_focus: FocusHandle,
    /// The open dialog's subscription to its text input, if it has one.
    pub(crate) dialog_subscription: Option<Subscription>,
    banner: Option<StatusBanner>,
    /// Whether the banner showing came from an agent (`status.set`) rather than xctx itself.
    banner_from_agent: bool,
    banner_generation: u64,
    banner_task: Option<Task<()>>,
    /// The timer that saves [`Workspace::pending_drag_saves`] once divider drags pause.
    pub(crate) persist_task: Option<Task<()>>,
    pub(crate) search: ProgramSearch,
    pub(crate) terminal_font: TerminalFont,
    pub(crate) terminal_theme: TerminalTheme,
    /// The control socket agents drive the window through; `None` when it could not be bound.
    pub(crate) control: Option<Control>,
    /// `space.draft` requests waiting for the repo index to be built.
    pub(crate) pending_control_drafts: Vec<PendingDraft>,
    /// Whether the startup mise check has finished, so the redactor knows the secrets it found.
    pub(crate) mise_ready: bool,
    /// Requests showing masked stored text (`inbox.list`, `channel.list`, `contract.get`, `contract.agree`)
    /// waiting for the startup mise check.
    pub(crate) pending_masked_reads: Vec<PendingMaskedRead>,
    /// Each space's agent graph, loop and shared space, loaded when first needed.
    pub(crate) graphs: SpaceGraphs,
    /// The space widening and attempt proposal requests being approved now, so none is approved twice (or turned
    /// down while it runs).
    pub(crate) widen_approvals: HashSet<ItemId>,
    /// Each space's competing attempts, loaded when first needed.
    pub(crate) attempts: SpaceAttempts,
    /// The tab the Inspector shows; whether it is open is in the window preferences.
    pub(crate) inspector_tab: xctx_ui::inspector::InspectorTab,
    /// The data directory (the store's), which holds the RPC agents' session folders; `None` when there is none.
    pub(crate) data_dir: Option<PathBuf>,
    /// Spaces whose fork, adopt or discard of attempts is running: one at a time per space, and no Ship or
    /// Discard of the space meanwhile.
    pub(crate) attempt_jobs: HashSet<SpaceId>,
    /// Per space, the attempt that was merged into it but could not be recorded as adopted, with what the merge
    /// did: adopting it again only retries the record (and the cleanup after it), and no other attempt of the
    /// space can be adopted meanwhile.
    pub(crate) unsaved_adoptions: HashMap<SpaceId, (u32, xctx_core::AdoptReport)>,
    /// Where pi keeps its sessions, for finding an agent's session from before named sessions and whether a forked
    /// session has started; `None` when neither `PI_CODING_AGENT_SESSION_DIR` nor `HOME` is set.
    pub(crate) sessions: Option<SessionStorage>,
    /// The Inbox; `None` when it could not be opened.
    pub(crate) inbox: Option<Inbox>,
    /// Why the Inbox could not be opened.
    pub(crate) inbox_error: Option<String>,
    /// The secret values (from mise) masked in what agents and the Inbox see.
    pub(crate) redactor: Redactor,
    /// The Inbox entry whose detail is shown.
    pub(crate) inbox_expanded: Option<SharedString>,
    /// The Plan Gate open for review on Home.
    pub(crate) plan_review: Option<ItemId>,
    /// The plan review card shows its repo picker (it always does while the plan has no repo).
    pub(crate) plan_add_open: bool,
    /// The Plan Gates launching or launched from this window.
    pub(crate) plan_launches: PlanLaunches,
    /// Where a repo is typed to add it to the reviewed plan.
    pub(crate) plan_repo_input: Entity<PromptInput>,
    /// The Set value input for a missing mise variable.
    pub(crate) env_prompt: Option<EnvPrompt>,
    /// The Parent Agent on Home.
    pub(crate) parent: ParentAgent,
    /// Every space's plugins: what can be mounted, what is, what it runs, and calls and mounts waiting in the
    /// Inbox.
    pub(crate) plugins: PluginHost,
    /// The agent runners (built-in and the user's).
    pub(crate) runners: RunnerRegistry,
    /// Whether the task that redraws logs panes and fires their events is running.
    pub(crate) plugin_ticker: bool,
    /// The drawn plugin panes' own state (inputs, selections, the views last built).
    pub(crate) plugin_panes: crate::plugin_panes::PluginPanes,
    /// The delinked side file explorer and standalone file viewer panels.
    pub(crate) file_panes: crate::file_panes::FilePanesState,
    /// The open spaces' live browsers (the browser panes' pages), drawn over the window.
    pub(crate) browsers: crate::plugin_browser::Browsers,
    /// The hint bubbles for local servers seen in the open spaces' terminals.
    pub(crate) serve_hints: crate::serve_hints::ServeHints,
    /// The tool dock's popovers and what each open space's dock lists.
    pub(crate) tool_dock: crate::tool_dock::DockState,
    /// The window's look: translucent or not, the Roster collapsed or not.
    pub(crate) prefs: UiPrefs,
    /// The ⌘K command palette, drawn over everything while open.
    pub(crate) palette: Entity<CommandPalette>,
    /// Whether the palette is open (kept here so browser pages can hide under it without reading the entity).
    pub(crate) palette_open: bool,
    /// Whether the Inbox overlay (⌘⇧I) is open over the view.
    pub(crate) inbox_open: bool,
    /// Where the keys go while the Inbox overlay is open (Esc closes it).
    pub(crate) overlay_focus: FocusHandle,
    /// Whether the Parent Agent dock (⌘J) is open over a space.
    pub(crate) dock_open: bool,
    /// The toasts in the window's bottom-right corner (new gates, mostly); its own entity, so a toast repaints
    /// only the stack.
    pub(crate) toasts: Entity<ToastStack>,
    /// The pending gates already announced, so each gets one toast; `None` until the Inbox was first looked at.
    pub(crate) seen_gates: Option<HashSet<ItemId>>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Build the terminal palette from the active window [`Theme`]. Every terminal sits in a frame painted with
/// `surface` (a pane, the Parent Agent card, the attempt view), so its own default background is fully
/// transparent: one surface layer, in opaque and translucent windows alike. Text and the cursor stay opaque.
pub(crate) fn terminal_theme(theme: &Theme) -> TerminalTheme {
    let opaque = |color: Hsla| color.alpha(1.0).to_rgb();
    TerminalTheme {
        foreground: opaque(theme.text),
        background: theme.surface.alpha(0.0).to_rgb(),
        cursor: opaque(theme.text),
        cursor_text: opaque(theme.surface),
        selection: theme.accent.alpha(0.33).to_rgb(),
        scrollback_indicator: theme.text_muted.alpha(0.40).to_rgb(),
        ansi: theme.ansi.map(Hsla::to_rgb),
        drop_highlight: theme.accent.alpha(0.18).to_rgb(),
    }
}

impl Workspace {
    /// The workspace over the store in the default data directory, with its active spaces in the Roster, drawn
    /// with the look `prefs` chose.
    pub(crate) fn new(prefs: UiPrefs, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let theme = theme_for(window.appearance(), prefs.translucent);
        cx.set_global(theme);
        xctx_ui::sync_kit_theme(theme, window.appearance(), cx);
        let (mut store, store_error) = match Store::open_default() {
            Ok(store) => (Some(store), None),
            Err(e) => (None, Some(format!("could not open the xctx store: {e}"))),
        };
        let imported = store.as_mut().and_then(import_prototype_spaces);
        let data_dir = store
            .as_ref()
            .map(|store| store.dir().to_path_buf())
            .or_else(|| xctx_core::store::data_dir().ok());
        let agent_skills = data_dir.as_deref().map(crate::agent_skills::install);
        let (inbox, inbox_error) = Workspace::open_inbox();
        let root_input =
            cx.new(|cx| PromptInput::new(cx).placeholder("A folder with your repos, e.g. ~/code"));
        let task_input = cx.new(|cx| {
            PromptInput::new(cx)
                .multiline(true)
                .borderless(true)
                .placeholder("What are we building?")
        });
        let plan_repo_input = cx.new(|cx| {
            PromptInput::new(cx)
                .placeholder("Add repos: type to search yours, or a folder such as ~/code/web")
        });
        let palette = cx.new(|cx| CommandPalette::new(window, cx));
        let subscriptions = vec![
            cx.observe(&task_input, |_, _, cx| cx.notify()),
            cx.observe(&plan_repo_input, |_, _, cx| cx.notify()),
            cx.observe_window_appearance(window, |this, window, cx| {
                this.apply_theme(window.appearance(), cx);
            }),
            cx.subscribe_in(
                &palette,
                window,
                |this, _, event: &PaletteEvent, window, cx| match event {
                    PaletteEvent::Run(id) => this.run_palette_item(id, window, cx),
                    PaletteEvent::Dismissed => this.palette_closed(window, cx),
                },
            ),
            cx.subscribe_in(
                &root_input,
                window,
                |this, _, event: &PromptEvent, _, cx| {
                    if let PromptEvent::Submit(path) = event {
                        this.add_root(path.clone(), cx);
                    }
                },
            ),
            cx.subscribe_in(
                &task_input,
                window,
                |this, _, event: &PromptEvent, window, cx| {
                    if let PromptEvent::Submit(text) = event {
                        this.submit_task(text.clone(), window, cx);
                    }
                },
            ),
            cx.subscribe_in(
                &plan_repo_input,
                window,
                |this, _, event: &PromptEvent, _, cx| {
                    if let PromptEvent::Submit(input) = event {
                        this.add_plan_repo(input.clone(), cx);
                    }
                },
            ),
            // A divider drag saves its layout after a pause; save everything once more on quit so none is lost.
            cx.on_app_quit(|this, cx| {
                let ids: Vec<SpaceId> = this.tabs.keys().cloned().collect();
                for id in &ids {
                    this.persist(id, cx);
                }
                // Tails stop as they are dropped; the live browsers are taken off the window.
                this.plugins.stop_all();
                this.close_all_browsers();
                async {}
            }),
        ];
        let mut workspace = Workspace {
            focus_handle: cx.focus_handle(),
            store,
            store_error,
            busy: None,
            spaces: Vec::new(),
            pending_persist: HashSet::new(),
            pending_drag_saves: HashSet::new(),
            in_flight_spaces: HashSet::new(),
            roots: Vec::new(),
            index: RepoIndex::default(),
            scanning: false,
            scan_generation: 0,
            root_input,
            task_input,
            home_agent_open: false,
            home_roots_open: false,
            view: View::Home,
            tabs: HashMap::new(),
            dialog: None,
            dialog_focus: cx.focus_handle(),
            dialog_subscription: None,
            banner: None,
            banner_from_agent: false,
            banner_generation: 0,
            banner_task: None,
            persist_task: None,
            search: ProgramSearch::from_login_shell(),
            terminal_font: TerminalFont::default(),
            terminal_theme: terminal_theme(&theme),
            control: None,
            pending_control_drafts: Vec::new(),
            mise_ready: false,
            pending_masked_reads: Vec::new(),
            graphs: SpaceGraphs::new(data_dir.clone()),
            widen_approvals: HashSet::new(),
            attempts: SpaceAttempts::new(data_dir.clone()),
            inspector_tab: xctx_ui::inspector::InspectorTab::Graph,
            data_dir: data_dir.clone(),
            attempt_jobs: HashSet::new(),
            unsaved_adoptions: HashMap::new(),
            sessions: SessionStorage::from_env(),
            inbox,
            inbox_error,
            redactor: Redactor::default(),
            inbox_expanded: None,
            plan_review: None,
            plan_add_open: false,
            plan_launches: PlanLaunches::default(),
            plan_repo_input,
            env_prompt: None,
            parent: ParentAgent::default(),
            plugins: {
                let mut host = PluginHost::new(data_dir, user_plugin_dir());
                // Screenshots go where the rest of the app keeps a space's scratch files.
                host.set_scratch_data_dir(std::env::var_os(xctx_core::store::DATA_DIR_ENV));
                host
            },
            runners: load_runners(user_runner_dir().as_deref()),
            plugin_ticker: false,
            plugin_panes: Default::default(),
            file_panes: Default::default(),
            browsers: Default::default(),
            serve_hints: Default::default(),
            tool_dock: Default::default(),
            prefs,
            palette,
            palette_open: false,
            inbox_open: false,
            overlay_focus: cx.focus_handle(),
            dock_open: false,
            toasts: cx.new(|_| ToastStack::new()),
            seen_gates: None,
            _subscriptions: subscriptions,
        };
        // The window's close button must not end a launch, ship or discard halfway.
        let weak = cx.weak_entity();
        window.on_window_should_close(cx, move |_, cx| {
            weak.update(cx, |this, cx| !this.refuse_while_busy(cx))
                .unwrap_or(true)
        });
        match agent_skills {
            Some(Ok(skills)) => workspace.search.skills = Some(skills),
            Some(Err(e)) => {
                workspace.error(format!("could not write xctx's agent skills: {e}"), cx)
            }
            None => {}
        }
        // Before any pane starts, so every pane gets the socket.
        workspace.start_control(window, cx);
        workspace.start_parent_agent(window, cx);
        workspace.start_serve_hints(cx);
        workspace.start_gate_toasts(window, cx);
        if let Some(e) = workspace.inbox_error.clone() {
            workspace.error(e, cx);
        }
        if let Some(message) = imported {
            workspace.info(message, cx);
        }
        workspace.refresh_spaces();
        workspace.check_mise_at_startup(cx);
        workspace.rescan(cx);
        workspace.focus_home(window, cx);
        workspace
    }

    /// Re-read the active spaces from the store.
    pub(crate) fn refresh_spaces(&mut self) {
        if let Some(store) = &self.store {
            self.spaces = store.active_spaces().cloned().collect();
        }
    }

    /// Show a message in the status bar; info messages clear themselves. An info message does not replace an
    /// error that is showing (see [`banner_replaces`]).
    pub(crate) fn set_banner(&mut self, banner: StatusBanner, cx: &mut Context<Self>) {
        if !banner_replaces(self.banner.as_ref(), &banner) {
            return;
        }
        self.show_banner(banner, false, cx);
    }

    /// Show an agent's message (from `status.set`) in the status bar. It never replaces an error of xctx's own
    /// (see [`agent_banner_replaces`]), so an agent cannot hide a real failure.
    pub(crate) fn set_agent_banner(&mut self, banner: StatusBanner, cx: &mut Context<Self>) {
        if !agent_banner_replaces(self.banner.as_ref(), self.banner_from_agent, &banner) {
            return;
        }
        self.show_banner(banner, true, cx);
    }

    fn show_banner(&mut self, banner: StatusBanner, from_agent: bool, cx: &mut Context<Self>) {
        self.banner_from_agent = from_agent;
        self.banner_generation += 1;
        let generation = self.banner_generation;
        self.banner_task = banner.auto_clear_after().map(|delay| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(delay).await;
                let _ = this.update(cx, |this, cx| {
                    if this.banner_generation == generation {
                        this.banner = None;
                        cx.notify();
                    }
                });
            })
        });
        self.banner = Some(banner);
        cx.notify();
    }

    /// Show an info message that clears itself.
    pub(crate) fn info(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.set_banner(StatusBanner::info(message), cx);
    }

    /// Show an error that stays until dismissed.
    pub(crate) fn error(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.set_banner(StatusBanner::error(message), cx);
    }

    fn dismiss_banner(&mut self, cx: &mut Context<Self>) {
        self.banner = None;
        self.banner_task = None;
        cx.notify();
    }

    /// Take the store for a background job that says it is doing `what`. `None`, with an error shown, when another
    /// job has it or there is no store.
    pub(crate) fn take_store(&mut self, what: &str, cx: &mut Context<Self>) -> Option<Store> {
        if let Some(busy) = &self.busy {
            self.error(wait_message(busy), cx);
            return None;
        }
        match self.store.take() {
            Some(store) => {
                self.busy = Some(what.to_owned().into());
                cx.notify();
                Some(store)
            }
            None => {
                let message = self
                    .store_error
                    .clone()
                    .unwrap_or_else(|| "the store is not available".to_owned());
                self.error(message, cx);
                None
            }
        }
    }

    /// Put back the store a background job took, and save layouts that changed meanwhile.
    pub(crate) fn return_store(&mut self, store: Store, cx: &mut Context<Self>) {
        self.store = Some(store);
        self.busy = None;
        self.refresh_spaces();
        for id in std::mem::take(&mut self.pending_persist) {
            self.persist(&id, cx);
        }
        cx.notify();
    }

    /// When a background job holds the store, say so and return `true`: quitting or closing the window now would
    /// stop it partway through its worktree or push operations.
    pub(crate) fn refuse_while_busy(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(busy) = self.busy.clone() {
            self.info(wait_message(&busy), cx);
            return true;
        }
        // An adopt or discard of attempts does not take the store, but stopping it would leave a merge or a
        // worktree half done.
        if let Some(id) = self.attempt_jobs.iter().next().cloned() {
            self.info(wait_message(&format!("the attempt job of {id}")), cx);
            return true;
        }
        false
    }

    /// Index the store's folders again, in the background.
    pub(crate) fn rescan(&mut self, cx: &mut Context<Self>) {
        let Some(store) = &self.store else {
            return;
        };
        let roots = store.roots().to_vec();
        self.roots = roots.clone();
        self.scan_generation += 1;
        let generation = self.scan_generation;
        self.scanning = true;
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let index = executor.spawn(async move { scan_index(roots) }).await;
            let _ = this.update(cx, |this, cx| {
                if this.scan_generation != generation {
                    return;
                }
                this.index = index;
                this.scanning = false;
                this.flush_control_drafts(cx);
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Show Home.
    pub(crate) fn show_home(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.view = View::Home;
        self.close_idle_dialog();
        self.focus_home(window, cx);
        cx.notify();
    }

    /// Give keyboard focus to Home's input: the Parent Agent's terminal when its drawer is open and running,
    /// else the task composer input.
    pub(crate) fn focus_home(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.home_agent_open
            && let Some(view) = &self.parent.view
        {
            view.focus_handle(cx).focus(window, cx);
        } else {
            self.task_input.focus_handle(cx).focus(window, cx);
        }
    }

    /// Show the space's tab, starting its terminals if this is the first time.
    pub(crate) fn show_space(&mut self, id: &SpaceId, window: &mut Window, cx: &mut Context<Self>) {
        if !self.spaces.iter().any(|s| &s.id == id) {
            self.error(format!("space {id} is not active"), cx);
            return;
        }
        if !self.open_space(id, window, cx) {
            return;
        }
        if self.view != View::Space(id.clone()) {
            self.close_idle_dialog();
        }
        self.view = View::Space(id.clone());
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    /// Close the dialog unless its job is running; a running job's dialog stays to show how it ended.
    fn close_idle_dialog(&mut self) {
        if !self.dialog.as_ref().is_some_and(Dialog::is_running) {
            self.dialog = None;
            self.dialog_subscription = None;
        }
    }

    /// ⌘1–⌘9: show the space in that Roster slot.
    fn show_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        match self.roster_space(index, cx) {
            Some(id) => self.show_space(&id, window, cx),
            None => self.info(format!("there is no space at ⌘{}", index + 1), cx),
        }
    }

    /// Cmd+W: close the focused pane. On Home there is no pane, and closing the window would stop every space's
    /// terminals, so it only says how to close the window or quit.
    fn on_close_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.view {
            View::Home => self.info(CLOSE_ON_HOME_HINT, cx),
            View::Space(_) => self.close_focused_pane(window, cx),
        }
    }

    fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.refuse_while_busy(cx) {
            window.remove_window();
        }
    }

    fn quit(&mut self, cx: &mut Context<Self>) {
        if !self.refuse_while_busy(cx) {
            cx.quit();
        }
    }

    /// A listener for a pane shortcut (split, close, focus, resize, zoom), which does nothing while a dialog is
    /// open over the panes.
    fn pane_shortcut<A: 'static>(
        cx: &mut Context<Self>,
        run: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) -> impl Fn(&A, &mut Window, &mut gpui::App) + 'static {
        cx.listener(move |this, _: &A, window, cx| {
            if this.dialog.is_none() {
                run(this, window, cx);
            }
        })
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> StatusBar {
        let entity = cx.entity().downgrade();
        let show_inbox = cx.entity().downgrade();
        let mut bar = StatusBar::new()
            .inbox(self.inbox_pending())
            .on_inbox(move |window, cx| {
                let _ = show_inbox.update(cx, |this, cx| this.show_inbox(None, window, cx));
            })
            .banner(self.banner.clone())
            .on_dismiss_banner(move |_, cx| {
                let _ = entity.update(cx, |this, cx| this.dismiss_banner(cx));
            });
        match self.active_tab() {
            Some(tab) => {
                let space = &tab.space;
                // An attempt shown in place of the space's panes runs on its own port and branch.
                let (port, branch) = self
                    .attempts
                    .get(&space.id)
                    .and_then(|set| shown_worktree(set, tab.shown_attempt))
                    .map_or_else(
                        || (space.port, space.branch()),
                        |worktree| (worktree.port, worktree.branch.clone()),
                    );
                let agent_panes = tab.layout.agent_panes();
                let running = agent_panes
                    .iter()
                    .filter(|pane| {
                        tab.terminals
                            .get(pane)
                            .is_some_and(|terminal| terminal.is_alive(cx))
                    })
                    .count();
                bar = bar
                    .space(space.id.as_str().to_owned())
                    .mise_env(space.effective_mise_env())
                    .port(port)
                    .branch(branch)
                    .shape(layout_shape(space.shape))
                    .zoomed(tab.layout.tree().is_zoomed())
                    .agents(running, agent_panes.len())
                    .hints(palette_hint());
            }
            None => {
                bar = bar.hints(palette_hint());
                if let Some(busy) = &self.busy {
                    bar = bar.space(format!("{busy}…"));
                }
            }
        }
        bar
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        self.browsers.attach(window);
        self.sync_plugin_panes(window, cx);
        self.refresh_plugin_panes(cx);
        // The Inspector takes the dock's place beside the panes; in a narrow window it covers them instead.
        let wide = f32::from(window.viewport_size().width) >= inspector::OVERLAY_BELOW;
        let placement = self
            .active_space_id()
            .and_then(|id| self.inspector_panel(&id, cx))
            .map(|panel| {
                if wide {
                    InspectorPlacement::Beside(panel.into_any_element())
                } else {
                    InspectorPlacement::Overlay(panel.into_any_element())
                }
            });
        // The dock reads what is open every frame, so a tool an agent opens lights up at once.
        let dock = if placement.is_some() {
            None
        } else {
            self.active_space_id().map(|id| self.tool_dock_view(&id))
        };
        let body = match self.active_tab() {
            Some(tab) => self.render_space(tab, dock, placement, cx),
            None => self.render_home(window, cx),
        };
        let dialog = self.render_dialog(cx);
        let parent_dock = self.render_dock(window, cx);
        let inbox_overlay = self.render_inbox_overlay(cx);
        div()
            .key_context(WORKSPACE_CONTEXT)
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &Quit, _, cx| this.quit(cx)))
            .on_action(
                cx.listener(|this, _: &CloseWindow, window, cx| this.close_window(window, cx)),
            )
            .on_action(Self::pane_shortcut::<ClosePane>(cx, |this, window, cx| {
                this.on_close_pane(window, cx)
            }))
            .on_action(Self::pane_shortcut::<SplitRight>(cx, |this, window, cx| {
                this.split(SplitAxis::Horizontal, window, cx)
            }))
            .on_action(Self::pane_shortcut::<SplitDown>(cx, |this, window, cx| {
                this.split(SplitAxis::Vertical, window, cx)
            }))
            .on_action(Self::pane_shortcut::<FocusLeft>(cx, |this, window, cx| {
                this.move_focus(FocusDirection::Left, window, cx)
            }))
            .on_action(Self::pane_shortcut::<FocusRight>(cx, |this, window, cx| {
                this.move_focus(FocusDirection::Right, window, cx)
            }))
            .on_action(Self::pane_shortcut::<FocusUp>(cx, |this, window, cx| {
                this.move_focus(FocusDirection::Up, window, cx)
            }))
            .on_action(Self::pane_shortcut::<FocusDown>(cx, |this, window, cx| {
                this.move_focus(FocusDirection::Down, window, cx)
            }))
            .on_action(Self::pane_shortcut::<FocusNext>(cx, |this, window, cx| {
                this.cycle_focus(true, window, cx)
            }))
            .on_action(Self::pane_shortcut::<FocusPrev>(cx, |this, window, cx| {
                this.cycle_focus(false, window, cx)
            }))
            .on_action(Self::pane_shortcut::<ResizeLeft>(cx, |this, window, cx| {
                this.resize_focused(SplitAxis::Horizontal, -1.0, window, cx)
            }))
            .on_action(Self::pane_shortcut::<ResizeRight>(
                cx,
                |this, window, cx| this.resize_focused(SplitAxis::Horizontal, 1.0, window, cx),
            ))
            .on_action(Self::pane_shortcut::<ResizeUp>(cx, |this, window, cx| {
                this.resize_focused(SplitAxis::Vertical, -1.0, window, cx)
            }))
            .on_action(Self::pane_shortcut::<ResizeDown>(cx, |this, window, cx| {
                this.resize_focused(SplitAxis::Vertical, 1.0, window, cx)
            }))
            .on_action(Self::pane_shortcut::<ToggleZoom>(cx, |this, window, cx| {
                this.toggle_zoom(window, cx)
            }))
            .on_action(Self::pane_shortcut::<ToggleFlow>(cx, |this, window, cx| {
                this.toggle_flow(window, cx)
            }))
            .on_action(Self::pane_shortcut::<ForkAttempts>(
                cx,
                |this, window, cx| this.fork_from_keys(window, cx),
            ))
            .on_action(Self::pane_shortcut::<ReviewAttempts>(
                cx,
                |this, window, cx| this.open_review(window, cx),
            ))
            .on_action(cx.listener(|this, _: &ShowHome, window, cx| this.show_home(window, cx)))
            .on_action(cx.listener(|this, action: &ActivateTab, window, cx| {
                if this.dialog.is_none() {
                    this.show_tab(action.0, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &LaunchDraft, window, cx| {
                if this.view == View::Home
                    && this.dialog.is_none()
                    && this.reviewed_plan().is_some()
                {
                    this.approve_plan(window, cx);
                } else {
                    cx.propagate();
                }
            }))
            .on_action(
                cx.listener(|this, _: &DismissDialog, window, cx| this.dismiss_dialog(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &TogglePalette, window, cx| this.toggle_palette(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleRoster, _, cx| this.toggle_roster(cx)))
            .on_action(cx.listener(|this, _: &ToggleInspector, _, cx| {
                let open = !this.prefs.inspector_open;
                this.set_inspector_open(open, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowInspectorPlugins, _, cx| {
                this.show_inspector_tab(xctx_ui::inspector::InspectorTab::Plugins, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowFiles, _, cx| this.toggle_files_sidebar(cx)))
            .on_action(cx.listener(|this, _: &ToggleTranslucency, window, cx| {
                this.toggle_translucency(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleInbox, window, cx| this.show_inbox(None, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleDock, window, cx| this.toggle_dock(window, cx)))
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.chrome)
            .text_color(theme.text)
            .text_size(Theme::TEXT_MD)
            .child(self.render_title_bar(cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_roster(cx))
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(body)
                            .children(parent_dock)
                            .children(dialog),
                    ),
            )
            .child(self.render_status_bar(cx))
            .child(
                div()
                    .absolute()
                    .right(Theme::SPACE_4)
                    .bottom(Theme::STATUS_BAR_HEIGHT + Theme::SPACE_3)
                    .child(self.toasts.clone()),
            )
            .children(inbox_overlay)
            .child(self.palette.clone())
            // Last, so it runs after every pane placed its browser this frame: hide the browsers none placed.
            .child(self.browsers.frame_end())
    }
}

/// Bring in the spaces of the Swift prototype (PixelWorkspace) on the real data directory, once: spaces already
/// in the store are skipped. Nothing happens when `XCTX_DATA_DIR` moves the data directory (tests and throwaway
/// runs). Returns what to tell the user, if anything.
fn import_prototype_spaces(store: &mut Store) -> Option<String> {
    if std::env::var_os(xctx_core::store::DATA_DIR_ENV).is_some_and(|d| !d.is_empty()) {
        return None;
    }
    let path = xctx_core::prototype::prototype_store_path(std::env::var_os("HOME"))?;
    match xctx_core::prototype::import_prototype(store, &path) {
        Ok(report) if !report.imported.is_empty() => {
            let names: Vec<&str> = report.imported.iter().map(|id| id.as_str()).collect();
            Some(format!(
                "imported {} from the PixelWorkspace prototype: {}",
                if names.len() == 1 {
                    "a space".to_owned()
                } else {
                    format!("{} spaces", names.len())
                },
                names.join(", ")
            ))
        }
        Ok(_) => None,
        Err(e) => Some(format!(
            "could not import the PixelWorkspace prototype's spaces: {e}"
        )),
    }
}

/// What Cmd+W on Home says instead of closing the window.
const CLOSE_ON_HOME_HINT: &str = "Press ⌘⇧W to close window or ⌘Q to quit";

/// The message shown when something has to wait for the background job `busy` (such as "shipping fix-retry").
pub(crate) fn wait_message(busy: &str) -> String {
    format!("Wait until {busy} finishes")
}

/// The message shown when space `id` is asked for while its ship or discard is running.
pub(crate) fn in_flight_message(id: &SpaceId) -> String {
    format!("{id} is being shipped or discarded")
}

/// Whether `new` may replace the banner showing, `current`: anything replaces nothing or an info message, but only
/// another error replaces an error, so an error stays until the user dismisses it.
pub(crate) fn banner_replaces(current: Option<&StatusBanner>, new: &StatusBanner) -> bool {
    !(new.kind == BannerKind::Info && current.is_some_and(|b| b.kind == BannerKind::Error))
}

/// Whether an agent's `new` banner may replace the banner showing, `current` (which came from an agent when
/// `current_from_agent`): as [`banner_replaces`], except that it never replaces an error of xctx's own.
pub(crate) fn agent_banner_replaces(
    current: Option<&StatusBanner>,
    current_from_agent: bool,
    new: &StatusBanner,
) -> bool {
    let app_error = !current_from_agent && current.is_some_and(|b| b.kind == BannerKind::Error);
    !app_error && banner_replaces(current, new)
}

/// Why the tab of space `id` cannot be opened or shown now, if it cannot. A space whose ship or discard is running
/// (`in_flight`) cannot. Otherwise an open tab can always be shown; a closed one needs the store, which holds its
/// saved layout (opening it without would replace that layout with a fresh one). `busy` is what the background job
/// holding the store is doing, if one is.
pub(crate) fn open_space_refusal(
    id: &SpaceId,
    in_flight: bool,
    tab_open: bool,
    store_available: bool,
    busy: Option<&str>,
) -> Option<String> {
    if in_flight {
        return Some(in_flight_message(id));
    }
    if tab_open || store_available {
        return None;
    }
    Some(match busy {
        Some(busy) => wait_message(busy),
        None => format!("the store is not available, so {id} cannot be opened"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_info_banner_does_not_replace_an_error() {
        let info = StatusBanner::info("launched");
        let error = StatusBanner::error("push failed");
        assert!(banner_replaces(None, &info));
        assert!(banner_replaces(None, &error));
        assert!(banner_replaces(Some(&info), &info));
        assert!(banner_replaces(Some(&info), &error));
        assert!(banner_replaces(Some(&error), &error));
        assert!(!banner_replaces(Some(&error), &info));
    }

    #[test]
    fn an_agent_banner_never_replaces_an_app_error() {
        let info = StatusBanner::info("agent: tests pass");
        let error = StatusBanner::error("agent: tests failed");
        let app_error = StatusBanner::error("push failed");
        assert!(agent_banner_replaces(None, false, &info));
        assert!(agent_banner_replaces(None, false, &error));
        assert!(!agent_banner_replaces(Some(&app_error), false, &error));
        assert!(!agent_banner_replaces(Some(&app_error), false, &info));
        assert!(agent_banner_replaces(Some(&error), true, &error));
        assert!(!agent_banner_replaces(Some(&error), true, &info));
        assert!(agent_banner_replaces(
            Some(&StatusBanner::info("launched")),
            false,
            &error
        ));
    }

    #[test]
    fn a_closed_space_does_not_open_while_the_store_is_busy() {
        let id = SpaceId::new("fix-retry");
        assert_eq!(
            open_space_refusal(&id, false, false, false, Some("launching rate limit")),
            Some("Wait until launching rate limit finishes".to_owned())
        );
        assert!(
            open_space_refusal(&id, false, false, false, None)
                .is_some_and(|reason| reason.contains("store is not available"))
        );
        assert_eq!(open_space_refusal(&id, false, false, true, None), None);
    }

    #[test]
    fn an_open_space_is_shown_while_the_store_is_busy() {
        let id = SpaceId::new("fix-retry");
        assert_eq!(
            open_space_refusal(&id, false, true, false, Some("shipping other")),
            None
        );
    }

    #[test]
    fn a_space_being_shipped_or_discarded_never_opens() {
        let id = SpaceId::new("fix-retry");
        for (tab_open, store_available) in [(false, true), (true, true), (true, false)] {
            assert_eq!(
                open_space_refusal(&id, true, tab_open, store_available, None),
                Some("fix-retry is being shipped or discarded".to_owned())
            );
        }
    }
}
