//! The Flow view's model: which agents work in which loop stage, where each agent sits on the canvas and how the
//! edges between them are drawn (with how much the two talked lately), and one timeline of what happened in the
//! space (messages, findings, notes, the contract, stage changes), optionally narrowed to one agent or one pair.
//!
//! Everything here is plain geometry and data: the view draws it, and every rule is tested without a window.

use std::collections::HashMap;

use xctx_channel::{ChannelMessage, Contract, Finding, FindingSeverity, MessageKind, Note};
use xctx_graph::{
    AgentGraph, AgentNode, AgentNodeId, AgentRole, EdgeKind, HelperKind, LoopStage, StageTransition,
};

/// Every loop stage, in the order the stage track shows them.
pub const STAGES: [LoopStage; 4] = [
    LoopStage::Write,
    LoopStage::Test,
    LoopStage::Probe,
    LoopStage::Review,
];

/// The loop stage an agent in `role` does its work in: writers write, the Tests helper tests, the Probe and
/// Browser helpers probe, the Reviewer reviews. The orchestrator works across every stage, so it has none.
pub fn stage_of(role: &AgentRole) -> Option<LoopStage> {
    match role {
        AgentRole::Orchestrator => None,
        AgentRole::Writer { .. } => Some(LoopStage::Write),
        AgentRole::Helper { kind } => Some(match kind {
            HelperKind::Tests => LoopStage::Test,
            HelperKind::Probe | HelperKind::Browser => LoopStage::Probe,
            HelperKind::Reviewer => LoopStage::Review,
        }),
    }
}

/// The agents of `graph` that work in `stage`, in graph order.
pub fn stage_agents(graph: &AgentGraph, stage: LoopStage) -> Vec<&AgentNode> {
    graph
        .nodes()
        .iter()
        .filter(|node| stage_of(&node.role) == Some(stage))
        .collect()
}

/// A point on the canvas, in pixels from its top-left corner.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FlowPoint {
    /// Rightwards.
    pub x: f32,
    /// Downwards.
    pub y: f32,
}

impl FlowPoint {
    /// The point (`x`, `y`).
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// The row of the canvas an agent is drawn in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FlowTier {
    /// The top row: the orchestrator.
    Orchestrator,
    /// The middle row: one writer per repo.
    Writer,
    /// The bottom row: the read-only helpers.
    Helper,
}

impl FlowTier {
    /// The tier of an agent in `role`.
    pub fn of(role: &AgentRole) -> Self {
        match role {
            AgentRole::Orchestrator => Self::Orchestrator,
            AgentRole::Writer { .. } => Self::Writer,
            AgentRole::Helper { .. } => Self::Helper,
        }
    }
}

/// One agent placed on the canvas.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowNode {
    /// The agent.
    pub id: AgentNodeId,
    /// Its row.
    pub tier: FlowTier,
    /// The centre of its card.
    pub center: FlowPoint,
}

/// One edge of the graph, as a cubic Bézier curve between two cards.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowEdge {
    /// Where it starts (the coordinating orchestrator, the older writer of a contract, the verifying helper).
    pub from: AgentNodeId,
    /// Where it ends.
    pub to: AgentNodeId,
    /// What it means.
    pub kind: EdgeKind,
    /// The curve's start, on the edge of `from`'s card.
    pub start: FlowPoint,
    /// The first control point.
    pub control1: FlowPoint,
    /// The second control point.
    pub control2: FlowPoint,
    /// The curve's end, on the edge of `to`'s card.
    pub end: FlowPoint,
    /// The point halfway along the curve, where its traffic badge is drawn.
    pub mid: FlowPoint,
    /// How many of the recent messages went between the two agents, either way.
    pub traffic: usize,
}

/// The canvas: every agent's card and every edge, laid out for a canvas of a given size.
#[derive(Clone, Debug, PartialEq)]
pub struct FlowLayout {
    /// The cards, in graph order.
    pub nodes: Vec<FlowNode>,
    /// The edges, in graph order.
    pub edges: Vec<FlowEdge>,
    /// Every card's width: [`NODE_WIDTH`], or less when a row has too many cards for the canvas.
    pub node_width: f32,
    /// Every card's height.
    pub node_height: f32,
}

/// The widest a card is drawn.
pub const NODE_WIDTH: f32 = 196.0;
/// The narrowest a card is drawn, however crowded its row.
pub const MIN_NODE_WIDTH: f32 = 120.0;
/// A card's height.
pub const NODE_HEIGHT: f32 = 72.0;
/// The least room between two cards of a row.
pub const NODE_GAP: f32 = 24.0;
/// Room kept clear above the top row, for the toolbar floating over the canvas.
pub const TOP_INSET: f32 = 44.0;
/// Room kept clear below the bottom row, for the legend floating over the canvas.
pub const BOTTOM_INSET: f32 = 36.0;
/// The share of the canvas's width kept free on each side of a row of several cards, so a row reads as a
/// spread rather than a cluster (less when the row would not fit otherwise).
pub const ROW_MARGIN_SHARE: f32 = 0.12;
/// How many of the latest channel messages count as recent traffic on the edges.
pub const RECENT_MESSAGES: usize = 50;

/// How many of `messages` (the latest [`RECENT_MESSAGES`] of them) went between each pair of agents, keyed by
/// the pair in either order (the smaller id first). Broadcasts belong to no pair.
pub fn pair_traffic(messages: &[ChannelMessage]) -> HashMap<(String, String), usize> {
    let mut traffic = HashMap::new();
    let recent = messages.len().saturating_sub(RECENT_MESSAGES);
    for message in &messages[recent..] {
        if let Some(to) = &message.to
            && to != &message.from
        {
            *traffic.entry(pair_key(&message.from, to)).or_insert(0) += 1;
        }
    }
    traffic
}

/// The key of the pair `a`, `b` in [`pair_traffic`]: the smaller id first.
pub fn pair_key(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_owned(), b.to_owned())
    } else {
        (b.to_owned(), a.to_owned())
    }
}

impl FlowLayout {
    /// Lay `graph` out on a canvas `width` by `height`: the orchestrator centred on the top row, the writers
    /// spread evenly across the middle, the helpers across the bottom, and every edge curved between the cards
    /// it joins, with its traffic from `traffic` (see [`pair_traffic`]). A size too small or not finite is
    /// treated as the smallest that fits one card per row.
    pub fn compute(
        graph: &AgentGraph,
        width: f32,
        height: f32,
        traffic: &HashMap<(String, String), usize>,
    ) -> Self {
        let width =
            if width.is_finite() { width } else { 0.0 }.max(MIN_NODE_WIDTH + 2.0 * NODE_GAP);
        let height = if height.is_finite() { height } else { 0.0 }
            .max(3.0 * (NODE_HEIGHT + NODE_GAP) + TOP_INSET + BOTTOM_INSET);
        let rows = [FlowTier::Orchestrator, FlowTier::Writer, FlowTier::Helper];
        let widest_row = rows
            .iter()
            .map(|tier| {
                graph
                    .nodes()
                    .iter()
                    .filter(|n| FlowTier::of(&n.role) == *tier)
                    .count()
            })
            .max()
            .unwrap_or(1)
            .max(1) as f32;
        let node_width = ((width - NODE_GAP * (widest_row + 1.0)) / widest_row)
            .clamp(MIN_NODE_WIDTH, NODE_WIDTH);
        // The three rows' centres, clear of the toolbar above and the legend below.
        let top = TOP_INSET + NODE_HEIGHT / 2.0 + NODE_GAP / 2.0;
        let bottom = height - BOTTOM_INSET - NODE_HEIGHT / 2.0 - NODE_GAP / 2.0;
        let row_y = |tier: FlowTier| match tier {
            FlowTier::Orchestrator => top,
            FlowTier::Writer => (top + bottom) / 2.0,
            FlowTier::Helper => bottom,
        };

        let mut nodes = Vec::with_capacity(graph.nodes().len());
        for tier in rows {
            let row: Vec<&AgentNode> = graph
                .nodes()
                .iter()
                .filter(|n| FlowTier::of(&n.role) == tier)
                .collect();
            let count = row.len();
            // A lone card is centred; several spread from margin to margin, closer to the edges when the row
            // would not fit with the wider margin.
            let min_margin = node_width / 2.0 + NODE_GAP;
            let wide_margin = (width * ROW_MARGIN_SHARE).max(min_margin);
            let fits = |margin: f32| {
                count < 2 || (width - 2.0 * margin) / (count - 1) as f32 >= node_width + NODE_GAP
            };
            let margin = if fits(wide_margin) {
                wide_margin
            } else {
                min_margin
            };
            for (index, node) in row.into_iter().enumerate() {
                let x = if count < 2 {
                    width / 2.0
                } else {
                    margin + (width - 2.0 * margin) * index as f32 / (count - 1) as f32
                };
                nodes.push(FlowNode {
                    id: node.id.clone(),
                    tier,
                    center: FlowPoint::new(x, row_y(tier)),
                });
            }
        }
        // Back to graph order, so the view and the timeline agree on it.
        let order: HashMap<&AgentNodeId, usize> = graph
            .nodes()
            .iter()
            .enumerate()
            .map(|(index, node)| (&node.id, index))
            .collect();
        nodes.sort_by_key(|node| order.get(&node.id).copied().unwrap_or(usize::MAX));

        let center_of: HashMap<&AgentNodeId, FlowPoint> =
            nodes.iter().map(|node| (&node.id, node.center)).collect();
        let half_h = NODE_HEIGHT / 2.0;
        let half_w = node_width / 2.0;
        let edges = graph
            .edges()
            .iter()
            .filter_map(|edge| {
                let a = *center_of.get(&edge.from)?;
                let b = *center_of.get(&edge.to)?;
                let (start, control1, control2, end) = if (a.y - b.y).abs() < f32::EPSILON {
                    // Same row (a contract between writers): leave from the facing sides and arch above the row,
                    // higher the further apart the two are, so arches of a row do not run into each other.
                    let (left, right) = if a.x <= b.x { (a, b) } else { (b, a) };
                    let start = FlowPoint::new(left.x + half_w, left.y);
                    let end = FlowPoint::new(right.x - half_w, right.y);
                    let lift = ((end.x - start.x) * 0.25).clamp(12.0, half_h + NODE_GAP);
                    (
                        start,
                        FlowPoint::new(start.x + (end.x - start.x) * 0.25, start.y - lift),
                        FlowPoint::new(end.x - (end.x - start.x) * 0.25, end.y - lift),
                        end,
                    )
                } else {
                    // Between rows: from the bottom of the upper card to the top of the lower one.
                    let (upper, lower) = if a.y < b.y { (a, b) } else { (b, a) };
                    let start = FlowPoint::new(upper.x, upper.y + half_h);
                    let end = FlowPoint::new(lower.x, lower.y - half_h);
                    let bend = (end.y - start.y) * 0.5;
                    let (start, control1, control2, end) = (
                        start,
                        FlowPoint::new(start.x, start.y + bend),
                        FlowPoint::new(end.x, end.y - bend),
                        end,
                    );
                    // Keep the curve pointing from `from` to `to`.
                    if a.y < b.y {
                        (start, control1, control2, end)
                    } else {
                        (end, control2, control1, start)
                    }
                };
                let mid = cubic_point(start, control1, control2, end, 0.5);
                Some(FlowEdge {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                    kind: edge.kind,
                    start,
                    control1,
                    control2,
                    end,
                    mid,
                    traffic: traffic
                        .get(&pair_key(edge.from.as_str(), edge.to.as_str()))
                        .copied()
                        .unwrap_or(0),
                })
            })
            .collect();
        Self {
            nodes,
            edges,
            node_width,
            node_height: NODE_HEIGHT,
        }
    }

    /// The card of agent `id`, if it is on the canvas.
    pub fn node(&self, id: &AgentNodeId) -> Option<&FlowNode> {
        self.nodes.iter().find(|node| &node.id == id)
    }
}

/// The point at `t` (0 to 1) along the cubic Bézier curve `p0`, `p1`, `p2`, `p3`.
pub fn cubic_point(
    p0: FlowPoint,
    p1: FlowPoint,
    p2: FlowPoint,
    p3: FlowPoint,
    t: f32,
) -> FlowPoint {
    let u = 1.0 - t;
    let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    FlowPoint::new(
        a * p0.x + b * p1.x + c * p2.x + d * p3.x,
        a * p0.y + b * p1.y + c * p2.y + d * p3.y,
    )
}

/// What the timeline is narrowed to.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FlowFocus {
    /// Everything one agent did or was told: its messages either way (and broadcasts it sent), its findings and
    /// notes, the contract when it wrote or agreed to it, and the stage changes of its stage.
    Agent(AgentNodeId),
    /// What two agents said to each other, either way.
    Pair(AgentNodeId, AgentNodeId),
}

/// What a timeline entry is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimelineKind {
    /// A channel message.
    Message(MessageKind),
    /// A finding, and whether it is resolved.
    Finding {
        /// Its id, for resolving it.
        id: u64,
        /// How bad it is.
        severity: FindingSeverity,
        /// Whether someone marked it resolved.
        resolved: bool,
    },
    /// A shared note.
    Note,
    /// The contract's current revision.
    Contract {
        /// Its version.
        version: u32,
    },
    /// The loop moved from one stage to another.
    Stage {
        /// The stage that finished.
        from: LoopStage,
        /// The stage that started.
        to: LoopStage,
        /// False when checks failed and the loop went back to `Write`.
        passed: bool,
        /// The iteration `to` runs in.
        iteration: u32,
    },
}

/// One entry of the timeline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineItem {
    /// Unique among the timeline's entries and stable while the entry exists, such as `message-12`; for element
    /// ids and keys.
    pub key: String,
    /// When it happened, in seconds since the Unix epoch (0 when unknown: stage changes saved before they were
    /// timed).
    pub at: u64,
    /// The agent it came from, if one did.
    pub actor: Option<String>,
    /// The agent it was for, when it was for one.
    pub target: Option<String>,
    /// What it is.
    pub kind: TimelineKind,
    /// The text: the message, the finding's summary with its location, the note, the contract's title and
    /// summary. Empty for stage changes. Already masked by the caller.
    pub body: String,
}

/// Everything the timeline is built from, already masked by the caller.
pub struct TimelineInput<'a> {
    /// The channel's messages, oldest first.
    pub messages: &'a [ChannelMessage],
    /// The findings, resolved ones included.
    pub findings: &'a [Finding],
    /// The shared notes.
    pub notes: &'a [Note],
    /// The contract's current revision, if there is one.
    pub contract: Option<&'a Contract>,
    /// The loop's stage changes, oldest first.
    pub stages: &'a [StageTransition],
    /// The graph, to know which stage an agent works in for [`FlowFocus::Agent`].
    pub graph: &'a AgentGraph,
}

/// The timeline, newest first, narrowed to `focus` when given. Entries at the same time keep their order of
/// kinds: stage changes, contract, messages, findings, notes.
pub fn timeline(input: &TimelineInput<'_>, focus: Option<&FlowFocus>) -> Vec<TimelineItem> {
    let agent_stage = |id: &AgentNodeId| input.graph.node(id).and_then(|node| stage_of(&node.role));
    let involves = |actor: Option<&str>, target: Option<&str>| match focus {
        None => true,
        Some(FlowFocus::Agent(id)) => actor == Some(id.as_str()) || target == Some(id.as_str()),
        Some(FlowFocus::Pair(a, b)) => {
            let (a, b) = (a.as_str(), b.as_str());
            matches!((actor, target), (Some(x), Some(y)) if (x == a && y == b) || (x == b && y == a))
        }
    };

    let mut items = Vec::new();
    for (index, stage) in input.stages.iter().enumerate() {
        let shown = match focus {
            None => true,
            Some(FlowFocus::Agent(id)) => {
                let own = agent_stage(id);
                own.is_some() && (own == Some(stage.from) || own == Some(stage.to))
            }
            Some(FlowFocus::Pair(..)) => false,
        };
        if shown {
            items.push(TimelineItem {
                key: format!("stage-{index}"),
                at: stage.at,
                actor: None,
                target: None,
                kind: TimelineKind::Stage {
                    from: stage.from,
                    to: stage.to,
                    passed: stage.checks_passed,
                    iteration: stage.iteration,
                },
                body: String::new(),
            });
        }
    }
    if let Some(contract) = input.contract {
        let shown = match focus {
            None => true,
            Some(FlowFocus::Agent(id)) => contract.agreed_by.iter().any(|a| a == id.as_str()),
            Some(FlowFocus::Pair(..)) => false,
        };
        if shown {
            items.push(TimelineItem {
                key: format!("contract-{}", contract.version),
                at: contract.updated_at,
                actor: Some(contract.author.clone()),
                target: None,
                kind: TimelineKind::Contract {
                    version: contract.version,
                },
                body: if contract.summary.is_empty() {
                    contract.title.clone()
                } else {
                    format!("{} — {}", contract.title, contract.summary)
                },
            });
        }
    }
    for message in input.messages {
        if involves(Some(&message.from), message.to.as_deref()) {
            items.push(TimelineItem {
                key: format!("message-{}", message.id),
                at: message.created_at,
                actor: Some(message.from.clone()),
                target: message.to.clone(),
                kind: TimelineKind::Message(message.kind),
                body: message.body.clone(),
            });
        }
    }
    let solo = !matches!(focus, Some(FlowFocus::Pair(..)));
    for finding in input.findings {
        if solo && involves(Some(&finding.author), None) {
            let location = match (&finding.repo, &finding.file) {
                (Some(repo), Some(file)) => format!(" ({repo}/{file})"),
                (Some(repo), None) => format!(" ({repo})"),
                (None, Some(file)) => format!(" ({file})"),
                (None, None) => String::new(),
            };
            items.push(TimelineItem {
                key: format!("finding-{}", finding.id),
                at: finding.created_at,
                actor: Some(finding.author.clone()),
                target: None,
                kind: TimelineKind::Finding {
                    id: finding.id,
                    severity: finding.severity,
                    resolved: finding.resolved,
                },
                body: format!("{}{location}", finding.summary),
            });
        }
    }
    for note in input.notes {
        if solo && involves(Some(&note.author), None) {
            items.push(TimelineItem {
                key: format!("note-{}", note.id),
                at: note.created_at,
                actor: Some(note.author.clone()),
                target: None,
                kind: TimelineKind::Note,
                body: note.body.clone(),
            });
        }
    }
    // Stable, so entries at the same second keep the order they were added in, reversed with the rest.
    items.sort_by_key(|item| item.at);
    items.reverse();
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn graph() -> AgentGraph {
        let mut graph = AgentGraph::for_space(&["api".to_owned(), "web".to_owned()]);
        for kind in [HelperKind::Tests, HelperKind::Reviewer] {
            graph
                .add_lane(AgentRole::Helper { kind }, None)
                .expect("a helper can always be added");
        }
        graph
    }

    fn id_of(graph: &AgentGraph, pick: impl Fn(&AgentRole) -> bool) -> AgentNodeId {
        graph
            .nodes()
            .iter()
            .find(|node| pick(&node.role))
            .map(|node| node.id.clone())
            .expect("the test graph has it")
    }

    fn message(id: u64, from: &str, to: Option<&str>, at: u64) -> ChannelMessage {
        ChannelMessage {
            id,
            from: from.to_owned(),
            to: to.map(str::to_owned),
            kind: MessageKind::Question,
            body: format!("m{id}"),
            created_at: at,
        }
    }

    #[test]
    fn every_role_but_the_orchestrator_works_in_one_stage() {
        let graph = graph();
        assert_eq!(stage_of(&AgentRole::Orchestrator), None);
        assert_eq!(stage_agents(&graph, LoopStage::Write).len(), 2);
        assert_eq!(stage_agents(&graph, LoopStage::Test).len(), 1);
        assert!(stage_agents(&graph, LoopStage::Probe).is_empty());
        assert_eq!(
            stage_of(&AgentRole::Helper {
                kind: HelperKind::Browser
            }),
            Some(LoopStage::Probe)
        );
    }

    #[test]
    fn cards_sit_in_three_rows_inside_the_canvas_and_edges_join_them() {
        let graph = graph();
        let layout = FlowLayout::compute(&graph, 900.0, 600.0, &HashMap::new());
        assert_eq!(layout.nodes.len(), graph.nodes().len());
        let orchestrator = layout
            .node(&id_of(&graph, |r| matches!(r, AgentRole::Orchestrator)))
            .expect("placed");
        assert!((orchestrator.center.x - 450.0).abs() < 0.01);
        let rows: Vec<f32> = [FlowTier::Orchestrator, FlowTier::Writer, FlowTier::Helper]
            .into_iter()
            .filter_map(|tier| {
                layout
                    .nodes
                    .iter()
                    .find(|n| n.tier == tier)
                    .map(|n| n.center.y)
            })
            .collect();
        assert!(rows.windows(2).all(|pair| pair[0] < pair[1]));
        for node in &layout.nodes {
            assert!(node.center.x - layout.node_width / 2.0 >= 0.0);
            assert!(node.center.x + layout.node_width / 2.0 <= 900.0);
            assert!(node.center.y + layout.node_height / 2.0 <= 600.0);
        }
        assert_eq!(layout.edges.len(), graph.edges().len());
        for edge in &layout.edges {
            let from = layout.node(&edge.from).expect("placed").center;
            let to = layout.node(&edge.to).expect("placed").center;
            // Each end touches its own card.
            assert!((edge.start.x - from.x).abs() <= layout.node_width / 2.0 + 0.01);
            assert!((edge.end.x - to.x).abs() <= layout.node_width / 2.0 + 0.01);
            assert!((edge.start.y - from.y).abs() <= layout.node_height / 2.0 + 0.01);
            assert!((edge.end.y - to.y).abs() <= layout.node_height / 2.0 + 0.01);
        }
        let contract = layout
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Contract)
            .expect("two writers share a contract");
        assert!(
            contract.mid.y < contract.start.y,
            "a contract arches above its row"
        );
    }

    #[test]
    fn a_crowded_row_shrinks_its_cards_but_not_below_the_minimum() {
        let repos: Vec<String> = (0..12).map(|i| format!("repo{i}")).collect();
        let graph = AgentGraph::for_space(&repos);
        let layout = FlowLayout::compute(&graph, 1200.0, 600.0, &HashMap::new());
        assert!(layout.node_width < NODE_WIDTH);
        assert!(layout.node_width >= MIN_NODE_WIDTH);
        let tiny = FlowLayout::compute(&graph, f32::NAN, -5.0, &HashMap::new());
        assert!(
            tiny.nodes
                .iter()
                .all(|n| n.center.x.is_finite() && n.center.y.is_finite())
        );
    }

    #[test]
    fn traffic_counts_recent_direct_messages_either_way() {
        let graph = graph();
        let api = id_of(&graph, |r| r.repo() == Some("api"));
        let web = id_of(&graph, |r| r.repo() == Some("web"));
        let messages = vec![
            message(1, api.as_str(), Some(web.as_str()), 10),
            message(2, web.as_str(), Some(api.as_str()), 11),
            message(3, api.as_str(), None, 12),
        ];
        let traffic = pair_traffic(&messages);
        assert_eq!(traffic.get(&pair_key(web.as_str(), api.as_str())), Some(&2));
        let layout = FlowLayout::compute(&graph, 900.0, 600.0, &traffic);
        let contract = layout
            .edges
            .iter()
            .find(|e| e.kind == EdgeKind::Contract)
            .expect("contract edge");
        assert_eq!(contract.traffic, 2);

        let many: Vec<ChannelMessage> = (0..RECENT_MESSAGES as u64 + 10)
            .map(|i| message(i, api.as_str(), Some(web.as_str()), i))
            .collect();
        assert_eq!(
            pair_traffic(&many).get(&pair_key(api.as_str(), web.as_str())),
            Some(&RECENT_MESSAGES)
        );
    }

    #[test]
    fn the_timeline_is_newest_first_and_narrows_to_an_agent_or_a_pair() {
        let graph = graph();
        let api = id_of(&graph, |r| r.repo() == Some("api"));
        let web = id_of(&graph, |r| r.repo() == Some("web"));
        let tests = id_of(&graph, |r| {
            *r == AgentRole::Helper {
                kind: HelperKind::Tests,
            }
        });
        let messages = vec![
            message(1, api.as_str(), Some(web.as_str()), 10),
            message(2, tests.as_str(), None, 20),
        ];
        let findings = vec![Finding {
            id: 1,
            author: tests.as_str().to_owned(),
            severity: FindingSeverity::Error,
            repo: Some("api".to_owned()),
            file: Some("src/a.rs".to_owned()),
            summary: "retry loops forever".to_owned(),
            resolved: false,
            created_at: 30,
        }];
        let stages = vec![StageTransition {
            from: LoopStage::Write,
            to: LoopStage::Test,
            iteration: 1,
            checks_passed: true,
            at: 15,
        }];
        let input = TimelineInput {
            messages: &messages,
            findings: &findings,
            notes: &[],
            contract: None,
            stages: &stages,
            graph: &graph,
        };
        let all = timeline(&input, None);
        let keys: Vec<&str> = all.iter().map(|item| item.key.as_str()).collect();
        assert_eq!(keys, ["finding-1", "message-2", "stage-0", "message-1"]);
        assert_eq!(all[0].body, "retry loops forever (api/src/a.rs)");

        let of_tests = timeline(&input, Some(&FlowFocus::Agent(tests.clone())));
        let keys: Vec<&str> = of_tests.iter().map(|item| item.key.as_str()).collect();
        assert_eq!(keys, ["finding-1", "message-2", "stage-0"]);

        let pair = timeline(&input, Some(&FlowFocus::Pair(web, api)));
        let keys: Vec<&str> = pair.iter().map(|item| item.key.as_str()).collect();
        assert_eq!(keys, ["message-1"]);
    }
}
