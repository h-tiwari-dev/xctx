//! A space's agent graph and its structured loop.
//!
//! - [`AgentGraph`] holds the agents of one space: exactly one **Orchestrator**, at most one **Writer** per repo,
//!   and any number of read-only **Helpers** (`Tests`, `Reviewer`, `Probe`), joined by [`GraphEdge`]s that are
//!   wired automatically as lanes are added and removed.
//! - [`LoopRun`] is the space's `Write -> Test -> Probe -> Review` loop, advanced under an [`AutonomyLevel`]:
//!   ask before each step, run within a budget, or run until checks pass.
//! - [`SpaceGraphDocument`] keeps both in `<data dir>/spaces/<slug>/graph.json`, written atomically and validated
//!   when read back, so a damaged or invalid graph is reported instead of used.
//!
//! The crate knows nothing about panes or windows: an [`AgentNode`] carries the pane it runs in as a plain `u64`.

#![warn(missing_docs)]

mod error;
mod graph;
mod loop_run;
mod store;

pub use error::GraphError;
pub use graph::{
    AgentGraph, AgentNode, AgentNodeId, AgentRole, AgentState, EdgeKind, GraphEdge, HelperKind,
    ORCHESTRATOR_ID,
};
pub use loop_run::{
    AutonomyLevel, LoopDecision, LoopError, LoopRun, LoopStage, LoopStatus, MAX_LOOP_HISTORY,
    StageTransition,
};
pub use store::{
    GRAPH_DOCUMENT_VERSION, GRAPH_FILE, SPACES_DIR, SpaceGraphDocument, graph_path, load, save,
};
