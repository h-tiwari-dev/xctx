//! Agent graph integration for the workspace shell: live pane states, pane bindings, and adding or opening an
//! agent. Activity is shown by the Inspector beside the panes; there is no separate graph screen.

use std::collections::HashMap;

use gpui::{App, Context, Window};
use xctx_control::RpcError;
use xctx_core::{Space, SpaceId};
use xctx_graph::{AgentGraph, AgentNodeId, AgentRole, HelperKind};
use xctx_layout::{PaneId, PaneKind, RolePane, SplitAxis};
use xctx_ui::{PaneLive, helper_kind_label};

use crate::plugin_host::MountSettings;
use crate::space_graph::{
    SpaceGraphState, add_role, graph_error, pane_agent, remove_lane, sync_pane_bindings,
};
use crate::space_tab::{PaneTerminal, SpaceTab};
use crate::spawn::PaneAgent;
use crate::workspace::Workspace;

/// The plugin a Browser helper drives.
const BROWSER_PLUGIN: &str = "browser";

/// What each agent of `graph` in `tab` is doing, as its pane's terminal shows: running, exited, or no pane. Agents
/// run over RPC are left out: their graph state comes from their frames, so it is what they show.
pub(crate) fn pane_lives(
    graph: &AgentGraph,
    tab: &SpaceTab,
    cx: &App,
) -> HashMap<AgentNodeId, PaneLive> {
    graph
        .nodes()
        .iter()
        .filter_map(|node| {
            let terminal = node
                .pane_id
                .and_then(|pane| tab.terminals.get(&PaneId(pane)));
            let live = match terminal {
                None => PaneLive::NoPane,
                Some(PaneTerminal::Omp(_)) => return None,
                // The shell opened after an agent quit is not the agent.
                Some(PaneTerminal::Running {
                    after_agent: true, ..
                }) => PaneLive::Exited(None),
                Some(terminal) if terminal.is_alive(cx) => PaneLive::Running,
                Some(PaneTerminal::Running { exited, .. }) => PaneLive::Exited(exited.flatten()),
                Some(PaneTerminal::Failed(_)) => PaneLive::Exited(None),
            };
            Some((node.id.clone(), live))
        })
        .collect()
}

impl Workspace {
    /// ⌘G or the title bar's Panes | Activity switch: show the Inspector on Graph without replacing the panes,
    /// or close it when that exact view is already open.
    pub(crate) fn toggle_flow(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        if self.active_space_id().is_none() {
            return;
        }
        let activity_open = self.prefs.inspector_open
            && self.inspector_tab == xctx_ui::inspector::InspectorTab::Graph;
        if activity_open {
            self.set_inspector_open(false, cx);
        } else {
            self.show_inspector_tab(xctx_ui::inspector::InspectorTab::Graph, cx);
        }
    }

    /// The agent `pane` of space `id` runs as (see [`pane_agent`]), after bringing the graph's pane bindings in
    /// step with the space's panes. `None` when the graph cannot be loaded.
    pub(crate) fn pane_agent_of(&mut self, id: &SpaceId, pane: &RolePane) -> Option<PaneAgent> {
        self.sync_agent_panes(id);
        let tab = self.tabs.get(id)?;
        let state = self.graphs.load(&tab.space).ok()?;
        pane_agent(state.graph(), pane)
    }

    /// Bring the pane bindings of the graph of open space `id` in step with its panes (see
    /// [`sync_pane_bindings`]), saving the graph when they change. Best effort: a graph that cannot be loaded or
    /// saved keeps its bindings, and panes then run as their repo's writer.
    pub(crate) fn sync_agent_panes(&mut self, id: &SpaceId) {
        let Some(tab) = self.tabs.get(id) else {
            return;
        };
        let panes = tab.layout.panes();
        let Ok(state) = self.graphs.load(&tab.space) else {
            return;
        };
        let mut graph = state.graph().clone();
        if sync_pane_bindings(&mut graph, &panes) {
            let _ = state.change(|doc| {
                doc.graph = graph;
                Ok(())
            });
        }
    }

    /// Bind `agent` of the graph of open space `id` to `pane`, and save the graph.
    pub(crate) fn bind_agent_pane(
        &mut self,
        id: &SpaceId,
        agent: &AgentNodeId,
        pane: PaneId,
    ) -> Result<(), RpcError> {
        self.change_graph_of(id, |state, space| {
            state.change(|doc| {
                doc.graph
                    .bind_pane(agent, Some(pane.0))
                    .map_err(|e| graph_error(&space.id, e))
            })
        })
    }

    /// Run `op` on the graph state of open space `id`, loading it on first use.
    fn change_graph_of<T>(
        &mut self,
        id: &SpaceId,
        op: impl FnOnce(&mut SpaceGraphState, &Space) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        let tab = self
            .tabs
            .get(id)
            .ok_or_else(|| RpcError::conflict(format!("space {id} is not open")))?;
        let state = self.graphs.load(&tab.space).map_err(RpcError::internal)?;
        op(state, &tab.space)
    }

    /// Run `op` from a panel button on the graph of the space being shown, redraw, and show what went wrong.
    /// Ignored while a dialog is open.
    pub(crate) fn panel_change<T>(
        &mut self,
        cx: &mut Context<Self>,
        op: impl FnOnce(&mut SpaceGraphState, &Space) -> Result<T, RpcError>,
    ) -> Option<T> {
        if self.dialog.is_some() {
            return None;
        }
        let id = self.active_space_id()?;
        let result = self.change_graph_of(&id, op);
        cx.notify();
        match result {
            Ok(value) => Some(value),
            Err(e) => {
                self.show_op_error(e, cx);
                None
            }
        }
    }

    /// Focus the pane `agent` of open space `id` runs in. Fails when the agent has no pane.
    pub(crate) fn open_agent_pane(
        &mut self,
        id: &SpaceId,
        agent: &AgentNodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), RpcError> {
        let pane = self
            .graphs
            .get(id)
            .and_then(|state| state.graph().node(agent))
            .and_then(|node| node.pane_id)
            .ok_or_else(|| RpcError::conflict(format!("{agent} has no pane; Start it first")))?;
        self.focus_pane_in(id, PaneId(pane), window, cx)
    }

    /// Start: split a new agent pane for `agent` of open space `id`, which has none (the orchestrator, or a lane
    /// whose pane was closed), in its repo (a writer's own, else the space's first). It starts with the agent's
    /// brief. A Browser helper also gets xctx's browser mounted.
    pub(crate) fn start_agent_pane(
        &mut self,
        id: &SpaceId,
        agent: &AgentNodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<PaneId, RpcError> {
        let tab = self.open_tab_mut(id)?;
        let space = tab.space.clone();
        let node = self
            .graphs
            .load(&space)
            .map_err(RpcError::internal)?
            .graph()
            .node(agent)
            .cloned()
            .ok_or_else(|| RpcError::not_found(format!("space {id} has no agent {agent}")))?;
        if node.pane_id.is_some() {
            return Err(RpcError::conflict(format!(
                "{agent} already has a pane; Open it"
            )));
        }
        let repo = node
            .role
            .repo()
            .map(str::to_owned)
            .or_else(|| space.repos.first().map(|r| r.name.clone()));
        let pane = self.split_for_agent(
            id,
            None,
            SplitAxis::Horizontal,
            PaneKind::Agent,
            repo,
            Some(agent.clone()),
            window,
            cx,
        )?;
        if node.role
            == (AgentRole::Helper {
                kind: HelperKind::Browser,
            })
        {
            self.ensure_browser_mounted(id, window, cx);
        }
        Ok(pane)
    }

    /// Mount xctx's browser in open space `id` unless it is mounted, for a Browser helper to drive. Best effort:
    /// a failure is shown, and the helper's brief tells it how to mount the browser itself.
    fn ensure_browser_mounted(
        &mut self,
        id: &SpaceId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .mounted_plugins(id)
            .iter()
            .any(|m| m.plugin == BROWSER_PLUGIN)
        {
            return;
        }
        if let Err(e) = self.mount_plugin(
            id,
            BROWSER_PLUGIN,
            None,
            MountSettings::default(),
            false,
            window,
            cx,
        ) {
            self.show_op_error(e, cx);
        }
    }

    /// Add a helper of `kind` to the graph and start its pane (see [`Workspace::start_agent_pane`]): a new session
    /// given its brief, in the space's first repo, with xctx's browser mounted for a Browser helper. When the
    /// pane cannot be split, the helper is taken out of the graph again so no lane is left without a pane.
    pub(crate) fn add_helper_lane(
        &mut self,
        kind: HelperKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(added) = self.panel_change(cx, |state, space| {
            let lane = add_role(state, &space.id, AgentRole::Helper { kind }, None)?;
            Ok((space.id.clone(), lane))
        }) else {
            return;
        };
        let (id, lane) = added;
        let agent = AgentNodeId::new(lane.id);
        match self.start_agent_pane(&id, &agent, window, cx) {
            Ok(_) => self.info(
                format!("Added {} ({agent}) to {id}", helper_kind_label(kind)),
                cx,
            ),
            Err(e) => {
                let removed = self.change_graph_of(&id, |state, space| {
                    remove_lane(state, &space.id, agent.as_str())
                });
                cx.notify();
                self.show_op_error(e, cx);
                if let Err(e) = removed {
                    self.show_op_error(e, cx);
                }
            }
        }
    }
}
