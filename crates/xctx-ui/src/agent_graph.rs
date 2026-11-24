//! The Agent Graph panel: a space's orchestrator, its writers (one per repo) and its read-only helpers, each as a
//! card with its state, turns used and the actions it allows (Pause or Resume, Talk, Remove).
//!
//! The panel draws an [`AgentGraph`] the app passes in and reports what the user did through callbacks with the
//! agent's [`AgentNodeId`] (or the [`HelperKind`] to add). It keeps no state.

use std::rc::Rc;

use gpui::{App, FontWeight, SharedString, Window, div, prelude::*, px};
use xctx_graph::{AgentGraph, AgentNode, AgentNodeId, AgentRole, AgentState, EdgeKind, HelperKind};

use crate::components::{Badge, Button, ButtonSize, ButtonVariant};
use crate::theme::{Theme, Tone};

/// Every helper kind, in the order the panel's add buttons show them.
pub const HELPER_KINDS: [HelperKind; 3] =
    [HelperKind::Tests, HelperKind::Reviewer, HelperKind::Probe];

/// The name of a helper kind as the panel shows it: `Tests`, `Reviewer` or `Probe`.
pub fn helper_kind_label(kind: HelperKind) -> &'static str {
    match kind {
        HelperKind::Tests => "Tests",
        HelperKind::Reviewer => "Reviewer",
        HelperKind::Probe => "Probe",
    }
}

/// A short lowercase name for a helper kind, used in test selectors: `graph-add-<name>`.
pub fn helper_kind_name(kind: HelperKind) -> &'static str {
    match kind {
        HelperKind::Tests => "tests",
        HelperKind::Reviewer => "reviewer",
        HelperKind::Probe => "probe",
    }
}

/// The badge text of an agent state.
pub fn agent_state_label(state: &AgentState) -> &'static str {
    match state {
        AgentState::Idle => "IDLE",
        AgentState::Running => "RUNNING",
        AgentState::Paused => "PAUSED",
        AgentState::Blocked { .. } => "BLOCKED",
        AgentState::Done => "DONE",
        AgentState::Failed { .. } => "FAILED",
    }
}

/// The badge and dot tone of an agent state.
pub fn agent_state_tone(state: &AgentState) -> Tone {
    match state {
        AgentState::Idle => Tone::Neutral,
        AgentState::Running => Tone::Success,
        AgentState::Paused | AgentState::Blocked { .. } => Tone::Warning,
        AgentState::Done => Tone::Accent,
        AgentState::Failed { .. } => Tone::Danger,
    }
}

/// Whether the panel offers Pause for an agent in `state`: the states [`AgentGraph::pause`] accepts and changes.
pub fn can_pause(state: &AgentState) -> bool {
    matches!(
        state,
        AgentState::Idle | AgentState::Running | AgentState::Blocked { .. }
    )
}

/// Whether the panel offers Resume for an agent in `state`: the states [`AgentGraph::resume`] accepts and changes.
pub fn can_resume(state: &AgentState) -> bool {
    matches!(state, AgentState::Paused | AgentState::Blocked { .. })
}

type NodeHandler = Rc<dyn Fn(&AgentNodeId, &mut Window, &mut App)>;
type HelperHandler = Rc<dyn Fn(HelperKind, &mut Window, &mut App)>;

/// The handlers shared by every card.
#[derive(Clone, Default)]
struct CardHandlers {
    on_pause: Option<NodeHandler>,
    on_resume: Option<NodeHandler>,
    on_talk: Option<NodeHandler>,
    on_remove: Option<NodeHandler>,
}

/// The Agent Graph panel: Orchestrator on top, Writers in the middle, Helpers and the add-helper buttons below.
///
/// Test selectors: `graph-pause-<id>`, `graph-resume-<id>`, `graph-talk-<id>`, `graph-remove-<id>` on each
/// card's buttons and `graph-add-<kind>` ([`helper_kind_name`]) on the add buttons. Talk appears only for an
/// agent bound to a pane, and Remove only for writers and helpers.
#[derive(IntoElement)]
pub struct AgentGraphView {
    graph: AgentGraph,
    handlers: CardHandlers,
    on_add_helper: Option<HelperHandler>,
}

impl AgentGraphView {
    /// A panel drawing `graph`, with no handlers.
    pub fn new(graph: AgentGraph) -> Self {
        Self {
            graph,
            handlers: CardHandlers::default(),
            on_add_helper: None,
        }
    }

    /// Call `handler` with an agent's id when its Pause button is clicked.
    pub fn on_pause(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.handlers.on_pause = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with an agent's id when its Resume button is clicked.
    pub fn on_resume(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.handlers.on_resume = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with an agent's id when its Talk button is clicked, to focus the agent's pane.
    pub fn on_talk(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.handlers.on_talk = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with a writer's or helper's id when its Remove button is clicked.
    pub fn on_remove(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.handlers.on_remove = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the helper kind of the add button clicked.
    pub fn on_add_helper(
        mut self,
        handler: impl Fn(HelperKind, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_add_helper = Some(Rc::new(handler));
        self
    }
}

/// The small uppercase heading of a tier.
fn tier_heading(text: &'static str, theme: &Theme) -> gpui::Div {
    div()
        .text_size(Theme::TEXT_XS)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.text_subtle)
        .child(text)
}

/// A card's action button, wired to `handler` with the card's id.
fn card_button(
    action: &'static str,
    label: &'static str,
    node: &AgentNode,
    variant: ButtonVariant,
    handler: Option<&NodeHandler>,
) -> Button {
    let selector = format!("graph-{action}-{}", node.id);
    let mut button = Button::new(SharedString::from(selector.clone()), label)
        .variant(variant)
        .size(ButtonSize::Compact)
        .selector(selector);
    if let Some(handler) = handler.cloned() {
        let id = node.id.clone();
        button = button.on_click(move |_, window, cx| handler(&id, window, cx));
    }
    button
}

fn agent_card(
    node: &AgentNode,
    handlers: &CardHandlers,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let tone = agent_state_tone(&node.state);
    let role_badge = match &node.role {
        AgentRole::Orchestrator => Badge::new("ORCHESTRATOR").tone(Tone::Accent),
        AgentRole::Writer { repo } => Badge::new(repo.clone()).tone(Tone::Accent),
        AgentRole::Helper { kind } => Badge::new(helper_kind_label(*kind).to_uppercase()),
    };
    let reason = match &node.state {
        AgentState::Blocked { reason } => Some((reason.clone(), theme.warning)),
        AgentState::Failed { reason } => Some((reason.clone(), theme.danger)),
        _ => None,
    }
    .map(|(reason, color)| {
        div()
            .min_w_0()
            .truncate()
            .text_size(Theme::TEXT_XS)
            .text_color(color)
            .child(reason)
    });
    let turns = if node.turns_used == 1 {
        "1 turn".to_owned()
    } else {
        format!("{} turns", node.turns_used)
    };

    let pause = can_pause(&node.state).then(|| {
        card_button(
            "pause",
            "Pause",
            node,
            ButtonVariant::Secondary,
            handlers.on_pause.as_ref(),
        )
    });
    let resume = can_resume(&node.state).then(|| {
        card_button(
            "resume",
            "Resume",
            node,
            ButtonVariant::Secondary,
            handlers.on_resume.as_ref(),
        )
    });
    let talk = node.pane_id.is_some().then(|| {
        card_button(
            "talk",
            "Talk",
            node,
            ButtonVariant::Ghost,
            handlers.on_talk.as_ref(),
        )
    });
    let remove = (!matches!(node.role, AgentRole::Orchestrator)).then(|| {
        card_button(
            "remove",
            "Remove",
            node,
            ButtonVariant::Danger,
            handlers.on_remove.as_ref(),
        )
    });

    div()
        .id(SharedString::from(format!("graph-card-{}", node.id)))
        .debug_selector(|| format!("graph-card-{}", node.id))
        .flex()
        .flex_col()
        .flex_none()
        .gap(Theme::SPACE_1)
        .w(px(196.0))
        .p(Theme::SPACE_2)
        .rounded(Theme::RADIUS_MD)
        .border_1()
        .border_color(theme.border)
        .bg(theme.surface)
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_1 + px(2.0))
                .child(
                    div()
                        .flex_none()
                        .size(px(7.0))
                        .rounded_full()
                        .bg(theme.tone(tone).fg),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(Theme::TEXT_MD)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child(node.label.clone()),
                )
                .child(Badge::new(agent_state_label(&node.state)).tone(tone)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(role_badge)
                .child(
                    div()
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_subtle)
                        .child(turns),
                ),
        )
        .children(reason)
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_1)
                .pt(Theme::SPACE_0_5)
                .children(pause)
                .children(resume)
                .children(talk)
                .child(div().flex_1())
                .children(remove),
        )
}

/// The link drawn between two writer cards that share a contract.
fn contract_link(theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(Theme::SPACE_0_5)
        .child(div().w(px(10.0)).h(px(1.0)).bg(theme.accent.alpha(0.55)))
        .child(
            div()
                .text_size(Theme::TEXT_XS)
                .text_color(theme.accent)
                .child("⇄"),
        )
        .child(div().w(px(10.0)).h(px(1.0)).bg(theme.accent.alpha(0.55)))
}

fn empty_line(text: &'static str, theme: &Theme) -> gpui::Div {
    div()
        .py(Theme::SPACE_1)
        .text_size(Theme::TEXT_SM)
        .text_color(theme.text_muted)
        .child(text)
}

impl RenderOnce for AgentGraphView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let handlers = &self.handlers;

        let orchestrator = self
            .graph
            .orchestrator()
            .map(|node| agent_card(node, handlers, &theme));

        let writers: Vec<&AgentNode> = self.graph.writers().collect();
        let contract_links = self
            .graph
            .edges()
            .iter()
            .filter(|edge| edge.kind == EdgeKind::Contract)
            .count();
        let mut writer_row = Vec::new();
        for (index, node) in writers.iter().enumerate() {
            if index > 0 && contract_links > 0 {
                writer_row.push(contract_link(&theme).into_any_element());
            }
            writer_row.push(agent_card(node, handlers, &theme).into_any_element());
        }
        let contract_badge = (writers.len() > 1 && contract_links > 0).then(|| {
            let text = if contract_links == 1 {
                "1 contract link".to_owned()
            } else {
                format!("{contract_links} contract links")
            };
            Badge::new(text).tone(Tone::Accent)
        });

        let helpers: Vec<_> = self
            .graph
            .helpers()
            .map(|node| agent_card(node, handlers, &theme))
            .collect();
        let add_buttons = HELPER_KINDS.into_iter().map(|kind| {
            let selector = format!("graph-add-{}", helper_kind_name(kind));
            let mut button = Button::new(
                SharedString::from(selector.clone()),
                format!("+ {}", helper_kind_label(kind)),
            )
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Compact)
            .selector(selector);
            if let Some(handler) = self.on_add_helper.clone() {
                button = button.on_click(move |_, window, cx| handler(kind, window, cx));
            }
            button
        });

        let writers_body = if writer_row.is_empty() {
            empty_line("No writers yet. Each repo in the space gets one.", &theme)
        } else {
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(Theme::SPACE_1)
                .children(writer_row)
        };
        let helpers_body = if helpers.is_empty() {
            empty_line("No helpers. Add one to test, review or probe.", &theme)
        } else {
            div()
                .flex()
                .flex_wrap()
                .gap(Theme::SPACE_2)
                .children(helpers)
        };

        div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_1)
            .w_full()
            .child(
                div()
                    .text_size(Theme::TEXT_LG)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child("Agent Graph"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_3)
                    .p(Theme::SPACE_3)
                    .rounded(Theme::RADIUS_MD)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.elevated)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(Theme::SPACE_1)
                            .child(tier_heading("ORCHESTRATOR", &theme))
                            .children(orchestrator),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(Theme::SPACE_1)
                            .pt(Theme::SPACE_2)
                            .border_t_1()
                            .border_color(theme.divider)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(Theme::SPACE_2)
                                    .child(tier_heading("WRITERS", &theme))
                                    .children(contract_badge),
                            )
                            .child(writers_body),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(Theme::SPACE_1)
                            .pt(Theme::SPACE_2)
                            .border_t_1()
                            .border_color(theme.divider)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(Theme::SPACE_2)
                                    .child(tier_heading("HELPERS", &theme))
                                    .child(div().flex_1())
                                    .children(add_buttons),
                            )
                            .child(helpers_body),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_and_resume_follow_the_graph_transitions() {
        let blocked = AgentState::Blocked {
            reason: "contract".into(),
        };
        let failed = AgentState::Failed {
            reason: "crash".into(),
        };
        assert!(can_pause(&AgentState::Idle) && !can_resume(&AgentState::Idle));
        assert!(can_pause(&AgentState::Running) && !can_resume(&AgentState::Running));
        assert!(!can_pause(&AgentState::Paused) && can_resume(&AgentState::Paused));
        assert!(can_pause(&blocked) && can_resume(&blocked));
        for state in [AgentState::Done, failed] {
            assert!(!can_pause(&state) && !can_resume(&state));
        }
    }

    #[test]
    fn helper_kinds_are_listed_once_with_labels() {
        let labels: Vec<_> = HELPER_KINDS.into_iter().map(helper_kind_label).collect();
        assert_eq!(labels, ["Tests", "Reviewer", "Probe"]);
        let names: Vec<_> = HELPER_KINDS.into_iter().map(helper_kind_name).collect();
        assert_eq!(names, ["tests", "reviewer", "probe"]);
    }

    #[test]
    fn failures_are_danger_and_running_is_success() {
        assert_eq!(agent_state_tone(&AgentState::Running), Tone::Success);
        assert_eq!(
            agent_state_tone(&AgentState::Failed { reason: "x".into() }),
            Tone::Danger
        );
        assert_eq!(agent_state_label(&AgentState::Paused), "PAUSED");
    }
}
