use serde::{Deserialize, Serialize};

use crate::tree::{PaneId, SplitAxis, SplitId};

/// An axis-aligned rectangle; `x` grows rightwards and `y` downwards, in whatever unit the bounds were given in.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Rect {
    /// Left edge.
    pub x: f32,
    /// Top edge.
    pub y: f32,
    /// Width; never negative in a layout result.
    pub width: f32,
    /// Height; never negative in a layout result.
    pub height: f32,
}

impl Rect {
    /// A rectangle with its top-left corner at (`x`, `y`).
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Right edge (`x + width`).
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    /// Bottom edge (`y + height`).
    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }
}

/// Where a pane is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PaneRect {
    /// The pane.
    pub id: PaneId,
    /// Its rectangle inside the layout bounds.
    pub rect: Rect,
    /// Whether it is the focused pane.
    pub focused: bool,
}

/// The gap between the two sides of a split, where a drag handle goes. Dragging it should call
/// [`crate::PaneTree::set_split_ratio`] with `split_id`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct DividerRect {
    /// The split this divider belongs to.
    pub split_id: SplitId,
    /// The split's axis: a [`SplitAxis::Horizontal`] split has a vertical divider bar, and dragging it moves
    /// along x.
    pub axis: SplitAxis,
    /// The divider's rectangle; its thickness is the divider thickness given to the layout (or less, if the
    /// split is narrower than that).
    pub rect: Rect,
    /// The split's current ratio: the share of the first side.
    pub ratio: f32,
    /// The rectangle of the whole split (both sides and the divider), so a drag position can be turned into a
    /// ratio.
    pub split_rect: Rect,
}

/// The rectangles of every visible pane and divider of a tree, from [`crate::PaneTree::layout`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LayoutResult {
    /// Visible panes in leaf order (left to right, top to bottom). When a pane is zoomed this holds only that
    /// pane, filling the bounds.
    pub panes: Vec<PaneRect>,
    /// Dividers, each split before the splits nested inside it. Empty when a pane is zoomed.
    pub dividers: Vec<DividerRect>,
}

impl LayoutResult {
    /// The rectangle of `id`, if it is visible.
    pub fn pane(&self, id: PaneId) -> Option<Rect> {
        self.panes.iter().find(|p| p.id == id).map(|p| p.rect)
    }
}
