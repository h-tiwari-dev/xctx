//! The pane tree of a space: split, close, focus, resize and zoom panes, compute their rectangles, and lay a
//! space out in one of its shapes (dev, debug, review) without losing its agents.
//!
//! The tree only knows pane ids. The app keeps what runs in each pane (a terminal, an agent) keyed by [`PaneId`],
//! and allocates those ids itself; the tree allocates [`SplitId`]s for its dividers.

#![warn(missing_docs)]

mod error;
mod geometry;
mod shape;
mod tree;

pub use error::LayoutError;
pub use geometry::{DividerRect, LayoutResult, PaneRect, Rect};
pub use shape::{PaneKind, PaneRole, Reshape, RolePane, Shape, reshape};
pub use tree::{
    FocusDirection, MAX_SPLIT_RATIO, MIN_SPLIT_RATIO, PaneId, PaneNode, PaneTree, SplitAxis,
    SplitId,
};
