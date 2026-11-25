//! "+ Agent" in a space: bring back an agent whose pane was closed, or add a helper. Closing an agent's pane keeps
//! the agent in the space's graph without a pane; this dialog lists those agents with Start (a new pane, given the
//! agent's brief, in its repo) and the helper kinds that can be added.

use gpui::{AnyElement, Context, ParentElement, Styled, Window, div, prelude::*};
use xctx_core::SpaceId;
use xctx_graph::{AgentNodeId, AgentRole};
use xctx_ui::{
    Button, ButtonSize, ButtonVariant, HELPER_KINDS, Theme, helper_kind_label, helper_kind_name,
};

use crate::dialogs::Dialog;
use crate::workspace::Workspace;

/// The open "+ Agent" dialog of a space.
pub(crate) struct AddAgentState {
    /// The space agents are added to.
    pub(crate) space: SpaceId,
}

impl Workspace {
    /// Open the "+ Agent" dialog for the space being shown, loading its graph. Refused while another dialog's job
    /// runs.
    pub(crate) fn open_add_agent_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_space_id() else {
            return;
        };
        if self.dialog.as_ref().is_some_and(Dialog::is_running) {
            return;
        }
        if let Some(space) = self.tabs.get(&id).map(|tab| tab.space.clone())
            && let Err(e) = self.graphs.load(&space)
        {
            self.error(e, cx);
            return;
        }
        self.sync_agent_panes(&id);
        self.dialog = Some(Dialog::AddAgent(AddAgentState { space: id }));
        self.dialog_subscription = None;
        self.dialog_focus.focus(window, cx);
        cx.notify();
    }

    /// Close the dialog, then start `agent`'s pane in space `id`.
    fn start_from_dialog(
        &mut self,
        id: &SpaceId,
        agent: &AgentNodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.dismiss_dialog(window, cx);
        match self.start_agent_pane(id, agent, window, cx) {
            Ok(_) => self.info(format!("Started {agent} in {id}"), cx),
            Err(e) => self.show_op_error(e, cx),
        }
    }

    /// The "+ Agent" dialog's panel: the agents with no pane, each with Start, then a button per helper kind.
    pub(crate) fn render_add_agent(
        &self,
        state: &AddAgentState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx);
        let id = state.space.clone();
        let stopped: Vec<(AgentNodeId, String)> = self
            .graphs
            .get(&id)
            .map(|graph| {
                graph
                    .graph()
                    .nodes()
                    .iter()
                    .filter(|node| node.pane_id.is_none())
                    .map(|node| {
                        let role = match &node.role {
                            AgentRole::Orchestrator => "orchestrator".to_owned(),
                            AgentRole::Writer { repo } => format!("writer of {repo}"),
                            AgentRole::Helper { kind } => {
                                format!("{} helper", helper_kind_label(*kind))
                            }
                        };
                        (node.id.clone(), format!("{} · {role}", node.label))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut panel = crate::dialogs::panel(&theme)
            .child(crate::dialogs::heading(format!("Add an agent to {id}")))
            .child(crate::dialogs::text(
                "An agent whose pane was closed is still part of the space: Start opens a new pane for it in its \
                 repo, with its role's brief. A helper is a new agent that reads every repo and edits none."
                    .to_owned(),
                &theme,
            ));
        if stopped.is_empty() {
            panel = panel.child(crate::dialogs::text(
                "Every agent of the space has a pane.".to_owned(),
                &theme,
            ));
        }
        for (index, (agent, label)) in stopped.into_iter().enumerate() {
            let space = id.clone();
            panel = panel.child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(div().flex_1().min_w_0().truncate().child(label))
                    .child(
                        Button::new(("add-agent-start", index), "Start")
                            .variant(ButtonVariant::Primary)
                            .size(ButtonSize::Compact)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_from_dialog(&space, &agent, window, cx)
                            })),
                    ),
            );
        }
        let mut helpers = div().flex().flex_wrap().gap(Theme::SPACE_2);
        for kind in HELPER_KINDS {
            helpers = helpers.child(
                Button::new(
                    gpui::SharedString::from(format!("add-agent-{}", helper_kind_name(kind))),
                    format!("+ {}", helper_kind_label(kind)),
                )
                .variant(ButtonVariant::Secondary)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.dismiss_dialog(window, cx);
                    this.add_helper_lane(kind, window, cx);
                })),
            );
        }
        panel
            .child(crate::dialogs::text("Add a helper".to_owned(), &theme))
            .child(helpers)
            .child(
                crate::dialogs::actions().child(
                    Button::new("close-add-agent", "Close")
                        .variant(ButtonVariant::Ghost)
                        .on_click(
                            cx.listener(|this, _, window, cx| this.dismiss_dialog(window, cx)),
                        ),
                ),
            )
            .into_any_element()
    }
}
