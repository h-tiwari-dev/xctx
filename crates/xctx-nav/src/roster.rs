//! Fleet roster model: spaces grouped by rolled-up state (`Needs you`, `Running`, `Idle`) with
//! nested agent rows, deterministic agent colour indices, and `⌘1`–`⌘9` shortcut order.

use std::collections::HashMap;

use xctx_core::{Space, SpaceId, SpaceStatus};
use xctx_graph::{AgentGraph, AgentNodeId, AgentRole, AgentState, LoopRun, LoopStatus};
use xctx_inbox::{InboxItem, ItemBody, ItemKind, ItemState, PluginGateKind};
use xctx_layout::PaneId;

/// Rolled-up state of a space, ordered worst-first for rollup precedence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SpaceState {
    /// Waiting on the user: a pending Inbox gate, a missing required var, a paused agent, or a
    /// loop waiting for approval / budget.
    NeedsYou,
    /// Something failed or blocked: a blocked loop, a dead pane, or a failed/blocked agent.
    Failed,
    /// At least one agent (or the loop) is actively running.
    Running,
    /// Nothing is running and nothing needs attention.
    Idle,
}

impl SpaceState {
    /// The Roster section this state appears under.
    ///
    /// [`SpaceState::Failed`] is grouped under [`RosterGroup::NeedsYou`] so broken spaces are
    /// always visible at the top of the Roster alongside spaces waiting on user decisions.
    pub fn group(self) -> RosterGroup {
        match self {
            Self::NeedsYou | Self::Failed => RosterGroup::NeedsYou,
            Self::Running => RosterGroup::Running,
            Self::Idle => RosterGroup::Idle,
        }
    }

    /// Sort priority within its group (lower sorts first).
    fn intra_group_rank(self) -> u8 {
        match self {
            Self::NeedsYou => 0,
            Self::Failed => 1,
            Self::Running => 2,
            Self::Idle => 3,
        }
    }
}

/// Top-level Roster section, ordered from highest urgency to lowest.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RosterGroup {
    /// Spaces waiting on user input or with failures (`NeedsYou`, `Failed`).
    NeedsYou,
    /// Spaces with at least one running agent.
    Running,
    /// Quiet spaces.
    Idle,
}

impl RosterGroup {
    /// All groups in display order.
    pub const ALL: [Self; 3] = [Self::NeedsYou, Self::Running, Self::Idle];

    /// Section heading text.
    pub fn label(self) -> &'static str {
        match self {
            Self::NeedsYou => "NEEDS YOU",
            Self::Running => "RUNNING",
            Self::Idle => "IDLE",
        }
    }
}

/// Live runtime metadata for one space that is not persisted in `Space` or `AgentGraph`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpaceActivity {
    /// Whether any pane in this space has exited unexpectedly / died.
    pub has_dead_pane: bool,
    /// Unix timestamp (seconds) of the space's most recent activity; falls back to
    /// `space.created_at` when `None`.
    pub last_activity_at: Option<u64>,
    /// One-line current activity text per agent id (e.g. latest tool call or status line).
    pub agent_activity: HashMap<AgentNodeId, String>,
}

/// Everything needed to compute one [`SpaceRow`] in the [`Roster`].
pub struct SpaceInput<'a> {
    /// The persisted space metadata.
    pub space: &'a Space,
    /// The space's agent graph, if loaded.
    pub graph: Option<&'a AgentGraph>,
    /// The space's structured loop run, if any.
    pub loop_run: Option<&'a LoopRun>,
    /// Live runtime signals (dead panes, last activity timestamp, per-agent activity lines).
    pub activity: SpaceActivity,
}

/// One agent row nested under its space in the Roster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentRow {
    /// Stable agent id in the space's graph.
    pub id: AgentNodeId,
    /// Display name (e.g. `writer · api`, `Tests`).
    pub label: String,
    /// The agent's role.
    pub role: AgentRole,
    /// Stable colour slot: `None` for the orchestrator (which uses `Theme::accent`), or `Some(i)`
    /// for non-orchestrators in `AgentGraph` creation order (`Theme::agent_color(i)`).
    pub color_index: Option<usize>,
    /// Current state of the agent.
    pub state: AgentState,
    /// Single-line summary of what the agent is currently doing.
    pub activity: String,
    /// The pane hosting this agent, if mounted.
    pub pane_id: Option<PaneId>,
}

/// One space row in the Roster.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpaceRow {
    /// Stable space id (slug).
    pub id: SpaceId,
    /// Human-readable space title.
    pub title: String,
    /// Rolled-up state of the space.
    pub state: SpaceState,
    /// Roster section this space belongs to.
    pub group: RosterGroup,
    /// Whether the space is running with `MISE_ENV=production`.
    pub production: bool,
    /// Number of pending gates / blocking items for this space in the Inbox.
    pub pending_gates: usize,
    /// Whether any pending gate for this space is high-risk (production, write tool, or repo
    /// plugin trust).
    pub has_danger_gate: bool,
    /// Seconds elapsed since the space's last recorded activity (saturated at 0).
    pub age_secs: u64,
    /// Unix timestamp (seconds) of the space's last activity.
    pub last_activity_at: u64,
    /// Agents nested under this space in graph creation order.
    pub agents: Vec<AgentRow>,
}

/// The fleet Roster: active spaces grouped by state (`NeedsYou`, `Running`, `Idle`) in display
/// and `⌘1`–`⌘9` shortcut order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Roster {
    spaces: Vec<SpaceRow>,
}

impl Roster {
    /// Build a [`Roster`] from active spaces, Inbox items, and the current Unix timestamp `now`.
    ///
    /// Non-active spaces (`Shipped`, `Discarded`) are excluded. Spaces are grouped in
    /// [`RosterGroup::ALL`] order (`NeedsYou`, `Running`, `Idle`). Within `NeedsYou`, spaces with a
    /// danger gate sort first, then `NeedsYou` before `Failed`, then most-recent activity first,
    /// with `SpaceId` as a deterministic tie-breaker.
    pub fn build<'a>(
        inputs: impl IntoIterator<Item = SpaceInput<'a>>,
        inbox: &[InboxItem],
        now: u64,
    ) -> Self {
        let mut spaces: Vec<SpaceRow> = inputs
            .into_iter()
            .filter(|input| input.space.status == SpaceStatus::Active)
            .map(|input| build_space_row(input, inbox, now))
            .collect();

        spaces.sort_by(|a, b| {
            a.group
                .cmp(&b.group)
                .then_with(|| b.has_danger_gate.cmp(&a.has_danger_gate))
                .then_with(|| a.state.intra_group_rank().cmp(&b.state.intra_group_rank()))
                .then_with(|| b.last_activity_at.cmp(&a.last_activity_at))
                .then_with(|| a.id.cmp(&b.id))
        });

        Self { spaces }
    }

    /// All spaces in flat Roster order (matching `⌘1`–`⌘9`).
    pub fn spaces(&self) -> &[SpaceRow] {
        &self.spaces
    }

    /// Spaces belonging to `group`, in Roster order.
    pub fn spaces_in(&self, group: RosterGroup) -> impl Iterator<Item = &SpaceRow> {
        self.spaces.iter().filter(move |s| s.group == group)
    }

    /// The space bound to `⌘<slot>` for `slot` in `1..=9`, following Roster order.
    pub fn space_for_shortcut(&self, slot: usize) -> Option<&SpaceRow> {
        if !(1..=9).contains(&slot) {
            return None;
        }
        self.spaces.get(slot - 1)
    }

    /// 1-based `⌘1`–`⌘9` shortcut slot for `id`, if it is in the first nine spaces of the Roster.
    pub fn shortcut_for_space(&self, id: &SpaceId) -> Option<usize> {
        self.spaces
            .iter()
            .take(9)
            .position(|s| &s.id == id)
            .map(|idx| idx + 1)
    }

    /// Total number of spaces in the `NeedsYou` group (both `NeedsYou` and `Failed`).
    pub fn needs_you_count(&self) -> usize {
        self.spaces_in(RosterGroup::NeedsYou).count()
    }

    /// Total number of pending space gates across all active spaces.
    pub fn total_pending_gates(&self) -> usize {
        self.spaces.iter().map(|s| s.pending_gates).sum()
    }
}

/// True when `item` is a pending gate or missing required env var belonging to `space_id`.
pub fn is_pending_space_gate(item: &InboxItem, space_id: &SpaceId) -> bool {
    if item.state != ItemState::Pending {
        return false;
    }
    if item.space.as_ref() != Some(space_id) {
        return false;
    }
    let kind = item.kind();
    kind.is_gate() || kind == ItemKind::MissingEnvVar
}

/// True when `item` is a high-risk ("danger") gate: widening to `production`, a plugin write call,
/// mounting a write-capable connector, or trusting a repo plugin.
pub fn is_danger_inbox_item(item: &InboxItem) -> bool {
    match &item.body {
        ItemBody::SpaceWiden(widen) => widen.production,
        ItemBody::OmpGate(gate) => gate.danger,
        ItemBody::PluginGate(gate) => matches!(
            gate.kind,
            PluginGateKind::WriteCall { .. }
                | PluginGateKind::Mount { .. }
                | PluginGateKind::TrustRepoPlugin { .. }
        ),
        ItemBody::PlanGate(_)
        | ItemBody::AttemptProposal(_)
        | ItemBody::MissingEnvVar(_)
        | ItemBody::Notice(_) => false,
    }
}

fn build_space_row(input: SpaceInput<'_>, inbox: &[InboxItem], now: u64) -> SpaceRow {
    let space_id = &input.space.id;

    let mut pending_gates = 0usize;
    let mut has_danger_gate = false;
    for item in inbox {
        if is_pending_space_gate(item, space_id) {
            pending_gates += 1;
            if is_danger_inbox_item(item) {
                has_danger_gate = true;
            }
        }
    }

    let agents = build_agent_rows(input.graph, &input.activity.agent_activity);
    let state = rollup_state(
        pending_gates,
        input.loop_run,
        &agents,
        input.activity.has_dead_pane,
    );
    let last_activity_at = input
        .activity
        .last_activity_at
        .unwrap_or(input.space.created_at);
    let age_secs = now.saturating_sub(last_activity_at);

    SpaceRow {
        id: input.space.id.clone(),
        title: input.space.title.clone(),
        state,
        group: state.group(),
        production: input.space.production,
        pending_gates,
        has_danger_gate,
        age_secs,
        last_activity_at,
        agents,
    }
}

fn build_agent_rows(
    graph: Option<&AgentGraph>,
    activity_map: &HashMap<AgentNodeId, String>,
) -> Vec<AgentRow> {
    let Some(graph) = graph else {
        return Vec::new();
    };
    let mut non_orch_index = 0usize;
    graph
        .nodes()
        .iter()
        .map(|node| {
            let color_index = match node.role {
                AgentRole::Orchestrator => None,
                AgentRole::Writer { .. } | AgentRole::Helper { .. } => {
                    let idx = non_orch_index;
                    non_orch_index += 1;
                    Some(idx)
                }
            };
            let activity = activity_map
                .get(&node.id)
                .map(|s| single_line(s))
                .unwrap_or_else(|| default_agent_activity(&node.state));
            AgentRow {
                id: node.id.clone(),
                label: node.label.clone(),
                role: node.role.clone(),
                color_index,
                state: node.state.clone(),
                activity,
                pane_id: node.pane_id.map(PaneId),
            }
        })
        .collect()
}

fn rollup_state(
    pending_gates: usize,
    loop_run: Option<&LoopRun>,
    agents: &[AgentRow],
    has_dead_pane: bool,
) -> SpaceState {
    // 1. NeedsYou: pending Inbox gate, loop waiting for approval / budget, or paused agent.
    let loop_needs_you = loop_run.is_some_and(|run| {
        matches!(
            run.status(),
            LoopStatus::AwaitingApproval { .. } | LoopStatus::BudgetExhausted
        )
    });
    let any_paused = agents
        .iter()
        .any(|agent| matches!(agent.state, AgentState::Paused));
    if pending_gates > 0 || loop_needs_you || any_paused {
        return SpaceState::NeedsYou;
    }

    // 2. Failed: blocked loop, dead pane, or failed/blocked agent.
    let loop_blocked =
        loop_run.is_some_and(|run| matches!(run.status(), LoopStatus::Blocked { .. }));
    let any_failed_or_blocked = agents.iter().any(|agent| {
        matches!(
            agent.state,
            AgentState::Failed { .. } | AgentState::Blocked { .. }
        )
    });
    if loop_blocked || has_dead_pane || any_failed_or_blocked {
        return SpaceState::Failed;
    }

    // 3. Running: any agent running or loop running.
    let loop_running = loop_run.is_some_and(|run| matches!(run.status(), LoopStatus::Running));
    let any_running = agents
        .iter()
        .any(|agent| matches!(agent.state, AgentState::Running));
    if any_running || loop_running {
        return SpaceState::Running;
    }

    // 4. Idle.
    SpaceState::Idle
}

fn single_line(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn default_agent_activity(state: &AgentState) -> String {
    match state {
        AgentState::Idle => "idle".to_owned(),
        AgentState::Running => "running".to_owned(),
        AgentState::Paused => "paused".to_owned(),
        AgentState::Blocked { reason } => {
            let clean = single_line(reason);
            if clean.is_empty() {
                "blocked".to_owned()
            } else {
                clean
            }
        }
        AgentState::Done => "done".to_owned(),
        AgentState::Failed { reason } => {
            let clean = single_line(reason);
            if clean.is_empty() {
                "failed".to_owned()
            } else {
                clean
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use xctx_core::SpaceShape;
    use xctx_graph::HelperKind;
    use xctx_inbox::{ItemId, Notice, PluginGate, Source, SpaceWiden};

    fn make_space(slug: &str, title: &str, created_at: u64) -> Space {
        Space {
            id: SpaceId::new(slug),
            title: title.to_owned(),
            task: title.to_owned(),
            shape: SpaceShape::Dev,
            status: SpaceStatus::Active,
            port: 4001,
            created_at,
            repos: Vec::new(),
            production: false,
        }
    }

    fn make_widen_gate(id: &str, space: &str, production: bool, state: ItemState) -> InboxItem {
        InboxItem {
            id: ItemId::new(id),
            space: Some(SpaceId::new(space)),
            source: Source::Agent,
            state,
            created_at: 100,
            updated_at: 100,
            note: None,
            body: ItemBody::SpaceWiden(SpaceWiden {
                space: SpaceId::new(space),
                add_repos: Vec::new(),
                production,
                reason: "need prod".to_owned(),
            }),
        }
    }

    fn make_notice(id: &str, space: &str) -> InboxItem {
        InboxItem {
            id: ItemId::new(id),
            space: Some(SpaceId::new(space)),
            source: Source::Agent,
            state: ItemState::Pending,
            created_at: 100,
            updated_at: 100,
            note: None,
            body: ItemBody::Notice(Notice {
                title: "fyi".to_owned(),
                body: None,
                level: xctx_inbox::Level::Info,
            }),
        }
    }

    #[test]
    fn rollup_precedence_is_needs_you_then_failed_then_running_then_idle() {
        let s = make_space("checkout", "Checkout", 10);
        let mut graph = AgentGraph::for_space(&["api".to_owned()]);
        let writer_id = graph.nodes()[1].id.clone();
        graph
            .set_state(&writer_id, AgentState::Running)
            .expect("writer exists");

        // Running agent -> Running
        let r = Roster::build(
            [SpaceInput {
                space: &s,
                graph: Some(&graph),
                loop_run: None,
                activity: SpaceActivity::default(),
            }],
            &[],
            100,
        );
        assert_eq!(r.spaces()[0].state, SpaceState::Running);
        assert_eq!(r.spaces()[0].group, RosterGroup::Running);

        // Dead pane beats Running -> Failed (grouped under NeedsYou)
        let r = Roster::build(
            [SpaceInput {
                space: &s,
                graph: Some(&graph),
                loop_run: None,
                activity: SpaceActivity {
                    has_dead_pane: true,
                    ..Default::default()
                },
            }],
            &[],
            100,
        );
        assert_eq!(r.spaces()[0].state, SpaceState::Failed);
        assert_eq!(r.spaces()[0].group, RosterGroup::NeedsYou);

        // Pending gate beats dead pane / blocked loop -> NeedsYou
        let gate = make_widen_gate("inbox-1", "checkout", false, ItemState::Pending);
        let mut blocked_loop = LoopRun::default();
        blocked_loop.block("tests failed");
        let r = Roster::build(
            [SpaceInput {
                space: &s,
                graph: Some(&graph),
                loop_run: Some(&blocked_loop),
                activity: SpaceActivity {
                    has_dead_pane: true,
                    ..Default::default()
                },
            }],
            &[gate],
            100,
        );
        assert_eq!(r.spaces()[0].state, SpaceState::NeedsYou);
        assert_eq!(r.spaces()[0].pending_gates, 1);

        // Approved gate and notice do not trigger NeedsYou
        let approved = make_widen_gate("inbox-2", "checkout", false, ItemState::Approved);
        let notice = make_notice("inbox-3", "checkout");
        let r = Roster::build(
            [SpaceInput {
                space: &s,
                graph: None,
                loop_run: None,
                activity: SpaceActivity::default(),
            }],
            &[approved, notice],
            100,
        );
        assert_eq!(r.spaces()[0].state, SpaceState::Idle);
        assert_eq!(r.spaces()[0].pending_gates, 0);
    }

    #[test]
    fn paused_loop_and_awaiting_approval_map_to_needs_you() {
        let s = make_space("billing", "Billing", 10);
        let mut run = LoopRun::default();
        run.start().expect("start loop");
        run.advance(true).expect("advance into awaiting approval");
        let r = Roster::build(
            [SpaceInput {
                space: &s,
                graph: None,
                loop_run: Some(&run),
                activity: SpaceActivity::default(),
            }],
            &[],
            50,
        );
        assert_eq!(r.spaces()[0].state, SpaceState::NeedsYou);
        assert_eq!(r.spaces()[0].age_secs, 40);
    }

    #[test]
    fn agent_color_indices_skip_orchestrator_and_follow_creation_order() {
        let s = make_space("multi", "Multi", 0);
        let mut graph = AgentGraph::for_space(&["api".to_owned(), "web".to_owned()]);
        let helper_id = graph
            .add_lane(
                AgentRole::Helper {
                    kind: HelperKind::Tests,
                },
                None,
            )
            .expect("add tests helper");
        let mut activity = SpaceActivity::default();
        activity
            .agent_activity
            .insert(helper_id.clone(), "running\n  cargo test".to_owned());

        let r = Roster::build(
            [SpaceInput {
                space: &s,
                graph: Some(&graph),
                loop_run: None,
                activity,
            }],
            &[],
            10,
        );
        let agents = &r.spaces()[0].agents;
        assert_eq!(agents.len(), 4);
        assert_eq!(agents[0].color_index, None);
        assert_eq!(agents[1].color_index, Some(0));
        assert_eq!(agents[2].color_index, Some(1));
        assert_eq!(agents[3].color_index, Some(2));
        assert_eq!(agents[3].activity, "running cargo test");
    }

    #[test]
    fn grouping_danger_priority_and_cmd_1_to_9_shortcuts() {
        let idle = make_space("docs", "Docs", 10);
        let running = make_space("billing", "Billing", 20);
        let failed = make_space("broken", "Broken", 30);
        let waiting = make_space("checkout", "Checkout", 40);
        let prod_gate_space = make_space("deploy", "Deploy", 5);
        let mut shipped = make_space("old", "Old", 99);
        shipped.status = SpaceStatus::Shipped;

        let mut running_graph = AgentGraph::for_space(&["api".to_owned()]);
        let w = running_graph.nodes()[1].id.clone();
        running_graph
            .set_state(&w, AgentState::Running)
            .expect("writer");

        let gates = vec![
            make_widen_gate("inbox-1", "checkout", false, ItemState::Pending),
            make_widen_gate("inbox-2", "deploy", true, ItemState::Pending),
        ];

        let roster = Roster::build(
            [
                SpaceInput {
                    space: &idle,
                    graph: None,
                    loop_run: None,
                    activity: SpaceActivity::default(),
                },
                SpaceInput {
                    space: &running,
                    graph: Some(&running_graph),
                    loop_run: None,
                    activity: SpaceActivity::default(),
                },
                SpaceInput {
                    space: &failed,
                    graph: None,
                    loop_run: None,
                    activity: SpaceActivity {
                        has_dead_pane: true,
                        ..Default::default()
                    },
                },
                SpaceInput {
                    space: &waiting,
                    graph: None,
                    loop_run: None,
                    activity: SpaceActivity::default(),
                },
                SpaceInput {
                    space: &prod_gate_space,
                    graph: None,
                    loop_run: None,
                    activity: SpaceActivity::default(),
                },
                SpaceInput {
                    space: &shipped,
                    graph: None,
                    loop_run: None,
                    activity: SpaceActivity::default(),
                },
            ],
            &gates,
            100,
        );

        let ids: Vec<&str> = roster.spaces().iter().map(|s| s.id.as_str()).collect();
        // Order: NeedsYou (danger first, then waiting, then failed), Running, Idle; shipped excluded.
        assert_eq!(ids, vec!["deploy", "checkout", "broken", "billing", "docs"]);
        assert_eq!(roster.needs_you_count(), 3);
        assert_eq!(roster.total_pending_gates(), 2);

        // Shortcuts 1..=9 follow Roster order.
        assert_eq!(roster.space_for_shortcut(0), None);
        assert_eq!(
            roster.space_for_shortcut(1).map(|s| s.id.as_str()),
            Some("deploy")
        );
        assert_eq!(
            roster.space_for_shortcut(4).map(|s| s.id.as_str()),
            Some("billing")
        );
        assert_eq!(roster.space_for_shortcut(6), None);
        assert_eq!(roster.space_for_shortcut(10), None);
        assert_eq!(roster.shortcut_for_space(&SpaceId::new("docs")), Some(5));
    }

    #[test]
    fn plugin_write_gate_is_flagged_as_danger() {
        let item = InboxItem {
            id: ItemId::new("inbox-9"),
            space: Some(SpaceId::new("s")),
            source: Source::Agent,
            state: ItemState::Pending,
            created_at: 1,
            updated_at: 1,
            note: None,
            body: ItemBody::PluginGate(PluginGate {
                space: SpaceId::new("s"),
                plugin: "sql".to_owned(),
                kind: PluginGateKind::WriteCall {
                    tool: "sql.write".to_owned(),
                    args: "DELETE FROM t".to_owned(),
                    pane: Some(1),
                    target: Some("postgres://local".to_owned()),
                },
                rationale: None,
            }),
        };
        assert!(is_danger_inbox_item(&item));
        assert!(is_pending_space_gate(&item, &SpaceId::new("s")));
    }
}
