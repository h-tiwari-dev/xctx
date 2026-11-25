use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::error::LayoutError;
use crate::geometry::{DividerRect, LayoutResult, PaneRect, Rect};

/// The smallest share a split can give its first side; ratios below it are raised to it.
pub const MIN_SPLIT_RATIO: f32 = 0.1;
/// The largest share a split can give its first side; ratios above it are lowered to it.
pub const MAX_SPLIT_RATIO: f32 = 0.9;

/// Identifies a pane. The app allocates these and keeps each pane's content keyed by its id; a tree never holds
/// the same id twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PaneId(pub u64);

/// Identifies a split (and so its divider) within one tree. The tree allocates these; use them with
/// [`PaneTree::set_split_ratio`] when a divider is dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SplitId(pub u64);

/// How a split places its two sides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SplitAxis {
    /// Side by side: the first side on the left, the second on the right, a vertical divider between them.
    Horizontal,
    /// Stacked: the first side on top, the second below, a horizontal divider between them.
    Vertical,
}

/// A direction to move focus in, by where panes sit on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FocusDirection {
    /// Towards smaller x.
    Left,
    /// Towards larger x.
    Right,
    /// Towards smaller y.
    Up,
    /// Towards larger y.
    Down,
}

/// A node of the binary split tree: a pane, or a split of two subtrees.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaneNode {
    /// A single pane.
    Leaf {
        /// The pane shown here.
        pane: PaneId,
    },
    /// Two subtrees sharing a rectangle.
    Split {
        /// This split's id, unique in the tree.
        id: SplitId,
        /// How the two sides are placed.
        axis: SplitAxis,
        /// The first side's share of the space left after the divider, between [`MIN_SPLIT_RATIO`] and
        /// [`MAX_SPLIT_RATIO`].
        ratio: f32,
        /// The left (horizontal) or top (vertical) side.
        first: Box<PaneNode>,
        /// The right (horizontal) or bottom (vertical) side.
        second: Box<PaneNode>,
    },
}

impl PaneNode {
    fn collect_panes(&self, out: &mut Vec<PaneId>) {
        match self {
            Self::Leaf { pane } => out.push(*pane),
            Self::Split { first, second, .. } => {
                first.collect_panes(out);
                second.collect_panes(out);
            }
        }
    }

    fn collect_splits(&self, out: &mut Vec<(SplitId, f32)>) {
        if let Self::Split {
            id,
            ratio,
            first,
            second,
            ..
        } = self
        {
            out.push((*id, *ratio));
            first.collect_splits(out);
            second.collect_splits(out);
        }
    }

    fn contains(&self, target: PaneId) -> bool {
        match self {
            Self::Leaf { pane } => *pane == target,
            Self::Split { first, second, .. } => first.contains(target) || second.contains(target),
        }
    }

    fn first_pane(&self) -> PaneId {
        match self {
            Self::Leaf { pane } => *pane,
            Self::Split { first, .. } => first.first_pane(),
        }
    }

    fn last_pane(&self) -> PaneId {
        match self {
            Self::Leaf { pane } => *pane,
            Self::Split { second, .. } => second.last_pane(),
        }
    }

    fn leaf_mut(&mut self, target: PaneId) -> Option<&mut PaneNode> {
        match self {
            Self::Leaf { pane } if *pane == target => Some(self),
            Self::Leaf { .. } => None,
            Self::Split { first, second, .. } => match first.leaf_mut(target) {
                Some(leaf) => Some(leaf),
                None => second.leaf_mut(target),
            },
        }
    }

    fn ratio_mut(&mut self, split: SplitId) -> Option<&mut f32> {
        match self {
            Self::Leaf { .. } => None,
            Self::Split {
                id,
                ratio,
                first,
                second,
                ..
            } => {
                if *id == split {
                    Some(ratio)
                } else {
                    match first.ratio_mut(split) {
                        Some(r) => Some(r),
                        None => second.ratio_mut(split),
                    }
                }
            }
        }
    }

    /// The deepest split along `axis` that contains `target`, and whether `target` is on its first side.
    fn nearest_split(&self, target: PaneId, axis: SplitAxis) -> Option<(SplitId, bool)> {
        let Self::Split {
            id,
            axis: split_axis,
            first,
            second,
            ..
        } = self
        else {
            return None;
        };
        let in_first = if first.contains(target) {
            true
        } else if second.contains(target) {
            false
        } else {
            return None;
        };
        let side = if in_first { first } else { second };
        side.nearest_split(target, axis)
            .or_else(|| (*split_axis == axis).then_some((*id, in_first)))
    }

    /// This node without the leaf `target`, and the pane next to it that took its place. `None` when this node
    /// is that leaf.
    fn without(self, target: PaneId) -> Option<(PaneNode, Option<PaneId>)> {
        match self {
            Self::Leaf { pane } if pane == target => None,
            Self::Leaf { .. } => Some((self, None)),
            Self::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => {
                if first.contains(target) {
                    match first.without(target) {
                        None => {
                            let heir = second.first_pane();
                            Some((*second, Some(heir)))
                        }
                        Some((rest, heir)) => Some((
                            Self::Split {
                                id,
                                axis,
                                ratio,
                                first: Box::new(rest),
                                second,
                            },
                            heir,
                        )),
                    }
                } else {
                    match second.without(target) {
                        None => {
                            let heir = first.last_pane();
                            Some((*first, Some(heir)))
                        }
                        Some((rest, heir)) => Some((
                            Self::Split {
                                id,
                                axis,
                                ratio,
                                first,
                                second: Box::new(rest),
                            },
                            heir,
                        )),
                    }
                }
            }
        }
    }

    fn layout(&self, rect: Rect, divider: f32, focused: PaneId, out: &mut LayoutResult) {
        match self {
            Self::Leaf { pane } => out.panes.push(PaneRect {
                id: *pane,
                rect,
                focused: *pane == focused,
            }),
            Self::Split {
                id,
                axis,
                ratio,
                first,
                second,
            } => {
                let (first_rect, divider_rect, second_rect) = match axis {
                    SplitAxis::Horizontal => {
                        let gap = divider.min(rect.width);
                        let avail = rect.width - gap;
                        let first_width = avail * ratio;
                        (
                            Rect::new(rect.x, rect.y, first_width, rect.height),
                            Rect::new(rect.x + first_width, rect.y, gap, rect.height),
                            Rect::new(
                                rect.x + first_width + gap,
                                rect.y,
                                avail - first_width,
                                rect.height,
                            ),
                        )
                    }
                    SplitAxis::Vertical => {
                        let gap = divider.min(rect.height);
                        let avail = rect.height - gap;
                        let first_height = avail * ratio;
                        (
                            Rect::new(rect.x, rect.y, rect.width, first_height),
                            Rect::new(rect.x, rect.y + first_height, rect.width, gap),
                            Rect::new(
                                rect.x,
                                rect.y + first_height + gap,
                                rect.width,
                                avail - first_height,
                            ),
                        )
                    }
                };
                out.dividers.push(DividerRect {
                    split_id: *id,
                    axis: *axis,
                    rect: divider_rect,
                    ratio: *ratio,
                    split_rect: rect,
                });
                first.layout(first_rect, divider, focused, out);
                second.layout(second_rect, divider, focused, out);
            }
        }
    }
}

/// A space's panes: a binary split tree with one focused pane and, optionally, one zoomed pane.
///
/// A tree always has at least one pane, the focused pane is always in it, and a zoomed pane is always the
/// focused one. Moving focus to another pane or splitting a pane ends the zoom. Every failing operation returns a
/// [`LayoutError`] and leaves the tree unchanged.
///
/// Serializes to JSON as `{ "root", "focused", "zoomed" }`; deserializing checks those invariants and fails
/// with the [`LayoutError`] message when they do not hold.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "PaneTreeRepr", into = "PaneTreeRepr")]
pub struct PaneTree {
    root: PaneNode,
    focused: PaneId,
    zoomed: Option<PaneId>,
}

#[derive(Serialize, Deserialize)]
struct PaneTreeRepr {
    root: PaneNode,
    focused: PaneId,
    #[serde(default)]
    zoomed: Option<PaneId>,
}

impl TryFrom<PaneTreeRepr> for PaneTree {
    type Error = LayoutError;

    fn try_from(repr: PaneTreeRepr) -> Result<Self, Self::Error> {
        Self::from_parts(repr.root, repr.focused, repr.zoomed)
    }
}

impl From<PaneTree> for PaneTreeRepr {
    fn from(tree: PaneTree) -> Self {
        Self {
            root: tree.root,
            focused: tree.focused,
            zoomed: tree.zoomed,
        }
    }
}

/// `ratio` clamped to the allowed range, or an error when it is not a number.
fn checked_ratio(ratio: f32) -> Result<f32, LayoutError> {
    if ratio.is_finite() {
        Ok(ratio.clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO))
    } else {
        Err(LayoutError::InvalidRatio(ratio))
    }
}

/// Length of the overlap of `[a0, a1]` and `[b0, b1]`; zero or negative when they do not overlap.
fn overlap(a0: f32, a1: f32, b0: f32, b1: f32) -> f32 {
    a1.min(b1) - a0.max(b0)
}

impl PaneTree {
    /// A tree of one pane, focused and not zoomed.
    pub fn new(root_pane: PaneId) -> Self {
        Self {
            root: PaneNode::Leaf { pane: root_pane },
            focused: root_pane,
            zoomed: None,
        }
    }

    /// A tree from its parts, after checking every invariant: unique pane and split ids, ratios in range, the
    /// focused pane in the tree, and the zoomed pane (if any) being the focused one.
    pub(crate) fn from_parts(
        root: PaneNode,
        focused: PaneId,
        zoomed: Option<PaneId>,
    ) -> Result<Self, LayoutError> {
        let mut panes = Vec::new();
        root.collect_panes(&mut panes);
        let mut seen = HashSet::new();
        for pane in &panes {
            if !seen.insert(*pane) {
                return Err(LayoutError::DuplicatePane(*pane));
            }
        }
        let mut splits = Vec::new();
        root.collect_splits(&mut splits);
        let mut seen_splits = HashSet::new();
        for (id, ratio) in splits {
            if !seen_splits.insert(id) {
                return Err(LayoutError::DuplicateSplit(id));
            }
            if !(MIN_SPLIT_RATIO..=MAX_SPLIT_RATIO).contains(&ratio) {
                return Err(LayoutError::InvalidRatio(ratio));
            }
        }
        if !seen.contains(&focused) {
            return Err(LayoutError::UnknownPane(focused));
        }
        if let Some(z) = zoomed
            && z != focused
        {
            return Err(LayoutError::InvalidZoom(z));
        }
        Ok(Self {
            root,
            focused,
            zoomed,
        })
    }

    /// The root of the split tree, for walking its structure.
    pub fn root(&self) -> &PaneNode {
        &self.root
    }

    /// Every pane, in leaf order: left to right and top to bottom, as [`PaneTree::focus_next`] visits them.
    pub fn panes(&self) -> Vec<PaneId> {
        let mut out = Vec::new();
        self.root.collect_panes(&mut out);
        out
    }

    /// The focused pane.
    pub fn focused(&self) -> PaneId {
        self.focused
    }

    /// The zoomed pane, if one is zoomed.
    pub fn zoomed(&self) -> Option<PaneId> {
        self.zoomed
    }

    /// Whether a pane is zoomed to fill the space.
    pub fn is_zoomed(&self) -> bool {
        self.zoomed.is_some()
    }

    /// Whether `pane` is in the tree.
    pub fn contains(&self, pane: PaneId) -> bool {
        self.root.contains(pane)
    }

    /// Split `target` in two along `axis`: `target` keeps the first side (left or top) with `ratio` of the
    /// space, and `new_pane` takes the second side and the focus. Ends any zoom. `ratio` is clamped to
    /// [`MIN_SPLIT_RATIO`]..=[`MAX_SPLIT_RATIO`]. Returns the new split's id.
    pub fn split(
        &mut self,
        target: PaneId,
        axis: SplitAxis,
        new_pane: PaneId,
        ratio: f32,
    ) -> Result<SplitId, LayoutError> {
        let ratio = checked_ratio(ratio)?;
        if self.contains(new_pane) {
            return Err(LayoutError::DuplicatePane(new_pane));
        }
        let id = self.fresh_split_id();
        let slot = self
            .root
            .leaf_mut(target)
            .ok_or(LayoutError::UnknownPane(target))?;
        *slot = PaneNode::Split {
            id,
            axis,
            ratio,
            first: Box::new(PaneNode::Leaf { pane: target }),
            second: Box::new(PaneNode::Leaf { pane: new_pane }),
        };
        self.focused = new_pane;
        self.zoomed = None;
        Ok(id)
    }

    /// Split the whole tree in two along `axis`: everything there is now keeps the first side (left or top) with
    /// `ratio` of the space, and `new_pane` takes the second side and the focus. Ends any zoom. `ratio` is
    /// clamped to [`MIN_SPLIT_RATIO`]..=[`MAX_SPLIT_RATIO`]. Returns the new split's id.
    pub fn split_root(
        &mut self,
        axis: SplitAxis,
        new_pane: PaneId,
        ratio: f32,
    ) -> Result<SplitId, LayoutError> {
        let ratio = checked_ratio(ratio)?;
        if self.contains(new_pane) {
            return Err(LayoutError::DuplicatePane(new_pane));
        }
        let id = self.fresh_split_id();
        let old = std::mem::replace(&mut self.root, PaneNode::Leaf { pane: new_pane });
        self.root = PaneNode::Split {
            id,
            axis,
            ratio,
            first: Box::new(old),
            second: Box::new(PaneNode::Leaf { pane: new_pane }),
        };
        self.focused = new_pane;
        self.zoomed = None;
        Ok(id)
    }

    /// Remove `target`; its sibling takes its space. If `target` was focused, focus moves to the pane that was
    /// next to it (the sibling's nearest pane), and if it was zoomed the zoom ends. Returns the focused pane
    /// afterwards. Fails with [`LayoutError::LastPane`] when `target` is the only pane.
    pub fn close(&mut self, target: PaneId) -> Result<PaneId, LayoutError> {
        if !self.contains(target) {
            return Err(LayoutError::UnknownPane(target));
        }
        let root = std::mem::replace(&mut self.root, PaneNode::Leaf { pane: target });
        let Some((root, heir)) = root.without(target) else {
            // The root was the leaf `target` itself, which the placeholder already restores.
            return Err(LayoutError::LastPane(target));
        };
        self.root = root;
        if self.focused == target {
            self.focused = heir.unwrap_or_else(|| self.root.first_pane());
        }
        if self.zoomed == Some(target) {
            self.zoomed = None;
        }
        Ok(self.focused)
    }

    /// Focus `pane`. Ends the zoom if another pane was zoomed.
    pub fn focus(&mut self, pane: PaneId) -> Result<(), LayoutError> {
        if !self.contains(pane) {
            return Err(LayoutError::UnknownPane(pane));
        }
        if self.zoomed.is_some_and(|z| z != pane) {
            self.zoomed = None;
        }
        self.focused = pane;
        Ok(())
    }

    /// Focus the pane next to the focused one in `direction`, by their rectangles in the unzoomed layout. Among
    /// several neighbours on that side it picks the nearest, then the one sharing the longest edge, then the
    /// first in leaf order. Returns the newly focused pane, or `None` (and changes nothing) when there is no
    /// pane that way. Moving ends any zoom.
    pub fn move_focus(&mut self, direction: FocusDirection) -> Option<PaneId> {
        let next = self.neighbor(self.focused, direction)?;
        self.zoomed = None;
        self.focused = next;
        Some(next)
    }

    /// Focus the next pane in leaf order, wrapping around. Ends the zoom if focus moves. Returns the focused pane.
    pub fn focus_next(&mut self) -> PaneId {
        self.cycle_focus(true)
    }

    /// Focus the previous pane in leaf order, wrapping around. Ends the zoom if focus moves. Returns the focused
    /// pane.
    pub fn focus_prev(&mut self) -> PaneId {
        self.cycle_focus(false)
    }

    /// Grow `pane` by `delta` (a fraction of the split, negative to shrink) along `axis`, by moving the divider
    /// of the nearest split along that axis that contains it. The ratio is clamped to
    /// [`MIN_SPLIT_RATIO`]..=[`MAX_SPLIT_RATIO`]. Returns the split that moved.
    pub fn resize(
        &mut self,
        pane: PaneId,
        axis: SplitAxis,
        delta: f32,
    ) -> Result<SplitId, LayoutError> {
        if !delta.is_finite() {
            return Err(LayoutError::InvalidDelta(delta));
        }
        if !self.contains(pane) {
            return Err(LayoutError::UnknownPane(pane));
        }
        let (split, in_first) = self
            .root
            .nearest_split(pane, axis)
            .ok_or(LayoutError::NoSplitOnAxis { pane, axis })?;
        let ratio = self
            .root
            .ratio_mut(split)
            .ok_or(LayoutError::UnknownSplit(split))?;
        let signed = if in_first { delta } else { -delta };
        *ratio = (*ratio + signed).clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO);
        Ok(split)
    }

    /// Set the first side's share of `split_id`, clamped to [`MIN_SPLIT_RATIO`]..=[`MAX_SPLIT_RATIO`]. This is
    /// what dragging a [`DividerRect`] does.
    pub fn set_split_ratio(&mut self, split_id: SplitId, ratio: f32) -> Result<(), LayoutError> {
        let ratio = checked_ratio(ratio)?;
        let slot = self
            .root
            .ratio_mut(split_id)
            .ok_or(LayoutError::UnknownSplit(split_id))?;
        *slot = ratio;
        Ok(())
    }

    /// Zoom the focused pane, or end the zoom if one is zoomed. Returns whether a pane is zoomed afterwards.
    pub fn toggle_zoom(&mut self) -> bool {
        self.zoomed = match self.zoomed {
            Some(_) => None,
            None => Some(self.focused),
        };
        self.zoomed.is_some()
    }

    /// Zoom `pane` (which also focuses it), or end the zoom with `None`.
    pub fn set_zoom(&mut self, pane: Option<PaneId>) -> Result<(), LayoutError> {
        if let Some(p) = pane {
            if !self.contains(p) {
                return Err(LayoutError::UnknownPane(p));
            }
            self.focused = p;
        }
        self.zoomed = pane;
        Ok(())
    }

    /// The rectangles of the visible panes and dividers inside `bounds`. Each split puts a divider
    /// `divider_thickness` wide (negative or non-finite counts as zero) between its sides and shares the rest by
    /// its ratio. When a pane is zoomed it alone fills `bounds` and there are no dividers.
    pub fn layout(&self, bounds: Rect, divider_thickness: f32) -> LayoutResult {
        match self.zoomed {
            Some(pane) => LayoutResult {
                panes: vec![PaneRect {
                    id: pane,
                    rect: Self::sanitized(bounds),
                    focused: pane == self.focused,
                }],
                dividers: Vec::new(),
            },
            None => self.unzoomed_layout(bounds, divider_thickness),
        }
    }

    fn sanitized(bounds: Rect) -> Rect {
        Rect::new(
            bounds.x,
            bounds.y,
            bounds.width.max(0.0),
            bounds.height.max(0.0),
        )
    }

    fn unzoomed_layout(&self, bounds: Rect, divider_thickness: f32) -> LayoutResult {
        let divider = if divider_thickness.is_finite() {
            divider_thickness.max(0.0)
        } else {
            0.0
        };
        let mut out = LayoutResult::default();
        self.root
            .layout(Self::sanitized(bounds), divider, self.focused, &mut out);
        out
    }

    fn neighbor(&self, from: PaneId, direction: FocusDirection) -> Option<PaneId> {
        const EPSILON: f32 = 1e-4;
        let layout = self.unzoomed_layout(Rect::new(0.0, 0.0, 1.0, 1.0), 0.0);
        let origin = layout.pane(from)?;
        let mut best: Option<(f32, f32, PaneId)> = None;
        for candidate in layout.panes.iter().filter(|p| p.id != from) {
            let r = candidate.rect;
            let (distance, shared) = match direction {
                FocusDirection::Left => (
                    origin.x - r.right(),
                    overlap(origin.y, origin.bottom(), r.y, r.bottom()),
                ),
                FocusDirection::Right => (
                    r.x - origin.right(),
                    overlap(origin.y, origin.bottom(), r.y, r.bottom()),
                ),
                FocusDirection::Up => (
                    origin.y - r.bottom(),
                    overlap(origin.x, origin.right(), r.x, r.right()),
                ),
                FocusDirection::Down => (
                    r.y - origin.bottom(),
                    overlap(origin.x, origin.right(), r.x, r.right()),
                ),
            };
            if distance < -EPSILON || shared <= EPSILON {
                continue;
            }
            let better = match best {
                None => true,
                Some((best_distance, best_shared, _)) => {
                    distance < best_distance - EPSILON
                        || ((distance - best_distance).abs() <= EPSILON
                            && shared > best_shared + EPSILON)
                }
            };
            if better {
                best = Some((distance, shared, candidate.id));
            }
        }
        best.map(|(_, _, id)| id)
    }

    fn cycle_focus(&mut self, forward: bool) -> PaneId {
        let panes = self.panes();
        let count = panes.len();
        if count < 2 {
            return self.focused;
        }
        let index = panes.iter().position(|p| *p == self.focused).unwrap_or(0);
        let next_index = if forward {
            (index + 1) % count
        } else {
            (index + count - 1) % count
        };
        if let Some(next) = panes.get(next_index).copied()
            && next != self.focused
        {
            self.zoomed = None;
            self.focused = next;
        }
        self.focused
    }

    /// A split id not used in the tree.
    fn fresh_split_id(&self) -> SplitId {
        let mut splits = Vec::new();
        self.root.collect_splits(&mut splits);
        let used: HashSet<SplitId> = splits.iter().map(|(id, _)| *id).collect();
        let max = used.iter().map(|id| id.0).max();
        match max.map_or(Some(0), |m| m.checked_add(1)) {
            Some(next) => SplitId(next),
            None => SplitId(
                (0..=u64::MAX)
                    .find(|n| !used.contains(&SplitId(*n)))
                    .unwrap_or(0),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: PaneId = PaneId(1);
    const P2: PaneId = PaneId(2);
    const P3: PaneId = PaneId(3);
    const P4: PaneId = PaneId(4);

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    fn assert_rect(actual: Rect, x: f32, y: f32, width: f32, height: f32) {
        assert!(
            approx(actual.x, x)
                && approx(actual.y, y)
                && approx(actual.width, width)
                && approx(actual.height, height),
            "{actual:?} != ({x}, {y}, {width}, {height})"
        );
    }

    fn split_ratio(tree: &PaneTree, split: SplitId) -> f32 {
        let mut splits = Vec::new();
        tree.root().collect_splits(&mut splits);
        splits
            .into_iter()
            .find(|(id, _)| *id == split)
            .map(|(_, r)| r)
            .unwrap_or(f32::NAN)
    }

    /// 1 | 2, then 1 / 3 and 2 / 4: a 2x2 grid with 1 top-left, 2 top-right, 3 bottom-left, 4 bottom-right.
    fn grid() -> PaneTree {
        let mut tree = PaneTree::new(P1);
        tree.split(P1, SplitAxis::Horizontal, P2, 0.5).unwrap();
        tree.split(P1, SplitAxis::Vertical, P3, 0.5).unwrap();
        tree.split(P2, SplitAxis::Vertical, P4, 0.5).unwrap();
        tree
    }

    /// 1 | (2 / 3).
    fn one_beside_two() -> (PaneTree, SplitId, SplitId) {
        let mut tree = PaneTree::new(P1);
        let outer = tree.split(P1, SplitAxis::Horizontal, P2, 0.5).unwrap();
        let inner = tree.split(P2, SplitAxis::Vertical, P3, 0.5).unwrap();
        (tree, outer, inner)
    }

    #[test]
    fn new_tree_has_one_focused_pane() {
        let tree = PaneTree::new(P1);
        assert_eq!(tree.panes(), vec![P1]);
        assert_eq!(tree.focused(), P1);
        assert!(tree.contains(P1));
        assert!(!tree.contains(P2));
        assert!(!tree.is_zoomed());
    }

    #[test]
    fn split_puts_new_pane_second_and_focuses_it() {
        let mut tree = PaneTree::new(P1);
        let split = tree.split(P1, SplitAxis::Horizontal, P2, 0.3).unwrap();
        assert_eq!(tree.panes(), vec![P1, P2]);
        assert_eq!(tree.focused(), P2);
        assert_eq!(
            tree.root(),
            &PaneNode::Split {
                id: split,
                axis: SplitAxis::Horizontal,
                ratio: 0.3,
                first: Box::new(PaneNode::Leaf { pane: P1 }),
                second: Box::new(PaneNode::Leaf { pane: P2 }),
            }
        );
        let other = tree.split(P1, SplitAxis::Vertical, P3, 0.5).unwrap();
        assert_ne!(split, other);
        assert_eq!(tree.panes(), vec![P1, P3, P2]);
    }

    #[test]
    fn split_root_puts_the_whole_tree_first_and_the_new_pane_second() {
        let (mut tree, outer, inner) = one_beside_two();
        tree.set_zoom(Some(P3)).unwrap();
        let root = tree.split_root(SplitAxis::Horizontal, P4, 0.65).unwrap();
        assert!(root != outer && root != inner);
        assert_eq!(tree.panes(), vec![P1, P2, P3, P4]);
        assert_eq!(tree.focused(), P4);
        assert!(!tree.is_zoomed());
        assert!(approx(split_ratio(&tree, root), 0.65));
        let layout = tree.layout(Rect::new(0.0, 0.0, 1000.0, 500.0), 0.0);
        let p4 = layout.panes.iter().find(|p| p.id == P4).map(|p| p.rect);
        assert_rect(p4.unwrap(), 650.0, 0.0, 350.0, 500.0);
        let before = tree.clone();
        assert_eq!(
            tree.split_root(SplitAxis::Vertical, P2, 0.5),
            Err(LayoutError::DuplicatePane(P2))
        );
        assert!(matches!(
            tree.split_root(SplitAxis::Vertical, PaneId(9), f32::NAN),
            Err(LayoutError::InvalidRatio(_))
        ));
        assert_eq!(tree, before);
    }

    #[test]
    fn split_clamps_ratio_and_rejects_bad_input() {
        let mut tree = PaneTree::new(P1);
        let split = tree.split(P1, SplitAxis::Horizontal, P2, 0.0).unwrap();
        assert_eq!(split_ratio(&tree, split), MIN_SPLIT_RATIO);
        let before = tree.clone();
        assert_eq!(
            tree.split(P4, SplitAxis::Horizontal, P3, 0.5),
            Err(LayoutError::UnknownPane(P4))
        );
        assert_eq!(
            tree.split(P1, SplitAxis::Horizontal, P2, 0.5),
            Err(LayoutError::DuplicatePane(P2))
        );
        assert!(matches!(
            tree.split(P1, SplitAxis::Horizontal, P3, f32::NAN),
            Err(LayoutError::InvalidRatio(_))
        ));
        assert_eq!(tree, before);
    }

    #[test]
    fn close_last_pane_is_refused() {
        let mut tree = PaneTree::new(P1);
        assert_eq!(tree.close(P1), Err(LayoutError::LastPane(P1)));
        assert_eq!(tree, PaneTree::new(P1));
        assert_eq!(tree.close(P2), Err(LayoutError::UnknownPane(P2)));
    }

    #[test]
    fn close_child_of_root_collapses_to_sibling() {
        let mut tree = PaneTree::new(P1);
        tree.split(P1, SplitAxis::Horizontal, P2, 0.5).unwrap();
        assert_eq!(tree.close(P2), Ok(P1));
        assert_eq!(tree, PaneTree::new(P1));
    }

    #[test]
    fn close_in_nested_split_keeps_outer_split() {
        let (mut tree, outer, _) = one_beside_two();
        tree.set_split_ratio(outer, 0.3).unwrap();
        tree.focus(P1).unwrap();
        assert_eq!(tree.close(P2), Ok(P1));
        assert_eq!(
            tree.root(),
            &PaneNode::Split {
                id: outer,
                axis: SplitAxis::Horizontal,
                ratio: 0.3,
                first: Box::new(PaneNode::Leaf { pane: P1 }),
                second: Box::new(PaneNode::Leaf { pane: P3 }),
            }
        );
    }

    #[test]
    fn closing_focused_pane_focuses_its_neighbour() {
        // Closing the second side focuses the last pane of the first side.
        let (mut tree, _, _) = one_beside_two();
        assert_eq!(tree.focused(), P3);
        assert_eq!(tree.close(P3), Ok(P2));
        // Closing the first side focuses the first pane of the second side.
        let (mut tree, _, _) = one_beside_two();
        tree.focus(P1).unwrap();
        assert_eq!(tree.close(P1), Ok(P2));
        assert_eq!(tree.panes(), vec![P2, P3]);
    }

    #[test]
    fn close_of_zoomed_pane_ends_zoom_but_other_close_keeps_it() {
        let (mut tree, _, _) = one_beside_two();
        tree.set_zoom(Some(P3)).unwrap();
        tree.close(P1).unwrap();
        assert_eq!(tree.zoomed(), Some(P3));
        tree.close(P3).unwrap();
        assert_eq!(tree.zoomed(), None);
        assert_eq!(tree.focused(), P2);
    }

    #[test]
    fn move_focus_in_all_four_directions() {
        let mut tree = grid();
        tree.focus(P1).unwrap();
        assert_eq!(tree.move_focus(FocusDirection::Left), None);
        assert_eq!(tree.move_focus(FocusDirection::Up), None);
        assert_eq!(tree.focused(), P1);
        assert_eq!(tree.move_focus(FocusDirection::Right), Some(P2));
        assert_eq!(tree.move_focus(FocusDirection::Down), Some(P4));
        assert_eq!(tree.move_focus(FocusDirection::Left), Some(P3));
        assert_eq!(tree.move_focus(FocusDirection::Up), Some(P1));
        assert_eq!(tree.move_focus(FocusDirection::Down), Some(P3));
        assert_eq!(tree.move_focus(FocusDirection::Down), None);
        assert_eq!(tree.move_focus(FocusDirection::Right), Some(P4));
        assert_eq!(tree.move_focus(FocusDirection::Right), None);
    }

    #[test]
    fn move_focus_prefers_longest_shared_edge() {
        // 1 | (2 / 3) with 2 taking most of the height: from 1, right goes to 2.
        let (mut tree, _, inner) = one_beside_two();
        tree.set_split_ratio(inner, 0.7).unwrap();
        tree.focus(P1).unwrap();
        assert_eq!(tree.move_focus(FocusDirection::Right), Some(P2));
        tree.set_split_ratio(inner, 0.3).unwrap();
        tree.focus(P1).unwrap();
        assert_eq!(tree.move_focus(FocusDirection::Right), Some(P3));
        // Equal edges fall back to leaf order.
        tree.set_split_ratio(inner, 0.5).unwrap();
        tree.focus(P1).unwrap();
        assert_eq!(tree.move_focus(FocusDirection::Right), Some(P2));
        // From the bottom-right pane, left goes to the full-height pane.
        tree.focus(P3).unwrap();
        assert_eq!(tree.move_focus(FocusDirection::Left), Some(P1));
    }

    #[test]
    fn move_focus_ends_zoom_only_when_it_moves() {
        let mut tree = grid();
        tree.focus(P1).unwrap();
        tree.toggle_zoom();
        assert_eq!(tree.move_focus(FocusDirection::Left), None);
        assert!(tree.is_zoomed());
        assert_eq!(tree.move_focus(FocusDirection::Right), Some(P2));
        assert!(!tree.is_zoomed());
    }

    #[test]
    fn focus_next_and_prev_wrap_in_leaf_order() {
        let mut tree = grid();
        assert_eq!(tree.panes(), vec![P1, P3, P2, P4]);
        tree.focus(P1).unwrap();
        assert_eq!(tree.focus_next(), P3);
        assert_eq!(tree.focus_next(), P2);
        assert_eq!(tree.focus_next(), P4);
        assert_eq!(tree.focus_next(), P1);
        assert_eq!(tree.focus_prev(), P4);
        assert_eq!(tree.focus_prev(), P2);
        let mut single = PaneTree::new(P1);
        assert_eq!(single.focus_next(), P1);
        assert_eq!(single.focus_prev(), P1);
    }

    #[test]
    fn focus_rejects_unknown_pane_and_ends_zoom_of_other_pane() {
        let mut tree = grid();
        assert_eq!(
            tree.focus(PaneId(9)),
            Err(LayoutError::UnknownPane(PaneId(9)))
        );
        tree.set_zoom(Some(P2)).unwrap();
        tree.focus(P2).unwrap();
        assert!(tree.is_zoomed());
        tree.focus(P1).unwrap();
        assert!(!tree.is_zoomed());
        assert_eq!(tree.focused(), P1);
    }

    #[test]
    fn resize_moves_nearest_split_on_axis_and_clamps() {
        let mut tree = PaneTree::new(P1);
        let split = tree.split(P1, SplitAxis::Horizontal, P2, 0.5).unwrap();
        assert_eq!(tree.resize(P1, SplitAxis::Horizontal, 0.2), Ok(split));
        assert!(approx(split_ratio(&tree, split), 0.7));
        // Growing the second side shrinks the first.
        tree.resize(P2, SplitAxis::Horizontal, 0.2).unwrap();
        assert!(approx(split_ratio(&tree, split), 0.5));
        tree.resize(P1, SplitAxis::Horizontal, 5.0).unwrap();
        assert_eq!(split_ratio(&tree, split), MAX_SPLIT_RATIO);
        tree.resize(P1, SplitAxis::Horizontal, -5.0).unwrap();
        assert_eq!(split_ratio(&tree, split), MIN_SPLIT_RATIO);
        assert_eq!(
            tree.resize(P1, SplitAxis::Vertical, 0.1),
            Err(LayoutError::NoSplitOnAxis {
                pane: P1,
                axis: SplitAxis::Vertical
            })
        );
        assert!(matches!(
            tree.resize(P1, SplitAxis::Horizontal, f32::INFINITY),
            Err(LayoutError::InvalidDelta(_))
        ));
        assert_eq!(
            tree.resize(P3, SplitAxis::Horizontal, 0.1),
            Err(LayoutError::UnknownPane(P3))
        );
    }

    #[test]
    fn resize_in_nested_split_finds_matching_ancestor() {
        let (mut tree, outer, inner) = one_beside_two();
        assert_eq!(tree.resize(P3, SplitAxis::Vertical, 0.1), Ok(inner));
        assert!(approx(split_ratio(&tree, inner), 0.4));
        assert_eq!(tree.resize(P3, SplitAxis::Horizontal, 0.1), Ok(outer));
        assert!(approx(split_ratio(&tree, outer), 0.4));
    }

    #[test]
    fn set_split_ratio_clamps_and_rejects_unknown_split() {
        let (mut tree, outer, _) = one_beside_two();
        tree.set_split_ratio(outer, 0.95).unwrap();
        assert_eq!(split_ratio(&tree, outer), MAX_SPLIT_RATIO);
        tree.set_split_ratio(outer, -1.0).unwrap();
        assert_eq!(split_ratio(&tree, outer), MIN_SPLIT_RATIO);
        assert_eq!(
            tree.set_split_ratio(SplitId(99), 0.5),
            Err(LayoutError::UnknownSplit(SplitId(99)))
        );
        assert!(matches!(
            tree.set_split_ratio(outer, f32::NAN),
            Err(LayoutError::InvalidRatio(_))
        ));
    }

    #[test]
    fn layout_places_panes_and_dividers_with_gap() {
        let (mut tree, outer, inner) = one_beside_two();
        tree.set_split_ratio(outer, 0.25).unwrap();
        let bounds = Rect::new(10.0, 20.0, 102.0, 52.0);
        let layout = tree.layout(bounds, 2.0);

        assert_eq!(
            layout.panes.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![P1, P2, P3]
        );
        assert_rect(layout.panes[0].rect, 10.0, 20.0, 25.0, 52.0);
        assert_rect(layout.panes[1].rect, 37.0, 20.0, 75.0, 25.0);
        assert_rect(layout.panes[2].rect, 37.0, 47.0, 75.0, 25.0);
        assert_eq!(
            layout.panes.iter().map(|p| p.focused).collect::<Vec<_>>(),
            vec![false, false, true]
        );

        assert_eq!(layout.dividers.len(), 2);
        let d0 = layout.dividers[0];
        assert_eq!(d0.split_id, outer);
        assert_eq!(d0.axis, SplitAxis::Horizontal);
        assert_eq!(d0.ratio, 0.25);
        assert_rect(d0.rect, 35.0, 20.0, 2.0, 52.0);
        assert_eq!(d0.split_rect, bounds);
        let d1 = layout.dividers[1];
        assert_eq!(d1.split_id, inner);
        assert_eq!(d1.axis, SplitAxis::Vertical);
        assert_rect(d1.rect, 37.0, 45.0, 75.0, 2.0);
        assert_rect(d1.split_rect, 37.0, 20.0, 75.0, 52.0);
    }

    #[test]
    fn layout_handles_degenerate_divider_and_bounds() {
        let mut tree = PaneTree::new(P1);
        tree.split(P1, SplitAxis::Horizontal, P2, 0.5).unwrap();
        let no_gap = tree.layout(Rect::new(0.0, 0.0, 100.0, 10.0), -3.0);
        assert_rect(no_gap.panes[0].rect, 0.0, 0.0, 50.0, 10.0);
        assert_rect(no_gap.dividers[0].rect, 50.0, 0.0, 0.0, 10.0);
        let nan_gap = tree.layout(Rect::new(0.0, 0.0, 100.0, 10.0), f32::NAN);
        assert_eq!(nan_gap, no_gap);
        let narrow = tree.layout(Rect::new(0.0, 0.0, 1.0, 10.0), 4.0);
        assert_rect(narrow.dividers[0].rect, 0.0, 0.0, 1.0, 10.0);
        assert_rect(narrow.panes[0].rect, 0.0, 0.0, 0.0, 10.0);
        assert_rect(narrow.panes[1].rect, 1.0, 0.0, 0.0, 10.0);
        let negative = tree.layout(Rect::new(0.0, 0.0, -5.0, 10.0), 0.0);
        assert!(negative.panes.iter().all(|p| p.rect.width >= 0.0));
    }

    #[test]
    fn zoom_lays_out_only_the_zoomed_pane() {
        let mut tree = grid();
        tree.focus(P4).unwrap();
        let bounds = Rect::new(0.0, 0.0, 200.0, 100.0);
        assert!(tree.toggle_zoom());
        assert_eq!(tree.zoomed(), Some(P4));
        let zoomed = tree.layout(bounds, 1.0);
        assert_eq!(
            zoomed.panes,
            vec![PaneRect {
                id: P4,
                rect: bounds,
                focused: true
            }]
        );
        assert!(zoomed.dividers.is_empty());

        assert!(!tree.toggle_zoom());
        let unzoomed = tree.layout(bounds, 1.0);
        assert_eq!(unzoomed.panes.len(), 4);
        assert_eq!(unzoomed.dividers.len(), 3);
    }

    #[test]
    fn set_zoom_focuses_pane_and_split_ends_zoom() {
        let mut tree = grid();
        tree.set_zoom(Some(P2)).unwrap();
        assert_eq!(tree.focused(), P2);
        assert_eq!(
            tree.set_zoom(Some(PaneId(9))),
            Err(LayoutError::UnknownPane(PaneId(9)))
        );
        assert_eq!(tree.zoomed(), Some(P2));
        tree.split(P2, SplitAxis::Horizontal, PaneId(5), 0.5)
            .unwrap();
        assert!(!tree.is_zoomed());
        tree.set_zoom(Some(P1)).unwrap();
        tree.set_zoom(None).unwrap();
        assert!(!tree.is_zoomed());
        assert_eq!(tree.focused(), P1);
    }

    #[test]
    fn split_ids_stay_unique_after_close() {
        let mut tree = grid();
        tree.close(P4).unwrap();
        let new = tree.split(P3, SplitAxis::Horizontal, P4, 0.5).unwrap();
        let mut splits = Vec::new();
        tree.root().collect_splits(&mut splits);
        let ids: HashSet<SplitId> = splits.iter().map(|(id, _)| *id).collect();
        assert_eq!(ids.len(), splits.len());
        assert!(ids.contains(&new));
    }

    #[test]
    fn tree_round_trips_through_json() {
        let mut tree = grid();
        tree.set_split_ratio(SplitId(0), 0.3).unwrap();
        tree.set_zoom(Some(P3)).unwrap();
        let json = serde_json::to_string(&tree).unwrap();
        let back: PaneTree = serde_json::from_str(&json).unwrap();
        assert_eq!(back, tree);
        assert_eq!(
            back.layout(Rect::new(0.0, 0.0, 10.0, 10.0), 0.0),
            tree.layout(Rect::new(0.0, 0.0, 10.0, 10.0), 0.0)
        );
    }

    #[test]
    fn tree_json_format_is_readable() {
        let mut tree = PaneTree::new(P1);
        tree.split(P1, SplitAxis::Vertical, P2, 0.5).unwrap();
        let value = serde_json::to_value(&tree).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "root": {
                    "type": "split",
                    "id": 0,
                    "axis": "vertical",
                    "ratio": 0.5,
                    "first": { "type": "leaf", "pane": 1 },
                    "second": { "type": "leaf", "pane": 2 }
                },
                "focused": 2,
                "zoomed": null
            })
        );
        let without_zoom: PaneTree = serde_json::from_value(serde_json::json!({
            "root": { "type": "leaf", "pane": 7 },
            "focused": 7
        }))
        .unwrap();
        assert_eq!(without_zoom, PaneTree::new(PaneId(7)));
    }

    #[test]
    fn deserializing_an_invalid_tree_fails() {
        let leaf = |pane: u64| serde_json::json!({ "type": "leaf", "pane": pane });
        let split = |id: u64, ratio: f32, a, b| {
            serde_json::json!({
                "type": "split", "id": id, "axis": "horizontal", "ratio": ratio, "first": a, "second": b
            })
        };
        let cases = [
            (
                serde_json::json!({ "root": leaf(1), "focused": 2 }),
                "pane 2 is not in the layout",
            ),
            (
                serde_json::json!({ "root": split(0, 0.5, leaf(1), leaf(1)), "focused": 1 }),
                "pane 1 is already in the layout",
            ),
            (
                serde_json::json!({
                    "root": split(0, 0.5, leaf(1), split(0, 0.5, leaf(2), leaf(3))),
                    "focused": 1
                }),
                "split 0 appears more than once",
            ),
            (
                serde_json::json!({ "root": split(0, 0.95, leaf(1), leaf(2)), "focused": 1 }),
                "split ratio 0.95",
            ),
            (
                serde_json::json!({
                    "root": split(0, 0.5, leaf(1), leaf(2)), "focused": 1, "zoomed": 2
                }),
                "zoomed pane 2",
            ),
        ];
        for (case, message) in cases {
            let result: Result<PaneTree, _> = serde_json::from_value(case.clone());
            match result {
                Ok(_) => panic!("accepted {case}"),
                Err(e) => assert!(e.to_string().contains(message), "{e} lacks {message:?}"),
            }
        }
    }
}
