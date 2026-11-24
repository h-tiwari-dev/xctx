//! The error type of the agent graph and its store.

use std::io;
use std::path::PathBuf;

use crate::graph::{AgentNodeId, AgentState};

/// An agent graph failure the app can show.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// No agent has this id.
    #[error("no agent {0} in the graph")]
    NotFound(AgentNodeId),

    /// The repo already has a writer; a space has at most one writer per repo.
    #[error("the repo {0} already has a writer")]
    DuplicateWriter(String),

    /// The graph already has an orchestrator, or a loaded graph has more than one.
    #[error("the graph already has an orchestrator")]
    DuplicateOrchestrator,

    /// A loaded graph has no orchestrator.
    #[error("the graph has no orchestrator")]
    MissingOrchestrator,

    /// The orchestrator cannot be removed from its space.
    #[error("the orchestrator cannot be removed")]
    CannotRemoveOrchestrator,

    /// A writer was given an empty repo.
    #[error("a writer needs a repo")]
    EmptyRepo,

    /// Two agents in a loaded graph share an id.
    #[error("two agents share the id {0}")]
    DuplicateNodeId(AgentNodeId),

    /// An edge in a loaded graph names an agent that is not in the graph.
    #[error("the edge {from} -> {to} names an agent that is not in the graph")]
    DanglingEdge {
        /// Where the edge starts.
        from: AgentNodeId,
        /// Where the edge ends.
        to: AgentNodeId,
    },

    /// The agent's state does not allow the action, e.g. pausing an agent that is done.
    #[error("cannot {action} agent {id} while it is {from:?}")]
    InvalidTransition {
        /// The agent.
        id: AgentNodeId,
        /// Its state.
        from: AgentState,
        /// What was attempted.
        action: &'static str,
    },

    /// The space slug cannot name a directory: it must be non-empty ASCII letters, digits, `-` or `_`.
    #[error("{0:?} is not a valid space slug")]
    InvalidSlug(String),

    /// Reading or writing a file failed.
    #[error("{path}: {source}")]
    Io {
        /// The file or directory.
        path: PathBuf,
        /// What went wrong.
        source: io::Error,
    },

    /// A file is not a valid graph document.
    #[error("{path} is not a valid graph document: {source}")]
    Json {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        source: serde_json::Error,
    },

    /// The graph document was written by a newer xctx.
    #[error("{path} is version {found}; this xctx reads up to version {supported}")]
    UnsupportedVersion {
        /// The file.
        path: PathBuf,
        /// Its version.
        found: u32,
        /// The newest this build reads.
        supported: u32,
    },
}
