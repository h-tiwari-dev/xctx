//! The error type of the attempt records.

use std::path::PathBuf;

use crate::set::AttemptState;

/// A refused change to a space's attempts, or a failure to store them.
#[derive(Debug, thiserror::Error)]
pub enum AttemptError {
    /// A fork asked for no attempts, or for more than one fork may make.
    #[error("a fork makes between 1 and {max} attempts, not {asked}")]
    BadCount {
        /// How many were asked for.
        asked: u32,
        /// The most allowed.
        max: u32,
    },

    /// The space already has this many live attempts; discard or adopt some first.
    #[error("the space already has {live} live attempts; at most {max} can run at once")]
    TooMany {
        /// How many are live.
        live: u32,
        /// The most allowed.
        max: u32,
    },

    /// No attempt has this number.
    #[error("no attempt {0}")]
    NotFound(u32),

    /// The attempt cannot make this change from the state it is in.
    #[error("attempt {number} is {state:?} and cannot be {action}")]
    WrongState {
        /// The attempt.
        number: u32,
        /// Its state.
        state: AttemptState,
        /// What was asked.
        action: &'static str,
    },

    /// Another attempt of the same round was already adopted.
    #[error("attempt {adopted} of this round was already adopted")]
    AlreadyAdopted {
        /// The adopted attempt.
        adopted: u32,
    },

    /// The recorded worktrees do not match the plan they were made for.
    #[error("the forked worktrees do not match the plan: {0}")]
    PlanMismatch(String),

    /// The slug cannot name a folder under the spaces directory.
    #[error("{0:?} is not a valid space slug")]
    InvalidSlug(String),

    /// Reading or writing `attempts.json` failed.
    #[error(transparent)]
    Store(#[from] xctx_core::Error),

    /// `attempts.json` was written by a newer xctx.
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

/// The result type of the attempt records' fallible operations.
pub type Result<T, E = AttemptError> = std::result::Result<T, E>;
