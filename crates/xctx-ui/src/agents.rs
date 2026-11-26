//! How an agent of a space's graph is labelled and coloured wherever it is drawn (the Flow view, the Roster, Home,
//! pane headers): helper kind names, state badges and tones, what its pane is doing, which of Pause and Resume
//! apply, and its identity colour.

use xctx_graph::{AgentGraph, AgentRole, AgentState, HelperKind};

use crate::theme::{Theme, Tone};

/// Every helper kind, in the order add-helper buttons show them.
pub const HELPER_KINDS: [HelperKind; 4] = [
    HelperKind::Tests,
    HelperKind::Reviewer,
    HelperKind::Probe,
    HelperKind::Browser,
];

/// The name of a helper kind as buttons and cards show it: `Tests`, `Reviewer`, `Probe` or `Browser`.
pub fn helper_kind_label(kind: HelperKind) -> &'static str {
    match kind {
        HelperKind::Tests => "Tests",
        HelperKind::Reviewer => "Reviewer",
        HelperKind::Probe => "Probe",
        HelperKind::Browser => "Browser",
    }
}

/// A short lowercase name for a helper kind, used in test selectors such as `flow-add-<name>`.
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

/// Whether Pause applies to an agent in `state`: the states [`AgentGraph::pause`] accepts and changes.
pub fn can_pause(state: &AgentState) -> bool {
    matches!(
        state,
        AgentState::Idle | AgentState::Running | AgentState::Blocked { .. }
    )
}

/// Whether Resume applies to an agent in `state`: the states [`AgentGraph::resume`] accepts and changes.
pub fn can_resume(state: &AgentState) -> bool {
    matches!(state, AgentState::Paused | AgentState::Blocked { .. })
}

/// The identity colour for an agent in `graph` (matched by id or label): [`Theme::accent`] for the orchestrator,
/// or [`Theme::agent_color`] indexed by its position among non-orchestrator nodes in creation order. `None` when
/// no agent of `graph` has that id or label.
pub fn agent_identity_color(
    graph: &AgentGraph,
    agent_id: &str,
    theme: &Theme,
) -> Option<gpui::Hsla> {
    let mut non_orch = 0usize;
    for node in graph.nodes() {
        let is_orch = matches!(node.role, AgentRole::Orchestrator);
        if node.id.as_str() == agent_id || node.label == agent_id {
            return Some(if is_orch {
                theme.accent
            } else {
                theme.agent_color(non_orch)
            });
        }
        if !is_orch {
            non_orch += 1;
        }
    }
    None
}

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

/// The badge an agent shows: its graph state, unless that is still [`AgentState::Idle`] (nothing was reported)
/// and the window knows what its pane does.
pub fn card_badge(state: &AgentState, live: Option<PaneLive>) -> (&'static str, Tone) {
    match (state, live) {
        (AgentState::Idle, Some(live)) => (live.label(), live.tone()),
        _ => (agent_state_label(state), agent_state_tone(state)),
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
    fn identity_colours_follow_creation_order_and_the_orchestrator_is_the_accent() {
        let theme = Theme::dark();
        let graph = AgentGraph::for_space(&["api".to_owned(), "web".to_owned()]);
        let ids: Vec<String> = graph
            .nodes()
            .iter()
            .map(|n| n.id.as_str().to_owned())
            .collect();
        assert_eq!(
            agent_identity_color(&graph, &ids[0], &theme),
            Some(theme.accent)
        );
        assert_eq!(
            agent_identity_color(&graph, &ids[2], &theme),
            Some(theme.agent_color(1))
        );
        assert_eq!(agent_identity_color(&graph, "nobody", &theme), None);
    }
}
