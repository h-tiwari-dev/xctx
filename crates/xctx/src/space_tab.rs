//! An open space: its layout and the terminal running in each pane, and the workspace operations on it (start
//! panes, split, close, focus, resize, zoom, reshape, persist) and how it is drawn.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AnyElement, App, Context, Entity, Focusable, IntoElement, ParentElement, SharedString, Styled,
    Subscription, Window, div,
};
use xctx_control::{ErrorCode, RpcError};
use xctx_core::{Space, SpaceId, SpaceShape};
use xctx_graph::AgentNodeId;
use xctx_layout::LayoutError;
use xctx_layout::{FocusDirection, PaneId, PaneKind, PaneRole, RolePane, Shape, SplitAxis};
use xctx_term::TerminalSession;
use xctx_term_view::{TerminalView, TerminalViewEvent};
use xctx_ui::{
    Button, ButtonSize, ButtonVariant, IconName, PaneView, SplitView, Theme, pane_kind_label,
};

use crate::agent_session::AgentSession;
use crate::inspector::InspectorPlacement;
use crate::lifecycle::reshape_space;
use crate::omp_agent::AgentStatus;
use crate::omp_panes::OmpPane;
use crate::persist::{restore_layout, save_layout};
use crate::space_graph::{scratch_dir_for, writer_panes};
use crate::space_layout::{RESIZE_STEP, SpaceLayout, space_shape};
use crate::spawn::{
    AgentStart, pane_launch_for_runner, restore_agent_starts, runs_pi, split_agent_start,
};
use crate::workspace::{View, Workspace, open_space_refusal, wait_message};

/// A layout error of space `id` as a control error: unknown panes are not found, the rest conflicts.
pub(crate) fn layout_error(id: &SpaceId, error: LayoutError) -> RpcError {
    match error {
        LayoutError::UnknownPane(pane) => {
            RpcError::not_found(format!("space {id} has no pane {}", pane.0))
        }
        other => RpcError::conflict(format!("{id}: {other}")),
    }
}

/// How long after the last divider drag movement the layout is saved.
const DRAG_PERSIST_DELAY: Duration = Duration::from_millis(400);

/// What a pane shows: a running terminal, or why it could not start.
pub(crate) enum PaneTerminal {
    /// The terminal, with the title its program set and its exit code once it exited.
    Running {
        view: Entity<TerminalView>,
        title: Option<String>,
        exited: Option<Option<i32>>,
        /// Whether this is the login shell opened in an agent pane after its agent exited on its own (see
        /// [`Workspace::open_shell_after_agent`]), rather than the pane's own program.
        after_agent: bool,
        _subscription: Subscription,
    },
    /// The program could not be started.
    Failed(String),
    /// An agent run over RPC rather than in a terminal (see [`crate::omp_agent`]).
    Omp(Box<OmpPane>),
}

impl PaneTerminal {
    /// Whether its program is still running.
    pub(crate) fn is_alive(&self, cx: &App) -> bool {
        match self {
            PaneTerminal::Running { view, exited, .. } => {
                exited.is_none() && view.read(cx).session().is_alive()
            }
            PaneTerminal::Failed(_) => false,
            PaneTerminal::Omp(pane) => pane.agent.is_live(),
        }
    }

    /// Whether its agent is at work: a terminal agent whose program runs (all a terminal can tell), an RPC agent
    /// whose turn runs or waits on the user.
    pub(crate) fn is_working(&self, cx: &App) -> bool {
        match self {
            PaneTerminal::Omp(pane) => pane.agent.status().is_working(),
            other => other.is_alive(cx),
        }
    }

    /// The RPC agent it runs, if it runs one.
    pub(crate) fn omp(&self) -> Option<&OmpPane> {
        match self {
            PaneTerminal::Omp(pane) => Some(pane),
            _ => None,
        }
    }
}

/// An open space tab: the space, its layout, and a terminal per pane.
pub(crate) struct SpaceTab {
    pub(crate) space: Space,
    pub(crate) layout: SpaceLayout,
    pub(crate) terminals: HashMap<PaneId, PaneTerminal>,
    /// The agent terminal of each live attempt, by attempt number. They are not in the layout; the attempt strip
    /// switches to them (see [`Workspace::attempt_terminal`]).
    pub(crate) attempt_terminals: HashMap<u32, PaneTerminal>,
    /// The attempt whose agent the pane area shows instead of the split view (chosen on the attempt strip), if
    /// any. The space's own panes keep running meanwhile.
    pub(crate) shown_attempt: Option<u32>,
    /// The web page of each of the space's repos (see [`crate::repo_links::repo_web_url`]), by repo name; read
    /// once when the tab opens and again when a repo is added.
    pub(crate) repo_links: HashMap<String, String>,
}

impl SpaceTab {
    /// A tab for `space` laid out as `layout`, with no terminals started yet, showing its panes.
    fn new(space: Space, layout: SpaceLayout) -> SpaceTab {
        let repo_links = repo_links_of(&space);
        SpaceTab {
            space,
            layout,
            terminals: HashMap::new(),
            attempt_terminals: HashMap::new(),
            shown_attempt: None,
            repo_links,
        }
    }
}

/// The web page of each repo of `space` that has one, by repo name.
pub(crate) fn repo_links_of(space: &Space) -> HashMap<String, String> {
    space
        .repos
        .iter()
        .filter_map(|repo| {
            crate::repo_links::repo_web_url(&repo.repo).map(|url| (repo.name.clone(), url))
        })
        .collect()
}

impl SpaceTab {
    /// The retry badge of graph agent `agent` when it runs over RPC and omp retries after a retryable error.
    pub(crate) fn rpc_retry_badge(&self, agent: &AgentNodeId) -> Option<&str> {
        self.terminals
            .values()
            .filter_map(PaneTerminal::omp)
            .find(|omp| omp.agent.agent() == Some(agent))
            .and_then(|omp| omp.agent.retry_badge())
    }

    /// Whether any agent pane's agent is still running (see [`PaneTerminal::is_working`]); the shell opened after an
    /// agent exited does not count.
    pub(crate) fn agent_running(&self, cx: &App) -> bool {
        self.layout.agent_panes().iter().any(|pane| {
            self.terminals.get(pane).is_some_and(|terminal| {
                terminal.is_working(cx)
                    && !matches!(
                        terminal,
                        PaneTerminal::Running {
                            after_agent: true,
                            ..
                        }
                    )
            })
        })
    }
}

/// Whether `shape` is the shape a space in `current` already has, so reshaping to it would change nothing.
pub(crate) fn is_current_shape(current: SpaceShape, shape: Shape) -> bool {
    current == space_shape(shape)
}

/// The header of a pane: the program's own title when it set one, else `<repo> · <kind>` (`<repo> · <plugin>`
/// for a plugin pane), with the exit code
/// once it exited.
pub(crate) fn pane_title(role: &PaneRole, terminal: Option<&PaneTerminal>) -> String {
    let fallback = match (&role.kind, &role.plugin) {
        (PaneKind::Plugin, Some(plugin)) => format!("{} · {plugin}", role.repo),
        _ => format!("{} · {}", role.repo, pane_kind_label(role.kind)),
    };
    match terminal {
        Some(PaneTerminal::Running { title, exited, .. }) => {
            let title = title
                .as_deref()
                .filter(|t| !t.trim().is_empty())
                .map_or(fallback, |t| format!("{} · {t}", role.repo));
            match exited {
                Some(Some(code)) => format!("{title} — exited ({code})"),
                Some(None) => format!("{title} — exited"),
                None => title,
            }
        }
        Some(PaneTerminal::Failed(_)) => format!("{fallback} — failed to start"),
        Some(PaneTerminal::Omp(pane)) => match pane.agent.status() {
            AgentStatus::Failed { .. } => format!("{fallback} — failed"),
            AgentStatus::Stopped => format!("{fallback} — stopped"),
            _ => fallback,
        },
        None => fallback,
    }
}

impl Workspace {
    /// The open tab of the space being shown, if a space is shown.
    pub(crate) fn active_tab(&self) -> Option<&SpaceTab> {
        match &self.view {
            View::Space(id) => self.tabs.get(id),
            View::Home => None,
        }
    }

    /// The space being shown, if one is.
    pub(crate) fn active_space_id(&self) -> Option<SpaceId> {
        match &self.view {
            View::Space(id) => Some(id.clone()),
            View::Home => None,
        }
    }

    /// Open the space's tab if it is not open yet: restore its saved layout (or make a fresh one) and start every
    /// pane, one agent per worktree resuming its last session and the others starting fresh (see
    /// [`restore_agent_starts`]). Returns whether the tab is open. It is refused, with the reason
    /// shown, while the space's ship or discard runs, and while the store (with the saved layout) is busy.
    pub(crate) fn open_space(
        &mut self,
        id: &SpaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if let Some(reason) = open_space_refusal(
            id,
            self.in_flight_spaces.contains(id),
            self.tabs.contains_key(id),
            self.store.is_some(),
            self.busy.as_deref(),
        ) {
            self.info(reason, cx);
            return false;
        }
        if self.tabs.contains_key(id) {
            return true;
        }
        let Some(space) = self.spaces.iter().find(|s| &s.id == id).cloned() else {
            self.error(format!("space {id} is not active"), cx);
            return false;
        };
        let Some(store) = &self.store else {
            return false;
        };
        let restored = match restore_layout(store.load_layout(id), &space) {
            Ok(restored) => restored,
            Err(e) => {
                self.error(format!("could not lay out {id}: {e}"), cx);
                return false;
            }
        };
        let panes = restored.layout.panes();
        self.tabs
            .insert(id.clone(), SpaceTab::new(space.clone(), restored.layout));
        self.sync_agent_panes(id);
        if let Err(e) = self.plugins.reconcile(id, &panes) {
            self.error(format!("could not read the tools open in {id}: {e}"), cx);
        }
        self.show_plugin_problems(&space, cx);
        let preferred = self
            .graphs
            .get(id)
            .map(|state| writer_panes(state.graph(), &panes))
            .unwrap_or_default();
        let starts = restore_agent_starts(&space, &panes, &preferred);
        for (pane, start) in panes.iter().zip(&starts) {
            self.spawn_pane(id, pane, start, window, cx);
        }
        self.start_live_attempts(id, window, cx);
        if let Some(reason) = restored.fallback {
            self.info(format!("{reason}; opened a fresh layout"), cx);
            self.persist(id, cx);
        }
        true
    }

    /// Open a space that was just launched with its first layout, its agents given the task, and set up what its
    /// repos call for (see [`crate::dev_setup`]): the first web app's dev server in a terminal pane on the space's
    /// `PORT` and xctx's browser on it, and the API client for the repos beside it. A mount that fails is shown
    /// and the rest still mount.
    pub(crate) fn open_launched_space(
        &mut self,
        space: Space,
        layout: SpaceLayout,
        panes: Vec<RolePane>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = space.id.clone();
        let start = AgentStart::Task(space.task.clone());
        let worktrees: Vec<&std::path::Path> =
            space.repos.iter().map(|r| r.worktree.as_path()).collect();
        let setup = crate::dev_setup::space_setup(&worktrees);
        let repos: Vec<String> = space.repos.iter().map(|r| r.name.clone()).collect();
        self.tabs.insert(id.clone(), SpaceTab::new(space, layout));
        for pane in &panes {
            self.spawn_pane(&id, pane, &start, window, cx);
        }
        for (repo, setup) in repos.into_iter().zip(setup) {
            let mut mounts = Vec::new();
            if let Some(app) = setup.web_app {
                mounts.push((
                    "terminal",
                    crate::plugin_host::MountSettings {
                        command: Some(app.command),
                        ..Default::default()
                    },
                ));
                mounts.push(("browser", Default::default()));
            }
            if setup.api_client {
                mounts.push(("api-client", Default::default()));
            }
            for (plugin, settings) in mounts {
                if let Err(e) =
                    self.mount_plugin(&id, plugin, Some(repo.clone()), settings, false, window, cx)
                {
                    self.show_op_error(e, cx);
                }
            }
        }
    }

    /// Start the program of `pane` in the space's tab, as the agent of the space's graph it runs (see
    /// [`Workspace::pane_agent_of`]), with the space's scratch directory made if it is missing.
    /// An agent pane bound to an agent of the graph runs that agent's named `pi` session (see
    /// [`Workspace::pane_session_of`]).
    pub(crate) fn spawn_pane(
        &mut self,
        id: &SpaceId,
        pane: &RolePane,
        start: &AgentStart,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if pane.role.kind == PaneKind::Plugin {
            self.start_plugin_pane(id, pane, window, cx);
            return;
        }
        if self.start_rpc_pane(id, pane, start, window, cx) {
            return;
        }
        let agent = self.pane_agent_of(id, pane);
        let session = match self.pane_session_of(id, pane, start) {
            Ok(session) => session,
            Err(message) => {
                self.error(message.clone(), cx);
                if let Some(tab) = self.tabs.get_mut(id) {
                    tab.terminals.insert(pane.id, PaneTerminal::Failed(message));
                }
                return;
            }
        };
        let Some(tab) = self.tabs.get(id) else {
            return;
        };
        if let Ok(scratch) = scratch_dir_for(id.as_str()) {
            // Best effort: agents may write there with their own tools, which need the directory.
            let _ = std::fs::create_dir_all(scratch.root());
        }
        let space = tab.space.clone();
        let (runner, runner_notice) = if pane.role.kind == PaneKind::Agent {
            self.pane_runner(&space, pane)
        } else {
            (None, None)
        };
        let launch = pane_launch_for_runner(
            &space,
            &pane.role,
            start,
            &session,
            &self.search,
            runner.as_ref(),
        )
        .with_agent(agent.as_ref());
        let program = launch.config.program.clone();
        let runs_agent = runs_pi(pane.role.kind, &launch, true) && launch.session_named;
        let notice = launch.notice.or(runner_notice);
        let stdin = launch.stdin;
        let spawned = TerminalView::spawn(
            launch.config,
            self.terminal_font.clone(),
            self.terminal_theme,
            window,
            cx,
        );
        if runs_agent && spawned.is_ok() {
            // Only now that pi itself runs it: a shell fallback or a failed start must not take the old session's
            // place on the agent.
            self.record_pane_session(id, pane, &session);
        }
        if let (Ok(view), Some(task)) = (&spawned, &stdin) {
            // A runner that reads its task on standard input gets it typed in once it started, with its control
            // characters flattened so the task cannot type keys of its own.
            let _ = view
                .read(cx)
                .session()
                .write(crate::spawn::typed_task(task).as_bytes());
        }
        let terminal = match spawned {
            Ok(view) => self.running_terminal(id, pane.id, view, false, window, cx),
            Err(e) => {
                let message = format!("could not start {program} for {}: {e}", pane.role.repo);
                self.error(message.clone(), cx);
                PaneTerminal::Failed(message)
            }
        };
        if let Some(tab) = self.tabs.get_mut(id) {
            tab.terminals.insert(pane.id, terminal);
        }
        if let Some(notice) = notice {
            self.info(notice, cx);
        }
    }

    /// `view`, running in `pane` of space `id`, as a pane terminal whose events reach
    /// [`Workspace::on_terminal_event`]. `after_agent` marks the shell opened after the pane's agent exited.
    pub(crate) fn running_terminal(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        view: Entity<TerminalView>,
        after_agent: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PaneTerminal {
        let space_id = id.clone();
        let subscription = cx.subscribe_in(
            &view,
            window,
            move |this, _view, event: &TerminalViewEvent, window, cx| {
                this.on_terminal_event(&space_id, pane, event, window, cx);
            },
        );
        PaneTerminal::Running {
            view,
            title: None,
            exited: None,
            after_agent,
            _subscription: subscription,
        }
    }

    /// Open the login shell in agent pane `pane` of space `id` in place of its agent, which exited on its own with
    /// `code`: the pane keeps a usable terminal in its repo's worktree, with the space's environment but not the
    /// agent's identity. The pane's role stays an agent, so the agent starts again the next time the space opens.
    /// When the shell cannot start, the exited agent's screen stays.
    pub(crate) fn open_shell_after_agent(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        code: Option<i32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get(id) else {
            return;
        };
        let Some(repo) = tab.layout.role(pane).map(|role| role.repo.clone()) else {
            return;
        };
        let role = PaneRole::new(PaneKind::Shell, repo.clone());
        let launch = pane_launch_for_runner(
            &tab.space,
            &role,
            &AgentStart::Fresh,
            &AgentSession::Unnamed,
            &self.search,
            None,
        );
        let spawned = TerminalView::spawn(
            launch.config,
            self.terminal_font.clone(),
            self.terminal_theme,
            window,
            cx,
        );
        let view = match spawned {
            Ok(view) => view,
            Err(e) => {
                self.error(
                    format!(
                        "the {repo} agent exited, and a shell could not start in its pane: {e}"
                    ),
                    cx,
                );
                return;
            }
        };
        let terminal = self.running_terminal(id, pane, view, true, window, cx);
        let Some(tab) = self.tabs.get_mut(id) else {
            return;
        };
        tab.terminals.insert(pane, terminal);
        let focused = tab.layout.focused() == pane;
        let exit = code.map_or_else(|| "exited".to_owned(), |c| format!("exited ({c})"));
        self.info(
            format!("the {repo} agent {exit}; opened a shell in its pane"),
            cx,
        );
        if focused && self.may_focus_pane_of(id) {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn on_terminal_event(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        event: &TerminalViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.get_mut(id) else {
            return;
        };
        match event {
            TerminalViewEvent::TitleChanged(new_title) => {
                if let Some(PaneTerminal::Running { title, .. }) = tab.terminals.get_mut(&pane) {
                    *title = (!new_title.is_empty()).then(|| new_title.clone());
                }
            }
            TerminalViewEvent::Exited(code) => {
                let is_agent = tab
                    .layout
                    .role(pane)
                    .is_some_and(|role| role.kind == PaneKind::Agent);
                // An agent xctx stopped (to adopt an attempt, say) is started again by whoever stopped it; one
                // that ended on its own leaves a shell behind, as quitting it in a terminal would.
                let mut reopen = false;
                if let Some(PaneTerminal::Running {
                    view,
                    exited,
                    after_agent,
                    ..
                }) = tab.terminals.get_mut(&pane)
                {
                    *exited = Some(*code);
                    reopen =
                        is_agent && !*after_agent && !view.read(cx).session().is_shutting_down();
                }
                if reopen {
                    self.open_shell_after_agent(id, pane, *code, window, cx);
                    return;
                }
            }
            TerminalViewEvent::Focused => {
                if tab.layout.focused() != pane {
                    // Clicking into a terminal focuses it; keep the tree in step.
                    let _ = tab.layout.focus(pane);
                }
            }
            TerminalViewEvent::Bell => return,
        }
        cx.notify();
    }

    /// Give keyboard focus to the focused pane's terminal of the space being shown, or to the terminal of the
    /// attempt it shows instead (see [`SpaceTab::shown_attempt`]).
    pub(crate) fn focus_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        if let Some(number) = tab.shown_attempt {
            match self.attempt_terminal(&tab.space.id, number) {
                Some(view) => {
                    let handle = view.focus_handle(cx);
                    handle.focus(window, cx);
                }
                // The hidden panes must not take the keys.
                None => self.focus_handle.focus(window, cx),
            }
            return;
        }
        let focused = tab.layout.focused();
        match tab.terminals.get(&focused) {
            Some(PaneTerminal::Running { view, .. }) => {
                let handle = view.focus_handle(cx);
                handle.focus(window, cx);
            }
            Some(PaneTerminal::Omp(omp)) => {
                let handle = omp.view.focus_handle(cx);
                handle.focus(window, cx);
            }
            // A plugin pane drawn by a renderer takes the keys in its main input (Esc leaves it).
            _ => match self.plugin_pane_focus(&(tab.space.id.clone(), focused), cx) {
                Some(handle) => handle.focus(window, cx),
                None => self.focus_handle.focus(window, cx),
            },
        }
    }

    /// Save the layout of the space's tab now. While the store is busy elsewhere, it is saved once it is back.
    pub(crate) fn persist(&mut self, id: &SpaceId, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(id) else {
            return;
        };
        let Some(store) = &self.store else {
            self.pending_persist.insert(id.clone());
            return;
        };
        if let Err(e) = save_layout(store, id, &tab.layout) {
            self.error(e, cx);
        }
    }

    /// Save the layout of the space's tab shortly, once divider drags (in this space or any other) have paused.
    fn persist_soon(&mut self, id: SpaceId, cx: &mut Context<Self>) {
        self.pending_drag_saves.insert(id);
        self.persist_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DRAG_PERSIST_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                for id in std::mem::take(&mut this.pending_drag_saves) {
                    this.persist(&id, cx);
                }
            });
        }));
    }

    /// Apply `change` to the layout of the space being shown, then save it, focus the focused pane and redraw.
    /// Does nothing on Home.
    pub(crate) fn change_layout(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut SpaceLayout) -> Result<(), String>,
    ) {
        let Some(id) = self.active_space_id() else {
            return;
        };
        let Some(tab) = self.tabs.get_mut(&id) else {
            return;
        };
        if let Err(message) = change(&mut tab.layout) {
            self.info(message, cx);
            return;
        }
        self.persist(&id, cx);
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    /// Split the focused pane of the space being shown with a new shell in its repo.
    pub(crate) fn split(&mut self, axis: SplitAxis, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_space_id() else {
            return;
        };
        if let Err(e) = self.split_in(&id, None, axis, PaneKind::Shell, None, window, cx) {
            self.show_op_error(e, cx);
        }
    }

    /// Show a failed pane operation: refusals (such as closing the last pane) as info, failures as errors.
    pub(crate) fn show_op_error(&mut self, error: RpcError, cx: &mut Context<Self>) {
        match error.kind() {
            Some(ErrorCode::Conflict) => self.info(error.message, cx),
            _ => self.error(error.message, cx),
        }
    }

    /// Whether `id` is the space being shown, whose panes have the keyboard focus.
    fn is_shown(&self, id: &SpaceId) -> bool {
        self.view == View::Space(id.clone())
    }

    /// Whether a layout change in `id` may move keyboard focus to its focused pane: only while the space is
    /// shown and no dialog is open over it, so a control request never takes the keyboard from a dialog the
    /// user is typing in. Closing the dialog focuses the focused pane.
    pub(crate) fn may_focus_pane_of(&self, id: &SpaceId) -> bool {
        self.is_shown(id) && self.dialog.is_none()
    }

    /// After a layout change in `id`: save it, focus its focused pane if it may (see
    /// [`Workspace::may_focus_pane_of`]), and redraw.
    pub(crate) fn layout_changed(
        &mut self,
        id: &SpaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.persist(id, cx);
        if self.may_focus_pane_of(id) {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
    }

    /// Split `target` (the focused pane when `None`) of an open space with a new pane of `kind` in `repo` (the
    /// target's repo when `None`), and start it. A new agent resumes the worktree's last session only when no
    /// other agent pane works there (see [`split_agent_start`]). Returns the new pane.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn split_in(
        &mut self,
        id: &SpaceId,
        target: Option<PaneId>,
        axis: SplitAxis,
        kind: PaneKind,
        repo: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PaneId, RpcError> {
        self.split_for_agent(id, target, axis, kind, repo, None, window, cx)
    }

    /// Split as [`Workspace::split_in`] does, binding the new pane to `agent` of the space's graph (when given)
    /// before it starts, so it runs as that agent. A pane split for an agent (a new lane, such as a helper) starts a
    /// new session given its brief ([`crate::agent_brief::lane_brief`]), never resuming another agent's.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn split_for_agent(
        &mut self,
        id: &SpaceId,
        target: Option<PaneId>,
        axis: SplitAxis,
        kind: PaneKind,
        repo: Option<String>,
        agent: Option<AgentNodeId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PaneId, RpcError> {
        let tab = self.open_tab_mut(id)?;
        if let Some(repo) = &repo
            && !tab.space.repos.iter().any(|r| &r.name == repo)
        {
            let names: Vec<&str> = tab.space.repos.iter().map(|r| r.name.as_str()).collect();
            return Err(RpcError::invalid_params(format!(
                "space {id} has no repo {repo}; its repos are {}",
                names.join(", ")
            )));
        }
        let target = target.unwrap_or_else(|| tab.layout.focused());
        let pane = tab
            .layout
            .split(target, axis, kind, repo)
            .map_err(|e| layout_error(id, e))?;
        let others: Vec<PaneRole> = tab
            .layout
            .panes()
            .into_iter()
            .filter(|other| other.id != pane.id)
            .map(|other| other.role)
            .collect();
        let mut start = split_agent_start(&tab.space, &pane.role, &others, agent.is_some());
        let space = tab.space.clone();
        if let Some(agent) = &agent
            && let Some(node) = self
                .graphs
                .load(&space)
                .ok()
                .and_then(|state| state.graph().node(agent).cloned())
        {
            start = AgentStart::Task(crate::agent_brief::lane_brief(&space, &node));
        }
        if let Some(agent) = &agent
            && let Err(e) = self.bind_agent_pane(id, agent, pane.id)
        {
            self.show_op_error(e, cx);
        }
        self.spawn_pane(id, &pane, &start, window, cx);
        self.layout_changed(id, window, cx);
        Ok(pane.id)
    }

    /// The open tab of `id`, or an error saying the space is not open.
    pub(crate) fn open_tab_mut(&mut self, id: &SpaceId) -> Result<&mut SpaceTab, RpcError> {
        self.tabs
            .get_mut(id)
            .ok_or_else(|| RpcError::conflict(format!("space {id} is not open")))
    }

    /// Close `pane` of the space being shown, stopping its program. The last pane stays open.
    pub(crate) fn close_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_space_id() else {
            return;
        };
        if let Err(e) = self.close_pane_in(&id, pane, window, cx) {
            self.show_op_error(e, cx);
        }
    }

    /// Close `pane` of an open space, stopping its program. The last pane stays open (a
    /// [`ErrorCode::Conflict`]).
    pub(crate) fn close_pane_in(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), RpcError> {
        let tab = self.open_tab_mut(id)?;
        if !tab.layout.tree().contains(pane) {
            return Err(RpcError::not_found(format!(
                "space {id} has no pane {}",
                pane.0
            )));
        }
        if tab.layout.tree().panes().len() <= 1 {
            return Err(RpcError::conflict(
                "This is the space's last pane; Discard or Ship the space to close it, or ⌘⇧W to close the window",
            ));
        }
        let was_plugin = tab
            .layout
            .role(pane)
            .is_some_and(|r| r.kind == PaneKind::Plugin);
        tab.layout.close(pane).map_err(|e| layout_error(id, e))?;
        tab.terminals.remove(&pane);
        self.plugin_panes.forget(id, pane);
        if was_plugin {
            self.plugin_pane_closed(id, pane, cx);
        }
        self.sync_agent_panes(id);
        self.layout_changed(id, window, cx);
        Ok(())
    }

    /// Close the focused pane of the space being shown.
    pub(crate) fn close_focused_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(pane) = self.active_tab().map(|tab| tab.layout.focused()) {
            self.close_pane(pane, window, cx);
        }
    }

    /// Focus `pane` of the space being shown, in the tree and its terminal. A change of focus is saved.
    pub(crate) fn focus_pane(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.active_space_id() {
            let _ = self.focus_pane_in(&id, pane, window, cx);
        }
    }

    /// Focus `pane` of an open space, and its terminal if the space is shown with no dialog open. A change of
    /// focus is saved.
    pub(crate) fn focus_pane_in(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), RpcError> {
        let tab = self.open_tab_mut(id)?;
        let changed = tab.layout.focused() != pane;
        tab.layout.focus(pane).map_err(|e| layout_error(id, e))?;
        if changed {
            self.persist(id, cx);
        }
        if self.may_focus_pane_of(id) {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
        Ok(())
    }

    /// Move focus to the neighbouring pane in `direction`.
    pub(crate) fn move_focus(
        &mut self,
        direction: FocusDirection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.active_space_id()
            && let Some(tab) = self.tabs.get_mut(&id)
            && tab.layout.move_focus(direction).is_some()
        {
            self.persist(&id, cx);
            self.focus_active_pane(window, cx);
            cx.notify();
        }
    }

    /// Focus the next (or previous) pane in order.
    pub(crate) fn cycle_focus(
        &mut self,
        forward: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(id) = self.active_space_id()
            && let Some(tab) = self.tabs.get_mut(&id)
        {
            let before = tab.layout.focused();
            if forward {
                tab.layout.focus_next();
            } else {
                tab.layout.focus_prev();
            }
            if tab.layout.focused() != before {
                self.persist(&id, cx);
            }
            self.focus_active_pane(window, cx);
            cx.notify();
        }
    }

    /// Grow (or with a negative `steps`, shrink) the focused pane along `axis`.
    pub(crate) fn resize_focused(
        &mut self,
        axis: SplitAxis,
        steps: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_layout(window, cx, |layout| {
            layout
                .resize_focused(axis, steps * RESIZE_STEP)
                .map(|_| ())
                .map_err(|e| format!("cannot resize: {e}"))
        });
    }

    /// Zoom the focused pane, or end the zoom.
    pub(crate) fn toggle_zoom(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.change_layout(window, cx, |layout| {
            layout.toggle_zoom();
            Ok(())
        });
    }

    fn toggle_zoom_of(&mut self, pane: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        self.change_layout(window, cx, |layout| {
            layout
                .toggle_zoom_of(pane)
                .map(|_| ())
                .map_err(|e| e.to_string())
        });
    }

    /// Zoom `pane` of an open space (focusing it), or toggle the zoom of its focused pane when `None`. Returns the
    /// zoomed pane afterwards.
    pub(crate) fn zoom_in(
        &mut self,
        id: &SpaceId,
        pane: Option<PaneId>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<Option<PaneId>, RpcError> {
        let tab = self.open_tab_mut(id)?;
        let zoomed = match pane {
            Some(pane) => tab
                .layout
                .set_zoom(Some(pane))
                .map_err(|e| layout_error(id, e))?,
            None => {
                tab.layout.toggle_zoom();
                tab.layout.tree().zoomed()
            }
        };
        self.layout_changed(id, window, cx);
        Ok(zoomed)
    }

    fn set_split_ratio(&mut self, split: xctx_layout::SplitId, ratio: f32, cx: &mut Context<Self>) {
        let Some(id) = self.active_space_id() else {
            return;
        };
        if let Some(tab) = self.tabs.get_mut(&id)
            && tab.layout.set_split_ratio(split, ratio).is_ok()
        {
            self.persist_soon(id, cx);
            cx.notify();
        }
    }

    /// Reshape the space being shown: agents keep running, panes the shape adds are started and panes it has no
    /// place for are stopped. The shape and layout are saved. Choosing the shape the space already has does
    /// nothing, so its split shells and divider positions are kept.
    pub(crate) fn reshape(&mut self, shape: Shape, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_space_id() else {
            return;
        };
        if let Err(e) = self.reshape_in(&id, shape, window, cx) {
            self.show_op_error(e, cx);
        }
    }

    /// Reshape an open space, as [`Workspace::reshape`] does for the space being shown.
    pub(crate) fn reshape_in(
        &mut self,
        id: &SpaceId,
        shape: Shape,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), RpcError> {
        if self
            .open_tab_mut(id)
            .is_ok_and(|tab| is_current_shape(tab.space.shape, shape))
        {
            return Ok(());
        }
        if let Some(busy) = &self.busy {
            return Err(RpcError::conflict(wait_message(busy)));
        }
        let Some(store) = self.store.as_mut() else {
            return Err(RpcError::internal("the store is not available"));
        };
        let Some(tab) = self.tabs.get_mut(id) else {
            return Err(RpcError::conflict(format!("space {id} is not open")));
        };
        let (result, warning) = reshape_space(store, &mut tab.space, &mut tab.layout, shape)
            .map_err(RpcError::internal)?;
        for pane in &result.removed {
            tab.terminals.remove(&pane.id);
            self.plugin_panes.forget(id, pane.id);
        }
        self.sync_agent_panes(id);
        for pane in &result.added {
            self.spawn_pane(id, pane, &AgentStart::Continue, window, cx);
        }
        self.refresh_spaces();
        if let Some(warning) = warning {
            self.error(warning, cx);
        }
        if self.may_focus_pane_of(id) {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
        Ok(())
    }

    /// Stop every terminal of the space (attempts' included) and close its tab, after saving its layout. Returns the
    /// live terminal sessions so a caller (such as Ship or Discard) can wait for them to exit
    /// before removing worktrees. Shows Home if the space was shown.
    pub(crate) fn close_space_tab(
        &mut self,
        id: &SpaceId,
        cx: &mut Context<Self>,
    ) -> Vec<Arc<TerminalSession>> {
        if self.tabs.contains_key(id) {
            self.persist(id, cx);
        }
        self.stop_space_plugins(id);
        self.plugin_panes.forget_space(id);
        let sessions = self
            .tabs
            .remove(id)
            .map(|tab| {
                tab.terminals
                    .into_values()
                    .chain(tab.attempt_terminals.into_values())
                    .filter_map(|terminal| match terminal {
                        PaneTerminal::Running { view, .. } => {
                            Some(Arc::clone(view.read(cx).session()))
                        }
                        PaneTerminal::Failed(_) => None,
                        // Dropping it closes its input; omp exits, or is killed after a grace period.
                        PaneTerminal::Omp(_) => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.pending_persist.remove(id);
        self.pending_drag_saves.remove(id);
        if self.view == View::Space(id.clone()) {
            self.view = View::Home;
        }
        cx.notify();
        sessions
    }

    /// The pane area of the space being shown: its split view (under the attempt strip when it has attempts).
    /// The Inspector, when open, takes the tool dock's place beside the panes, or covers them in a narrow window.
    pub(crate) fn render_space(
        &self,
        tab: &SpaceTab,
        dock: Option<xctx_ui::ToolDockView>,
        inspector: Option<InspectorPlacement>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let main = self.render_space_body(tab, cx);
        match inspector {
            Some(InspectorPlacement::Beside(panel)) => div()
                .flex()
                .size_full()
                .child(div().flex_1().min_w_0().h_full().child(main))
                .child(panel)
                .into_any_element(),
            Some(InspectorPlacement::Overlay(panel)) => div()
                .relative()
                .size_full()
                .child(div().size_full().child(main))
                .child(div().absolute().top_0().right_0().bottom_0().child(panel))
                .into_any_element(),
            None => div()
                .flex()
                .size_full()
                .child(div().flex_1().min_w_0().h_full().child(main))
                .children(dock.map(|view| self.render_tool_dock(view, cx)))
                .into_any_element(),
        }
    }

    /// The split view of `tab`'s panes.
    pub(crate) fn render_split(&self, tab: &SpaceTab, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx);
        let panes = tab
            .layout
            .panes()
            .into_iter()
            .map(|pane| {
                let terminal = tab.terminals.get(&pane.id);
                let title = pane_title(&pane.role, terminal);
                let content: AnyElement = match terminal {
                    Some(PaneTerminal::Running { view, .. }) => div()
                        .relative()
                        .size_full()
                        .pl(Theme::SPACE_2)
                        .pt(Theme::SPACE_1)
                        .child(view.clone())
                        .children(self.render_serve_hint(&tab.space.id, pane.id, cx))
                        .into_any_element(),
                    Some(PaneTerminal::Failed(message)) => div()
                        .size_full()
                        .p(Theme::SPACE_3)
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.danger)
                        .child(message.clone())
                        .into_any_element(),
                    Some(PaneTerminal::Omp(omp)) => self.render_rpc_pane(omp, cx),
                    // Plugin panes drawn by a first-party renderer have no terminal.
                    None if pane.role.kind == PaneKind::Plugin => self
                        .render_plugin_pane(&tab.space.id, pane.id, cx)
                        .unwrap_or_else(|| {
                            div()
                                .size_full()
                                .p(Theme::SPACE_3)
                                .text_size(Theme::TEXT_SM)
                                .text_color(theme.text_subtle)
                                .child(format!(
                                    "{} tool",
                                    pane.role.plugin.as_deref().unwrap_or("unknown")
                                ))
                                .into_any_element()
                        }),
                    None => div()
                        .size_full()
                        .p(Theme::SPACE_3)
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_subtle)
                        .child("starting…")
                        .into_any_element(),
                };
                let kind: PaneKind = pane.role.kind;
                let mut view = PaneView::new(pane.id, title, content).kind(kind);
                if let Some(state) = self.graphs.get(&tab.space.id)
                    && let Some(node) = state
                        .graph()
                        .nodes()
                        .iter()
                        .find(|node| node.pane_id == Some(pane.id.0))
                    && let Some(color) =
                        xctx_ui::agent_identity_color(state.graph(), node.id.as_str(), &theme)
                {
                    view = view.agent_color(color);
                }
                match self.pane_accessories(tab, &pane, cx) {
                    Some(accessories) => view.accessories(accessories),
                    None => view,
                }
            })
            .collect();
        let entity = cx.entity().downgrade();
        let on_focus = entity.clone();
        let on_zoom = entity.clone();
        let on_close = entity.clone();
        let on_drop_pane = entity.clone();
        let on_drop_tool = entity.clone();
        let on_resize = entity;
        SplitView::new(
            SharedString::from(format!("split-{}", tab.space.id)),
            tab.layout.tree(),
            panes,
        )
        .on_focus(move |pane, window, cx| {
            let _ = on_focus.update(cx, |this, cx| this.focus_pane(pane, window, cx));
        })
        .on_zoom(move |pane, window, cx| {
            let _ = on_zoom.update(cx, |this, cx| this.toggle_zoom_of(pane, window, cx));
        })
        .on_close(move |pane, window, cx| {
            let _ = on_close.update(cx, |this, cx| this.close_pane(pane, window, cx));
        })
        .on_resize(move |split, ratio, _window, cx| {
            let _ = on_resize.update(cx, |this, cx| this.set_split_ratio(split, ratio, cx));
        })
        .on_drop_pane(move |pane, target, edge, window, cx| {
            let _ = on_drop_pane.update(cx, |this, cx| {
                this.drop_pane(pane, target, edge, window, cx)
            });
        })
        .on_drop_tool(move |tool, target, edge, window, cx| {
            let tool = tool.clone();
            let _ = on_drop_tool.update(cx, |this, cx| {
                this.drop_tool(&tool, target, edge, window, cx)
            });
        })
        .into_any_element()
    }

    /// What a pane's header shows after its title, for an agent or shell pane working in one of the space's repos:
    /// the repo's address on its git host (click copies it), its branch, Files (xctx's in-app file explorer of its
    /// worktree) and Git (a lazygit pane in it); for an agent pane whose agent exited, Restart agent. Nothing here
    /// leaves the app.
    fn pane_accessories(
        &self,
        tab: &SpaceTab,
        pane: &RolePane,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if !matches!(pane.role.kind, PaneKind::Agent | PaneKind::Shell) {
            return None;
        }
        let repo = tab.space.repos.iter().find(|r| r.name == pane.role.repo)?;
        let id = pane.id.0 as usize;
        let mut row = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_0_5);
        if let Some(url) = tab.repo_links.get(&repo.name).cloned() {
            let shown = url.trim_start_matches("https://").to_owned();
            row = row.child(
                Button::new(("pane-remote", id), "")
                    .icon(IconName::Globe)
                    .tooltip(format!("Copy {shown}"))
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.write_to_clipboard(gpui::ClipboardItem::new_string(url.clone()));
                        this.info(format!("copied {url}"), cx);
                    })),
            );
        }
        let exited = pane.role.kind == PaneKind::Agent
            && tab.terminals.get(&pane.id).is_some_and(|terminal| {
                matches!(
                    terminal,
                    PaneTerminal::Running {
                        after_agent: true,
                        ..
                    }
                ) || !terminal.is_alive(cx)
            });
        if exited {
            let (space, restart) = (tab.space.id.clone(), pane.clone());
            row = row.child(
                Button::new(("pane-restart", id), "Restart agent")
                    .variant(ButtonVariant::Primary)
                    .size(ButtonSize::Compact)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.restart_agent_in(&space, &restart, window, cx)
                    })),
            );
        }
        if tab
            .terminals
            .get(&pane.id)
            .and_then(PaneTerminal::omp)
            .is_some_and(|omp| omp.agent.is_live())
        {
            let (space, rpc_pane) = (tab.space.id.clone(), pane.id);
            row = row.child(
                Button::new(("pane-open-terminal", id), "Open in terminal")
                    .tooltip("Stop this agent here and resume its omp session in a terminal")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_rpc_agent_in_terminal(&space, rpc_pane, window, cx)
                    })),
            );
        }
        let (files_space, files_repo) = (tab.space.id.clone(), repo.name.clone());
        let (git_space, git_repo, git_pane) = (tab.space.id.clone(), repo.name.clone(), pane.id);
        Some(
            row.child(
                Button::new(("pane-files", id), "")
                    .icon(IconName::Folder)
                    .tooltip(format!("Files of {}", repo.name))
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_repo_files(&files_space, &files_repo, window, cx)
                    })),
            )
            .child(
                Button::new(("pane-git", id), "")
                    .icon(IconName::Git)
                    .tooltip(format!("Git (lazygit) in {}", repo.name))
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_repo_git(&git_space, &git_repo, git_pane, window, cx)
                    })),
            )
            .into_any_element(),
        )
    }

    /// Files: show xctx's file explorer of `repo`'s worktree in open space `id`, mounting the `files` plugin there
    /// unless it is mounted already.
    fn open_repo_files(
        &mut self,
        id: &SpaceId,
        repo: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mounted = self
            .mounted_plugins(id)
            .into_iter()
            .find(|m| m.plugin == "files" && m.repo == repo);
        let result = match mounted {
            Some(mount) => self.focus_pane_in(id, mount.pane, window, cx),
            None => self
                .mount_plugin(
                    id,
                    "files",
                    Some(repo.to_owned()),
                    crate::plugin_host::MountSettings::default(),
                    false,
                    window,
                    cx,
                )
                .map(|_| ()),
        };
        if let Err(e) = result {
            self.show_op_error(e, cx);
        }
    }

    /// Git: focus the lazygit pane of `repo` in open space `id`, or split one beside `beside`.
    fn open_repo_git(
        &mut self,
        id: &SpaceId,
        repo: &str,
        beside: PaneId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.tabs.get(id).and_then(|tab| {
            tab.layout
                .panes()
                .into_iter()
                .find(|p| p.role.kind == PaneKind::Git && p.role.repo == repo)
                .map(|p| p.id)
        });
        let result = match existing {
            Some(pane) => self.focus_pane_in(id, pane, window, cx),
            None => self
                .split_in(
                    id,
                    Some(beside),
                    SplitAxis::Vertical,
                    PaneKind::Git,
                    Some(repo.to_owned()),
                    window,
                    cx,
                )
                .map(|_| ()),
        };
        if let Err(e) = result {
            self.show_op_error(e, cx);
        }
    }

    /// Start the agent of agent pane `pane` of open space `id` again (it exited, and its pane may run the shell
    /// opened after it): the pane's terminal is replaced by the agent resuming its session.
    pub(crate) fn restart_agent_in(
        &mut self,
        id: &SpaceId,
        pane: &RolePane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.rpc_pane_is_live(id, pane.id) {
            return;
        }
        let Some(tab) = self.tabs.get_mut(id) else {
            return;
        };
        tab.terminals.remove(&pane.id);
        self.spawn_pane(id, pane, &AgentStart::Continue, window, cx);
        if self.may_focus_pane_of(id) {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reshaping_to_the_current_shape_is_a_no_op() {
        let shapes = [
            (SpaceShape::Dev, Shape::Dev),
            (SpaceShape::Debug, Shape::Debug),
            (SpaceShape::Review, Shape::Review),
        ];
        for (current, _) in shapes {
            for (other, shape) in shapes {
                assert_eq!(is_current_shape(current, shape), current == other);
            }
        }
    }

    #[test]
    fn titles_fall_back_to_repo_and_kind() {
        let role = PaneRole::new(PaneKind::Agent, "api");
        assert_eq!(pane_title(&role, None), "api · agent");
        assert_eq!(
            pane_title(&PaneRole::for_plugin("api-client", "api"), None),
            "api · api-client"
        );
        assert_eq!(
            pane_title(&role, Some(&PaneTerminal::Failed("x".into()))),
            "api · agent — failed to start"
        );
    }
}
