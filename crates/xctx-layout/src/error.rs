use std::fmt;

use crate::tree::{MAX_SPLIT_RATIO, MIN_SPLIT_RATIO, PaneId, SplitAxis, SplitId};

/// Why a pane tree operation, a reshape, or loading a saved tree failed. The tree is unchanged when an operation
/// returns one of these.
#[derive(Debug, Clone, PartialEq)]
pub enum LayoutError {
    /// The pane is not in the tree.
    UnknownPane(PaneId),
    /// The pane id is already used by another pane.
    DuplicatePane(PaneId),
    /// The split is not in the tree.
    UnknownSplit(SplitId),
    /// The split id is used by more than one split (only in a saved tree).
    DuplicateSplit(SplitId),
    /// The pane is the only one left; a tree always has at least one pane.
    LastPane(PaneId),
    /// No split along `axis` contains the pane, so it cannot be resized along that axis.
    NoSplitOnAxis {
        /// The pane that was to be resized.
        pane: PaneId,
        /// The axis it was to be resized along.
        axis: SplitAxis,
    },
    /// A ratio was not a finite number, or (in a saved tree) was outside the allowed range.
    InvalidRatio(f32),
    /// A resize delta was not a finite number.
    InvalidDelta(f32),
    /// A saved tree zooms a pane that is missing or is not the focused pane.
    InvalidZoom(PaneId),
    /// A reshape would leave the space with no panes: no repos and no agents to keep.
    NoPanes,
}

impl fmt::Display for LayoutError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownPane(id) => write!(f, "pane {} is not in the layout", id.0),
            Self::DuplicatePane(id) => write!(f, "pane {} is already in the layout", id.0),
            Self::UnknownSplit(id) => write!(f, "split {} is not in the layout", id.0),
            Self::DuplicateSplit(id) => {
                write!(f, "split {} appears more than once in the layout", id.0)
            }
            Self::LastPane(id) => write!(f, "pane {} is the last pane and cannot be closed", id.0),
            Self::NoSplitOnAxis { pane, axis } => {
                write!(f, "pane {} has no {axis:?} split to resize", pane.0)
            }
            Self::InvalidRatio(r) => write!(
                f,
                "split ratio {r} is not a number between {MIN_SPLIT_RATIO} and {MAX_SPLIT_RATIO}"
            ),
            Self::InvalidDelta(d) => write!(f, "resize amount {d} is not a finite number"),
            Self::InvalidZoom(id) => write!(
                f,
                "zoomed pane {} is not the focused pane in the layout",
                id.0
            ),
            Self::NoPanes => write!(
                f,
                "the shape has no panes: the space has no repos and no agents"
            ),
        }
    }
}

impl std::error::Error for LayoutError {}
