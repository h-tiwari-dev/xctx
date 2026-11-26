//! The bar across the top of the window: Home, a tab per open space, and for the active space its shape
//! selector and the Ship and Discard actions.

use std::rc::Rc;

use gpui::{
    App, ElementId, FontWeight, Pixels, SharedString, Window, div, prelude::*, px,
    transparent_black,
};
use xctx_layout::Shape;

use crate::components::{Button, ButtonVariant, Pill};
use crate::theme::Theme;

/// Every shape, in the order the selector shows them.
pub const SHAPES: [Shape; 3] = [Shape::Dev, Shape::Debug, Shape::Review];

/// The label the shape selector shows for `shape`.
pub fn shape_label(shape: Shape) -> &'static str {
    match shape {
        Shape::Dev => "Dev",
        Shape::Debug => "Debug",
        Shape::Review => "Review",
    }
}

/// One open space in the tab bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabItem {
    /// The space's id (its slug); handed back to [`TabBar::on_select`] when the tab is clicked.
    pub id: SharedString,
    /// The tab's text, usually the task or slug.
    pub title: SharedString,
    /// Short extra text after the title, such as the number of repos.
    pub badge: Option<SharedString>,
    /// Whether this is the space being shown.
    pub active: bool,
    /// Whether the space has a running agent; shown as a dot before the title.
    pub running: bool,
}

impl TabItem {
    /// An inactive, idle tab with no badge.
    pub fn new(id: impl Into<SharedString>, title: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            badge: None,
            active: false,
            running: false,
        }
    }

    /// Set the short text after the title.
    pub fn badge(mut self, badge: impl Into<SharedString>) -> Self {
        self.badge = Some(badge.into());
        self
    }

    /// Mark the tab as the one being shown.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Mark the space as having a running agent.
    pub fn running(mut self, running: bool) -> Self {
        self.running = running;
        self
    }
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type SelectHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;
type ShapeHandler = Rc<dyn Fn(Shape, &mut Window, &mut App)>;

/// The top bar. The app passes what to show and wires the callbacks; the bar keeps no state.
///
/// The shape selector, Ship and Discard appear only when [`TabBar::active_shape`] is set, which the app does
/// while a space is shown. The Graph toggle (test selector `tab-graph`) appears next to the shape selector when
/// a space is shown and [`TabBar::on_graph`] is set; "+ Agent" (`tab-add-agent`) next to it with
/// [`TabBar::on_add_agent`] and "+ Repo" (`tab-add-repo`) with [`TabBar::on_add_repo`].
#[derive(IntoElement)]
pub struct TabBar {
    home_active: bool,
    tabs: Vec<TabItem>,
    active_shape: Option<Shape>,
    graph_open: bool,
    leading_inset: Pixels,
    on_home: Option<Handler>,
    on_select: Option<SelectHandler>,
    on_shape: Option<ShapeHandler>,
    on_ship: Option<Handler>,
    on_discard: Option<Handler>,
    on_graph: Option<Handler>,
    on_add_repo: Option<Handler>,
    on_add_agent: Option<Handler>,
}

impl Default for TabBar {
    fn default() -> Self {
        Self::new()
    }
}

impl TabBar {
    /// An empty bar with Home inactive and no space shown.
    pub fn new() -> Self {
        Self {
            home_active: false,
            tabs: Vec::new(),
            active_shape: None,
            graph_open: false,
            leading_inset: Theme::SPACE_2,
            on_home: None,
            on_select: None,
            on_shape: None,
            on_ship: None,
            on_discard: None,
            on_graph: None,
            on_add_repo: None,
            on_add_agent: None,
        }
    }

    /// Highlight the Home tab.
    pub fn home_active(mut self, active: bool) -> Self {
        self.home_active = active;
        self
    }

    /// The open spaces, in tab order.
    pub fn tabs(mut self, tabs: impl IntoIterator<Item = TabItem>) -> Self {
        self.tabs = tabs.into_iter().collect();
        self
    }

    /// The shape of the space being shown, or `None` on Home. Shows the shape selector, Ship and Discard.
    pub fn active_shape(mut self, shape: Option<Shape>) -> Self {
        self.active_shape = shape;
        self
    }

    /// Show the Graph toggle as on, while the Agent Graph and Shared Space panel are open.
    pub fn graph_open(mut self, open: bool) -> Self {
        self.graph_open = open;
        self
    }

    /// Space to leave before the Home tab, for example for the traffic lights of a transparent title bar.
    pub fn leading_inset(mut self, inset: Pixels) -> Self {
        self.leading_inset = inset;
        self
    }

    /// Called when Home is clicked.
    pub fn on_home(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_home = Some(Rc::new(handler));
        self
    }

    /// Called with a tab's [`TabItem::id`] when it is clicked.
    pub fn on_select(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    /// Called with the shape picked in the shape selector (also when it is the current one).
    pub fn on_shape(mut self, handler: impl Fn(Shape, &mut Window, &mut App) + 'static) -> Self {
        self.on_shape = Some(Rc::new(handler));
        self
    }

    /// Called when Ship is clicked.
    pub fn on_ship(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_ship = Some(Rc::new(handler));
        self
    }

    /// Called when Discard is clicked.
    pub fn on_discard(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_discard = Some(Rc::new(handler));
        self
    }

    /// Offer the Graph toggle while a space is shown, calling `handler` when it is clicked.
    pub fn on_graph(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_graph = Some(Rc::new(handler));
        self
    }

    /// Offer "+ Repo" while a space is shown (add another repo to it), calling `handler` when it is clicked.
    pub fn on_add_repo(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_add_repo = Some(Rc::new(handler));
        self
    }

    /// Offer "+ Agent" while a space is shown (start an agent whose pane was closed, or add a helper), calling
    /// `handler` when it is clicked.
    pub fn on_add_agent(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_add_agent = Some(Rc::new(handler));
        self
    }
}

/// One tab's look, shared by Home and the space tabs.
fn tab(
    id: impl Into<ElementId>,
    theme: &Theme,
    active: bool,
    on_click: Option<Handler>,
) -> gpui::Stateful<gpui::Div> {
    let hover_bg = theme.raised;
    let mut tab = div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(Theme::SPACE_1 + px(2.0))
        .h(px(26.0))
        .px(Theme::SPACE_2 + px(2.0))
        .rounded(Theme::RADIUS_MD)
        .border_1()
        .text_size(Theme::TEXT_SM)
        .whitespace_nowrap()
        .cursor_pointer();
    tab = if active {
        tab.bg(theme.raised)
            .border_color(theme.border)
            .text_color(theme.text)
            .font_weight(FontWeight::SEMIBOLD)
    } else {
        tab.border_color(transparent_black())
            .text_color(theme.text_muted)
            .hover(move |style| style.bg(hover_bg))
    };
    if let Some(handler) = on_click {
        tab = tab.on_click(move |_, window, cx| handler(window, cx));
    }
    tab
}

impl RenderOnce for TabBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);

        let home = tab("tab-home", &theme, self.home_active, self.on_home).child("Home");

        let space_tabs = self.tabs.into_iter().enumerate().map(|(index, item)| {
            let on_select = self.on_select.clone().map(|on_select| {
                let space = item.id.clone();
                Rc::new(move |window: &mut Window, cx: &mut App| on_select(&space, window, cx))
                    as Handler
            });
            let running = item.running.then(|| {
                div()
                    .flex_none()
                    .size(px(6.0))
                    .rounded_full()
                    .bg(theme.success)
            });
            let badge = item.badge.map(|badge| {
                div()
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_subtle)
                    .child(badge)
            });
            tab(("tab-space", index), &theme, item.active, on_select)
                .max_w(px(240.0))
                .children(running)
                .child(div().min_w_0().truncate().child(item.title))
                .children(badge)
        });

        let space_actions = self.active_shape.map(|current| {
            let on_shape = self.on_shape.clone();
            let pills = SHAPES.into_iter().map(move |shape| {
                let mut pill = Pill::new(("tab-shape", shape as usize), shape_label(shape))
                    .selected(shape == current);
                if let Some(on_shape) = on_shape.clone() {
                    pill = pill.on_click(move |_, window, cx| on_shape(shape, window, cx));
                }
                pill
            });
            let graph = self.on_graph.map(|handler| {
                div()
                    .flex()
                    .items_center()
                    .p(Theme::SPACE_0_5)
                    .rounded(Theme::RADIUS_FULL)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.surface)
                    .child(
                        Pill::new("tab-graph", "Graph")
                            .selected(self.graph_open)
                            .selector("tab-graph")
                            .on_click(move |_, window, cx| handler(window, cx)),
                    )
            });
            let add_repo = self.on_add_repo.map(|handler| {
                Button::new("tab-add-repo", "+ Repo")
                    .variant(ButtonVariant::Ghost)
                    .selector("tab-add-repo")
                    .on_click(move |_, window, cx| handler(window, cx))
            });
            let add_agent = self.on_add_agent.map(|handler| {
                Button::new("tab-add-agent", "+ Agent")
                    .variant(ButtonVariant::Ghost)
                    .selector("tab-add-agent")
                    .on_click(move |_, window, cx| handler(window, cx))
            });
            let mut discard = Button::new("tab-discard", "Discard").variant(ButtonVariant::Danger);
            if let Some(handler) = self.on_discard {
                discard = discard.on_click(move |_, window, cx| handler(window, cx));
            }
            let mut ship = Button::new("tab-ship", "Ship").variant(ButtonVariant::Primary);
            if let Some(handler) = self.on_ship {
                ship = ship.on_click(move |_, window, cx| handler(window, cx));
            }
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(Theme::SPACE_0_5)
                        .p(Theme::SPACE_0_5)
                        .rounded(Theme::RADIUS_FULL)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.surface)
                        .children(pills),
                )
                .children(graph)
                .children(add_agent)
                .children(add_repo)
                .child(div().w(px(1.0)).h(px(16.0)).bg(theme.border_subtle))
                .child(discard)
                .child(ship)
        });

        div()
            .flex()
            .flex_none()
            .items_center()
            .w_full()
            .h(Theme::TAB_BAR_HEIGHT)
            .pl(self.leading_inset)
            .pr(Theme::SPACE_2)
            .gap(Theme::SPACE_2)
            .bg(theme.chrome)
            .border_b_1()
            .border_color(theme.border_subtle)
            .child(home)
            .child(
                div()
                    .flex_none()
                    .w(px(1.0))
                    .h(px(16.0))
                    .bg(theme.border_subtle),
            )
            .child(
                div()
                    .id("tab-spaces")
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .overflow_x_scroll()
                    .children(space_tabs),
            )
            .children(space_actions)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_are_listed_once_with_labels() {
        let labels: Vec<_> = SHAPES.into_iter().map(shape_label).collect();
        assert_eq!(labels, ["Dev", "Debug", "Review"]);
    }

    #[test]
    fn tab_item_builder_sets_fields() {
        let item = TabItem::new("fix-retry", "make checkout retry safe")
            .badge("2 repos")
            .active(true)
            .running(true);
        assert_eq!(item.id.as_ref(), "fix-retry");
        assert_eq!(item.badge.as_ref().map(|b| b.as_ref()), Some("2 repos"));
        assert!(item.active && item.running);
        let idle = TabItem::new("a", "b");
        assert!(!idle.active && !idle.running && idle.badge.is_none());
    }

    #[test]
    fn a_new_bar_shows_no_space_actions() {
        let bar = TabBar::new();
        assert!(bar.active_shape.is_none());
        assert!(!bar.home_active);
        assert!(bar.on_graph.is_none() && !bar.graph_open);
        let bar = bar
            .home_active(true)
            .active_shape(Some(Shape::Debug))
            .graph_open(true);
        assert_eq!(bar.active_shape, Some(Shape::Debug));
        assert!(bar.graph_open);
    }
}
