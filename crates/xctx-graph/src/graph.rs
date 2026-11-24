//! The agents of one space and the edges between them.

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::GraphError;

/// The id of the orchestrator; a graph has exactly one.
pub const ORCHESTRATOR_ID: &str = "orchestrator";

/// The stable id of an agent within its space, e.g. `"orchestrator"`, `"writer-api"` or `"helper-tests-1"`. Ids
/// are unique within a graph and never change while the agent is in it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentNodeId(String);

impl AgentNodeId {
    /// Wrap an id. [`AgentGraph`] makes its own ids when lanes are added; this is for ids that come back from the
    /// user, the control socket or disk.
    pub fn new(id: impl Into<String>) -> AgentNodeId {
        AgentNodeId(id.into())
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for AgentNodeId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// What a read-only helper does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperKind {
    /// Runs and writes about tests, and verifies the contract on both sides.
    Tests,
    /// Reviews the writers' changes.
    Reviewer,
    /// Probes the running system (requests, logs, repros).
    Probe,
}

impl HelperKind {
    fn id_part(self) -> &'static str {
        match self {
            HelperKind::Tests => "tests",
            HelperKind::Reviewer => "reviewer",
            HelperKind::Probe => "probe",
        }
    }

    fn label(self) -> &'static str {
        match self {
            HelperKind::Tests => "Tests",
            HelperKind::Reviewer => "Reviewer",
            HelperKind::Probe => "Probe",
        }
    }
}

/// What an agent is in its space.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum AgentRole {
    /// Splits the task across the other agents and holds the contract between them. One per space.
    Orchestrator,
    /// The only agent that edits `repo`. At most one per repo.
    Writer {
        /// The repo it writes to, as the space names it.
        repo: String,
    },
    /// Reads across the space's repos without write access.
    Helper {
        /// What it does.
        kind: HelperKind,
    },
}

impl AgentRole {
    /// True for a writer.
    pub fn is_writer(&self) -> bool {
        matches!(self, AgentRole::Writer { .. })
    }

    /// The repo a writer writes to; `None` for every other role.
    pub fn repo(&self) -> Option<&str> {
        match self {
            AgentRole::Writer { repo } => Some(repo),
            _ => None,
        }
    }

    /// True when the agent has no write access to any repo: the orchestrator and helpers.
    pub fn is_read_only(&self) -> bool {
        !self.is_writer()
    }

    fn is_orchestrator(&self) -> bool {
        matches!(self, AgentRole::Orchestrator)
    }

    fn is_helper(&self) -> bool {
        matches!(self, AgentRole::Helper { .. })
    }
}

/// What an agent is doing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AgentState {
    /// Not started yet.
    #[default]
    Idle,
    /// Working.
    Running,
    /// Paused by the user; [`AgentGraph::resume`] continues it.
    Paused,
    /// Waiting on something outside it, e.g. a contract change or a missing variable.
    Blocked {
        /// What it waits on.
        reason: String,
    },
    /// Finished its part.
    Done,
    /// Stopped with an error.
    Failed {
        /// What went wrong.
        reason: String,
    },
}

/// One agent in a space.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentNode {
    /// Its stable id.
    pub id: AgentNodeId,
    /// What it is.
    pub role: AgentRole,
    /// The name to show.
    pub label: String,
    /// What it is doing.
    #[serde(default)]
    pub state: AgentState,
    /// The pane it runs in (an `xctx_layout::PaneId`'s value), if it has one.
    #[serde(default)]
    pub pane_id: Option<u64>,
    /// How many turns it has taken.
    #[serde(default)]
    pub turns_used: u32,
}

/// What an edge between two agents means.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// The orchestrator coordinates a writer or helper (orchestrator -> agent).
    Coordinates,
    /// Two writers share a contract. One edge per pair, from the older writer to the newer one; it means both
    /// directions.
    Contract,
    /// A helper verifies a writer's work (helper -> writer).
    Verifies,
}

/// A directed edge between two agents.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GraphEdge {
    /// Where it starts.
    pub from: AgentNodeId,
    /// Where it ends.
    pub to: AgentNodeId,
    /// What it means.
    pub kind: EdgeKind,
}

/// The agents of one space: exactly one orchestrator, at most one writer per repo, and read-only helpers. Edges
/// are wired when lanes are added: the orchestrator coordinates every other agent, every pair of writers shares a
/// contract, and every helper verifies every writer. Removing a lane removes its edges.
///
/// Every method keeps [`AgentGraph::validate`] true. A graph read from disk should be checked with it, as
/// [`crate::load`] does.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentGraph {
    nodes: Vec<AgentNode>,
    edges: Vec<GraphEdge>,
}

impl AgentGraph {
    /// The graph a space starts with: the orchestrator and one writer per repo in `repos`, in order. Repeated and
    /// empty repo names are skipped.
    pub fn for_space(repos: &[String]) -> AgentGraph {
        let mut graph = AgentGraph {
            nodes: Vec::new(),
            edges: Vec::new(),
        };
        graph.insert(AgentRole::Orchestrator, None);
        for repo in repos {
            if !repo.is_empty() && graph.writer_for(repo).is_none() {
                graph.insert(AgentRole::Writer { repo: repo.clone() }, None);
            }
        }
        graph
    }

    /// Add an agent in `role`, labelled `label` or a default from the role, and wire its edges. Returns its id.
    ///
    /// Fails with [`GraphError::DuplicateWriter`] when the repo already has a writer, [`GraphError::EmptyRepo`]
    /// for a writer without a repo, and [`GraphError::DuplicateOrchestrator`] for a second orchestrator.
    pub fn add_lane(
        &mut self,
        role: AgentRole,
        label: Option<String>,
    ) -> Result<AgentNodeId, GraphError> {
        match &role {
            AgentRole::Orchestrator if self.orchestrator().is_some() => {
                return Err(GraphError::DuplicateOrchestrator);
            }
            AgentRole::Writer { repo } if repo.is_empty() => return Err(GraphError::EmptyRepo),
            AgentRole::Writer { repo } if self.writer_for(repo).is_some() => {
                return Err(GraphError::DuplicateWriter(repo.clone()));
            }
            _ => {}
        }
        Ok(self.insert(role, label))
    }

    /// Remove an agent and every edge touching it. Returns the removed agent. The orchestrator cannot be removed.
    pub fn remove_lane(&mut self, id: &AgentNodeId) -> Result<AgentNode, GraphError> {
        let index = self.index_of(id)?;
        if self.nodes[index].role.is_orchestrator() {
            return Err(GraphError::CannotRemoveOrchestrator);
        }
        self.edges.retain(|e| &e.from != id && &e.to != id);
        Ok(self.nodes.remove(index))
    }

    /// Pause an agent that is idle, running or blocked. Pausing a paused agent does nothing; an agent that is done
    /// or failed cannot be paused.
    pub fn pause(&mut self, id: &AgentNodeId) -> Result<(), GraphError> {
        let node = self.node_mut(id)?;
        match node.state {
            AgentState::Idle | AgentState::Running | AgentState::Blocked { .. } => {
                node.state = AgentState::Paused;
                Ok(())
            }
            AgentState::Paused => Ok(()),
            AgentState::Done | AgentState::Failed { .. } => Err(GraphError::InvalidTransition {
                id: id.clone(),
                from: node.state.clone(),
                action: "pause",
            }),
        }
    }

    /// Set a paused or blocked agent running. Resuming a running agent does nothing; an idle, done or failed agent
    /// cannot be resumed.
    pub fn resume(&mut self, id: &AgentNodeId) -> Result<(), GraphError> {
        let node = self.node_mut(id)?;
        match node.state {
            AgentState::Paused | AgentState::Blocked { .. } => {
                node.state = AgentState::Running;
                Ok(())
            }
            AgentState::Running => Ok(()),
            AgentState::Idle | AgentState::Done | AgentState::Failed { .. } => {
                Err(GraphError::InvalidTransition {
                    id: id.clone(),
                    from: node.state.clone(),
                    action: "resume",
                })
            }
        }
    }

    /// Set an agent's state as reported by its runner, without the checks of [`AgentGraph::pause`] and
    /// [`AgentGraph::resume`].
    pub fn set_state(&mut self, id: &AgentNodeId, state: AgentState) -> Result<(), GraphError> {
        self.node_mut(id)?.state = state;
        Ok(())
    }

    /// Record the pane an agent runs in, or `None` when its pane closed.
    pub fn bind_pane(&mut self, id: &AgentNodeId, pane_id: Option<u64>) -> Result<(), GraphError> {
        self.node_mut(id)?.pane_id = pane_id;
        Ok(())
    }

    /// Count one more turn for an agent. Returns its new count.
    pub fn record_turn(&mut self, id: &AgentNodeId) -> Result<u32, GraphError> {
        let node = self.node_mut(id)?;
        node.turns_used = node.turns_used.saturating_add(1);
        Ok(node.turns_used)
    }

    /// The agent with this id.
    pub fn node(&self, id: &AgentNodeId) -> Option<&AgentNode> {
        self.nodes.iter().find(|n| &n.id == id)
    }

    /// Every agent, in the order they were added.
    pub fn nodes(&self) -> &[AgentNode] {
        &self.nodes
    }

    /// Every edge.
    pub fn edges(&self) -> &[GraphEdge] {
        &self.edges
    }

    /// The writers, in the order they were added.
    pub fn writers(&self) -> impl Iterator<Item = &AgentNode> {
        self.nodes.iter().filter(|n| n.role.is_writer())
    }

    /// The helpers, in the order they were added.
    pub fn helpers(&self) -> impl Iterator<Item = &AgentNode> {
        self.nodes.iter().filter(|n| n.role.is_helper())
    }

    /// The orchestrator. Always present in a graph that passes [`AgentGraph::validate`].
    pub fn orchestrator(&self) -> Option<&AgentNode> {
        self.nodes.iter().find(|n| n.role.is_orchestrator())
    }

    /// Check the graph's invariants: agent ids are unique, there is exactly one orchestrator and its id is
    /// [`ORCHESTRATOR_ID`], no repo has more than one writer and no writer has an empty repo, and every edge joins
    /// two different agents in the graph and appears once.
    pub fn validate(&self) -> Result<(), GraphError> {
        let mut ids = HashSet::new();
        let mut repos = HashSet::new();
        let mut orchestrators = Vec::new();
        for node in &self.nodes {
            if !ids.insert(&node.id) {
                return Err(GraphError::DuplicateNodeId(node.id.clone()));
            }
            match &node.role {
                AgentRole::Orchestrator => orchestrators.push(&node.id),
                AgentRole::Writer { repo } if repo.is_empty() => return Err(GraphError::EmptyRepo),
                AgentRole::Writer { repo } => {
                    if !repos.insert(repo.as_str()) {
                        return Err(GraphError::DuplicateWriter(repo.clone()));
                    }
                }
                AgentRole::Helper { .. } => {}
            }
        }
        match orchestrators.as_slice() {
            [] => return Err(GraphError::MissingOrchestrator),
            [id] if id.as_str() != ORCHESTRATOR_ID => {
                return Err(GraphError::WrongOrchestratorId((*id).clone()));
            }
            [_] => {}
            _ => return Err(GraphError::DuplicateOrchestrator),
        }
        let mut edges = HashSet::new();
        for edge in &self.edges {
            if !ids.contains(&edge.from) || !ids.contains(&edge.to) {
                return Err(GraphError::DanglingEdge {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                });
            }
            if edge.from == edge.to {
                return Err(GraphError::SelfLoopEdge(edge.from.clone()));
            }
            if !edges.insert(edge) {
                return Err(GraphError::DuplicateEdge {
                    from: edge.from.clone(),
                    to: edge.to.clone(),
                });
            }
        }
        Ok(())
    }

    fn writer_for(&self, repo: &str) -> Option<&AgentNode> {
        self.writers().find(|n| n.role.repo() == Some(repo))
    }

    fn index_of(&self, id: &AgentNodeId) -> Result<usize, GraphError> {
        self.nodes
            .iter()
            .position(|n| &n.id == id)
            .ok_or_else(|| GraphError::NotFound(id.clone()))
    }

    fn node_mut(&mut self, id: &AgentNodeId) -> Result<&mut AgentNode, GraphError> {
        let index = self.index_of(id)?;
        Ok(&mut self.nodes[index])
    }

    /// Add a node whose role the caller has already checked, and wire its edges.
    fn insert(&mut self, role: AgentRole, label: Option<String>) -> AgentNodeId {
        let (id, default_label) = self.new_id_and_label(&role);
        let id = AgentNodeId(id);
        let mut edges = Vec::new();
        match &role {
            AgentRole::Orchestrator => {
                for other in &self.nodes {
                    edges.push(edge(&id, &other.id, EdgeKind::Coordinates));
                }
            }
            AgentRole::Writer { .. } => {
                if let Some(orchestrator) = self.orchestrator() {
                    edges.push(edge(&orchestrator.id, &id, EdgeKind::Coordinates));
                }
                for writer in self.writers() {
                    edges.push(edge(&writer.id, &id, EdgeKind::Contract));
                }
                for helper in self.helpers() {
                    edges.push(edge(&helper.id, &id, EdgeKind::Verifies));
                }
            }
            AgentRole::Helper { .. } => {
                if let Some(orchestrator) = self.orchestrator() {
                    edges.push(edge(&orchestrator.id, &id, EdgeKind::Coordinates));
                }
                for writer in self.writers() {
                    edges.push(edge(&id, &writer.id, EdgeKind::Verifies));
                }
            }
        }
        self.nodes.push(AgentNode {
            id: id.clone(),
            role,
            label: label.unwrap_or(default_label),
            state: AgentState::Idle,
            pane_id: None,
            turns_used: 0,
        });
        self.edges.extend(edges);
        id
    }

    /// A free id for a new agent in `role`, and its default label.
    fn new_id_and_label(&self, role: &AgentRole) -> (String, String) {
        let taken = |id: &str| self.nodes.iter().any(|n| n.id.as_str() == id);
        match role {
            AgentRole::Orchestrator => {
                let id = first_free(ORCHESTRATOR_ID.to_owned(), &taken);
                (id, "Orchestrator".to_owned())
            }
            AgentRole::Writer { repo } => {
                let id = first_free(format!("writer-{}", id_slug(repo)), &taken);
                (id, repo.clone())
            }
            AgentRole::Helper { kind } => {
                let mut n = 1u32;
                loop {
                    let id = format!("helper-{}-{n}", kind.id_part());
                    if !taken(&id) {
                        let label = if n == 1 {
                            kind.label().to_owned()
                        } else {
                            format!("{} {n}", kind.label())
                        };
                        return (id, label);
                    }
                    n += 1;
                }
            }
        }
    }
}

fn edge(from: &AgentNodeId, to: &AgentNodeId, kind: EdgeKind) -> GraphEdge {
    GraphEdge {
        from: from.clone(),
        to: to.clone(),
        kind,
    }
}

/// `base` if no agent has it, else `base-2`, `base-3`, ...
fn first_free(base: String, taken: &impl Fn(&str) -> bool) -> String {
    if !taken(&base) {
        return base;
    }
    let mut n = 2u32;
    loop {
        let id = format!("{base}-{n}");
        if !taken(&id) {
            return id;
        }
        n += 1;
    }
}

/// The part of a writer's id that comes from its repo: the repo's last path component, lowercased, with runs of
/// anything but ASCII letters and digits turned into one `-`.
fn id_slug(repo: &str) -> String {
    let name = repo
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(repo);
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    if slug.is_empty() {
        "repo".to_owned()
    } else {
        slug.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_slug_uses_the_last_path_component() {
        assert_eq!(id_slug("api"), "api");
        assert_eq!(id_slug("/code/Orders_Svc/"), "orders-svc");
        assert_eq!(id_slug("web app!!"), "web-app");
        assert_eq!(id_slug("///"), "repo");
        assert_eq!(id_slug("日本"), "repo");
    }

    #[test]
    fn writers_whose_repos_slug_alike_get_distinct_ids() {
        let graph = AgentGraph::for_space(&["/a/api".to_owned(), "/b/api".to_owned()]);
        let ids: Vec<&str> = graph.writers().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["writer-api", "writer-api-2"]);
        assert!(graph.validate().is_ok());
    }
}
