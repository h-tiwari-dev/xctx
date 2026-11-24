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
use xctx_ui::{PaneView, SharedTab, SplitView, Theme, pane_kind_label};

use crate::lifecycle::reshape_space;
use crate::persist::{restore_layout, save_layout};
use crate::space_graph::{scratch_dir_for, writer_panes};
use crate::space_layout::{RESIZE_STEP, SpaceLayout, space_shape};
use crate::spawn::{AgentStart, pane_launch_with, restore_agent_starts, split_agent_start};
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
        _subscription: Subscription,
    },
    /// The program could not be started.
    Failed(String),
}

impl PaneTerminal {
    /// Whether its program is still running.
    pub(crate) fn is_alive(&self, cx: &App) -> bool {
        match self {
            PaneTerminal::Running { view, exited, .. } => {
                exited.is_none() && view.read(cx).session().is_alive()
            }
            PaneTerminal::Failed(_) => false,
        }
    }
}

/// An open space tab: the space, its layout, and a terminal per pane.
pub(crate) struct SpaceTab {
    pub(crate) space: Space,
    pub(crate) layout: SpaceLayout,
    pub(crate) terminals: HashMap<PaneId, PaneTerminal>,
    /// Whether the agent graph panel is open beside the panes.
    pub(crate) graph_open: bool,
    /// The tab of the Shared Space panel shown in the graph panel.
    pub(crate) shared_tab: SharedTab,
    /// The agent terminal of each live attempt, by attempt number. They are not in the layout; the attempt strip
    /// switches to them (see [`Workspace::attempt_terminal`]).
    pub(crate) attempt_terminals: HashMap<u32, PaneTerminal>,
}

impl SpaceTab {
    /// A tab for `space` laid out as `layout`, with no terminals started yet and the graph panel closed.
    fn new(space: Space, layout: SpaceLayout) -> SpaceTab {
        SpaceTab {
            space,
            layout,
            terminals: HashMap::new(),
            graph_open: false,
            shared_tab: SharedTab::default(),
            attempt_terminals: HashMap::new(),
        }
    }
}

impl SpaceTab {
    /// Whether any agent pane's program is still running.
    pub(crate) fn agent_running(&self, cx: &App) -> bool {
        self.layout.agent_panes().iter().any(|pane| {
            self.terminals
                .get(pane)
                .is_some_and(|terminal| terminal.is_alive(cx))
        })
    }
}

/// Whether `shape` is the shape a space in `current` already has, so reshaping to it would change nothing.
pub(crate) fn is_current_shape(current: SpaceShape, shape: Shape) -> bool {
    current == space_shape(shape)
}

/// The header of a pane: the program's own title when it set one, else `<repo> · <kind>`, with the exit code
/// once it exited.
pub(crate) fn pane_title(role: &PaneRole, terminal: Option<&PaneTerminal>) -> String {
    let fallback = format!("{} · {}", role.repo, pane_kind_label(role.kind));
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

    /// Open a space that was just launched with its first layout, its agents given the task.
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
        self.tabs.insert(id.clone(), SpaceTab::new(space, layout));
        for pane in &panes {
            self.spawn_pane(&id, pane, &start, window, cx);
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
        let agent = self.pane_agent_of(id, pane);
        let session = self.pane_session_of(id, pane, start);
        let Some(tab) = self.tabs.get(id) else {
            return;
        };
        if let Ok(scratch) = scratch_dir_for(id.as_str()) {
            // Best effort: agents may write there with their own tools, which need the directory.
            let _ = std::fs::create_dir_all(scratch.root());
        }
        let launch = pane_launch_with(&tab.space, &pane.role, start, &session, &self.search)
            .with_agent(agent.as_ref());
        let program = launch.config.program.clone();
        let terminal = match TerminalView::spawn(
            launch.config,
            self.terminal_font.clone(),
            self.terminal_theme,
            window,
            cx,
        ) {
            Ok(view) => {
                let space_id = id.clone();
                let pane_id = pane.id;
                let subscription = cx.subscribe_in(
                    &view,
                    window,
                    move |this, _view, event: &TerminalViewEvent, window, cx| {
                        this.on_terminal_event(&space_id, pane_id, event, window, cx);
                    },
                );
                PaneTerminal::Running {
                    view,
                    title: None,
                    exited: None,
                    _subscription: subscription,
                }
            }
            Err(e) => {
                let message = format!("could not start {program} for {}: {e}", pane.role.repo);
                self.error(message.clone(), cx);
                PaneTerminal::Failed(message)
            }
        };
        if let Some(tab) = self.tabs.get_mut(id) {
            tab.terminals.insert(pane.id, terminal);
        }
        if let Some(notice) = launch.notice {
            self.info(notice, cx);
        }
    }

    fn on_terminal_event(
        &mut self,
        id: &SpaceId,
        pane: PaneId,
        event: &TerminalViewEvent,
        _window: &mut Window,
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
                if let Some(PaneTerminal::Running { exited, .. }) = tab.terminals.get_mut(&pane) {
                    *exited = Some(*code);
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

    /// Give keyboard focus to the focused pane's terminal of the space being shown.
    pub(crate) fn focus_active_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let focused = tab.layout.focused();
        match tab.terminals.get(&focused) {
            Some(PaneTerminal::Running { view, .. }) => {
                let handle = view.focus_handle(cx);
                handle.focus(window, cx);
            }
            _ => self.focus_handle.focus(window, cx),
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
    fn change_layout(
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
    fn may_focus_pane_of(&self, id: &SpaceId) -> bool {
        self.is_shown(id) && self.dialog.is_none()
    }

    /// After a layout change in `id`: save it, focus its focused pane if it may (see
    /// [`Workspace::may_focus_pane_of`]), and redraw.
    fn layout_changed(&mut self, id: &SpaceId, window: &mut Window, cx: &mut Context<Self>) {
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
    /// before it starts, so it runs as that agent. A pane split for an agent (a new lane, such as a helper) always
    /// starts fresh: `pi --continue` would resume the session of another agent in the worktree.
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
        let start = split_agent_start(&tab.space, &pane.role, &others, agent.is_some());
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
    fn open_tab_mut(&mut self, id: &SpaceId) -> Result<&mut SpaceTab, RpcError> {
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
        tab.layout.close(pane).map_err(|e| layout_error(id, e))?;
        tab.terminals.remove(&pane);
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

    /// The split view of the space being shown, with the agent graph panel beside it when open.
    pub(crate) fn render_space(&self, tab: &SpaceTab, cx: &mut Context<Self>) -> AnyElement {
        let split = self.render_split(tab, cx);
        if !tab.graph_open {
            return split;
        }
        div()
            .flex()
            .size_full()
            .child(div().flex_1().min_w_0().h_full().child(split))
            .child(self.render_graph_panel(tab, cx))
            .into_any_element()
    }

    fn render_split(&self, tab: &SpaceTab, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx);
        let panes = tab
            .layout
            .panes()
            .into_iter()
            .map(|pane| {
                let terminal = tab.terminals.get(&pane.id);
                let title = pane_title(&pane.role, terminal);
                let content: AnyElement = match terminal {
                    Some(PaneTerminal::Running { view, .. }) => {
                        div().size_full().child(view.clone()).into_any_element()
                    }
                    Some(PaneTerminal::Failed(message)) => div()
                        .size_full()
                        .p(Theme::SPACE_3)
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.danger)
                        .child(message.clone())
                        .into_any_element(),
                    None => div()
                        .size_full()
                        .p(Theme::SPACE_3)
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_subtle)
                        .child("starting…")
                        .into_any_element(),
                };
                let kind: PaneKind = pane.role.kind;
                PaneView::new(pane.id, title, content).kind(kind)
            })
            .collect();
        let entity = cx.entity().downgrade();
        let on_focus = entity.clone();
        let on_zoom = entity.clone();
        let on_close = entity.clone();
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
        .into_any_element()
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
        let role = PaneRole {
            kind: PaneKind::Agent,
            repo: "api".into(),
        };
        assert_eq!(pane_title(&role, None), "api · agent");
        assert_eq!(
            pane_title(&role, Some(&PaneTerminal::Failed("x".into()))),
            "api · agent — failed to start"
        );
    }
}
