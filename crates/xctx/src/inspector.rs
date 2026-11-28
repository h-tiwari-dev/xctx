//! The Inspector beside a space: what its four tabs show for the space being shown, and what their buttons do. The
//! panel is `xctx_ui::inspector`. This file reads the space's graph, attempts, channel and tools into it, and sends
//! each click to the code that already does that thing (pause, focus, select, fork, review, open a tool).

use gpui::{AnyElement, App, Context, Window};
use xctx_core::{MAX_ATTEMPTS, SpaceId};
use xctx_graph::{AgentRole, AgentState, LoopStatus};
use xctx_layout::PaneId;
use xctx_ui::inspector::{
    Inspector, InspectorAction, InspectorAgent, InspectorContract, InspectorData, InspectorLoop,
    InspectorTab,
};

use crate::attempt_ui::{can_review, strip_chips};
use crate::flow::pane_lives;
use crate::space_graph::{
    loop_advance, loop_error, pending_loop_approvals, set_agent_paused_and_tell,
};
use crate::workspace::Workspace;

/// Below this window width, in pixels, the Inspector overlays the panes instead of sitting beside them.
pub(crate) const OVERLAY_BELOW: f32 = 1100.0;

/// Where the Inspector panel sits: beside the panes (which shrink), or over them in a narrow window.
pub(crate) enum InspectorPlacement {
    /// Beside the panes.
    Beside(AnyElement),
    /// Over the panes.
    Overlay(AnyElement),
}

impl Workspace {
    /// The Inspector panel for the space `id`, when the Inspector is open.
    pub(crate) fn inspector_panel(
        &mut self,
        id: &SpaceId,
        cx: &mut Context<Self>,
    ) -> Option<Inspector> {
        if !self.prefs.inspector_open {
            return None;
        }
        let data = self.inspector_data(id, cx)?;
        let entity = cx.entity().downgrade();
        Some(Inspector::new(data).on_action(move |action, window, cx| {
            let _ = entity.update(cx, |this, cx| this.inspector_action(action, window, cx));
        }))
    }

    /// What the Inspector's tabs show for the space `id`: its graph's agents, its attempts, its channel and its tools.
    fn inspector_data(&mut self, id: &SpaceId, cx: &App) -> Option<InspectorData> {
        // The dock's tools come first: they need `&mut self`, the rest is read below.
        let tools = self.tool_dock_view(id).items;
        let tab = self.tabs.get(id)?;
        let mut data = InspectorData {
            tab: self.inspector_tab,
            agents: Vec::new(),
            work_loop: None,
            attempts: Vec::new(),
            can_fork: false,
            can_review: false,
            contract: None,
            open_findings: 0,
            notes: 0,
            tools,
        };
        if let Some(state) = self.graphs.get(id) {
            let graph = state.graph();
            let run = state.loop_run();
            let action = match run.status() {
                LoopStatus::Idle => "Start",
                LoopStatus::Running => "Advance",
                LoopStatus::AwaitingApproval { .. } => "Approve next",
                LoopStatus::BudgetExhausted | LoopStatus::Blocked { .. } => "Resume",
                LoopStatus::Passed => "Reset",
            };
            data.work_loop = Some(InspectorLoop {
                stage: xctx_ui::loop_stage_label(run.stage()).into(),
                status: loop_status_label(run.status()).into(),
                action: action.into(),
            });
            // Agents over RPC have no terminal, so they are missing from `lives`: they show as such.
            let lives = pane_lives(graph, tab, cx);
            data.agents = graph
                .nodes()
                .iter()
                .map(|node| InspectorAgent {
                    id: node.id.as_str().to_owned().into(),
                    label: node.label.clone().into(),
                    role: role_label(&node.role).into(),
                    state: state_label(&node.state).into(),
                    pane: lives.get(&node.id).cloned(),
                    pane_id: node.pane_id,
                    paused: matches!(node.state, AgentState::Paused),
                })
                .collect();
            let shared = state.shared();
            data.contract = shared.contract(&self.redactor).map(|c| InspectorContract {
                title: c.title.into(),
                version: c.version,
                author: c.author.into(),
                summary: c.summary.into(),
            });
            data.open_findings = shared.findings(true, &self.redactor).len();
            data.notes = shared.notes(&self.redactor).len();
        }
        if let Some(set) = self.attempts.get(id) {
            data.attempts = strip_chips(set, tab.shown_attempt, &self.redactor);
            data.can_review = can_review(set);
            data.can_fork = (set.live().count() as u32) < MAX_ATTEMPTS && self.strip_can_fork(tab);
        }
        Some(data)
    }

    /// Carry out a click in the Inspector.
    pub(crate) fn inspector_action(
        &mut self,
        action: InspectorAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match action {
            InspectorAction::SelectTab(tab) => self.inspector_tab = tab,
            InspectorAction::Close => self.set_inspector_open(false, cx),
            InspectorAction::Focus(pane) => {
                self.focus_pane(PaneId(pane), window, cx);
            }
            InspectorAction::SetPaused { agent, paused } => {
                self.panel_change(cx, |state, space| {
                    set_agent_paused_and_tell(state, &space.id, agent.as_ref(), paused)
                });
            }
            InspectorAction::LoopPrimary => self.run_loop_primary(cx),
            InspectorAction::SelectAttempt(number) => self.select_attempt(Some(number), window, cx),
            InspectorAction::Fork => self.fork_from_keys(window, cx),
            InspectorAction::Review => self.open_review(window, cx),
            InspectorAction::ToggleTool { name, open } => {
                if open {
                    self.dock_close(&name, window, cx);
                } else {
                    self.dock_open(&name, window, cx);
                }
            }
        }
        cx.notify();
    }

    fn run_loop_primary(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.active_space_id() else {
            return;
        };
        let status = self
            .graphs
            .get(&id)
            .map(|state| state.loop_run().status().clone());
        match status {
            Some(LoopStatus::Idle | LoopStatus::Blocked { .. } | LoopStatus::BudgetExhausted) => {
                self.panel_change(cx, |state, space| {
                    state.change(|doc| doc.loop_run.start().map_err(|e| loop_error(&space.id, e)))
                });
            }
            Some(LoopStatus::Running) => {
                self.panel_change(cx, |state, space| {
                    loop_advance(state, &space.id, Some(true))
                });
            }
            Some(LoopStatus::AwaitingApproval { .. }) => {
                let approved = self.panel_change(cx, |state, space| {
                    state.change(|doc| {
                        doc.loop_run
                            .approve_next_stage()
                            .map_err(|e| loop_error(&space.id, e))
                    })?;
                    Ok(space.id.clone())
                });
                if let (Some(id), Some(inbox)) = (approved, self.inbox.as_mut()) {
                    for item in pending_loop_approvals(inbox, &id) {
                        if let Err(error) = inbox.dismiss(&item) {
                            self.error(format!("could not update the Inbox: {error}"), cx);
                            break;
                        }
                    }
                }
            }
            Some(LoopStatus::Passed) => {
                self.panel_change(cx, |state, _| {
                    state.change(|doc| {
                        doc.loop_run.reset();
                        Ok(())
                    })
                });
            }
            None => {}
        }
    }

    /// Open or close the Inspector, and keep the choice for the next run.
    pub(crate) fn set_inspector_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.prefs.inspector_open != open {
            self.prefs.inspector_open = open;
            self.save_prefs(cx);
        }
        cx.notify();
    }

    /// Show the Inspector on `tab`, opening it if it is closed.
    pub(crate) fn show_inspector_tab(&mut self, tab: InspectorTab, cx: &mut Context<Self>) {
        self.inspector_tab = tab;
        self.set_inspector_open(true, cx);
    }
}

/// A role as the Graph tab says it: "Orchestrator", "Writer · api", "Helper · Tests".
fn role_label(role: &AgentRole) -> String {
    match role {
        AgentRole::Orchestrator => "Orchestrator".to_owned(),
        AgentRole::Writer { repo } => format!("Writer · {repo}"),
        AgentRole::Helper { kind } => {
            format!("Helper · {}", xctx_ui::helper_kind_label(*kind))
        }
    }
}

/// An agent's graph state as the Graph tab says it.
fn state_label(state: &AgentState) -> &'static str {
    match state {
        AgentState::Idle => "idle",
        AgentState::Running => "running",
        AgentState::Paused => "paused",
        AgentState::Blocked { .. } => "blocked",
        AgentState::Done => "done",
        AgentState::Failed { .. } => "failed",
    }
}

fn loop_status_label(status: &LoopStatus) -> &'static str {
    match status {
        LoopStatus::Idle => "Not started",
        LoopStatus::Running => "Agents are working",
        LoopStatus::AwaitingApproval { .. } => "Needs your approval",
        LoopStatus::BudgetExhausted => "Budget exhausted",
        LoopStatus::Passed => "Review passed",
        LoopStatus::Blocked { .. } => "Blocked",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_and_states_are_named_for_the_graph_tab() {
        assert_eq!(role_label(&AgentRole::Orchestrator), "Orchestrator");
        assert_eq!(
            role_label(&AgentRole::Writer {
                repo: "api".to_owned()
            }),
            "Writer · api"
        );
        assert_eq!(state_label(&AgentState::Paused), "paused");
        assert_eq!(state_label(&AgentState::Idle), "idle");
    }
}
