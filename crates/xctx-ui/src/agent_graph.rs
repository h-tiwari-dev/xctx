//! The Agent Graph panel: a space's orchestrator, its writers (one per repo) and its read-only helpers, each as a
//! card with its state, turns used and the actions it allows (Pause or Resume, Talk, Remove).
//!
//! The panel draws an [`AgentGraph`] the app passes in and reports what the user did through callbacks with the
//! agent's [`AgentNodeId`] (or the [`HelperKind`] to add). It keeps no state.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use gpui::{App, FontWeight, SharedString, Window, div, prelude::*, px};
use xctx_graph::{AgentGraph, AgentNode, AgentNodeId, AgentRole, AgentState, EdgeKind, HelperKind};

use crate::components::{Badge, Button, ButtonSize, ButtonVariant};
use crate::theme::{Theme, Tone};

/// Every helper kind, in the order the panel's add buttons show them.
pub const HELPER_KINDS: [HelperKind; 4] = [
    HelperKind::Tests,
    HelperKind::Reviewer,
    HelperKind::Probe,
    HelperKind::Browser,
];

/// The name of a helper kind as the panel shows it: `Tests`, `Reviewer`, `Probe` or `Browser`.
pub fn helper_kind_label(kind: HelperKind) -> &'static str {
    match kind {
        HelperKind::Tests => "Tests",
        HelperKind::Reviewer => "Reviewer",
        HelperKind::Probe => "Probe",
        HelperKind::Browser => "Browser",
    }
}

/// A short lowercase name for a helper kind, used in test selectors: `graph-add-<name>`.
pub fn helper_kind_name(kind: HelperKind) -> &'static str {
    match kind {
        HelperKind::Tests => "tests",
        HelperKind::Reviewer => "reviewer",
        HelperKind::Probe => "probe",
        HelperKind::Browser => "browser",
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

/// What an agent's pane is doing, as the window sees its terminal. Agents report their graph state through
/// `xctx-ctl`; runners that never do (omp, claude) would otherwise stay IDLE whatever they do, so a card whose graph
/// state is still [`AgentState::Idle`] shows this instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaneLive {
    /// Its program runs.
    Running,
    /// Its program exited, with its exit code when it had one.
    Exited(Option<i32>),
    /// It has no pane: Start opens one.
    NoPane,
}

impl PaneLive {
    /// The badge text.
    pub fn label(self) -> &'static str {
        match self {
            PaneLive::Running => "LIVE",
            PaneLive::Exited(_) => "EXITED",
            PaneLive::NoPane => "NOT STARTED",
        }
    }

    /// The badge and dot tone.
    pub fn tone(self) -> Tone {
        match self {
            PaneLive::Running => Tone::Success,
            PaneLive::Exited(_) => Tone::Warning,
            PaneLive::NoPane => Tone::Neutral,
        }
    }
}

/// The badge an agent's card shows: its graph state, unless that is still [`AgentState::Idle`] (nothing was
/// reported) and the window knows what its pane does.
pub fn card_badge(state: &AgentState, live: Option<PaneLive>) -> (&'static str, Tone) {
    match (state, live) {
        (AgentState::Idle, Some(live)) => (live.label(), live.tone()),
        _ => (agent_state_label(state), agent_state_tone(state)),
    }
}

type HelperHandler = Rc<dyn Fn(HelperKind, &mut Window, &mut App)>;
type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The handlers shared by every card.
#[derive(Clone, Default)]
struct CardHandlers {
    on_pause: Option<NodeHandler>,
    on_resume: Option<NodeHandler>,
    on_talk: Option<NodeHandler>,
    on_start: Option<NodeHandler>,
    on_remove: Option<NodeHandler>,
    on_fork: Option<NodeHandler>,
    /// Writers whose "Fork…" is hidden (their runner cannot fork sessions).
    no_fork: HashSet<AgentNodeId>,
    /// What each agent's pane does, by agent.
    live: HashMap<AgentNodeId, PaneLive>,
}

/// The Agent Graph panel: Orchestrator on top, Writers in the middle, Helpers and the add-helper buttons below.
///
/// Test selectors: `graph-pause-<id>`, `graph-resume-<id>`, `graph-talk-<id>`, `graph-start-<id>`,
/// `graph-remove-<id>` on each card's buttons, `graph-add-<kind>` ([`helper_kind_name`]) on the add-helper buttons
/// and `graph-add-repo` on the writers' "+ Repo". Open (`graph-talk-`) appears only for an agent bound to a pane,
/// Start only for one without, and Remove only for helpers: a writer belongs to one of the space's repos and the
/// panel cannot add it back. `graph-fork-<id>` is the "Fork…" button on writers, shown with
/// [`AgentGraphView::on_fork`], and `graph-note-<id>` a card's note ([`AgentGraphView::notes`]).
#[derive(IntoElement)]
pub struct AgentGraphView {
    graph: AgentGraph,
    handlers: CardHandlers,
    on_add_helper: Option<HelperHandler>,
    on_add_repo: Option<Handler>,
    notes: HashMap<AgentNodeId, SharedString>,
}

impl AgentGraphView {
    /// A panel drawing `graph`, with no handlers.
    pub fn new(graph: AgentGraph) -> Self {
        Self {
            graph,
            handlers: CardHandlers::default(),
            on_add_helper: None,
            on_add_repo: None,
            notes: HashMap::new(),
        }
    }

    /// Show a "Fork…" button on each writer's card and call `handler` with the writer's id when it is clicked.
    pub fn on_fork(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.handlers.on_fork = Some(Rc::new(handler));
        self
    }

    /// Hide "Fork…" on these writers' cards, for example because their runner cannot fork sessions (say why
    /// in their [`AgentGraphView::notes`]).
    pub fn fork_hidden(mut self, writers: HashSet<AgentNodeId>) -> Self {
        self.handlers.no_fork = writers;
        self
    }

    /// A small line of text under each named agent's card details, such as a writer's attempt lineage
    /// ("attempts: #1 adopted, #2 discarded").
    pub fn notes(mut self, notes: HashMap<AgentNodeId, SharedString>) -> Self {
        self.notes = notes;
        self
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

    /// What each agent's pane does; a card whose graph state is still IDLE shows it instead (see
    /// [`card_badge`]).
    pub fn live(mut self, live: HashMap<AgentNodeId, PaneLive>) -> Self {
        self.handlers.live = live;
        self
    }

    /// Call `handler` with an agent's id when its Open button is clicked, to focus the agent's pane.
    pub fn on_talk(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.handlers.on_talk = Some(Rc::new(handler));
        self
    }

    /// Show Start on each agent with no pane and call `handler` with its id when it is clicked, to open one.
    pub fn on_start(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.handlers.on_start = Some(Rc::new(handler));
        self
    }

    /// Show "+ Repo" beside the writers and call `handler` when it is clicked, to add a repo to the space.
    pub fn on_add_repo(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_add_repo = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with a helper's id when its Remove button is clicked.
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
    note: Option<&SharedString>,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let (state_label, tone) = card_badge(&node.state, handlers.live.get(&node.id).copied());
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
    // Turns are only counted by the loop; a card with none says nothing rather than "0 turns".
    let turns = match node.turns_used {
        0 => None,
        1 => Some("1 turn".to_owned()),
        n => Some(format!("{n} turns")),
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
            "Open",
            node,
            ButtonVariant::Secondary,
            handlers.on_talk.as_ref(),
        )
    });
    let start = (node.pane_id.is_none() && handlers.on_start.is_some()).then(|| {
        card_button(
            "start",
            "Start",
            node,
            ButtonVariant::Primary,
            handlers.on_start.as_ref(),
        )
    });
    let fork = (node.role.is_writer()
        && handlers.on_fork.is_some()
        && !handlers.no_fork.contains(&node.id))
    .then(|| {
        card_button(
            "fork",
            "Fork…",
            node,
            ButtonVariant::Ghost,
            handlers.on_fork.as_ref(),
        )
    });
    let note = note.cloned().map(|note| {
        let selector = format!("graph-note-{}", node.id);
        div()
            .debug_selector(move || selector)
            .min_w_0()
            .truncate()
            .text_size(Theme::TEXT_XS)
            .text_color(theme.text_subtle)
            .child(note)
    });
    let remove = matches!(node.role, AgentRole::Helper { .. }).then(|| {
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
                .child(Badge::new(state_label).tone(tone)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(role_badge)
                .children(turns.map(|turns| {
                    div()
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_subtle)
                        .child(turns)
                })),
        )
        .children(reason)
        .children(note)
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_1)
                .pt(Theme::SPACE_0_5)
                .children(pause)
                .children(resume)
                .children(start)
                .children(talk)
                .children(fork)
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
            .map(|node| agent_card(node, handlers, self.notes.get(&node.id), &theme));

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
            writer_row.push(
                agent_card(node, handlers, self.notes.get(&node.id), &theme).into_any_element(),
            );
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
            .map(|node| agent_card(node, handlers, self.notes.get(&node.id), &theme))
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
        let add_repo = self.on_add_repo.clone().map(|handler| {
            Button::new("graph-add-repo", "+ Repo")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .selector("graph-add-repo")
                .on_click(move |_, window, cx| handler(window, cx))
        });

        let writers_body = if writer_row.is_empty() {
            empty_line("No writers yet. Each repo in the space gets one.", &theme)
        } else {
            div()
                .flex()
                .flex_wrap()
                // Cards differ in height (a note, a turn count); their tops line up.
                .items_start()
                .gap(Theme::SPACE_1)
                .children(writer_row)
        };
        let helpers_body = if helpers.is_empty() {
            empty_line(
                "No helpers. Add one to test, review, probe or check the app in the browser.",
                &theme,
            )
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
                                    .children(contract_badge)
                                    .child(div().flex_1())
                                    .children(add_repo),
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
        assert_eq!(labels, ["Tests", "Reviewer", "Probe", "Browser"]);
        let names: Vec<_> = HELPER_KINDS.into_iter().map(helper_kind_name).collect();
        assert_eq!(names, ["tests", "reviewer", "probe", "browser"]);
    }

    #[test]
    fn an_unreported_agent_shows_its_pane_and_a_reported_one_its_state() {
        assert_eq!(
            card_badge(&AgentState::Idle, Some(PaneLive::Running)),
            ("LIVE", Tone::Success)
        );
        assert_eq!(
            card_badge(&AgentState::Idle, Some(PaneLive::NoPane)).0,
            "NOT STARTED"
        );
        assert_eq!(card_badge(&AgentState::Idle, None).0, "IDLE");
        assert_eq!(
            card_badge(&AgentState::Paused, Some(PaneLive::Running)).0,
            "PAUSED"
        );
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
