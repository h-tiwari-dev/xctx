//! The window's top row, [`Theme::TITLE_BAR_HEIGHT`] tall: room for the traffic lights and the Roster toggle over
//! the Roster's column, then the shown view's title, for a space its branch and env, a search pill that opens the
//! command palette, and for a space its actions (shape, Graph, + Agent, + Repo, Discard, Ship).
//!
//! The window's system title bar is transparent and the app moves the window itself, so every part of the row that
//! is not a control moves the window when dragged and zooms it when double-clicked: one
//! [`window_drag_area`] fills the row behind its content, and each control blocks the mouse from reaching it.
//!
//! Test selectors: `title-sidebar`, `title-palette`, `title-shape-<i>` (in [`SHAPES`] order), `title-mode-panes`,
//! `title-mode-flow`,
//! `title-add-agent`, `title-add-repo`, `title-discard`, `title-ship`.

use std::rc::Rc;

use gpui::{App, FontWeight, Pixels, SharedString, Window, div, prelude::*, px, svg};
use xctx_layout::Shape;

use crate::components::{Badge, Button, ButtonVariant, Tooltip};
use crate::icon::{Icon, IconName, IconSize};
use crate::roster::ROSTER_WIDTH;
use crate::status_bar::keycap;
use crate::theme::{Theme, Tone};
use crate::window_chrome::window_drag_area;

/// Every shape, in the order the shape control shows them.
pub const SHAPES: [Shape; 3] = [Shape::Dev, Shape::Debug, Shape::Review];

/// The label the shape control shows for `shape`.
pub fn shape_label(shape: Shape) -> &'static str {
    match shape {
        Shape::Dev => "Dev",
        Shape::Debug => "Debug",
        Shape::Review => "Review",
    }
}

/// What the title bar shows about the space being shown.
#[derive(Debug, Clone, PartialEq)]
pub struct SpaceHeaderInfo {
    /// The space's current shape, selected in the shape control.
    pub shape: Shape,
    /// Whether the Activity panel is open on its Graph tab.
    pub flow_open: bool,
    /// Whether the space runs with a `production` env; draws the env chip as a danger badge.
    pub production: bool,
    /// The git branch of the focused pane's worktree; empty to hide its chip.
    pub branch: SharedString,
    /// The space's `MISE_ENV`; empty to hide its chip.
    pub env: SharedString,
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type ShapeHandler = Rc<dyn Fn(Shape, &mut Window, &mut App)>;

/// Width of the search pill at rest; it shrinks first when the window is narrow.
const SEARCH_WIDTH: Pixels = px(280.0);
/// Height of the search pill, the shape control and the buttons.
const CONTROL_HEIGHT: Pixels = px(26.0);
/// Width of the Roster toggle and the room right of it when the Roster is collapsed.
const TOGGLE_WIDTH: Pixels = px(28.0);

/// The top row of the window. It has no background of its own (it sits on the window chrome) and a
/// `border_subtle` line along its bottom; with the Roster expanded the line stops at the Roster's column, whose
/// right edge it continues, so the sidebar reads as one full-height column.
#[derive(IntoElement)]
pub struct TitleBar {
    title: SharedString,
    sidebar_width: Pixels,
    sidebar_collapsed: bool,
    space: Option<SpaceHeaderInfo>,
    on_toggle_sidebar: Option<Handler>,
    on_palette: Option<Handler>,
    on_toggle_flow: Option<Handler>,
    on_add_agent: Option<Handler>,
    on_add_repo: Option<Handler>,
    on_shape: Option<ShapeHandler>,
    on_ship: Option<Handler>,
    on_discard: Option<Handler>,
}

impl TitleBar {
    /// A title bar for Home showing `title`, with an expanded [`ROSTER_WIDTH`] sidebar column.
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            sidebar_width: ROSTER_WIDTH,
            sidebar_collapsed: false,
            space: None,
            on_toggle_sidebar: None,
            on_palette: None,
            on_toggle_flow: None,
            on_add_agent: None,
            on_add_repo: None,
            on_shape: None,
            on_ship: None,
            on_discard: None,
        }
    }

    /// Line the left part up with a Roster `width` wide. When `collapsed`, the left part is just the traffic lights'
    /// room and the toggle, whatever `width` is.
    pub fn sidebar(mut self, width: Pixels, collapsed: bool) -> Self {
        self.sidebar_width = width;
        self.sidebar_collapsed = collapsed;
        self
    }

    /// Show a space's branch, env and actions. Without it the bar is Home's: title and search only.
    pub fn space(mut self, info: SpaceHeaderInfo) -> Self {
        self.space = Some(info);
        self
    }

    /// Call `handler` when the Roster toggle is clicked.
    pub fn on_toggle_sidebar(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle_sidebar = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when the search pill is clicked, to open the command palette.
    pub fn on_palette(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_palette = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when the mode switch's other side (Panes or Activity) is clicked.
    pub fn on_toggle_flow(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle_flow = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when "+ Agent" is clicked.
    pub fn on_add_agent(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_add_agent = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when "+ Repo" is clicked.
    pub fn on_add_repo(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_add_repo = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the shape chosen in the shape control (also when it is the current one).
    pub fn on_shape(mut self, handler: impl Fn(Shape, &mut Window, &mut App) + 'static) -> Self {
        self.on_shape = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Ship is clicked.
    pub fn on_ship(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_ship = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Discard is clicked; the app asks for confirmation.
    pub fn on_discard(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_discard = Some(Rc::new(handler));
        self
    }
}

/// Wrap a control so presses on it never reach the drag area behind the row.
fn control(element: impl IntoElement) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .occlude()
        .child(element)
}

/// A ghost icon-and-label (or icon-only) button of the title bar.
fn tool_button(
    selector: &'static str,
    icon: IconName,
    label: &'static str,
    tooltip: &'static str,
    handler: Option<Handler>,
) -> Button {
    let mut button = Button::new(selector, label)
        .icon(icon)
        .variant(ButtonVariant::Ghost)
        .tooltip(tooltip)
        .selector(selector);
    if let Some(handler) = handler {
        button = button.on_click(move |_, window, cx| handler(window, cx));
    }
    button
}

impl TitleBar {
    fn render_sidebar_segment(&self, theme: &Theme) -> gpui::Div {
        let mut toggle = Button::new("title-sidebar", "")
            .icon(IconName::Sidebar)
            .variant(ButtonVariant::Ghost)
            .selected(false)
            .tooltip("Toggle sidebar ⌘B")
            .selector("title-sidebar");
        if let Some(handler) = self.on_toggle_sidebar.clone() {
            toggle = toggle.on_click(move |_, window, cx| handler(window, cx));
        }
        let width = if self.sidebar_collapsed {
            Theme::TRAFFIC_LIGHT_INSET + TOGGLE_WIDTH + Theme::SPACE_2
        } else {
            self.sidebar_width
                .max(Theme::TRAFFIC_LIGHT_INSET + TOGGLE_WIDTH)
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_end()
            .w(width)
            .h_full()
            .pr(Theme::SPACE_2)
            .when(!self.sidebar_collapsed, |segment| {
                segment.border_r_1().border_color(theme.border_subtle)
            })
            .child(control(toggle))
    }

    fn render_search(&self, theme: &Theme) -> gpui::Stateful<gpui::Div> {
        let (hover_bg, hover_border) = (theme.hover, theme.border);
        let mut pill = div()
            .id("title-palette")
            .debug_selector(|| "title-palette".to_owned())
            .flex()
            .items_center()
            .gap(Theme::SPACE_2)
            .w(SEARCH_WIDTH)
            .min_w(px(120.0))
            .flex_shrink_1()
            .h(CONTROL_HEIGHT)
            .pl(Theme::SPACE_2 + px(2.0))
            .pr(Theme::SPACE_1 + px(1.0))
            .rounded(Theme::RADIUS_MD)
            .border_1()
            .border_color(theme.border_subtle)
            .bg(theme.hover)
            .cursor_pointer()
            .occlude()
            .hover(move |style| {
                style
                    .bg(hover_bg.alpha(hover_bg.a * 1.8))
                    .border_color(hover_border)
            })
            .child(
                Icon::new(IconName::Search)
                    .size(IconSize::Xs)
                    .color(theme.text_subtle),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(Theme::TEXT_SM)
                    .text_color(theme.text_subtle)
                    .child("Search or run a command"),
            )
            .child(keycap("⌘K", theme));
        if let Some(handler) = self.on_palette.clone() {
            pill = pill.on_click(move |_, window, cx| handler(window, cx));
        }
        pill
    }

    fn render_shapes(&self, current: Shape, theme: &Theme) -> gpui::Div {
        let segments = SHAPES.iter().enumerate().map(|(i, &shape)| {
            let selected = shape == current;
            let selector = format!("title-shape-{i}");
            let (hover, text) = (theme.hover, theme.text);
            let mut segment = div()
                .id(("title-shape", i))
                .debug_selector(move || selector)
                .flex()
                .items_center()
                .h_full()
                .px(Theme::SPACE_2)
                .rounded(Theme::RADIUS_SM)
                .cursor_pointer()
                .text_size(Theme::TEXT_SM)
                .when(selected, |segment| {
                    segment
                        .bg(theme.raised)
                        .border_1()
                        .border_color(theme.border)
                        .shadow_sm()
                        .text_color(theme.text)
                        .font_weight(FontWeight::SEMIBOLD)
                })
                .when(!selected, |segment| {
                    segment
                        .text_color(theme.text_muted)
                        .hover(move |style| style.bg(hover).text_color(text))
                })
                .child(shape_label(shape));
            if let Some(handler) = self.on_shape.clone() {
                segment = segment.on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    handler(shape, window, cx);
                });
            }
            segment
        });
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .h(CONTROL_HEIGHT)
            .p(px(2.0))
            .rounded(Theme::RADIUS_MD)
            .border_1()
            .border_color(theme.border_subtle)
            .bg(theme.hover)
            .children(segments)
    }

    /// The Panes | Activity switch: the selected side is drawn raised; Activity opens beside the panes.
    fn render_mode(&self, flow_open: bool, theme: &Theme) -> gpui::Div {
        let sides = [
            ("title-mode-panes", IconName::SplitH, "Panes", !flow_open),
            (
                "title-mode-flow",
                IconName::Orchestrator,
                "Activity",
                flow_open,
            ),
        ];
        let segments = sides.into_iter().map(|(selector, icon, label, selected)| {
            let (hover, text) = (theme.hover, theme.text);
            let mut segment = div()
                .id(selector)
                .debug_selector(move || selector.to_owned())
                .flex()
                .items_center()
                .gap(Theme::SPACE_1)
                .h_full()
                .px(Theme::SPACE_2)
                .rounded(Theme::RADIUS_SM)
                .text_size(Theme::TEXT_SM)
                .child(Icon::new(icon).size(IconSize::Xs).color(if selected {
                    theme.accent
                } else {
                    theme.text_muted
                }))
                .child(label)
                .tooltip(|_, cx| Tooltip::view("Show or hide Activity ⌘G", cx));
            segment = if selected {
                segment
                    .bg(theme.raised)
                    .border_1()
                    .border_color(theme.border)
                    .shadow_sm()
                    .text_color(theme.text)
                    .font_weight(FontWeight::SEMIBOLD)
            } else {
                let segment = segment
                    .cursor_pointer()
                    .text_color(theme.text_muted)
                    .hover(move |style| style.bg(hover).text_color(text));
                match self.on_toggle_flow.clone() {
                    Some(handler) => segment.on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        handler(window, cx);
                    }),
                    None => segment,
                }
            };
            segment
        });
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.0))
            .h(CONTROL_HEIGHT)
            .p(px(2.0))
            .rounded(Theme::RADIUS_MD)
            .border_1()
            .border_color(theme.border_subtle)
            .bg(theme.hover)
            .children(segments)
    }

    fn render_discard(&self, theme: &Theme) -> gpui::Stateful<gpui::Div> {
        let danger = theme.danger;
        let mut button = div()
            .id("title-discard")
            .debug_selector(|| "title-discard".to_owned())
            .group("title-discard")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(24.0))
            .rounded(Theme::RADIUS_SM)
            .cursor_pointer()
            .hover(move |style| style.bg(danger.alpha(0.14)))
            .tooltip(|_, cx| Tooltip::view("Discard space…", cx))
            .child(
                svg()
                    .path(IconName::Trash.path())
                    .size(IconSize::Sm.pixels())
                    .flex_none()
                    .text_color(theme.text_muted)
                    .group_hover("title-discard", move |style| style.text_color(danger)),
            );
        if let Some(handler) = self.on_discard.clone() {
            button = button.on_click(move |_, window, cx| {
                cx.stop_propagation();
                handler(window, cx);
            });
        }
        button
    }

    fn render_space_chips(info: &SpaceHeaderInfo, theme: &Theme) -> Vec<gpui::AnyElement> {
        let mut chips = Vec::new();
        if !info.branch.is_empty() {
            chips.push(
                div()
                    .flex()
                    .flex_shrink_1()
                    .min_w_0()
                    .max_w(px(220.0))
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .h(px(20.0))
                    .px(Theme::SPACE_1 + px(2.0))
                    .rounded(Theme::RADIUS_SM)
                    .border_1()
                    .border_color(theme.border_subtle)
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_muted)
                    .child(
                        Icon::new(IconName::Git)
                            .size(IconSize::Xs)
                            .color(theme.text_subtle),
                    )
                    .child(div().min_w_0().truncate().child(info.branch.clone()))
                    .into_any_element(),
            );
        }
        if !info.env.is_empty() {
            let chip = if info.production {
                Badge::new(info.env.clone())
                    .tone(Tone::Danger)
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .h(px(20.0))
                    .px(Theme::SPACE_1 + px(2.0))
                    .rounded(Theme::RADIUS_SM)
                    .border_1()
                    .border_color(theme.border_subtle)
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_muted)
                    .child(
                        Icon::new(IconName::Layers)
                            .size(IconSize::Xs)
                            .color(theme.text_subtle),
                    )
                    .child(info.env.clone())
                    .into_any_element()
            };
            chips.push(chip);
        }
        chips
    }

    fn render_actions(&self, info: &SpaceHeaderInfo, theme: &Theme) -> gpui::Div {
        let mut ship = Button::new("title-ship", "Ship")
            .icon(IconName::Ship)
            .variant(ButtonVariant::Primary)
            .tooltip("Ship space")
            .selector("title-ship");
        if let Some(handler) = self.on_ship.clone() {
            ship = ship.on_click(move |_, window, cx| handler(window, cx));
        }
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_1)
            .child(control(self.render_mode(info.flow_open, theme)).mr(Theme::SPACE_2))
            .child(control(self.render_shapes(info.shape, theme)).mr(Theme::SPACE_2))
            .child(control(tool_button(
                "title-add-agent",
                IconName::Plus,
                "Agent",
                "Add an agent",
                self.on_add_agent.clone(),
            )))
            .child(control(tool_button(
                "title-add-repo",
                IconName::Folder,
                "Repo",
                "Add a repo",
                self.on_add_repo.clone(),
            )))
            .child(
                div()
                    .flex_none()
                    .w(px(1.0))
                    .h(px(16.0))
                    .mx(Theme::SPACE_1)
                    .bg(theme.border),
            )
            .child(control(self.render_discard(theme)))
            .child(control(ship))
    }
}

impl RenderOnce for TitleBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let chips = self
            .space
            .as_ref()
            .map(|info| Self::render_space_chips(info, &theme))
            .unwrap_or_default();
        let actions = self
            .space
            .as_ref()
            .map(|info| self.render_actions(info, &theme));

        let main = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .h_full()
            .pl(Theme::SPACE_3)
            .pr(Theme::SPACE_2)
            .gap(Theme::SPACE_2)
            .when(!self.sidebar_collapsed, |main| {
                main.border_b_1().border_color(theme.border_subtle)
            })
            .child(
                div()
                    .flex_shrink_1()
                    .min_w(px(40.0))
                    .truncate()
                    .text_size(Theme::TEXT_MD)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(self.title.clone()),
            )
            .children(chips)
            // Flexible room either side of the search pill keeps it near the centre; it is not a control, so the
            // drag area behind the row receives presses on it.
            .child(div().flex_1().min_w(Theme::SPACE_2))
            .child(self.render_search(&theme))
            .child(div().flex_1().min_w(Theme::SPACE_2))
            .children(actions);

        div()
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .w_full()
            .h(Theme::TITLE_BAR_HEIGHT)
            .when(self.sidebar_collapsed, |row| {
                row.border_b_1().border_color(theme.border_subtle)
            })
            .child(window_drag_area("title-drag").absolute().inset_0())
            .child(self.render_sidebar_segment(&theme))
            .child(main)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_bar_is_home_with_an_expanded_sidebar() {
        let bar = TitleBar::new("Home");
        assert!(bar.space.is_none());
        assert_eq!(bar.sidebar_width, ROSTER_WIDTH);
        assert!(!bar.sidebar_collapsed);
        let info = SpaceHeaderInfo {
            shape: Shape::Debug,
            flow_open: true,
            production: false,
            branch: "space/fix".into(),
            env: "fix".into(),
        };
        let bar = bar.sidebar(px(52.0), true).space(info.clone());
        assert_eq!(bar.space, Some(info));
        assert!(bar.sidebar_collapsed);
    }
}
