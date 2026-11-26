//! The Flow view of a space (⌘G flips a space between its panes and its Flow): the loop's stage track, the live
//! agent graph, the selected agent's detail, and the timeline of what happened, and what their buttons do. Every
//! change is saved before the view redraws. Also what keeps the graph's pane bindings in step with the panes.

use std::collections::HashMap;

use gpui::{AnyElement, App, Context, ParentElement, Styled, Window, div, prelude::*};
use xctx_control::RpcError;
use xctx_core::{Space, SpaceId};
use xctx_graph::{AgentGraph, AgentNodeId, AgentRole, AutonomyLevel, HelperKind, LoopStatus};
use xctx_layout::{PaneId, PaneKind, RolePane, SplitAxis};
use xctx_nav::flow::{FlowFocus, TimelineInput, pair_traffic, timeline};
use xctx_ui::flow_timeline::{FlowTimeline, sync_list_state};
use xctx_ui::flow_view::{AgentDetail, FlowCanvas, FlowView};
use xctx_ui::stage_track::StageTrack;
use xctx_ui::{PaneLive, Theme, agent_identity_color, helper_kind_label};

use crate::plugin_host::MountSettings;
use crate::space_graph::{
    SpaceGraphState, add_role, channel_error, graph_error, loop_advance, loop_error, pane_agent,
    pending_loop_approvals, remove_lane, set_agent_paused_and_tell, sync_pane_bindings,
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
    /// ⌘G or the title bar's Panes | Flow switch: show the Flow view of the space being shown, or its panes
    /// again. Going back to the panes gives the focused pane its keyboard back.
    pub(crate) fn toggle_flow(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_space_id() else {
            return;
        };
        let Some(tab) = self.tabs.get_mut(&id) else {
            return;
        };
        tab.flow_open = !tab.flow_open;
        if tab.flow_open {
            let space = tab.space.clone();
            if let Err(e) = self.graphs.load(&space) {
                self.error(e, cx);
            }
            self.sync_agent_panes(&id);
            // The Flow view takes the keys from the terminals (nothing in it types).
            self.focus_handle.focus(window, cx);
        } else {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
    }

    /// Narrow the Flow view of the space being shown to `focus` (an agent or a pair), or show everything.
    fn set_flow_focus(&mut self, focus: Option<FlowFocus>, cx: &mut Context<Self>) {
        if let Some(id) = self.active_space_id()
            && let Some(tab) = self.tabs.get_mut(&id)
            && tab.flow_focus != focus
        {
            tab.flow_focus = focus;
            cx.notify();
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
    fn panel_change<T>(
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

    /// Open: show the panes with the pane of `agent` focused.
    fn talk_to_agent(&mut self, agent: &AgentNodeId, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        let Some(id) = self.active_space_id() else {
            return;
        };
        if let Err(e) = self.open_agent_pane(&id, agent, window, cx) {
            self.show_op_error(e, cx);
        }
    }

    /// Focus the pane `agent` of open space `id` runs in, leaving the Flow view so the pane has the keyboard.
    /// Fails when the agent has no pane.
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
        if let Some(tab) = self.tabs.get_mut(id) {
            tab.flow_open = false;
        }
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

    /// Approve next: approve the stage change the loop of the space being shown waits for, and dismiss its
    /// pending loop approval notices in the Inbox so they do not pile up.
    fn approve_next_stage(&mut self, cx: &mut Context<Self>) {
        let approved = self.panel_change(cx, |state, space| {
            state.change(|doc| {
                doc.loop_run
                    .approve_next_stage()
                    .map_err(|e| loop_error(&space.id, e))
            })?;
            Ok(space.id.clone())
        });
        let Some(id) = approved else {
            return;
        };
        let Some(inbox) = self.inbox.as_mut() else {
            return;
        };
        for item in pending_loop_approvals(inbox, &id) {
            if let Err(e) = inbox.dismiss(&item) {
                self.error(format!("could not update the Inbox: {e}"), cx);
                return;
            }
        }
    }

    /// Remove a helper from the graph (the panel offers Remove only on helpers). Its pane keeps running.
    fn remove_agent_lane(&mut self, agent: &AgentNodeId, cx: &mut Context<Self>) {
        if let Some(node) = self.panel_change(cx, |state, space| {
            remove_lane(state, &space.id, agent.as_str())
        }) {
            let message = if node.pane_id.is_some() {
                format!(
                    "Removed {} from the graph; its pane keeps running",
                    node.label
                )
            } else {
                format!("Removed {} from the graph", node.label)
            };
            self.info(message, cx);
        }
    }

    /// The Flow view of `tab`.
    pub(crate) fn render_flow(&self, tab: &SpaceTab, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx);
        let Some(state) = self.graphs.get(&tab.space.id) else {
            let message = self
                .graphs
                .error(&tab.space.id)
                .map(|e| format!("The agent graph could not be opened: {e}"))
                .unwrap_or_else(|| "The agent graph is not loaded.".to_owned());
            return div()
                .size_full()
                .p(Theme::SPACE_4)
                .text_size(Theme::TEXT_SM)
                .text_color(theme.danger)
                .child(message)
                .into_any_element();
        };
        let graph = state.graph().clone();
        let run = state.loop_run().clone();
        let live = pane_lives(&graph, tab, cx);
        let entity = cx.entity().downgrade();
        let selected_agent = match &tab.flow_focus {
            Some(FlowFocus::Agent(id)) => Some(id.clone()),
            _ => None,
        };

        let (start, advance, approve, reset, autonomy, pick, add) = (
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
        );
        let track = StageTrack::new(run.clone(), graph.clone())
            .live(live.clone())
            .selected(selected_agent)
            .on_start(move |_, cx| {
                let _ = start.update(cx, |this, cx| {
                    this.panel_change(cx, |state, space| {
                        state.change(|doc| {
                            doc.loop_run.start().map_err(|e| loop_error(&space.id, e))
                        })
                    });
                });
            })
            .on_advance(move |_, cx| {
                let _ = advance.update(cx, |this, cx| {
                    this.panel_change(cx, |state, space| {
                        loop_advance(state, &space.id, Some(true))
                    });
                });
            })
            .on_approve_next(move |_, cx| {
                let _ = approve.update(cx, |this, cx| this.approve_next_stage(cx));
            })
            .on_reset(move |_, cx| {
                let _ = reset.update(cx, |this, cx| {
                    this.panel_change(cx, |state, _| {
                        state.change(|doc| {
                            doc.loop_run.reset();
                            Ok(())
                        })
                    });
                });
            })
            .on_autonomy(move |level: AutonomyLevel, _, cx| {
                let _ = autonomy.update(cx, |this, cx| {
                    this.panel_change(cx, |state, _| {
                        state.change(|doc| {
                            doc.loop_run.set_autonomy(level);
                            Ok(())
                        })
                    });
                });
            })
            .on_select_agent(move |agent, _, cx| {
                let agent = agent.clone();
                let _ = pick.update(cx, |this, cx| {
                    this.set_flow_focus(Some(FlowFocus::Agent(agent)), cx)
                });
            })
            .on_add_helper(move |kind, window, cx| {
                let _ = add.update(cx, |this, cx| this.add_helper_lane(kind, window, cx));
            });

        let refusals = self.fork_refusals_in(state);
        let mut notes = self.attempt_notes(&tab.space.id);
        for (agent, why) in &refusals {
            let note = match notes.get(agent) {
                Some(lineage) => format!("{lineage} · no attempts: {why}"),
                None => format!("no attempts: {why}"),
            };
            notes.insert(agent.clone(), note.into());
        }
        let shared = state.shared();
        let messages = shared.list_messages(None, None, None, None, &self.redactor);
        let running_stage = matches!(run.status(), LoopStatus::Running).then(|| run.stage());
        let (select, open) = (entity.clone(), entity.clone());
        let canvas = FlowCanvas::new(format!("flow-canvas-{}", tab.space.id), graph.clone())
            .live(live.clone())
            .traffic(pair_traffic(&messages))
            .running_stage(running_stage)
            .focus(tab.flow_focus.clone())
            .notes(notes.clone())
            .on_select(move |focus, _, cx| {
                let _ = select.update(cx, |this, cx| this.set_flow_focus(focus, cx));
            })
            .on_open(move |agent, window, cx| {
                let agent = agent.clone();
                let _ = open.update(cx, |this, cx| this.talk_to_agent(&agent, window, cx));
            });

        let findings = shared.findings(false, &self.redactor);
        let shared_notes = shared.notes(&self.redactor);
        let contract = shared.contract(&self.redactor);
        let items = timeline(
            &TimelineInput {
                messages: &messages,
                findings: &findings,
                notes: &shared_notes,
                contract: contract.as_ref(),
                stages: run.history(),
                graph: &graph,
            },
            tab.flow_focus.as_ref(),
        );
        sync_list_state(&tab.timeline, items.len());
        let label_of = |id: &AgentNodeId| {
            graph
                .node(id)
                .map_or_else(|| id.as_str().to_owned(), |node| node.label.clone())
        };
        let focus_label = tab.flow_focus.as_ref().map(|focus| match focus {
            FlowFocus::Agent(id) => label_of(id).into(),
            FlowFocus::Pair(a, b) => format!("{} ↔ {}", label_of(a), label_of(b)).into(),
        });
        let (clear, chosen, resolve) = (entity.clone(), entity.clone(), entity.clone());
        let timeline = FlowTimeline::new(items, tab.timeline.clone())
            .graph(graph.clone())
            // A pair's conversation is shown on its own, without the contract pinned over it.
            .contract(contract.filter(|_| !matches!(tab.flow_focus, Some(FlowFocus::Pair(..)))))
            .focus_label(focus_label)
            .now(crate::attempts::now())
            .on_clear_focus(move |_, cx| {
                let _ = clear.update(cx, |this, cx| this.set_flow_focus(None, cx));
            })
            .on_select_agent(move |agent, _, cx| {
                let agent = agent.clone();
                let _ = chosen.update(cx, |this, cx| {
                    this.set_flow_focus(Some(FlowFocus::Agent(agent)), cx)
                });
            })
            .on_resolve_finding(move |finding, _, cx| {
                let _ = resolve.update(cx, |this, cx| {
                    this.panel_change(cx, |state, space| {
                        state
                            .shared_mut()
                            .resolve_finding(finding)
                            .map_err(|e| channel_error(&space.id, e))
                    });
                });
            });

        let detail = match &tab.flow_focus {
            Some(FlowFocus::Agent(id)) => graph.node(id).map(|node| {
                self.render_agent_detail(node, &graph, &live, &notes, &refusals, state, cx)
            }),
            _ => None,
        };
        let (add_helper, add_repo) = (entity.clone(), entity);
        FlowView::new(track, canvas, timeline)
            .detail(detail)
            .on_add_helper(move |kind, window, cx| {
                let _ = add_helper.update(cx, |this, cx| this.add_helper_lane(kind, window, cx));
            })
            .on_add_repo(move |window, cx| {
                let _ = add_repo.update(cx, |this, cx| this.open_add_repo_dialog(window, cx));
            })
            .into_any_element()
    }

    /// The detail drawer of `node`, the agent selected in the Flow view, wired to what its buttons do.
    #[allow(clippy::too_many_arguments)]
    fn render_agent_detail(
        &self,
        node: &xctx_graph::AgentNode,
        graph: &AgentGraph,
        live: &HashMap<AgentNodeId, PaneLive>,
        notes: &HashMap<AgentNodeId, gpui::SharedString>,
        refusals: &HashMap<AgentNodeId, String>,
        state: &SpaceGraphState,
        cx: &mut Context<Self>,
    ) -> AgentDetail {
        let theme = Theme::of(cx);
        let color = agent_identity_color(graph, node.id.as_str(), &theme).unwrap_or(theme.accent);
        let runner = node
            .runner
            .clone()
            .or_else(|| state.doc().runner.clone())
            .unwrap_or_else(|| xctx_plugin::DEFAULT_RUNNER.to_owned());
        let entity = cx.entity().downgrade();
        let (open, start, pause, resume, fork, remove, close) = (
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity,
        );
        AgentDetail::new(node.clone(), color)
            .live(live.get(&node.id).copied())
            .note(notes.get(&node.id).cloned())
            .runner(runner.into())
            .fork_allowed(node.role.is_writer() && !refusals.contains_key(&node.id))
            .on_open(move |agent, window, cx| {
                let agent = agent.clone();
                let _ = open.update(cx, |this, cx| this.talk_to_agent(&agent, window, cx));
            })
            .on_start(move |agent, window, cx| {
                let agent = agent.clone();
                let _ = start.update(cx, |this, cx| {
                    if this.dialog.is_some() {
                        return;
                    }
                    let Some(id) = this.active_space_id() else {
                        return;
                    };
                    match this.start_agent_pane(&id, &agent, window, cx) {
                        Ok(_) => this.info(format!("Started {agent} in {id}"), cx),
                        Err(e) => this.show_op_error(e, cx),
                    }
                });
            })
            .on_pause(move |agent, _, cx| {
                let agent = agent.clone();
                let _ = pause.update(cx, |this, cx| {
                    this.panel_change(cx, |state, space| {
                        set_agent_paused_and_tell(state, &space.id, agent.as_str(), true)
                    });
                });
            })
            .on_resume(move |agent, _, cx| {
                let agent = agent.clone();
                let _ = resume.update(cx, |this, cx| {
                    this.panel_change(cx, |state, space| {
                        set_agent_paused_and_tell(state, &space.id, agent.as_str(), false)
                    });
                });
            })
            .on_fork(move |agent, window, cx| {
                let agent = agent.clone();
                let _ = fork.update(cx, |this, cx| this.open_fork_dialog(agent, window, cx));
            })
            .on_remove(move |agent, _, cx| {
                let agent = agent.clone();
                let _ = remove.update(cx, |this, cx| this.remove_agent_lane(&agent, cx));
            })
            .on_close(move |_, cx| {
                let _ = close.update(cx, |this, cx| this.set_flow_focus(None, cx));
            })
    }
}
