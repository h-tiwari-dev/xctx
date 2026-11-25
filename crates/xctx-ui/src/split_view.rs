//! Draws an `xctx-layout` [`PaneTree`]: each visible pane in a frame with a header, and draggable dividers
//! between split panes.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, CursorStyle, DispatchPhase, Edges, Element, ElementId,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, SharedString, Style, StyleRefinement,
    Window, div, fill, point, prelude::*, px, size,
};
use xctx_layout::{
    DividerRect, MAX_SPLIT_RATIO, MIN_SPLIT_RATIO, PaneId, PaneKind, PaneRect, PaneTree, Rect,
    SplitAxis, SplitId,
};

use crate::components::{Badge, Button, ButtonSize, ButtonVariant};
use crate::theme::{Theme, Tone};

/// The word a pane header's role badge shows for `kind`.
pub fn pane_kind_label(kind: PaneKind) -> &'static str {
    match kind {
        PaneKind::Agent => "agent",
        PaneKind::Shell => "shell",
        PaneKind::Git => "git",
        // The plugin's own name and badge come with the Plugins menu (Milestone 5, Wave 2).
        PaneKind::Plugin => "plugin",
    }
}

/// The ratio `divider`'s split takes when its divider is dragged so that its centre is at (`x`, `y`), in the
/// same coordinates as the layout. Only the coordinate along the split's axis counts (`x` for a
/// [`SplitAxis::Horizontal`] split, `y` for a vertical one). The result is clamped to
/// [`MIN_SPLIT_RATIO`]..=[`MAX_SPLIT_RATIO`]; when the split has no room to share, or a coordinate is not
/// finite, the divider's current ratio is returned.
pub fn ratio_for_drag(divider: &DividerRect, x: f32, y: f32) -> f32 {
    let split = divider.split_rect;
    let (position, start, length, gap) = match divider.axis {
        SplitAxis::Horizontal => (x, split.x, split.width, divider.rect.width),
        SplitAxis::Vertical => (y, split.y, split.height, divider.rect.height),
    };
    let available = length - gap;
    if !position.is_finite() || !available.is_finite() || available <= 0.0 {
        return divider.ratio;
    }
    let ratio = (position - start - gap / 2.0) / available;
    ratio.clamp(MIN_SPLIT_RATIO, MAX_SPLIT_RATIO)
}

/// What one pane of a [`SplitView`] shows: a header title, an optional role badge, optional header accessories,
/// and the content below.
pub struct PaneView {
    /// The pane in the tree this content belongs to.
    pub id: PaneId,
    /// The header text, such as `pi · api` or the shell's working directory.
    pub title: SharedString,
    /// The pane's role, shown as a badge before the title.
    pub kind: Option<PaneKind>,
    accessories: Option<AnyElement>,
    /// Optional agent identity colour, drawn as a 2px bar on the left of the pane header.
    pub agent_color: Option<gpui::Hsla>,
    content: AnyElement,
}

impl PaneView {
    /// Content for pane `id`, with no role badge.
    pub fn new(id: PaneId, title: impl Into<SharedString>, content: impl IntoElement) -> Self {
        Self {
            id,
            title: title.into(),
            kind: None,
            accessories: None,
            agent_color: None,
            content: content.into_any_element(),
        }
    }

    /// Show `kind` as the pane's role badge.
    pub fn kind(mut self, kind: PaneKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Draw `accessories` in the header between the title and the zoom and close buttons, such as the pane's
    /// branch and links to its repo.
    pub fn accessories(mut self, accessories: impl IntoElement) -> Self {
        self.accessories = Some(accessories.into_any_element());
        self
    }

    /// Draw a 2px identity bar in `color` on the left edge of the pane header.
    pub fn agent_color(mut self, color: gpui::Hsla) -> Self {
        self.agent_color = Some(color);
        self
    }
}

type PaneHandler = Rc<dyn Fn(PaneId, &mut Window, &mut App)>;
type ResizeHandler = Rc<dyn Fn(SplitId, f32, &mut Window, &mut App)>;

/// Draws a [`PaneTree`] filling its own bounds (by default the whole parent).
///
/// The view keeps no layout state: it takes a copy of the tree each frame, reports what the user does through
/// the callbacks, and the app applies that to its tree (for example [`PaneTree::set_split_ratio`] in
/// [`SplitView::on_resize`]) and re-renders. Panes of the tree with no [`PaneView`] are drawn empty, and
/// [`PaneView`]s of panes that are not visible (another pane is zoomed) are dropped. The `id` must be stable
/// across frames, as an in-progress divider drag is kept under it.
pub struct SplitView {
    id: ElementId,
    tree: PaneTree,
    panes: Vec<PaneView>,
    divider_thickness: Pixels,
    style: StyleRefinement,
    on_focus: Option<PaneHandler>,
    on_zoom: Option<PaneHandler>,
    on_close: Option<PaneHandler>,
    on_resize: Option<ResizeHandler>,
}

impl SplitView {
    /// A view of `tree` showing `panes`, with [`Theme::DIVIDER_THICKNESS`] dividers.
    pub fn new(id: impl Into<ElementId>, tree: &PaneTree, panes: Vec<PaneView>) -> Self {
        Self {
            id: id.into(),
            tree: tree.clone(),
            panes,
            divider_thickness: Theme::DIVIDER_THICKNESS,
            style: StyleRefinement::default(),
            on_focus: None,
            on_zoom: None,
            on_close: None,
            on_resize: None,
        }
        .size_full()
    }

    /// Set the gap between split panes, where the divider is drawn and grabbed.
    pub fn divider_thickness(mut self, thickness: Pixels) -> Self {
        self.divider_thickness = thickness;
        self
    }

    /// Called with a pane when the mouse is pressed anywhere in it, before the pane's content sees the press.
    pub fn on_focus(mut self, handler: impl Fn(PaneId, &mut Window, &mut App) + 'static) -> Self {
        self.on_focus = Some(Rc::new(handler));
        self
    }

    /// Show a zoom toggle in each header, calling `handler` with its pane when clicked.
    pub fn on_zoom(mut self, handler: impl Fn(PaneId, &mut Window, &mut App) + 'static) -> Self {
        self.on_zoom = Some(Rc::new(handler));
        self
    }

    /// Show a close button in each header, calling `handler` with its pane when clicked.
    pub fn on_close(mut self, handler: impl Fn(PaneId, &mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }

    /// Called while a divider is dragged with its split and the ratio from [`ratio_for_drag`]; pass them to
    /// [`PaneTree::set_split_ratio`]. Without it, dividers do not move.
    pub fn on_resize(
        mut self,
        handler: impl Fn(SplitId, f32, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_resize = Some(Rc::new(handler));
        self
    }

    /// The frame of one pane: header strip over its content, outlined in the focus colour when focused.
    fn pane_frame(
        &self,
        pane: &PaneRect,
        view: Option<PaneView>,
        zoomed: bool,
        theme: &Theme,
    ) -> AnyElement {
        let id = pane.id;
        let focused = pane.focused;
        let (title, kind, accessories, agent_color, content) = match view {
            Some(view) => (
                view.title,
                view.kind,
                view.accessories,
                view.agent_color,
                view.content,
            ),
            None => (
                SharedString::default(),
                None,
                None,
                None,
                div().into_any_element(),
            ),
        };
        let zoom = self.on_zoom.clone().map(|on_zoom| {
            Button::new(("pane-zoom", id.0), if zoomed { "⤡" } else { "⤢" })
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .selected(zoomed)
                .on_click(move |_, window, cx| on_zoom(id, window, cx))
        });
        let close = self.on_close.clone().map(|on_close| {
            Button::new(("pane-close", id.0), "×")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .on_click(move |_, window, cx| on_close(id, window, cx))
        });
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(Theme::PANE_HEADER_HEIGHT)
            .when(agent_color.is_none(), |header| header.pl(Theme::SPACE_2))
            .pr(Theme::SPACE_0_5)
            .gap(Theme::SPACE_1 + px(2.0))
            .bg(if focused { theme.raised } else { theme.surface })
            .border_b_1()
            .border_color(theme.border_subtle)
            .text_size(Theme::TEXT_XS)
            .children(agent_color.map(|color| div().flex_none().w(px(2.0)).h_full().bg(color)))
            .children(
                kind.map(|kind| Badge::new(pane_kind_label(kind)).tone(Tone::for_pane_kind(kind))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if focused {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .child(title),
            )
            .children(accessories)
            .children(zoom)
            .children(close);
        div()
            .flex()
            .flex_col()
            .w(px(pane.rect.width))
            .h(px(pane.rect.height))
            .rounded(Theme::RADIUS_MD)
            .border_1()
            .border_color(theme.outline(focused))
            .bg(theme.surface)
            .overflow_hidden()
            .child(header)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(content),
            )
            .into_any_element()
    }
}

impl Styled for SplitView {
    fn style(&mut self) -> &mut StyleRefinement {
        &mut self.style
    }
}

fn to_bounds(rect: Rect) -> Bounds<Pixels> {
    Bounds::new(
        point(px(rect.x), px(rect.y)),
        size(px(rect.width), px(rect.height)),
    )
}

/// The divider being dragged, kept across frames under the view's id.
type DragState = Rc<Cell<Option<SplitId>>>;

/// A laid-out pane, ready to paint.
struct PreparedPane {
    hitbox: Hitbox,
    element: AnyElement,
    id: PaneId,
}

/// A laid-out divider, ready to paint.
struct PreparedDivider {
    divider: DividerRect,
    bounds: Bounds<Pixels>,
    hitbox: Hitbox,
}

/// What [`SplitView`] computes in prepaint for paint; only GPUI uses it.
#[doc(hidden)]
pub struct SplitViewPrepaint {
    panes: Vec<PreparedPane>,
    dividers: Vec<PreparedDivider>,
    drag: DragState,
    theme: Theme,
}

impl Element for SplitView {
    type RequestLayoutState = ();
    type PrepaintState = SplitViewPrepaint;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.refine(&self.style);
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let theme = Theme::of(cx);
        let drag = window.with_optional_element_state::<DragState, _>(id, |state, _| {
            let state = state.flatten().unwrap_or_default();
            (state.clone(), Some(state))
        });

        let layout = self.tree.layout(
            Rect::new(
                bounds.origin.x.into(),
                bounds.origin.y.into(),
                bounds.size.width.into(),
                bounds.size.height.into(),
            ),
            self.divider_thickness.into(),
        );
        let zoomed = self.tree.is_zoomed();
        let mut views: HashMap<PaneId, PaneView> =
            self.panes.drain(..).map(|view| (view.id, view)).collect();

        let mut panes = Vec::with_capacity(layout.panes.len());
        for pane in &layout.panes {
            let pane_bounds = to_bounds(pane.rect);
            // Inserted before the pane's content, so content hitboxes stay on top of it.
            let hitbox = window.insert_hitbox(pane_bounds, HitboxBehavior::Normal);
            let mut element = self.pane_frame(pane, views.remove(&pane.id), zoomed, &theme);
            element.prepaint_as_root(
                pane_bounds.origin,
                pane_bounds.size.map(AvailableSpace::Definite),
                window,
                cx,
            );
            panes.push(PreparedPane {
                hitbox,
                element,
                id: pane.id,
            });
        }

        let dividers = layout
            .dividers
            .iter()
            .map(|divider| {
                let bounds = to_bounds(divider.rect);
                let slop = Theme::DIVIDER_GRAB_SLOP;
                let grab = match divider.axis {
                    SplitAxis::Horizontal => Edges {
                        left: slop,
                        right: slop,
                        ..Edges::default()
                    },
                    SplitAxis::Vertical => Edges {
                        top: slop,
                        bottom: slop,
                        ..Edges::default()
                    },
                };
                let hitbox = window.insert_hitbox(bounds.extend(grab), HitboxBehavior::Normal);
                PreparedDivider {
                    divider: *divider,
                    bounds,
                    hitbox,
                }
            })
            .collect();

        SplitViewPrepaint {
            panes,
            dividers,
            drag,
            theme,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let theme = prepaint.theme;
        window.paint_quad(fill(bounds, theme.chrome));
        for pane in &mut prepaint.panes {
            pane.element.paint(window, cx);
        }

        if let Some(on_focus) = self.on_focus.clone() {
            let panes: Vec<(PaneId, Hitbox)> = prepaint
                .panes
                .iter()
                .map(|pane| (pane.id, pane.hitbox.clone()))
                .collect();
            window.on_mouse_event(move |_: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                if let Some((id, _)) = panes.iter().find(|(_, hitbox)| hitbox.is_hovered(window)) {
                    on_focus(*id, window, cx);
                }
            });
        }

        // Without a resize handler the dividers are plain gaps.
        let Some(on_resize) = self.on_resize.clone() else {
            return;
        };

        let dragging = prepaint.drag.get();
        let mut hovered_at_paint = Vec::with_capacity(prepaint.dividers.len());
        for divider in &prepaint.dividers {
            let hovered = divider.hitbox.is_hovered(window);
            hovered_at_paint.push(hovered);
            let cursor = match divider.divider.axis {
                SplitAxis::Horizontal => CursorStyle::ResizeLeftRight,
                SplitAxis::Vertical => CursorStyle::ResizeUpDown,
            };
            window.set_cursor_style(cursor, &divider.hitbox);
            if dragging == Some(divider.divider.split_id) {
                // A fast drag leaves the divider's hitbox behind; keep the resize cursor for the whole window.
                window.set_window_cursor_style(cursor);
            }
            if hovered || dragging == Some(divider.divider.split_id) {
                window.paint_quad(fill(divider.bounds, theme.accent));
            }
        }

        let dividers: Rc<Vec<(DividerRect, Hitbox)>> = Rc::new(
            prepaint
                .dividers
                .iter()
                .map(|divider| (divider.divider, divider.hitbox.clone()))
                .collect(),
        );

        let drag = prepaint.drag.clone();
        let down_dividers = dividers.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                return;
            }
            if let Some((divider, hitbox)) = down_dividers
                .iter()
                .find(|(_, hitbox)| hitbox.is_hovered(window))
            {
                drag.set(Some(divider.split_id));
                window.capture_pointer(hitbox.id);
                window.refresh();
                cx.stop_propagation();
            }
        });

        let drag = prepaint.drag.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let hover_changed = dividers
                .iter()
                .zip(&hovered_at_paint)
                .any(|((_, hitbox), was)| hitbox.is_hovered(window) != *was);
            if hover_changed {
                window.refresh();
            }
            let Some(split_id) = drag.get() else {
                return;
            };
            if event.pressed_button != Some(MouseButton::Left) {
                // The button went up outside the window; end the drag.
                drag.set(None);
                window.release_pointer();
                window.refresh();
                return;
            }
            let Some((divider, _)) = dividers
                .iter()
                .find(|(divider, _)| divider.split_id == split_id)
            else {
                return;
            };
            let ratio = ratio_for_drag(divider, event.position.x.into(), event.position.y.into());
            if (ratio - divider.ratio).abs() > f32::EPSILON {
                on_resize(split_id, ratio, window, cx);
            }
            cx.stop_propagation();
        });

        let drag = prepaint.drag.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                return;
            }
            if drag.take().is_some() {
                window.release_pointer();
                window.refresh();
                cx.stop_propagation();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn divider(axis: SplitAxis, ratio: f32) -> DividerRect {
        // A 100x50 split at (10, 20) with a 4px gap, as the layout would produce it.
        let split_rect = Rect::new(10.0, 20.0, 100.0, 50.0);
        let rect = match axis {
            SplitAxis::Horizontal => Rect::new(10.0 + 96.0 * ratio, 20.0, 4.0, 50.0),
            SplitAxis::Vertical => Rect::new(10.0, 20.0 + 46.0 * ratio, 100.0, 4.0),
        };
        DividerRect {
            split_id: SplitId(7),
            axis,
            rect,
            ratio,
            split_rect,
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn horizontal_drag_follows_x_and_ignores_y() {
        let d = divider(SplitAxis::Horizontal, 0.5);
        // Divider centre at x = 10 + 0.25 * 96 + 2.
        assert!(close(ratio_for_drag(&d, 36.0, 0.0), 0.25));
        assert!(close(ratio_for_drag(&d, 36.0, 1000.0), 0.25));
        assert!(close(ratio_for_drag(&d, 10.0 + 48.0 + 2.0, 35.0), 0.5));
    }

    #[test]
    fn vertical_drag_follows_y_and_ignores_x() {
        let d = divider(SplitAxis::Vertical, 0.5);
        // Divider centre at y = 20 + 0.75 * 46 + 2.
        let y = 20.0 + 0.75 * 46.0 + 2.0;
        assert!(close(ratio_for_drag(&d, 0.0, y), 0.75));
        assert!(close(ratio_for_drag(&d, -500.0, y), 0.75));
    }

    #[test]
    fn dragging_where_the_divider_is_keeps_the_ratio() {
        for axis in [SplitAxis::Horizontal, SplitAxis::Vertical] {
            for ratio in [0.2, 0.5, 0.68] {
                let d = divider(axis, ratio);
                let centre_x = d.rect.x + d.rect.width / 2.0;
                let centre_y = d.rect.y + d.rect.height / 2.0;
                assert!(close(ratio_for_drag(&d, centre_x, centre_y), ratio));
            }
        }
    }

    #[test]
    fn drag_is_clamped_to_the_allowed_ratios() {
        let d = divider(SplitAxis::Horizontal, 0.5);
        assert_eq!(ratio_for_drag(&d, -1000.0, 0.0), MIN_SPLIT_RATIO);
        assert_eq!(ratio_for_drag(&d, 11.0, 0.0), MIN_SPLIT_RATIO);
        assert_eq!(ratio_for_drag(&d, 1000.0, 0.0), MAX_SPLIT_RATIO);
        let d = divider(SplitAxis::Vertical, 0.5);
        assert_eq!(ratio_for_drag(&d, 0.0, -5.0), MIN_SPLIT_RATIO);
        assert_eq!(ratio_for_drag(&d, 0.0, 500.0), MAX_SPLIT_RATIO);
    }

    #[test]
    fn degenerate_splits_and_bad_positions_keep_the_ratio() {
        let mut d = divider(SplitAxis::Horizontal, 0.4);
        assert_eq!(ratio_for_drag(&d, f32::NAN, 0.0), 0.4);
        assert_eq!(ratio_for_drag(&d, f32::INFINITY, 0.0), 0.4);
        d.split_rect.width = 4.0;
        assert_eq!(ratio_for_drag(&d, 12.0, 0.0), 0.4);
        d.split_rect.width = 0.0;
        d.rect.width = 0.0;
        assert_eq!(ratio_for_drag(&d, 12.0, 0.0), 0.4);
    }

    #[test]
    fn drag_ratio_matches_the_tree_layout() {
        let mut tree = PaneTree::new(PaneId(1));
        assert!(
            tree.split(PaneId(1), SplitAxis::Horizontal, PaneId(2), 0.5)
                .is_ok()
        );
        let bounds = Rect::new(0.0, 0.0, 204.0, 100.0);
        let layout = tree.layout(bounds, 4.0);
        let Some(d) = layout.dividers.first().copied() else {
            panic!("a split tree has a divider");
        };
        let ratio = ratio_for_drag(&d, 60.0 + 2.0, 50.0);
        assert!(tree.set_split_ratio(d.split_id, ratio).is_ok());
        let after = tree.layout(bounds, 4.0);
        let first = after.pane(PaneId(1)).unwrap_or_default();
        assert!(close(first.width, 60.0), "first pane is {}", first.width);
    }

    #[test]
    fn kind_labels() {
        assert_eq!(pane_kind_label(PaneKind::Agent), "agent");
        assert_eq!(pane_kind_label(PaneKind::Shell), "shell");
        assert_eq!(pane_kind_label(PaneKind::Git), "git");
    }
}
