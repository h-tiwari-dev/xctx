//! The error type of the Inbox.

use std::path::PathBuf;

use crate::item::{ItemId, ItemKind, ItemState};

/// An Inbox failure the app can show.
#[derive(Debug, thiserror::Error)]
pub enum InboxError {
    /// Reading or writing the store failed.
    #[error(transparent)]
    Store(#[from] xctx_core::Error),

    /// `inbox.json` was written by a newer xctx.
    #[error("{path} is version {found}; this xctx reads up to version {supported}")]
    UnsupportedVersion {
        /// The file.
        path: PathBuf,
        /// Its version.
        found: u32,
        /// The newest this build reads.
        supported: u32,
    },

    /// No item has this id.
    #[error("no Inbox item {0}")]
    NotFound(ItemId),

    /// The item was already decided.
    #[error("Inbox item {id} is already {state:?}")]
    NotPending {
        /// The item.
        id: ItemId,
        /// What it is now.
        state: ItemState,
    },

    /// The action does not apply to this kind of item, e.g. approving a notice.
    #[error("cannot {action} a {kind:?} item ({id})")]
    WrongKind {
        /// The item.
        id: ItemId,
        /// Its kind.
        kind: ItemKind,
        /// What was attempted.
        action: &'static str,
    },

    /// A Plan Gate cannot be approved without at least one repo.
    #[error("the plan in {0} has no repositories")]
    EmptyPlan(ItemId),

    /// A space widening cannot be approved when it adds no repos and does not switch to production.
    #[error("the widening in {0} changes nothing")]
    EmptyWiden(ItemId),
}

/// The result type of the Inbox's fallible operations.
pub type Result<T, E = InboxError> = std::result::Result<T, E>;
