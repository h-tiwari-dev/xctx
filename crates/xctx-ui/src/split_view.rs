//! Draws an `xctx-layout` [`PaneTree`]: each visible pane in a frame with a header, draggable dividers between
//! split panes, and drag and drop of panes (by their headers) and of tool dock items onto panes.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, CursorStyle, DispatchPhase, Div, Edges, Element,
    ElementId, FontWeight, GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, LayoutId,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, SharedString, Style,
    StyleRefinement, Window, div, fill, point, prelude::*, px, size,
};
use xctx_layout::{
    DividerRect, MAX_SPLIT_RATIO, MIN_SPLIT_RATIO, PaneId, PaneKind, PaneRect, PaneTree, Rect,
    SplitAxis, SplitId,
};

use crate::components::{Button, ButtonSize, ButtonVariant};
use crate::drag::{DragPreview, DraggedPane, DraggedTool, DropEdge};
use crate::icon::{Icon, IconName, IconSize};
use crate::theme::Theme;

/// The word for a pane of `kind`, such as in a pane's title.
pub fn pane_kind_label(kind: PaneKind) -> &'static str {
    match kind {
        PaneKind::Agent => "agent",
        PaneKind::Shell => "shell",
        PaneKind::Git => "git",
        // A tool pane (Terminal, Browser, ...) opened from the tool dock; its title names the tool.
        PaneKind::Plugin => "tool",
    }
}

/// The icon a pane header shows for `kind` when its [`PaneView::icon`] is not set.
pub fn pane_kind_icon(kind: PaneKind) -> IconName {
    match kind {
        PaneKind::Agent => IconName::Writer,
        PaneKind::Shell => IconName::Terminal,
        PaneKind::Git => IconName::Git,
        PaneKind::Plugin => IconName::Plugin,
    }
}

/// What the drop preview over a pane says a drop at `edge` does.
pub fn drop_edge_label(edge: DropEdge) -> &'static str {
    match edge {
        DropEdge::Left => "Split left",
        DropEdge::Right => "Split right",
        DropEdge::Top => "Split above",
        DropEdge::Bottom => "Split below",
        DropEdge::Center => "Swap",
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

/// What one pane of a [`SplitView`] shows: a header with an icon, a title and optional accessories, and the
/// content below.
pub struct PaneView {
    /// The pane in the tree this content belongs to.
    pub id: PaneId,
    /// The header text, such as `pi · api` or the shell's working directory.
    pub title: SharedString,
    /// The pane's role; it picks the header icon ([`pane_kind_icon`]) when [`PaneView::icon`] is not set.
    pub kind: Option<PaneKind>,
    /// The header icon, overriding the one for [`PaneView::kind`].
    pub icon: Option<IconName>,
    accessories: Option<AnyElement>,
    /// Optional agent identity colour: a 2px bar on the left of the pane header, and the header icon's tint.
    pub agent_color: Option<gpui::Hsla>,
    content: AnyElement,
}

impl PaneView {
    /// Content for pane `id`, with no role and no icon.
    pub fn new(id: PaneId, title: impl Into<SharedString>, content: impl IntoElement) -> Self {
        Self {
            id,
            title: title.into(),
            kind: None,
            icon: None,
            accessories: None,
            agent_color: None,
            content: content.into_any_element(),
        }
    }

    /// Set the pane's role, which picks its header icon unless [`PaneView::icon`] is set.
    pub fn kind(mut self, kind: PaneKind) -> Self {
        self.kind = Some(kind);
        self
    }

    /// Show `icon` before the title, instead of the icon for the pane's [`PaneView::kind`]. It is also the icon
    /// on the chip that follows the pointer while the pane is dragged.
    pub fn icon(mut self, icon: IconName) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Draw `accessories` in the header between the title and the zoom and close buttons, such as the pane's
    /// branch and links to its repo. They are not part of the header's drag handle.
    pub fn accessories(mut self, accessories: impl IntoElement) -> Self {
        self.accessories = Some(accessories.into_any_element());
        self
    }

    /// Draw a 2px identity bar in `color` on the left edge of the pane header, and tint the header icon with it.
    pub fn agent_color(mut self, color: gpui::Hsla) -> Self {
        self.agent_color = Some(color);
        self
    }
}

type PaneHandler = Rc<dyn Fn(PaneId, &mut Window, &mut App)>;
type ResizeHandler = Rc<dyn Fn(SplitId, f32, &mut Window, &mut App)>;
type DropPaneHandler = Rc<dyn Fn(PaneId, PaneId, DropEdge, &mut Window, &mut App)>;
type DropToolHandler = Rc<dyn Fn(&DraggedTool, PaneId, DropEdge, &mut Window, &mut App)>;

/// Draws a [`PaneTree`] filling its own bounds (by default the whole parent).
///
/// The view keeps no layout state: it takes a copy of the tree each frame, reports what the user does through
/// the callbacks, and the app applies that to its tree (for example [`PaneTree::set_split_ratio`] in
/// [`SplitView::on_resize`]) and re-renders. Panes of the tree with no [`PaneView`] are drawn empty, and
/// [`PaneView`]s of panes that are not visible (another pane is zoomed) are dropped. The `id` must be stable
/// across frames, as an in-progress divider drag is kept under it.
///
/// The view paints no background of its own: the panes float on whatever is behind it (the window's chrome).
///
/// With [`SplitView::on_drop_pane`], a pane's header (its icon and title; not its accessories or buttons) drags a
/// [`DraggedPane`]; with [`SplitView::on_drop_tool`], a [`DraggedTool`] from the tool dock can be dropped too.
/// While one is over a pane that takes it, the half (or, in the middle, the whole) of the pane the drop would
/// take is outlined, with what the drop does ([`drop_edge_label`]); the dragged pane is dimmed. The edge is
/// [`DropEdge::at`] the pane's rectangle (header included, in window coordinates) and the pointer's position when
/// the button is released. Drops are taken before the pane's content sees the release, so content such as a
/// terminal cannot swallow them; a pane is never a drop target for itself.
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
    on_drop_pane: Option<DropPaneHandler>,
    on_drop_tool: Option<DropToolHandler>,
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
            on_drop_pane: None,
            on_drop_tool: None,
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

    /// Make pane headers draggable, and call `handler` with the dragged pane, the pane it was dropped on (never
    /// the same) and the [`DropEdge`] under the pointer; pass them to [`PaneTree::drop_pane`]. Without it,
    /// headers do not drag and panes take no [`DraggedPane`].
    pub fn on_drop_pane(
        mut self,
        handler: impl Fn(PaneId, PaneId, DropEdge, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_drop_pane = Some(Rc::new(handler));
        self
    }

    /// Let panes take a [`DraggedTool`] from the tool dock, calling `handler` with it, the pane it was dropped on
    /// and the [`DropEdge`] under the pointer. Without it, panes take no tools.
    pub fn on_drop_tool(
        mut self,
        handler: impl Fn(&DraggedTool, PaneId, DropEdge, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_drop_tool = Some(Rc::new(handler));
        self
    }

    /// Whether a drop of `dragged` on pane `target` would be taken.
    fn accepts(&self, dragged: &Dragged, target: PaneId) -> bool {
        match dragged {
            Dragged::Pane(pane) => self.on_drop_pane.is_some() && pane.pane != target,
            Dragged::Tool(_) => self.on_drop_tool.is_some(),
        }
    }

    /// The frame of one pane: header strip over its content, outlined in the focus colour when focused, dimmed
    /// while it is being dragged.
    fn pane_frame(
        &self,
        pane: &PaneRect,
        view: Option<PaneView>,
        zoomed: bool,
        dimmed: bool,
        state: &SharedState,
        theme: &Theme,
    ) -> AnyElement {
        let id = pane.id;
        let focused = pane.focused;
        let (title, icon, accessories, agent_color, content) = match view {
            Some(view) => (
                view.title,
                view.icon.or(view.kind.map(pane_kind_icon)),
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
        let icon = icon.unwrap_or(IconName::Layers);
        let zoom = self.on_zoom.clone().map(|on_zoom| {
            Button::new(("pane-zoom", id.0), "")
                .icon(if zoomed {
                    IconName::Minimize
                } else {
                    IconName::Maximize
                })
                .tooltip("Zoom ⌘⇧Z")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .selected(zoomed)
                .on_click(move |_, window, cx| on_zoom(id, window, cx))
        });
        let close = self.on_close.clone().map(|on_close| {
            Button::new(("pane-close", id.0), "")
                .icon(IconName::Close)
                .tooltip("Close ⌘W")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .on_click(move |_, window, cx| on_close(id, window, cx))
        });
        let mut handle = div()
            .id(("pane-handle", id.0))
            .flex()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .gap(Theme::SPACE_2)
            .pl(Theme::SPACE_2)
            .child(
                Icon::new(icon)
                    .size(IconSize::Sm)
                    .color(agent_color.unwrap_or(theme.text_muted)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(Theme::TEXT_SM)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(if focused {
                        theme.text
                    } else {
                        theme.text_muted
                    })
                    .child(title.clone()),
            );
        if self.on_drop_pane.is_some() {
            let dragged = DraggedPane {
                pane: id,
                title: if title.is_empty() {
                    SharedString::from("Pane")
                } else {
                    title
                },
                icon,
            };
            handle = handle.cursor_grab().on_drag(dragged, |pane, _, _, cx| {
                cx.new(|_| DragPreview::new(pane.icon, pane.title.clone()))
            });
        }
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .h(Theme::PANE_HEADER_HEIGHT)
            .border_b_1()
            .border_color(theme.border_subtle)
            .children(agent_color.map(|color| div().flex_none().w(px(2.0)).h_full().bg(color)))
            .child(handle)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .pl(Theme::SPACE_1)
                    .pr(Theme::SPACE_1)
                    .children(accessories)
                    .children(zoom)
                    .children(close),
            );
        let frame = div()
            .flex()
            .flex_col()
            .w(px(pane.rect.width))
            .h(px(pane.rect.height))
            .rounded(Theme::RADIUS_LG)
            .border_1()
            .border_color(if focused {
                theme.accent
            } else {
                theme.border_subtle
            })
            .bg(theme.surface)
            .overflow_hidden()
            .when(dimmed, |frame| frame.opacity(0.5))
            .child(header)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(content),
            );
        self.track_drags(frame, state).into_any_element()
    }

    /// Record in `state` what is being dragged, for the drop preview, the dimmed pane and the drop itself. GPUI
    /// gives the dragged value only to element listeners, so each pane frame listens; the first to see a move
    /// records it.
    fn track_drags(&self, mut frame: Div, state: &SharedState) -> Div {
        if self.on_drop_pane.is_some() {
            let state = state.clone();
            frame = frame.on_drag_move::<DraggedPane>(move |event, window, cx| {
                if let Some(pane) = event.dragged_item().downcast_ref::<DraggedPane>() {
                    state.note_drag(|| Dragged::Pane(pane.clone()), window, cx);
                }
            });
        }
        if self.on_drop_tool.is_some() {
            let state = state.clone();
            frame = frame.on_drag_move::<DraggedTool>(move |event, window, cx| {
                if let Some(tool) = event.dragged_item().downcast_ref::<DraggedTool>() {
                    state.note_drag(|| Dragged::Tool(tool.clone()), window, cx);
                }
            });
        }
        frame
    }
}

/// The drop preview: the part of the target pane the drop would take, with what the drop does in its middle.
fn drop_preview(rect: Rect, edge: DropEdge, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .w(px(rect.width))
        .h(px(rect.height))
        .rounded(Theme::RADIUS_LG)
        .border_2()
        .border_color(theme.accent)
        .bg(theme.accent.alpha(0.16))
        .child(
            div()
                .flex()
                .items_center()
                .h(px(22.0))
                .px(Theme::SPACE_2)
                .rounded(Theme::RADIUS_MD)
                .border_1()
                .border_color(theme.border_strong)
                .bg(theme.overlay)
                .shadow_md()
                .text_size(Theme::TEXT_XS)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(drop_edge_label(edge)),
        )
        .into_any_element()
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

/// What is being dragged over the view.
#[derive(Debug, Clone)]
enum Dragged {
    Pane(DraggedPane),
    Tool(DraggedTool),
}

/// What the view keeps across frames under its id.
#[derive(Default)]
struct ViewState {
    /// The divider being dragged.
    divider: Cell<Option<SplitId>>,
    /// The pane or tool being dragged, once a move has been seen during the drag.
    dragged: RefCell<Option<Dragged>>,
}

impl ViewState {
    /// Record the drag in progress (built by `dragged`) unless one is recorded already, and show the grabbing
    /// cursor for it.
    fn note_drag(&self, dragged: impl FnOnce() -> Dragged, window: &mut Window, cx: &mut App) {
        {
            let mut slot = self.dragged.borrow_mut();
            if slot.is_none() {
                *slot = Some(dragged());
            }
        }
        if cx.active_drag_cursor_style() != Some(CursorStyle::ClosedHand) {
            cx.set_active_drag_cursor_style(CursorStyle::ClosedHand, window);
        }
    }
}

type SharedState = Rc<ViewState>;

/// A laid-out pane, ready to paint.
struct PreparedPane {
    /// Under the content: the press that focuses the pane.
    hitbox: Hitbox,
    /// Over the content: whether a drop would land on this pane rather than on something above it.
    drop_hitbox: Hitbox,
    element: AnyElement,
    id: PaneId,
    rect: Rect,
}

/// A laid-out divider, ready to paint.
struct PreparedDivider {
    divider: DividerRect,
    bounds: Bounds<Pixels>,
    hitbox: Hitbox,
}

/// The drop preview over the pane under the pointer.
struct PreparedPreview {
    pane: PaneId,
    element: AnyElement,
}

/// What [`SplitView`] computes in prepaint for paint; only GPUI uses it.
#[doc(hidden)]
pub struct SplitViewPrepaint {
    panes: Vec<PreparedPane>,
    dividers: Vec<PreparedDivider>,
    preview: Option<PreparedPreview>,
    state: SharedState,
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
        let state = window.with_optional_element_state::<SharedState, _>(id, |state, _| {
            let state = state.flatten().unwrap_or_default();
            (state.clone(), Some(state))
        });
        if !cx.has_active_drag() {
            // The drag ended somewhere the view did not see (another window, a cancelled drag).
            state.dragged.borrow_mut().take();
        }
        let dragged = state.dragged.borrow().clone();
        let dimmed_pane = match &dragged {
            Some(Dragged::Pane(pane)) => Some(pane.pane),
            _ => None,
        };

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
            let mut element = self.pane_frame(
                pane,
                views.remove(&pane.id),
                zoomed,
                dimmed_pane == Some(pane.id),
                &state,
                &theme,
            );
            element.prepaint_as_root(
                pane_bounds.origin,
                pane_bounds.size.map(AvailableSpace::Definite),
                window,
                cx,
            );
            // Inserted after the content, so nothing the content draws (even an occluding element) hides a drop on
            // the pane, while overlays drawn later over the pane still do. It is `Normal`, so it takes nothing from
            // the content's own hover and clicks.
            let drop_hitbox = window.insert_hitbox(pane_bounds, HitboxBehavior::Normal);
            panes.push(PreparedPane {
                hitbox,
                drop_hitbox,
                element,
                id: pane.id,
                rect: pane.rect,
            });
        }

        let mouse = window.mouse_position();
        let preview = dragged.as_ref().and_then(|dragged| {
            let pane = layout.panes.iter().find(|pane| {
                to_bounds(pane.rect).contains(&mouse) && self.accepts(dragged, pane.id)
            })?;
            let edge = DropEdge::at(pane.rect, mouse.x.into(), mouse.y.into());
            let area = to_bounds(edge.preview(pane.rect));
            let mut element = drop_preview(edge.preview(pane.rect), edge, &theme);
            element.prepaint_as_root(
                area.origin,
                area.size.map(AvailableSpace::Definite),
                window,
                cx,
            );
            Some(PreparedPreview {
                pane: pane.id,
                element,
            })
        });

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
            preview,
            state,
            theme,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let theme = prepaint.theme;

        if self.on_drop_pane.is_some() || self.on_drop_tool.is_some() {
            // Registered before the panes' content and run in the capture phase, so a drop is taken before
            // anything inside a pane (a terminal ending a selection) can stop the release from propagating.
            let targets: Vec<(PaneId, Rect, Hitbox)> = prepaint
                .panes
                .iter()
                .map(|pane| (pane.id, pane.rect, pane.drop_hitbox.clone()))
                .collect();
            let state = prepaint.state.clone();
            let on_drop_pane = self.on_drop_pane.clone();
            let on_drop_tool = self.on_drop_tool.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                let Some(dragged) = state.dragged.borrow_mut().take() else {
                    return;
                };
                if !cx.has_active_drag() {
                    return;
                }
                let Some((target, rect, _)) = targets
                    .iter()
                    .find(|(_, _, hitbox)| hitbox.is_hovered(window))
                else {
                    return;
                };
                let edge = DropEdge::at(*rect, event.position.x.into(), event.position.y.into());
                let dropped = match &dragged {
                    Dragged::Pane(pane) if pane.pane != *target => {
                        if let Some(handler) = &on_drop_pane {
                            handler(pane.pane, *target, edge, window, cx);
                            true
                        } else {
                            false
                        }
                    }
                    Dragged::Pane(_) => false,
                    Dragged::Tool(tool) => {
                        if let Some(handler) = &on_drop_tool {
                            handler(tool, *target, edge, window, cx);
                            true
                        } else {
                            false
                        }
                    }
                };
                if dropped {
                    window.refresh();
                    cx.stop_propagation();
                }
            });
        }

        for pane in &mut prepaint.panes {
            pane.element.paint(window, cx);
        }
        if let Some(preview) = &mut prepaint.preview
            && prepaint
                .panes
                .iter()
                .any(|pane| pane.id == preview.pane && pane.drop_hitbox.is_hovered(window))
        {
            preview.element.paint(window, cx);
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

        let dragging = prepaint.state.divider.get();
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
                window.paint_quad(fill(divider_line(divider), theme.accent).corner_radii(px(1.0)));
            }
        }

        let dividers: Rc<Vec<(DividerRect, Hitbox)>> = Rc::new(
            prepaint
                .dividers
                .iter()
                .map(|divider| (divider.divider, divider.hitbox.clone()))
                .collect(),
        );

        let state = prepaint.state.clone();
        let down_dividers = dividers.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                return;
            }
            if let Some((divider, hitbox)) = down_dividers
                .iter()
                .find(|(_, hitbox)| hitbox.is_hovered(window))
            {
                state.divider.set(Some(divider.split_id));
                window.capture_pointer(hitbox.id);
                window.refresh();
                cx.stop_propagation();
            }
        });

        let state = prepaint.state.clone();
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
            let Some(split_id) = state.divider.get() else {
                return;
            };
            if event.pressed_button != Some(MouseButton::Left) {
                // The button went up outside the window; end the drag.
                state.divider.set(None);
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

        let state = prepaint.state.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                return;
            }
            if state.divider.take().is_some() {
                window.release_pointer();
                window.refresh();
                cx.stop_propagation();
            }
        });
    }
}

/// The 2px line drawn down the middle of a hovered or dragged divider's gap.
fn divider_line(divider: &PreparedDivider) -> Bounds<Pixels> {
    let bounds = divider.bounds;
    let line = px(2.0);
    match divider.divider.axis {
        SplitAxis::Horizontal => Bounds::new(
            point(
                bounds.origin.x + (bounds.size.width - line) / 2.0,
                bounds.origin.y,
            ),
            size(line, bounds.size.height),
        ),
        SplitAxis::Vertical => Bounds::new(
            point(
                bounds.origin.x,
                bounds.origin.y + (bounds.size.height - line) / 2.0,
            ),
            size(bounds.size.width, line),
        ),
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

    #[test]
    fn kind_icons() {
        assert_eq!(pane_kind_icon(PaneKind::Agent), IconName::Writer);
        assert_eq!(pane_kind_icon(PaneKind::Shell), IconName::Terminal);
        assert_eq!(pane_kind_icon(PaneKind::Git), IconName::Git);
        assert_eq!(pane_kind_icon(PaneKind::Plugin), IconName::Plugin);
    }

    #[test]
    fn drop_labels() {
        assert_eq!(drop_edge_label(DropEdge::Center), "Swap");
        assert_eq!(drop_edge_label(DropEdge::Left), "Split left");
        assert_eq!(drop_edge_label(DropEdge::Right), "Split right");
        assert_eq!(drop_edge_label(DropEdge::Top), "Split above");
        assert_eq!(drop_edge_label(DropEdge::Bottom), "Split below");
    }

    mod drops {
        use gpui::{
            Context, Modifiers, MouseButton, Pixels, Point, Render, TestAppContext,
            VisualTestContext, Window, div, point, px,
        };
        use xctx_layout::{PaneId, PaneTree, SplitAxis};

        use super::super::*;
        use crate::tool_dock::{DockItem, ToolDock, ToolDockView, ToolGroup};

        type Log = Rc<RefCell<Vec<String>>>;

        /// Two side-by-side panes whose content stops every mouse release from propagating, as a terminal might,
        /// next to a tool dock.
        struct Host {
            tree: PaneTree,
            log: Log,
        }

        impl Render for Host {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                let panes = [PaneId(1), PaneId(2)]
                    .into_iter()
                    .map(|id| {
                        let content = div()
                            .id(("content", id.0))
                            .size_full()
                            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation());
                        PaneView::new(id, format!("pane {}", id.0), content).kind(PaneKind::Shell)
                    })
                    .collect();
                let (pane_log, tool_log) = (self.log.clone(), self.log.clone());
                let dock = ToolDock::new(ToolDockView {
                    items: vec![DockItem {
                        name: "browser".into(),
                        title: "Browser".into(),
                        detail: None,
                        icon: IconName::Globe,
                        group: ToolGroup::Panes,
                        open: false,
                        locked: false,
                        setting: None,
                        needs_input: false,
                    }],
                    ..ToolDockView::default()
                });
                div()
                    .size_full()
                    .flex()
                    .child(
                        div().flex_1().h_full().child(
                            SplitView::new("split", &self.tree, panes)
                                .on_drop_pane(move |dragged, target, edge, _, _| {
                                    pane_log.borrow_mut().push(format!(
                                        "pane {} on {} {edge:?}",
                                        dragged.0, target.0
                                    ));
                                })
                                .on_drop_tool(move |tool, target, edge, _, _| {
                                    tool_log.borrow_mut().push(format!(
                                        "tool {} on {} {edge:?}",
                                        tool.name, target.0
                                    ));
                                }),
                        ),
                    )
                    .child(dock)
            }
        }

        fn host(cx: &mut TestAppContext) -> (Log, &mut VisualTestContext) {
            cx.update(|cx| cx.set_global(Theme::dark()));
            let mut tree = PaneTree::new(PaneId(1));
            assert!(
                tree.split(PaneId(1), SplitAxis::Horizontal, PaneId(2), 0.5)
                    .is_ok()
            );
            let log: Log = Rc::default();
            let host_log = log.clone();
            let (_host, cx) = cx.add_window_view(move |_, _| Host {
                tree,
                log: host_log,
            });
            cx.run_until_parked();
            (log, cx)
        }

        /// Press at `from`, move past GPUI's drag threshold, then over `to` and release there.
        fn drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
            cx.simulate_mouse_move(from, None, Modifiers::default());
            cx.simulate_mouse_down(from, MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_move(
                from + point(px(12.0), px(6.0)),
                MouseButton::Left,
                Modifiers::default(),
            );
            cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::default());
            cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::default());
            cx.run_until_parked();
        }

        /// The middle of pane `n` (1 or 2) of the two side-by-side panes, and the window's size.
        fn pane_point(cx: &mut VisualTestContext, n: f32, x_share: f32) -> Point<Pixels> {
            let viewport = cx.update(|window, _| window.viewport_size());
            let split_width = viewport.width - crate::tool_dock::TOOL_DOCK_WIDTH;
            point(
                split_width * ((n - 1.0) / 2.0 + x_share / 2.0),
                viewport.height / 2.0,
            )
        }

        #[gpui::test]
        fn a_header_dropped_on_another_pane_reports_the_edge(cx: &mut TestAppContext) {
            let (log, cx) = host(cx);
            let header = point(px(48.0), Theme::PANE_HEADER_HEIGHT / 2.0);
            let middle_of_second = pane_point(cx, 2.0, 0.5);
            drag(cx, header, middle_of_second);
            let right_of_second = pane_point(cx, 2.0, 0.95);
            drag(cx, header, right_of_second);
            // Onto itself: nothing.
            let first = pane_point(cx, 1.0, 0.1);
            drag(cx, header, first);
            assert_eq!(*log.borrow(), ["pane 1 on 2 Center", "pane 1 on 2 Right"]);
        }

        #[gpui::test]
        fn a_dock_item_dropped_on_a_pane_reports_the_tool(cx: &mut TestAppContext) {
            let (log, cx) = host(cx);
            let Some(item) = cx.debug_bounds("tool-browser") else {
                panic!("the dock item is drawn");
            };
            let left_of_first = pane_point(cx, 1.0, 0.05);
            drag(cx, item.center(), left_of_first);
            assert_eq!(*log.borrow(), ["tool browser on 1 Left"]);
        }
    }
}
