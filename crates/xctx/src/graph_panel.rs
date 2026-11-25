//! The agent graph panel of a space tab (⌘G): the loop bar, the agent graph and the Shared Space panel, and what
//! their buttons do. Every change is saved before the panel redraws.

use gpui::{AnyElement, Context, ParentElement, Styled, Window, div, prelude::*, px};
use xctx_control::RpcError;
use xctx_core::{Space, SpaceId};
use xctx_graph::{AgentNodeId, AgentRole, AutonomyLevel, HelperKind};
use xctx_layout::{PaneId, PaneKind, RolePane, SplitAxis};
use xctx_ui::{AgentGraphView, LoopBar, SharedSpacePanel, SharedTab, Theme, helper_kind_label};

use crate::space_graph::{
    SpaceGraphState, add_role, channel_error, graph_error, loop_advance, loop_error, pane_agent,
    pending_loop_approvals, remove_lane, set_agent_paused_and_tell, sync_pane_bindings,
};
use crate::space_tab::SpaceTab;
use crate::spawn::PaneAgent;
use crate::workspace::Workspace;

/// The width of the agent graph panel.
const GRAPH_PANEL_WIDTH: f32 = 560.0;

impl Workspace {
    /// ⌘G or the tab bar's Graph button: open or close the agent graph panel of the space being shown. Closing
    /// it gives the focused pane its keyboard back.
    pub(crate) fn toggle_graph(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_space_id() else {
            return;
        };
        let Some(tab) = self.tabs.get_mut(&id) else {
            return;
        };
        tab.graph_open = !tab.graph_open;
        if tab.graph_open {
            let space = tab.space.clone();
            if let Err(e) = self.graphs.load(&space) {
                self.error(e, cx);
            }
            self.sync_agent_panes(&id);
        } else {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
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

    /// Talk: focus the pane of `agent` and close the panel so it has the keyboard.
    fn talk_to_agent(&mut self, agent: &AgentNodeId, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        let Some(id) = self.active_space_id() else {
            return;
        };
        let pane = self
            .graphs
            .get(&id)
            .and_then(|state| state.graph().node(agent))
            .and_then(|node| node.pane_id);
        let Some(pane) = pane else {
            self.info(format!("{agent} has no pane to talk to"), cx);
            return;
        };
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.graph_open = false;
        }
        if let Err(e) = self.focus_pane_in(&id, PaneId(pane), window, cx) {
            self.show_op_error(e, cx);
        }
    }

    /// Add a helper of `kind` to the graph and split a new agent pane, in the space's first repo, that runs as it.
    /// The pane always starts a fresh `pi` session (see [`Workspace::split_for_agent`]), never resuming another
    /// agent's. When the pane cannot be split, the helper is taken out of the graph again so no lane is left
    /// without a pane.
    fn add_helper_lane(&mut self, kind: HelperKind, window: &mut Window, cx: &mut Context<Self>) {
        let Some(added) = self.panel_change(cx, |state, space| {
            let lane = add_role(state, &space.id, AgentRole::Helper { kind }, None)?;
            Ok((
                space.id.clone(),
                space.repos.first().map(|r| r.name.clone()),
                lane,
            ))
        }) else {
            return;
        };
        let (id, repo, lane) = added;
        let agent = AgentNodeId::new(lane.id);
        match self.split_for_agent(
            &id,
            None,
            SplitAxis::Horizontal,
            PaneKind::Agent,
            repo,
            Some(agent.clone()),
            window,
            cx,
        ) {
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

    /// The agent graph panel of `tab`.
    pub(crate) fn render_graph_panel(&self, tab: &SpaceTab, cx: &mut Context<Self>) -> AnyElement {
        let theme = Theme::of(cx);
        let panel = div()
            .id("graph-panel")
            .flex()
            .flex_col()
            .flex_none()
            .w(px(GRAPH_PANEL_WIDTH))
            .h_full()
            .min_h_0()
            .overflow_y_scroll()
            .gap(Theme::SPACE_3)
            .p(Theme::SPACE_3)
            .border_l_1()
            .border_color(theme.border)
            .bg(theme.surface);
        let Some(state) = self.graphs.get(&tab.space.id) else {
            let message = self
                .graphs
                .error(&tab.space.id)
                .map(|e| format!("The agent graph could not be opened: {e}"))
                .unwrap_or_else(|| "The agent graph is not loaded.".to_owned());
            return panel
                .child(
                    div()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.danger)
                        .child(message),
                )
                .into_any_element();
        };
        let entity = cx.entity().downgrade();

        let (start, advance, approve, reset, autonomy) = (
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
        );
        let loop_bar = LoopBar::new(state.loop_run().clone())
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
            });

        let (pause, resume, talk, remove, add, fork) = (
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
        );
        let refusals = self.fork_refusals_in(state);
        let mut notes = self.attempt_notes(&tab.space.id);
        for (agent, why) in &refusals {
            let note = match notes.get(agent) {
                Some(lineage) => format!("{lineage} · no attempts: {why}"),
                None => format!("no attempts: {why}"),
            };
            notes.insert(agent.clone(), note.into());
        }
        let graph = AgentGraphView::new(state.graph().clone())
            .notes(notes)
            .fork_hidden(refusals.into_keys().collect())
            .on_fork(move |agent, window, cx| {
                let agent = agent.clone();
                let _ = fork.update(cx, |this, cx| this.open_fork_dialog(agent, window, cx));
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
            .on_talk(move |agent, window, cx| {
                let agent = agent.clone();
                let _ = talk.update(cx, |this, cx| this.talk_to_agent(&agent, window, cx));
            })
            .on_remove(move |agent, _, cx| {
                let agent = agent.clone();
                let _ = remove.update(cx, |this, cx| this.remove_agent_lane(&agent, cx));
            })
            .on_add_helper(move |kind, window, cx| {
                let _ = add.update(cx, |this, cx| this.add_helper_lane(kind, window, cx));
            });

        let (select_tab, resolve) = (entity.clone(), entity);
        let shared = state.shared();
        let shared_panel = SharedSpacePanel::new()
            .tab(tab.shared_tab)
            .contract(shared.contract(&self.redactor))
            .messages(shared.list_messages(None, None, None, None, &self.redactor))
            .findings(shared.findings(false, &self.redactor))
            .notes(shared.notes(&self.redactor))
            .on_tab(move |shared_tab: SharedTab, _, cx| {
                let _ = select_tab.update(cx, |this, cx| {
                    if let Some(id) = this.active_space_id()
                        && let Some(tab) = this.tabs.get_mut(&id)
                    {
                        tab.shared_tab = shared_tab;
                        cx.notify();
                    }
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

        panel
            .child(loop_bar)
            .child(graph)
            .child(shared_panel)
            .into_any_element()
    }
}
