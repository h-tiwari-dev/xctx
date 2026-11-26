//! The Flow view of a space: the loop's stage track across the top, a live node-link graph of the space's agents
//! (the canvas) below it with a timeline column on its right, and a drawer for the selected agent floating over
//! the canvas.
//!
//! - [`FlowCanvas`] draws an [`AgentGraph`] laid out by [`FlowLayout::compute`] for the canvas's real size: the
//!   orchestrator on top, the writers in the middle, the helpers below, with every edge as a curve (coordinates
//!   dashed, contract in the accent, verifies dotted) and a badge on each edge two agents talked over lately.
//! - [`AgentDetail`] is the drawer for one agent: who it is, what it does, its pane, and the actions it allows.
//! - [`FlowView`] lays the stage track, the canvas (with its add-helper toolbar, legend and drawer) and the timeline
//!   out together.
//!
//! All three keep no state: the app passes in what to show and receives what the user did through callbacks.
//!
//! Test selectors:
//! - canvas: `canvas-node-<id>` on each agent's card (click selects it, double-click opens its pane),
//!   `canvas-note-<id>` on a card's note line, and `canvas-edge-<from>-<to>` on the traffic badge of an edge two
//!   agents talked over (click selects the pair). Clicking anywhere else on the canvas clears the selection.
//! - drawer: `agent-detail` on the drawer, `detail-open`, `detail-start`, `detail-pause`, `detail-resume`,
//!   `detail-fork`, `detail-remove` and `detail-close` on its buttons.
//! - view: `flow-add-<kind>` ([`helper_kind_name`]) and `flow-add-repo` on the canvas toolbar's buttons.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, AvailableSpace, Bounds, BoxShadow, ContentMask, DispatchPhase, Element,
    ElementId, FontWeight, GlobalElementId, Hitbox, HitboxBehavior, Hsla, InspectorElementId,
    LayoutId, MouseButton, MouseDownEvent, Path, PathBuilder, Pixels, Point, SharedString, Style,
    StyleRefinement, Window, canvas, div, fill, hsla, point, prelude::*, px, size,
};
use xctx_graph::{
    AgentGraph, AgentNode, AgentNodeId, AgentRole, AgentState, EdgeKind, HelperKind, LoopStage,
};
use xctx_nav::flow::{FlowEdge, FlowFocus, FlowLayout, FlowPoint, pair_key, stage_of};

use crate::agents::{
    HELPER_KINDS, PaneLive, agent_identity_color, can_pause, can_resume, card_badge,
    helper_kind_label, helper_kind_name,
};
use crate::components::{Badge, Button, ButtonSize, ButtonVariant};
use crate::icon::{Icon, IconName, IconSize};
use crate::spinner::Spinner;
use crate::stage_track::loop_stage_label;
use crate::theme::{Theme, Tone};

type SelectHandler = Rc<dyn Fn(Option<FlowFocus>, &mut Window, &mut App)>;
type NodeHandler = Rc<dyn Fn(&AgentNodeId, &mut Window, &mut App)>;
type HelperHandler = Rc<dyn Fn(HelperKind, &mut Window, &mut App)>;
type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The width [`FlowView`] gives the timeline column unless [`FlowView::timeline_width`] says otherwise.
pub const DEFAULT_TIMELINE_WIDTH: Pixels = px(360.0);

/// The width of the [`AgentDetail`] drawer.
pub const DETAIL_WIDTH: Pixels = px(340.0);

/// Room between the canvas's background dots.
const DOT_SPACING: f32 = 24.0;
/// How strongly an edge that does not touch the selection is drawn, against 1.0 for one that does.
const FADED: f32 = 0.35;
/// Dashes of a coordinates edge.
const DASHED: [Pixels; 2] = [px(5.0), px(4.0)];
/// Dots of a verifies edge.
const DOTTED: [Pixels; 2] = [px(1.5), px(3.5)];

/// The icon of an agent's role.
fn role_icon(role: &AgentRole) -> IconName {
    match role {
        AgentRole::Orchestrator => IconName::Orchestrator,
        AgentRole::Writer { .. } => IconName::Writer,
        AgentRole::Helper { .. } => IconName::Helper,
    }
}

/// The role line of a card: `orchestrator`, `writer · api` or `helper · tests`.
fn role_line(role: &AgentRole) -> String {
    match role {
        AgentRole::Orchestrator => "orchestrator".to_owned(),
        AgentRole::Writer { repo } => format!("writer · {repo}"),
        AgentRole::Helper { kind } => format!("helper · {}", helper_kind_name(*kind)),
    }
}

/// Whether the agent is working: its graph says so, or nothing was reported and its pane's program runs.
fn is_running(node: &AgentNode, live: Option<PaneLive>) -> bool {
    match node.state {
        AgentState::Running => true,
        AgentState::Idle => live == Some(PaneLive::Running),
        _ => false,
    }
}

/// The reason a blocked or failed agent gives, with the colour it is drawn in.
fn state_reason(state: &AgentState, theme: &Theme) -> Option<(SharedString, Hsla)> {
    match state {
        AgentState::Blocked { reason } => Some((reason.clone().into(), theme.warning)),
        AgentState::Failed { reason } => Some((reason.clone().into(), theme.danger)),
        _ => None,
    }
}

/// `1 turn`, `3 turns`; `None` for an agent that has taken none.
fn turns_text(turns: u32) -> Option<String> {
    match turns {
        0 => None,
        1 => Some("1 turn".to_owned()),
        n => Some(format!("{n} turns")),
    }
}

/// How an edge of `kind` is stroked at rest: its width, colour and dash pattern.
fn edge_stroke(kind: EdgeKind, theme: &Theme) -> (f32, Hsla, Option<&'static [Pixels]>) {
    match kind {
        EdgeKind::Coordinates => (1.0, theme.border_strong, Some(&DASHED)),
        EdgeKind::Contract => (2.0, theme.accent, None),
        EdgeKind::Verifies => (1.5, theme.success.opacity(0.7), Some(&DOTTED)),
    }
}

/// Whether `edge` joins the agents of `focus`: it touches the focused agent, or it is the focused pair's edge.
fn edge_in_focus(edge: &FlowEdge, focus: &FlowFocus) -> bool {
    match focus {
        FlowFocus::Agent(id) => &edge.from == id || &edge.to == id,
        FlowFocus::Pair(a, b) => {
            pair_key(a.as_str(), b.as_str()) == pair_key(edge.from.as_str(), edge.to.as_str())
        }
    }
}

/// A stroked cubic Bézier through `points` (start, first control, second control, end), in window coordinates.
fn stroke_curve(
    points: [Point<Pixels>; 4],
    width: f32,
    dash: Option<&[Pixels]>,
) -> Option<Path<Pixels>> {
    let mut builder = PathBuilder::stroke(px(width));
    if let Some(dash) = dash {
        builder = builder.dash_array(dash);
    }
    builder.move_to(points[0]);
    builder.cubic_bezier_to(points[3], points[1], points[2]);
    // A degenerate curve (two cards on top of each other) fails to tessellate; it is simply not drawn.
    builder.build().ok()
}

// ---------------------------------------------------------------------------------------------------------------
// FlowCanvas

/// The live node-link graph of a space's agents, filling the bounds it is given.
///
/// The layout ([`FlowLayout::compute`]) is computed from the element's own bounds in the same frame they are known
/// (in prepaint), so a resize never draws a frame laid out for the old size. Edges are painted under the cards;
/// an edge touching the [`FlowCanvas::focus`] is drawn at full strength and the others fade. Agents of the
/// [`FlowCanvas::running_stage`] glow in the accent, paused agents are dimmed, and a working agent shows a spinner.
///
/// See the module documentation for its test selectors.
#[derive(IntoElement)]
pub struct FlowCanvas {
    id: ElementId,
    graph: AgentGraph,
    live: HashMap<AgentNodeId, PaneLive>,
    traffic: HashMap<(String, String), usize>,
    running_stage: Option<LoopStage>,
    focus: Option<FlowFocus>,
    notes: HashMap<AgentNodeId, SharedString>,
    on_select: Option<SelectHandler>,
    on_open: Option<NodeHandler>,
}

impl FlowCanvas {
    /// A canvas drawing `graph`, with no traffic, selection or handlers. `id` must be unique among its siblings.
    pub fn new(id: impl Into<ElementId>, graph: AgentGraph) -> Self {
        Self {
            id: id.into(),
            graph,
            live: HashMap::new(),
            traffic: HashMap::new(),
            running_stage: None,
            focus: None,
            notes: HashMap::new(),
            on_select: None,
            on_open: None,
        }
    }

    /// What each agent's pane does; a card whose graph state is still IDLE shows it instead (see [`card_badge`]),
    /// and an IDLE agent whose pane runs counts as working (it spins).
    pub fn live(mut self, live: HashMap<AgentNodeId, PaneLive>) -> Self {
        self.live = live;
        self
    }

    /// How many recent messages went between each pair of agents, from [`xctx_nav::flow::pair_traffic`]. An edge
    /// with traffic is drawn thicker and gets a badge with the count that selects the pair.
    pub fn traffic(mut self, traffic: HashMap<(String, String), usize>) -> Self {
        self.traffic = traffic;
        self
    }

    /// The loop stage that is running, if any: the agents working in it ([`stage_of`]) glow.
    pub fn running_stage(mut self, stage: Option<LoopStage>) -> Self {
        self.running_stage = stage;
        self
    }

    /// The selected agent or pair: an agent's card gets an accent outline (both cards of a pair a lighter one),
    /// and the edges touching it are drawn at full strength while the rest fade.
    pub fn focus(mut self, focus: Option<FlowFocus>) -> Self {
        self.focus = focus;
        self
    }

    /// A one-line note on each named agent's card, such as a writer's attempt lineage. A blocked or failed agent
    /// without a note shows its reason there instead.
    pub fn notes(mut self, notes: HashMap<AgentNodeId, SharedString>) -> Self {
        self.notes = notes;
        self
    }

    /// Call `handler` with what the user selected: `Some(FlowFocus::Agent)` for a card, `Some(FlowFocus::Pair)`
    /// (the edge's `from`, then `to`) for an edge's traffic badge, and `None` for a click on empty canvas while
    /// something is selected.
    pub fn on_select(
        mut self,
        handler: impl Fn(Option<FlowFocus>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with an agent's id when its card is double-clicked, to open (focus) its pane. The first
    /// click of the double-click still selects the agent.
    pub fn on_open(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for FlowCanvas {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        CanvasElement(self)
    }
}

/// The element behind [`FlowCanvas`]: lays the cards out at the positions its bounds give them.
struct CanvasElement(FlowCanvas);

/// What [`CanvasElement`] computes in prepaint for paint.
struct CanvasPrepaint {
    /// Under everything: a click on it (and on nothing above it) clears the selection.
    background: Hitbox,
    /// The edges' strokes, in graph order.
    edges: Vec<(Path<Pixels>, Hsla)>,
    /// The `contract` labels on contract edges.
    labels: Vec<AnyElement>,
    /// The agents' cards.
    cards: Vec<AnyElement>,
    /// The traffic badges on edges.
    badges: Vec<AnyElement>,
    theme: Theme,
}

impl Element for CanvasElement {
    type RequestLayoutState = ();
    type PrepaintState = CanvasPrepaint;

    fn id(&self) -> Option<ElementId> {
        Some(self.0.id.clone())
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
        style.refine(&StyleRefinement::default().size_full());
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let theme = Theme::of(cx);
        let canvas = &self.0;
        let layout = FlowLayout::compute(
            &canvas.graph,
            bounds.size.width.into(),
            bounds.size.height.into(),
            &canvas.traffic,
        );
        let at = |p: FlowPoint| point(bounds.origin.x + px(p.x), bounds.origin.y + px(p.y));

        // First, so every card and badge inserted after it is above it.
        let background = window.insert_hitbox(bounds, HitboxBehavior::Normal);

        let edges = layout
            .edges
            .iter()
            .filter_map(|edge| {
                let (mut width, color, dash) = edge_stroke(edge.kind, &theme);
                if edge.traffic > 0 {
                    width += 1.0;
                }
                let color = match &canvas.focus {
                    Some(focus) if edge_in_focus(edge, focus) => color.alpha(1.0),
                    Some(_) => color.opacity(FADED),
                    None => color,
                };
                let path = stroke_curve(
                    [
                        at(edge.start),
                        at(edge.control1),
                        at(edge.control2),
                        at(edge.end),
                    ],
                    width,
                    dash,
                )?;
                Some((path, color))
            })
            .collect();

        let mut labels = Vec::new();
        let mut badges = Vec::new();
        for edge in &layout.edges {
            let emphasis = match &canvas.focus {
                Some(focus) if !edge_in_focus(edge, focus) => FADED,
                _ => 1.0,
            };
            let mid = at(edge.mid);
            if edge.kind == EdgeKind::Contract {
                let mut label = contract_label(emphasis, &theme).into_any_element();
                let label_size = label.layout_as_root(AvailableSpace::min_size(), window, cx);
                // Above the traffic badge when there is one, else on the curve.
                let lift = if edge.traffic > 0 { px(20.0) } else { px(0.0) };
                label.prepaint_at(
                    point(
                        mid.x - label_size.width / 2.0,
                        mid.y - label_size.height / 2.0 - lift,
                    ),
                    window,
                    cx,
                );
                labels.push(label);
            }
            if edge.traffic > 0 {
                let selected = canvas.focus.as_ref().is_some_and(|focus| {
                    matches!(focus, FlowFocus::Pair(..)) && edge_in_focus(edge, focus)
                });
                let mut badge = traffic_badge(edge, selected, canvas.on_select.clone(), &theme)
                    .into_any_element();
                let badge_size = badge.layout_as_root(AvailableSpace::min_size(), window, cx);
                badge.prepaint_at(
                    point(
                        mid.x - badge_size.width / 2.0,
                        mid.y - badge_size.height / 2.0,
                    ),
                    window,
                    cx,
                );
                badges.push(badge);
            }
        }

        let card_size = size(px(layout.node_width), px(layout.node_height));
        let mut cards = Vec::with_capacity(layout.nodes.len());
        for flow_node in &layout.nodes {
            let Some(node) = canvas.graph.node(&flow_node.id) else {
                continue;
            };
            let live = canvas.live.get(&node.id).copied();
            let spinner = is_running(node, live).then(|| {
                window.use_keyed_state(
                    SharedString::from(format!("canvas-spinner-{}", node.id)),
                    cx,
                    |_, _| Spinner::new().size(IconSize::Xs).tone(Tone::Success),
                )
            });
            let card = NodeCard {
                node,
                live,
                color: agent_identity_color(&canvas.graph, node.id.as_str(), &theme)
                    .unwrap_or(theme.accent),
                note: canvas.notes.get(&node.id).cloned(),
                selected: canvas.focus == Some(FlowFocus::Agent(node.id.clone())),
                in_pair: matches!(&canvas.focus, Some(FlowFocus::Pair(a, b)) if a == &node.id || b == &node.id),
                glowing: canvas.running_stage.is_some()
                    && stage_of(&node.role) == canvas.running_stage,
                size: card_size,
            };
            let mut element = card
                .build(
                    spinner,
                    canvas.on_select.clone(),
                    canvas.on_open.clone(),
                    &theme,
                )
                .into_any_element();
            let origin = at(FlowPoint::new(
                flow_node.center.x - layout.node_width / 2.0,
                flow_node.center.y - layout.node_height / 2.0,
            ));
            element.prepaint_as_root(origin, card_size.map(AvailableSpace::Definite), window, cx);
            cards.push(element);
        }

        CanvasPrepaint {
            background,
            edges,
            labels,
            cards,
            badges,
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
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            paint_dot_grid(bounds, theme.border_subtle, window);
            for (path, color) in prepaint.edges.drain(..) {
                window.paint_path(path, color);
            }
            for label in &mut prepaint.labels {
                label.paint(window, cx);
            }
            for card in &mut prepaint.cards {
                card.paint(window, cx);
            }
            for badge in &mut prepaint.badges {
                badge.paint(window, cx);
            }
        });

        if let Some(on_select) = self.0.on_select.clone()
            && self.0.focus.is_some()
        {
            // Cards and badges occlude the background, so it is hovered only where the pointer is over empty
            // canvas (and over nothing another element drew on top of the canvas while blocking the mouse).
            let background = prepaint.background.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase == DispatchPhase::Bubble
                    && event.button == MouseButton::Left
                    && background.is_hovered(window)
                {
                    on_select(None, window, cx);
                }
            });
        }
    }
}

/// A sparse grid of 1px dots over `bounds`.
fn paint_dot_grid(bounds: Bounds<Pixels>, color: Hsla, window: &mut Window) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    let mut y = DOT_SPACING / 2.0;
    while y < height {
        let mut x = DOT_SPACING / 2.0;
        while x < width {
            let origin = point(bounds.origin.x + px(x), bounds.origin.y + px(y));
            window.paint_quad(fill(Bounds::new(origin, size(px(1.0), px(1.0))), color));
            x += DOT_SPACING;
        }
        y += DOT_SPACING;
    }
}

/// The small `contract` tag on a contract edge.
fn contract_label(emphasis: f32, theme: &Theme) -> gpui::Div {
    div()
        .px(Theme::SPACE_1)
        .rounded(Theme::RADIUS_SM)
        .bg(theme.surface)
        .text_size(px(10.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.accent.opacity(emphasis))
        .child("contract")
}

/// The badge on an edge two agents talked over: a mail icon and the message count. Clicking it selects the pair.
fn traffic_badge(
    edge: &FlowEdge,
    selected: bool,
    on_select: Option<SelectHandler>,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let selector = format!("canvas-edge-{}-{}", edge.from, edge.to);
    let (from, to) = (edge.from.clone(), edge.to.clone());
    let border_hover = theme.border_strong;
    div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector)
        .occlude()
        .flex()
        .items_center()
        .gap(Theme::SPACE_1)
        .h(px(20.0))
        .px(Theme::SPACE_2)
        .rounded(Theme::RADIUS_FULL)
        .border_1()
        .border_color(if selected { theme.accent } else { theme.border })
        .bg(theme.overlay)
        .text_size(Theme::TEXT_XS)
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(theme.meta)
        .cursor_pointer()
        .when(!selected, |badge| {
            badge.hover(move |style| style.border_color(border_hover))
        })
        .child(
            Icon::new(IconName::Send)
                .size(IconSize::Xs)
                .color(theme.meta),
        )
        .child(edge.traffic.to_string())
        .on_click(move |_, window, cx| {
            if let Some(on_select) = &on_select {
                on_select(Some(FlowFocus::Pair(from.clone(), to.clone())), window, cx);
            }
        })
}

/// What one agent's card on the canvas shows.
struct NodeCard<'a> {
    node: &'a AgentNode,
    live: Option<PaneLive>,
    color: Hsla,
    note: Option<SharedString>,
    /// The agent is the selection.
    selected: bool,
    /// The agent is one of the selected pair.
    in_pair: bool,
    /// The agent works in the running stage.
    glowing: bool,
    size: gpui::Size<Pixels>,
}

impl NodeCard<'_> {
    fn build(
        self,
        spinner: Option<gpui::Entity<Spinner>>,
        on_select: Option<SelectHandler>,
        on_open: Option<NodeHandler>,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        let node = self.node;
        let (state_label, tone) = card_badge(&node.state, self.live);
        let note = self
            .note
            .map(|note| (note, theme.text_subtle))
            .or_else(|| state_reason(&node.state, theme));

        let mut shadows = vec![BoxShadow {
            color: hsla(0.0, 0.0, 0.0, 0.18),
            offset: point(px(0.0), px(1.0)),
            blur_radius: px(3.0),
            spread_radius: px(0.0),
            inset: false,
        }];
        if self.glowing {
            shadows.push(BoxShadow {
                color: theme.accent.opacity(0.35),
                offset: point(px(0.0), px(0.0)),
                blur_radius: px(14.0),
                spread_radius: px(1.0),
                inset: false,
            });
        }
        if self.selected {
            // With the 1px accent border, a 2px outline that does not move the card's content.
            shadows.push(BoxShadow {
                color: theme.accent,
                offset: point(px(0.0), px(0.0)),
                blur_radius: px(0.0),
                spread_radius: px(1.0),
                inset: false,
            });
        }
        let border = if self.selected {
            theme.accent
        } else if self.in_pair {
            theme.accent.opacity(0.7)
        } else if self.glowing {
            theme.accent.opacity(0.5)
        } else {
            theme.border_subtle
        };
        let border_hover = theme.border_strong;
        let hoverable = !(self.selected || self.in_pair);

        let selector = format!("canvas-node-{}", node.id);
        let note_selector = format!("canvas-note-{}", node.id);
        let id = node.id.clone();
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector)
            .occlude()
            .flex()
            .w(self.size.width)
            .h(self.size.height)
            .rounded(Theme::RADIUS_LG)
            .border_1()
            .border_color(border)
            .bg(theme.surface)
            .shadow(shadows)
            .cursor_pointer()
            .when(hoverable, |card| {
                card.hover(move |style| style.border_color(border_hover))
            })
            .when(node.state == AgentState::Paused, |card| card.opacity(0.6))
            .child(
                div()
                    .flex_none()
                    .w(px(3.0))
                    .my(Theme::SPACE_2)
                    .ml(Theme::SPACE_2)
                    .rounded_full()
                    .bg(self.color),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .justify_center()
                    .flex_1()
                    .min_w_0()
                    .gap(Theme::SPACE_0_5)
                    .px(Theme::SPACE_2)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(Theme::SPACE_1 + px(2.0))
                            .child(
                                Icon::new(role_icon(&node.role))
                                    .size(IconSize::Xs)
                                    .color(self.color),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(Theme::TEXT_MD)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text)
                                    .child(node.label.clone()),
                            )
                            .children(spinner)
                            .child(Badge::new(state_label).tone(tone)),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(Theme::SPACE_2)
                            .text_size(Theme::TEXT_XS)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(theme.text_muted)
                                    .child(role_line(&node.role)),
                            )
                            .children(turns_text(node.turns_used).map(|turns| {
                                div().flex_none().text_color(theme.meta).child(turns)
                            })),
                    )
                    .children(note.map(|(note, color)| {
                        div()
                            .debug_selector(move || note_selector)
                            .min_w_0()
                            .truncate()
                            .text_size(Theme::TEXT_XS)
                            .text_color(color)
                            .child(note)
                    })),
            )
            .on_click(move |event, window, cx| {
                if event.click_count() >= 2 {
                    if let Some(on_open) = &on_open {
                        on_open(&id, window, cx);
                    }
                } else if let Some(on_select) = &on_select {
                    on_select(Some(FlowFocus::Agent(id.clone())), window, cx);
                }
            })
    }
}

// ---------------------------------------------------------------------------------------------------------------
// AgentDetail

/// The drawer for the selected agent, [`DETAIL_WIDTH`] wide: its identity, role and loop stage, its state (with
/// the reason when blocked or failed), runner, turns and pane, and the actions it allows: Open pane (bound to a
/// pane) or Start (not), Pause or Resume as its state allows, Fork… for a writer, Remove for a helper, and close.
///
/// It blocks the mouse from what is under it, so a click in it never reaches the canvas. See the module
/// documentation for its test selectors.
#[derive(IntoElement)]
pub struct AgentDetail {
    node: AgentNode,
    color: Hsla,
    live: Option<PaneLive>,
    note: Option<SharedString>,
    runner: Option<SharedString>,
    fork_allowed: bool,
    on_open: Option<NodeHandler>,
    on_start: Option<NodeHandler>,
    on_pause: Option<NodeHandler>,
    on_resume: Option<NodeHandler>,
    on_fork: Option<NodeHandler>,
    on_remove: Option<NodeHandler>,
    on_close: Option<Handler>,
}

impl AgentDetail {
    /// A drawer for `node`, marked with its identity colour `color` ([`agent_identity_color`]).
    pub fn new(node: AgentNode, color: Hsla) -> Self {
        Self {
            node,
            color,
            live: None,
            note: None,
            runner: None,
            fork_allowed: true,
            on_open: None,
            on_start: None,
            on_pause: None,
            on_resume: None,
            on_fork: None,
            on_remove: None,
            on_close: None,
        }
    }

    /// What the agent's pane does, shown as its pane status and in its badge while its graph state is IDLE.
    pub fn live(mut self, live: Option<PaneLive>) -> Self {
        self.live = live;
        self
    }

    /// A line of text under the facts, such as a writer's attempt lineage or why it cannot fork.
    pub fn note(mut self, note: Option<SharedString>) -> Self {
        self.note = note;
        self
    }

    /// The runner the agent's pane runs (`omp`, `pi`, `claude`). Without it the drawer shows the node's own
    /// runner, or `omp`, the default.
    pub fn runner(mut self, runner: SharedString) -> Self {
        self.runner = Some(runner);
        self
    }

    /// Whether a writer may fork competing attempts (its runner can fork sessions). True unless set; Fork… shows
    /// only for writers, with [`AgentDetail::on_fork`] set.
    pub fn fork_allowed(mut self, allowed: bool) -> Self {
        self.fork_allowed = allowed;
        self
    }

    /// Call `handler` with the agent's id when Open pane is clicked (shown when the agent has a pane).
    pub fn on_open(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the agent's id when Start is clicked (shown when the agent has no pane).
    pub fn on_start(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_start = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the agent's id when Pause is clicked (shown when [`can_pause`]).
    pub fn on_pause(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_pause = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the agent's id when Resume is clicked (shown when [`can_resume`]).
    pub fn on_resume(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_resume = Some(Rc::new(handler));
        self
    }

    /// Show Fork… on a writer (unless [`AgentDetail::fork_allowed`] is false) and call `handler` with its id
    /// when it is clicked.
    pub fn on_fork(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_fork = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the helper's id when Remove is clicked (shown for helpers only).
    pub fn on_remove(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_remove = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when the drawer's × is clicked.
    pub fn on_close(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }
}

/// A drawer action button, wired to `handler` with the agent's id.
fn detail_button(
    action: &'static str,
    label: &'static str,
    icon: IconName,
    variant: ButtonVariant,
    id: &AgentNodeId,
    handler: Option<NodeHandler>,
) -> Button {
    let selector = format!("detail-{action}");
    let mut button = Button::new(SharedString::from(selector.clone()), label)
        .icon(icon)
        .variant(variant)
        .size(ButtonSize::Compact)
        .selector(selector);
    if let Some(handler) = handler {
        let id = id.clone();
        button = button.on_click(move |_, window, cx| handler(&id, window, cx));
    }
    button
}

/// One `label  value` row of the drawer's facts.
fn fact(
    label: &'static str,
    value: impl Into<SharedString>,
    color: Hsla,
    theme: &Theme,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(Theme::SPACE_2)
        .text_size(Theme::TEXT_SM)
        .child(
            div()
                .flex_none()
                .w(px(64.0))
                .text_size(Theme::TEXT_XS)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text_subtle)
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(color)
                .child(value.into()),
        )
}

impl RenderOnce for AgentDetail {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let node = &self.node;
        let (state_label, tone) = card_badge(&node.state, self.live);
        let mut subtitle = role_line(&node.role);
        if let Some(stage) = stage_of(&node.role) {
            subtitle.push_str(" · ");
            subtitle.push_str(loop_stage_label(stage));
            subtitle.push_str(" stage");
        }
        let runner = self
            .runner
            .clone()
            .or_else(|| node.runner.clone().map(SharedString::from))
            .unwrap_or_else(|| SharedString::from("omp"));
        let (pane, pane_color) = match self.live {
            Some(PaneLive::Running) => ("running".to_owned(), theme.success),
            Some(PaneLive::Exited(Some(code))) => (format!("exited ({code})"), theme.warning),
            Some(PaneLive::Exited(None)) => ("exited".to_owned(), theme.warning),
            Some(PaneLive::NoPane) => ("no pane".to_owned(), theme.text_muted),
            None if node.pane_id.is_none() => ("no pane".to_owned(), theme.text_muted),
            None => ("open".to_owned(), theme.text),
        };

        let primary = if node.pane_id.is_some() {
            detail_button(
                "open",
                "Open pane",
                IconName::Terminal,
                ButtonVariant::Primary,
                &node.id,
                self.on_open.clone(),
            )
        } else {
            detail_button(
                "start",
                "Start",
                IconName::Play,
                ButtonVariant::Primary,
                &node.id,
                self.on_start.clone(),
            )
        };
        let pause = can_pause(&node.state).then(|| {
            detail_button(
                "pause",
                "Pause",
                IconName::Pause,
                ButtonVariant::Secondary,
                &node.id,
                self.on_pause.clone(),
            )
        });
        let resume = can_resume(&node.state).then(|| {
            detail_button(
                "resume",
                "Resume",
                IconName::Play,
                ButtonVariant::Secondary,
                &node.id,
                self.on_resume.clone(),
            )
        });
        let fork =
            (node.role.is_writer() && self.fork_allowed && self.on_fork.is_some()).then(|| {
                detail_button(
                    "fork",
                    "Fork…",
                    IconName::Fork,
                    ButtonVariant::Ghost,
                    &node.id,
                    self.on_fork.clone(),
                )
            });
        let remove = matches!(node.role, AgentRole::Helper { .. }).then(|| {
            detail_button(
                "remove",
                "Remove",
                IconName::Trash,
                ButtonVariant::Danger,
                &node.id,
                self.on_remove.clone(),
            )
        });
        let mut close = Button::new("detail-close", "")
            .icon(IconName::Close)
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Compact)
            .tooltip("Close")
            .selector("detail-close");
        if let Some(on_close) = self.on_close.clone() {
            close = close.on_click(move |_, window, cx| on_close(window, cx));
        }

        div()
            .id("agent-detail")
            .debug_selector(|| "agent-detail".to_owned())
            .occlude()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_3)
            .w_full()
            .p(Theme::SPACE_3)
            .rounded(Theme::RADIUS_LG)
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.overlay)
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(
                        div()
                            .flex_none()
                            .w(px(3.0))
                            .h(px(30.0))
                            .rounded_full()
                            .bg(self.color),
                    )
                    .child(
                        Icon::new(role_icon(&node.role))
                            .size(IconSize::Md)
                            .color(self.color),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .truncate()
                                    .text_size(Theme::TEXT_LG)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text)
                                    .child(node.label.clone()),
                            )
                            .child(
                                div()
                                    .truncate()
                                    .text_size(Theme::TEXT_XS)
                                    .text_color(theme.text_muted)
                                    .child(subtitle),
                            ),
                    )
                    .child(close),
            )
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap(Theme::SPACE_2)
                    .child(Badge::new(state_label).tone(tone))
                    .children(state_reason(&node.state, &theme).map(|(reason, color)| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(Theme::TEXT_SM)
                            .text_color(color)
                            .child(reason)
                    })),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_1)
                    .child(fact("RUNNER", runner, theme.text, &theme))
                    .child(fact(
                        "TURNS",
                        node.turns_used.to_string(),
                        theme.meta,
                        &theme,
                    ))
                    .child(fact("PANE", pane, pane_color, &theme)),
            )
            .children(self.note.clone().map(|note| {
                div()
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_subtle)
                    .child(note)
            }))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .pt(Theme::SPACE_2)
                    .border_t_1()
                    .border_color(theme.border_subtle)
                    .child(primary)
                    .children(pause)
                    .children(resume)
                    .children(fork)
                    .child(div().flex_1())
                    .children(remove),
            )
    }
}

// ---------------------------------------------------------------------------------------------------------------
// FlowView

/// The Flow view's layout: the stage track across the top, and below it the canvas (with its add-agent toolbar
/// top-right and an edge legend bottom-right) beside the right column: the selected agent's [`AgentDetail`], if
/// any, over the timeline.
///
/// It draws no background of its own, as it sits on the window's translucent chrome. See the module documentation
/// for its test selectors.
#[derive(IntoElement)]
pub struct FlowView {
    track: AnyElement,
    canvas: FlowCanvas,
    timeline: AnyElement,
    detail: Option<AgentDetail>,
    on_add_helper: Option<HelperHandler>,
    on_add_repo: Option<Handler>,
    timeline_width: Pixels,
}

impl FlowView {
    /// A Flow view of `track` (the stage track), `canvas` and `timeline`, with no drawer and no toolbar.
    pub fn new(track: impl IntoElement, canvas: FlowCanvas, timeline: impl IntoElement) -> Self {
        Self {
            track: track.into_any_element(),
            canvas,
            timeline: timeline.into_any_element(),
            detail: None,
            on_add_helper: None,
            on_add_repo: None,
            timeline_width: DEFAULT_TIMELINE_WIDTH,
        }
    }

    /// The selected agent's detail, shown at the top of the right column above its activity; `None` hides it.
    pub fn detail(mut self, detail: Option<AgentDetail>) -> Self {
        self.detail = detail;
        self
    }

    /// Show the toolbar's add-helper buttons (Tests, Reviewer, Probe, Browser) and call `handler` with the kind
    /// clicked.
    pub fn on_add_helper(
        mut self,
        handler: impl Fn(HelperKind, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_add_helper = Some(Rc::new(handler));
        self
    }

    /// Show the toolbar's Repo button and call `handler` when it is clicked, to add a repo to the space.
    pub fn on_add_repo(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_add_repo = Some(Rc::new(handler));
        self
    }

    /// The width of the timeline column; [`DEFAULT_TIMELINE_WIDTH`] unless set.
    pub fn timeline_width(mut self, width: Pixels) -> Self {
        self.timeline_width = width;
        self
    }
}

/// A legend entry: a short sample of an edge of `kind` and what it means.
fn legend_item(kind: EdgeKind, label: &'static str, theme: &Theme) -> gpui::Div {
    let (width, color, dash) = edge_stroke(kind, theme);
    div()
        .flex()
        .items_center()
        .gap(Theme::SPACE_1)
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, (), window, _| {
                    let y = bounds.center().y;
                    let (left, right) = (bounds.left(), bounds.right());
                    let third = (right - left) / 3.0;
                    let points = [
                        point(left, y),
                        point(left + third, y),
                        point(right - third, y),
                        point(right, y),
                    ];
                    if let Some(path) = stroke_curve(points, width, dash) {
                        window.paint_path(path, color);
                    }
                },
            )
            .w(px(20.0))
            .h(px(8.0)),
        )
        .child(label)
}

impl RenderOnce for FlowView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);

        let helper_buttons = self.on_add_helper.clone().map(|handler| {
            HELPER_KINDS.into_iter().map(move |kind| {
                let selector = format!("flow-add-{}", helper_kind_name(kind));
                let handler = handler.clone();
                Button::new(
                    SharedString::from(selector.clone()),
                    helper_kind_label(kind),
                )
                .icon(IconName::Plus)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .tooltip(format!(
                    "Add a {} helper",
                    helper_kind_label(kind).to_lowercase()
                ))
                .selector(selector)
                .on_click(move |_, window, cx| handler(kind, window, cx))
            })
        });
        let repo_button = self.on_add_repo.clone().map(|handler| {
            Button::new("flow-add-repo", "Repo")
                .icon(IconName::Plus)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .tooltip("Add a repo and its writer")
                .selector("flow-add-repo")
                .on_click(move |_, window, cx| handler(window, cx))
        });
        let has_toolbar = helper_buttons.is_some() || repo_button.is_some();
        let divider = (helper_buttons.is_some() && repo_button.is_some()).then(|| {
            div()
                .w(px(1.0))
                .h(px(14.0))
                .mx(Theme::SPACE_0_5)
                .bg(theme.border_subtle)
        });
        let toolbar = has_toolbar.then(|| {
            div()
                .absolute()
                .top(Theme::SPACE_3)
                .right(Theme::SPACE_3)
                .occlude()
                .flex()
                .items_center()
                .gap(Theme::SPACE_0_5)
                .p(Theme::SPACE_0_5)
                .rounded(Theme::RADIUS_LG)
                .border_1()
                .border_color(theme.border_subtle)
                .bg(theme.surface)
                .shadow_sm()
                .children(helper_buttons.into_iter().flatten())
                .children(divider)
                .children(repo_button)
        });
        let legend = div()
            .absolute()
            .bottom(Theme::SPACE_3)
            .right(Theme::SPACE_3)
            .occlude()
            .flex()
            .items_center()
            .gap(Theme::SPACE_3)
            .px(Theme::SPACE_2)
            .py(Theme::SPACE_1)
            .rounded(Theme::RADIUS_MD)
            .bg(theme.surface.opacity(0.6))
            .text_size(Theme::TEXT_XS)
            .text_color(theme.text_subtle)
            .child(legend_item(EdgeKind::Coordinates, "coordinates", &theme))
            .child(legend_item(EdgeKind::Contract, "contract", &theme))
            .child(legend_item(EdgeKind::Verifies, "verifies", &theme));
        // The selected agent's detail heads the activity column (whose entries are narrowed to that agent), so it
        // never covers a card of the canvas.
        let detail = self.detail.map(|detail| {
            div()
                .flex_none()
                .p(Theme::SPACE_2)
                .border_b_1()
                .border_color(theme.border_subtle)
                .child(detail)
        });

        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(
                div()
                    .flex_none()
                    .w_full()
                    .border_b_1()
                    .border_color(theme.border_subtle)
                    .child(self.track),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .relative()
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .overflow_hidden()
                            .bg(theme.surface.opacity(0.25))
                            .child(self.canvas)
                            .children(toolbar)
                            .child(legend),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .w(self.timeline_width)
                            .h_full()
                            .min_h_0()
                            .overflow_hidden()
                            .border_l_1()
                            .border_color(theme.border_subtle)
                            .children(detail)
                            .child(div().flex_1().min_h_0().child(self.timeline)),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_lines_name_the_role_and_its_repo_or_kind() {
        assert_eq!(role_line(&AgentRole::Orchestrator), "orchestrator");
        assert_eq!(
            role_line(&AgentRole::Writer { repo: "api".into() }),
            "writer · api"
        );
        assert_eq!(
            role_line(&AgentRole::Helper {
                kind: HelperKind::Tests
            }),
            "helper · tests"
        );
    }

    #[test]
    fn an_idle_agent_counts_as_running_only_while_its_pane_runs() {
        let mut graph = AgentGraph::for_space(&["api".to_owned()]);
        let api = AgentNodeId::new("writer-api");
        let idle = graph.node(&api).cloned();
        let Some(idle) = idle else {
            panic!("the writer is in the graph");
        };
        assert!(!is_running(&idle, None));
        assert!(is_running(&idle, Some(PaneLive::Running)));
        assert!(!is_running(&idle, Some(PaneLive::Exited(Some(0)))));
        assert!(graph.pause(&api).is_ok());
        let paused = graph.node(&api).cloned();
        let Some(paused) = paused else {
            panic!("the writer is in the graph");
        };
        assert!(!is_running(&paused, Some(PaneLive::Running)));
    }
}
