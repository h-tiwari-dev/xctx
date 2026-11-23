//! The parts of xctx that need no window: git and worktrees, the repo index, the space model and its lifecycle, the
//! per-space mise file, and the store on disk.

#![warn(missing_docs)]

pub mod error;
pub mod git;
pub mod index;
pub mod mise;
pub mod space;
pub mod store;

pub use error::{Error, Result};
pub use index::{IndexedRepo, RepoIndex, RepoMatch};
pub use space::{
    DiscardReport, DraftRepo, PullRequestOutcome, RemovedWorktree, RepoShipReport, ShipOptions,
    ShipOutcome, ShipReport, ShipSkipReason, Space, SpaceDraft, SpaceId, SpaceRepo, SpaceShape,
    SpaceStatus, discard, launch, ship,
};
pub use store::Store;

/// The app's name, as shown in the window title and menu bar.
pub const APP_NAME: &str = "xctx";

/// The text the app skeleton shows until Home exists.
pub fn placeholder_message() -> &'static str {
    "xctx skeleton"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_message_mentions_app_name() {
        assert!(placeholder_message().contains(APP_NAME));
    }
}
