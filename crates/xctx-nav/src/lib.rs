//! UI-free navigation models for the xctx shell: the fleet [`roster`] (spaces and agents grouped by
//! rolled-up state in shortcut order) and the command [`palette`] (prefix-filtered fuzzy matcher
//! with recent-command ordering).
//!
//! This crate never depends on GPUI so every rollup, ordering and fuzzy-matching rule can be tested
//! without a window.

#![warn(missing_docs)]

pub mod palette;
pub mod roster;

pub use palette::{
    DEFAULT_MAX_RECENTS, PaletteItem, PaletteKind, PaletteMatch, PalettePrefix, Recents,
    fuzzy_score, parse_query, search,
};
pub use roster::{
    AgentRow, Roster, RosterGroup, SpaceActivity, SpaceInput, SpaceRow, SpaceState,
    is_danger_inbox_item, is_pending_space_gate,
};
