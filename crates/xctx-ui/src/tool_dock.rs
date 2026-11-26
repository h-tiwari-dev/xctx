//! The tool dock: a slim column of icon buttons on the right edge of a space, one per tool the space can use
//! (Terminal, Logs, Browser, Files, the API client, Database, Jira, Figma, and the user's and repos' own), in
//! groups. A click opens a closed tool's pane or closes an open one; dragging a tool onto a pane opens it there; a
//! tool that takes a setting (a command, a database, a URL) can be opened with another value from a small popover;
//! tool folders whose manifest could not be read show as a warning item at the bottom.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AnyElement, AnyView, App, Context, FontWeight, MouseButton, Render, SharedString, Window,
    deferred, div, prelude::*, px,
};

use crate::components::{Button, ButtonSize, ButtonVariant};
use crate::drag::{DragPreview, DraggedTool};
use crate::icon::{Icon, IconName, IconSize};
use crate::theme::{Theme, Tone};

/// The dock's width.
pub const TOOL_DOCK_WIDTH: gpui::Pixels = px(40.0);

/// The most problems the problems popover lists; the rest are counted in a note.
pub const TOOL_PROBLEM_LIMIT: usize = 50;

/// The most items the dock draws; the rest are left out.
pub const TOOL_ITEM_LIMIT: usize = 64;

/// What the hover label of a tool waiting for approval says under its name.
pub const TOOL_LOCKED_HINT: &str = "Needs your approval: a click asks for it in the Inbox.";

/// The group a tool is drawn in, in the order the groups are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ToolGroup {
    /// Panes that show the space's work: a terminal, logs, a browser, files.
    Panes,
    /// Tools to poke the space's app: the API client, a database.
    Tools,
    /// Outside services, such as Jira or Figma.
    Connectors,
    /// Anything else.
    Other,
}

impl ToolGroup {
    /// Every group, in the order drawn.
    pub const ALL: [Self; 4] = [Self::Panes, Self::Tools, Self::Connectors, Self::Other];
}

/// The one setting a tool can be opened with, such as a terminal's command or the browser's page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolSetting {
    /// What the popover's input is for ("Command", "Page").
    pub label: SharedString,
    /// The text the host puts in its input when the popover opens. Already masked by the caller.
    pub default: SharedString,
    /// A line under the input saying what a blank or another value does. Drawn as given.
    pub hint: Option<SharedString>,
}

/// One tool in the dock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockItem {
    /// Its manifest name, unique in the dock; passed back by the handlers and used in selectors.
    pub name: SharedString,
    /// The name shown on hover, such as "API client".
    pub title: SharedString,
    /// What its pane shows (the browser's page, the terminal's command), shown on hover under its name. Drawn as
    /// given: already masked by the caller.
    pub detail: Option<SharedString>,
    /// Its icon.
    pub icon: IconName,
    /// The group it is drawn in.
    pub group: ToolGroup,
    /// It has a pane in the space: an accent dot and an active background, and a click closes it.
    pub open: bool,
    /// It is a repo tool not trusted yet: a lock, and a click asks for approval (the host does what opening it
    /// does).
    pub locked: bool,
    /// The setting it can be opened with: a right-click or the hover chevron opens the popover for it.
    pub setting: Option<ToolSetting>,
    /// It has no usable default for its setting (Logs with no dev server): a click on it while closed opens the
    /// popover instead of the tool.
    pub needs_input: bool,
}

/// A tool folder whose manifest could not be used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolProblem {
    /// The manifest's path. Drawn as given: already masked by the caller.
    pub path: SharedString,
    /// What is wrong with it. Drawn as given: already masked by the caller.
    pub message: SharedString,
}

/// What the tool dock shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ToolDockView {
    /// The tools, in the order drawn within each group.
    pub items: Vec<DockItem>,
    /// The manifests that could not be used: a warning item with their count when any.
    pub problems: Vec<ToolProblem>,
    /// The tool whose setting popover is open, if any.
    pub popover: Option<SharedString>,
    /// The problems popover is open.
    pub problems_open: bool,
    /// Something else has the window (a dialog): nothing in the dock reacts.
    pub busy: bool,
}

type NameHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;
type PopoverHandler = Rc<dyn Fn(Option<&SharedString>, &mut Window, &mut App)>;
type ToggleHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;
type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The tool dock, drawn from a [`ToolDockView`].
///
/// A click on a closed tool calls [`ToolDock::on_open`] (or, when it [`DockItem::needs_input`], opens its popover);
/// a click on an open tool calls [`ToolDock::on_close`]. A press on the tool whose popover is open only closes the
/// popover. Every tool but a [`DockItem::locked`] one can also be dragged, as a [`DraggedTool`], onto a pane of the
/// [`crate::SplitView`] (see [`crate::SplitView::on_drop_tool`]); a drag (a press that moves) is not a click. A
/// right-click or the chevron shown on hover, on a tool with a setting, calls [`ToolDock::on_popover`] with its
/// name; the host then sets [`ToolDockView::popover`], puts [`ToolSetting::default`] in its input and passes that
/// input to [`ToolDock::setting_input`]. The popover's Open calls [`ToolDock::on_submit`] (Enter and Escape in the
/// input are the host's to handle), and a click outside it calls [`ToolDock::on_popover`] with `None`. The problems
/// item toggles its list with [`ToolDock::on_problems`]. Nothing reacts while [`ToolDockView::busy`].
///
/// The dock paints no background: it sits on the window's chrome, behind a thin line on its left.
///
/// Selectors for tests: `tool-dock`, `tool-<name>` (an item's button), `tool-open-<name>` (its open dot),
/// `tool-lock-<name>`, `tool-more-<name>` (the hover chevron), `tool-popover-<name>`, `tool-setting-<name>` (the
/// popover's input), `tool-submit-<name>` (its Open button), `tool-divider-<i>`, `tool-problems` (the warning item),
/// `tool-problems-list`, `tool-problem-<i>` and `tool-problems-more`.
#[derive(IntoElement)]
pub struct ToolDock {
    view: ToolDockView,
    setting_input: Option<AnyElement>,
    on_open: Option<NameHandler>,
    on_close: Option<NameHandler>,
    on_popover: Option<PopoverHandler>,
    on_submit: Option<NameHandler>,
    on_problems: Option<ToggleHandler>,
}

impl ToolDock {
    /// A dock for `view` with no handlers.
    pub fn new(view: ToolDockView) -> Self {
        Self {
            view,
            setting_input: None,
            on_open: None,
            on_close: None,
            on_popover: None,
            on_submit: None,
            on_problems: None,
        }
    }

    /// Draw `input` (the host's text input holding the setting) in the open popover.
    pub fn setting_input(mut self, input: impl IntoElement) -> Self {
        self.setting_input = Some(input.into_any_element());
        self
    }

    /// Call `handler` with the tool's name when a closed tool is clicked.
    pub fn on_open(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_open = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the tool's name when an open tool is clicked.
    pub fn on_close(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with a tool's name to open its setting popover, or with `None` to close the open one.
    pub fn on_popover(
        mut self,
        handler: impl Fn(Option<&SharedString>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_popover = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the tool's name when the open popover's Open is clicked.
    pub fn on_submit(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_submit = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with whether the problems list should show, when the problems item is clicked (or the list
    /// is clicked away).
    pub fn on_problems(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_problems = Some(Rc::new(handler));
        self
    }
}

/// The hover label of a dock item.
struct DockTooltip {
    title: SharedString,
    detail: Option<SharedString>,
}

impl Render for DockTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        div()
            .flex()
            .flex_col()
            .max_w(px(320.0))
            .px(Theme::SPACE_2)
            .py(Theme::SPACE_1)
            .rounded(Theme::RADIUS_SM)
            .border_1()
            .border_color(theme.border)
            .bg(theme.overlay)
            .shadow_md()
            .child(
                div()
                    .text_size(Theme::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(self.title.clone()),
            )
            .children(self.detail.clone().map(|detail| {
                div()
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_muted)
                    .child(detail)
            }))
    }
}

/// A popover drawn to the left of the dock, over the panes, closed by a click outside it.
fn popover(theme: &Theme, on_away: Option<Handler>) -> gpui::Div {
    let mut panel = div()
        .occlude()
        .absolute()
        .right(TOOL_DOCK_WIDTH)
        .w(px(320.0))
        .flex()
        .flex_col()
        .gap(Theme::SPACE_2)
        .p(Theme::SPACE_3)
        .rounded(Theme::RADIUS_LG)
        .border_1()
        .border_color(theme.border)
        .bg(theme.overlay)
        .shadow_lg();
    if let Some(handler) = on_away {
        panel = panel.on_mouse_down_out(move |_, window, cx| handler(window, cx));
    }
    panel
}

impl ToolDock {
    fn render_item(
        &self,
        item: &DockItem,
        setting_input: &mut Option<AnyElement>,
        theme: &Theme,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let busy = self.view.busy;
        let name = item.name.clone();
        let group = SharedString::from(format!("tool-item-{name}"));
        let selector = format!("tool-{name}");
        let mut button = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector)
            .flex()
            .items_center()
            .justify_center()
            .size(px(32.0))
            .rounded(Theme::RADIUS_MD)
            .when(item.open, |b| b.bg(theme.selection))
            .child(Icon::new(item.icon).size(IconSize::Md).color(if item.open {
                theme.text
            } else {
                theme.text_muted
            }));
        let detail = if item.locked {
            Some(SharedString::from(TOOL_LOCKED_HINT))
        } else {
            item.detail.clone()
        };
        let (title, tip_detail) = (item.title.clone(), detail);
        button = button.tooltip(move |_, cx| -> AnyView {
            let (title, detail) = (title.clone(), tip_detail.clone());
            cx.new(|_| DockTooltip { title, detail }).into()
        });
        if busy {
            button = button.opacity(0.45);
        } else {
            let hover = theme.hover;
            // An open tool keeps its active look under the pointer.
            button = button
                .cursor_pointer()
                .when(!item.open, |b| b.hover(move |s| s.bg(hover)));
            let click: Option<Handler> = if item.open {
                self.on_close.clone().map(|handler| {
                    let name = name.clone();
                    Rc::new(move |window: &mut Window, cx: &mut App| handler(&name, window, cx))
                        as Handler
                })
            } else if item.needs_input && item.setting.is_some() {
                self.on_popover.clone().map(|handler| {
                    let name = name.clone();
                    Rc::new(move |window: &mut Window, cx: &mut App| {
                        handler(Some(&name), window, cx)
                    }) as Handler
                })
            } else {
                self.on_open.clone().map(|handler| {
                    let name = name.clone();
                    Rc::new(move |window: &mut Window, cx: &mut App| handler(&name, window, cx))
                        as Handler
                })
            };
            // The tool acts on the click, so that a press that turns into a drag does nothing. A press on the tool
            // whose popover is open only closes the popover: its click-away runs on that press and the dock is
            // redrawn without it before the release, so whether the press counts is decided when it happens.
            let popover_here = self.view.popover.as_ref() == Some(&item.name);
            if let Some(click) = click {
                let armed = window.use_keyed_state(
                    SharedString::from(format!("tool-armed-{name}")),
                    cx,
                    |_, _| Cell::new(false),
                );
                let pressed = armed.clone();
                button = button
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        pressed.read(cx).set(!popover_here);
                    })
                    .on_click(move |_, window, cx| {
                        if armed.read(cx).replace(false) {
                            click(window, cx);
                        }
                    });
            }
            if !item.locked {
                let dragged = DraggedTool {
                    name: item.name.clone(),
                    title: item.title.clone(),
                    icon: item.icon,
                };
                button = button.on_drag(dragged, |tool, _, _, cx| {
                    cx.new(|_| DragPreview::new(tool.icon, tool.title.clone()))
                });
            }
            if item.setting.is_some()
                && let Some(handler) = self.on_popover.clone()
            {
                let name = name.clone();
                button = button.on_mouse_down(MouseButton::Right, move |_, window, cx| {
                    cx.stop_propagation();
                    handler(Some(&name), window, cx);
                });
            }
        }
        let dot = item.open.then(|| {
            let name = name.clone();
            div()
                .debug_selector(move || format!("tool-open-{name}"))
                .absolute()
                .top(px(2.0))
                .right(px(2.0))
                .size(px(6.0))
                .rounded(Theme::RADIUS_FULL)
                .bg(theme.accent)
        });
        let lock = item.locked.then(|| {
            let name = name.clone();
            div()
                .debug_selector(move || format!("tool-lock-{name}"))
                .absolute()
                .bottom(px(1.0))
                .right(px(1.0))
                .p(px(1.0))
                .rounded(Theme::RADIUS_SM)
                .bg(theme.raised)
                .child(
                    Icon::new(IconName::Lock)
                        .size_px(px(9.0))
                        .tone(Tone::Warning),
                )
        });
        let more = item.setting.as_ref().filter(|_| !busy).map(|_| {
            let more_selector = format!("tool-more-{name}");
            let mut chevron = div()
                .id(SharedString::from(more_selector.clone()))
                .debug_selector(move || more_selector)
                // Over the item's button: a press here is the chevron's, not the tool's.
                .occlude()
                .absolute()
                .bottom(px(0.0))
                .left(px(0.0))
                .size(px(12.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(Theme::RADIUS_SM)
                .bg(theme.raised)
                .cursor_pointer()
                .opacity(0.0)
                .group_hover(group.clone(), |s| s.opacity(1.0))
                .child(Icon::new(IconName::ChevronDown).size_px(px(9.0)));
            if let Some(handler) = self.on_popover.clone() {
                let name = name.clone();
                chevron = chevron.on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    handler(Some(&name), window, cx);
                });
            }
            chevron
        });
        let popover = item
            .setting
            .as_ref()
            .filter(|_| self.view.popover.as_ref() == Some(&item.name))
            .map(|setting| {
                let away = self.on_popover.clone().map(|handler| {
                    Rc::new(move |window: &mut Window, cx: &mut App| handler(None, window, cx))
                        as Handler
                });
                let mut open =
                    Button::new(SharedString::from(format!("tool-submit-{name}")), "Open")
                        .variant(ButtonVariant::Primary)
                        .size(ButtonSize::Compact)
                        .disabled(busy)
                        .selector(format!("tool-submit-{name}"));
                if let Some(handler) = self.on_submit.clone().filter(|_| !busy) {
                    let name = name.clone();
                    open = open.on_click(move |_, window, cx| handler(&name, window, cx));
                }
                let (popover_selector, input_selector) = (
                    format!("tool-popover-{name}"),
                    format!("tool-setting-{name}"),
                );
                deferred(
                    popover(theme, away)
                        .debug_selector(move || popover_selector)
                        .top(px(0.0))
                        .child(
                            div()
                                .text_size(Theme::TEXT_MD)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme.text)
                                .child(format!("Open {}", item.title)),
                        )
                        .child(
                            div()
                                .text_size(Theme::TEXT_XS)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text_muted)
                                .child(setting.label.clone()),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(Theme::SPACE_2)
                                .child(
                                    div()
                                        .debug_selector(move || input_selector)
                                        .flex_1()
                                        .min_w_0()
                                        .children(setting_input.take()),
                                )
                                .child(open),
                        )
                        .children(setting.hint.clone().map(|hint| {
                            div()
                                .text_size(Theme::TEXT_XS)
                                .text_color(theme.text_subtle)
                                .child(hint)
                        })),
                )
                .with_priority(1)
            });
        div()
            .group(group)
            .relative()
            .flex_none()
            .size(px(32.0))
            .child(button)
            .children(dot)
            .children(lock)
            .children(more)
            .children(popover)
            .into_any_element()
    }

    fn render_problems(&self, theme: &Theme) -> Option<AnyElement> {
        let problems = &self.view.problems;
        if problems.is_empty() {
            return None;
        }
        let busy = self.view.busy;
        let open = self.view.problems_open && !busy;
        let count = problems.len();
        let mut button = div()
            .id("tool-problems")
            .debug_selector(|| "tool-problems".to_owned())
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .w(px(32.0))
            .py(Theme::SPACE_1)
            .rounded(Theme::RADIUS_MD)
            .when(open, |b| b.bg(theme.selection))
            .child(
                Icon::new(IconName::Warning)
                    .size(IconSize::Sm)
                    .tone(Tone::Warning),
            )
            .child(
                div()
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.warning)
                    .child(count.to_string()),
            );
        let tip = SharedString::from(if count == 1 {
            "1 tool could not be loaded".to_owned()
        } else {
            format!("{count} tools could not be loaded")
        });
        button = button.tooltip(move |_, cx| -> AnyView {
            let title = tip.clone();
            cx.new(|_| DockTooltip {
                title,
                detail: None,
            })
            .into()
        });
        if busy {
            button = button.opacity(0.45);
        } else if let Some(handler) = self.on_problems.clone() {
            let hover = theme.hover;
            // On mouse down, in the same event as the open list's click-away (which runs first and closes it): a
            // press on the item while the list is open only closes it.
            button = button
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    if !open {
                        handler(true, window, cx);
                    }
                });
        }
        let list = open.then(|| {
            let away = self.on_problems.clone().map(|handler| {
                Rc::new(move |window: &mut Window, cx: &mut App| handler(false, window, cx))
                    as Handler
            });
            let rows = problems
                .iter()
                .take(TOOL_PROBLEM_LIMIT)
                .enumerate()
                .map(|(i, problem)| {
                    div()
                        .debug_selector(move || format!("tool-problem-{i}"))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .font_family("Menlo")
                                .text_size(Theme::TEXT_XS)
                                .text_color(theme.text_muted)
                                .child(problem.path.clone()),
                        )
                        .child(
                            div()
                                .text_size(Theme::TEXT_SM)
                                .text_color(theme.danger)
                                .child(problem.message.clone()),
                        )
                });
            let hidden = count.saturating_sub(TOOL_PROBLEM_LIMIT);
            deferred(
                popover(theme, away)
                    .id("tool-problems-list")
                    .debug_selector(|| "tool-problems-list".to_owned())
                    .bottom(px(0.0))
                    .max_h(px(420.0))
                    .overflow_y_scroll()
                    .child(
                        div()
                            .text_size(Theme::TEXT_MD)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("Tools that could not be loaded"),
                    )
                    .children(rows)
                    .children((hidden > 0).then(|| {
                        div()
                            .debug_selector(|| "tool-problems-more".to_owned())
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_subtle)
                            .child(format!("… {hidden} more"))
                    })),
            )
            .with_priority(1)
        });
        Some(
            div()
                .relative()
                .flex_none()
                .child(button)
                .children(list)
                .into_any_element(),
        )
    }
}

impl RenderOnce for ToolDock {
    fn render(mut self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let mut setting_input = self.setting_input.take();
        let items: Vec<&DockItem> = self.view.items.iter().take(TOOL_ITEM_LIMIT).collect();
        let mut column = div().flex().flex_col().items_center().gap(Theme::SPACE_1);
        // A divider goes between drawn groups, not above the first.
        let mut drawn = 0;
        for group in ToolGroup::ALL {
            let members: Vec<AnyElement> = items
                .iter()
                .filter(|item| item.group == group)
                .map(|item| self.render_item(item, &mut setting_input, &theme, window, cx))
                .collect();
            if members.is_empty() {
                continue;
            }
            if drawn > 0 {
                let i = drawn - 1;
                column = column.child(
                    div()
                        .debug_selector(move || format!("tool-divider-{i}"))
                        .w(px(20.0))
                        .h(px(1.0))
                        .my(Theme::SPACE_1)
                        .bg(theme.border_subtle),
                );
            }
            drawn += 1;
            column = column.children(members);
        }
        let problems = self.render_problems(&theme);
        div()
            .id("tool-dock")
            .debug_selector(|| "tool-dock".to_owned())
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .justify_between()
            .w(TOOL_DOCK_WIDTH)
            .h_full()
            .py(Theme::SPACE_2)
            .border_l_1()
            .border_color(theme.border_subtle)
            .child(column)
            .children(problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_are_drawn_panes_first() {
        let mut groups = ToolGroup::ALL.to_vec();
        groups.sort();
        assert_eq!(groups, ToolGroup::ALL);
    }
}
