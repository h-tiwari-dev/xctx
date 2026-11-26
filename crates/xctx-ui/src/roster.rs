//! The Roster: the left sidebar listing Home, the Inbox and every space with its agents, grouped into sections
//! (Needs you, Running, Idle). Collapsed, it is a rail of icon tiles. It replaces the tab bar.
//!
//! The Roster keeps no state: the app builds the sections (in the order to show them) and receives clicks through
//! callbacks. Test selectors: `roster-home`, `roster-inbox`, `roster-new-space`, `roster-space-<space id>`,
//! `roster-agent-<space id>-<agent id>`, `roster-more-<space id>`, and on the rail `roster-rail-home`,
//! `roster-rail-inbox`, `roster-rail-new-space`, `roster-rail-<space id>`.

use std::rc::Rc;

use gpui::{
    AnyElement, App, ElementId, FontWeight, Hsla, Pixels, SharedString, Window, div, prelude::*, px,
};

use crate::components::Tooltip;
use crate::icon::{Icon, IconName, IconSize};
use crate::spinner::Spinner;
use crate::status_bar::keycap;
use crate::theme::{Theme, Tone};

/// Width of the expanded Roster.
pub const ROSTER_WIDTH: Pixels = px(240.0);

/// Width of the collapsed Roster rail.
pub const ROSTER_RAIL_WIDTH: Pixels = px(52.0);

/// The most agents listed under a space; the rest are counted in a `+N more` row.
pub const ROSTER_AGENT_LIMIT: usize = 6;

/// Height of a Home, Inbox or space row.
const ROW_HEIGHT: Pixels = px(30.0);
/// Height of an agent row under its space.
const AGENT_ROW_HEIGHT: Pixels = px(24.0);
/// Side of a tile on the collapsed rail.
const TILE_SIZE: Pixels = px(36.0);
/// How far agent rows are indented, so their dot lines up under the space's title.
const AGENT_INDENT: Pixels = px(30.0);

/// The state of a space or agent row, worst first: what its icon and colour show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowState {
    /// Waiting on the user: a pending gate or a paused loop.
    NeedsYou,
    /// A blocked loop or a dead pane.
    Failed,
    /// An agent is working.
    Running,
    /// Paused by the user.
    Paused,
    /// Nothing is happening.
    Idle,
}

impl RowState {
    /// The icon drawn for this state. A running row draws a [`Spinner`] instead where it can.
    pub fn icon(self) -> IconName {
        match self {
            Self::NeedsYou => IconName::Warning,
            Self::Failed => IconName::Error,
            Self::Running => IconName::Running,
            Self::Paused => IconName::Paused,
            Self::Idle => IconName::Idle,
        }
    }

    /// The colour of this state's icon: needs-you `warning`, failed `danger`, running `success`, paused
    /// `text_muted`, idle `text_subtle`.
    pub fn color(self, theme: &Theme) -> Hsla {
        match self {
            Self::NeedsYou => theme.warning,
            Self::Failed => theme.danger,
            Self::Running => theme.success,
            Self::Paused => theme.text_muted,
            Self::Idle => theme.text_subtle,
        }
    }
}

/// One agent listed under its space.
#[derive(Debug, Clone, PartialEq)]
pub struct RosterAgentItem {
    /// The agent's id, handed back to [`Roster::on_select_agent`].
    pub id: SharedString,
    /// Its name, such as `writer · api`.
    pub label: SharedString,
    /// Its identity colour (`accent` for the orchestrator, else [`Theme::agent_color`]).
    pub color: Hsla,
    /// What it is doing.
    pub state: RowState,
    /// One line of current activity; truncated to fit, may be empty.
    pub activity: SharedString,
}

/// One space row.
#[derive(Debug, Clone, PartialEq)]
pub struct RosterSpaceItem {
    /// The space's id (its slug), handed back to [`Roster::on_select_space`].
    pub id: SharedString,
    /// The row's text, usually the task.
    pub title: SharedString,
    /// A short description shown in the row's tooltip, such as `2 repos · :4012`; may be empty.
    pub subtitle: SharedString,
    /// The space's rolled-up state.
    pub state: RowState,
    /// Time since its last activity, such as `3m`; empty to hide.
    pub age: SharedString,
    /// Whether this is the space being shown.
    pub active: bool,
    /// The `⌘N` chord that shows it (1 to 9), shown on hover and while active.
    pub shortcut: Option<usize>,
    /// Whether it runs with a `production` env; marked with a danger badge.
    pub production: bool,
    /// Pending Inbox items of this space; shown as a warning count when not zero.
    pub pending: usize,
    /// Its agents, in graph order; at most [`ROSTER_AGENT_LIMIT`] are listed.
    pub agents: Vec<RosterAgentItem>,
}

/// A titled group of spaces, such as "Needs you". A section with no spaces is not drawn.
#[derive(Debug, Clone, PartialEq)]
pub struct RosterSection {
    /// The heading, drawn as given.
    pub label: SharedString,
    /// Its spaces, in the order drawn.
    pub spaces: Vec<RosterSpaceItem>,
}

/// How many of `count` agents are left out of the list under a space, counted in its `+N more` row.
pub fn hidden_agent_count(count: usize) -> usize {
    count.saturating_sub(ROSTER_AGENT_LIMIT)
}

/// The key chord shown for the `n`-th space shortcut (`⌘3`), or `None` outside 1 to 9.
pub fn shortcut_label(n: usize) -> Option<SharedString> {
    (1..=9).contains(&n).then(|| format!("⌘{n}").into())
}

/// The letter a rail tile shows for a space titled `title`: its first alphanumeric character, upper-cased, or `?`.
pub fn rail_initial(title: &str) -> SharedString {
    title.chars().find(|c| c.is_alphanumeric()).map_or_else(
        || "?".into(),
        |c| c.to_uppercase().collect::<String>().into(),
    )
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type SpaceHandler = Rc<dyn Fn(&SharedString, &mut Window, &mut App)>;
type AgentHandler = Rc<dyn Fn(&SharedString, &SharedString, &mut Window, &mut App)>;

/// The left sidebar. Its height is whatever the parent gives it (it fills it); its width is [`ROSTER_WIDTH`], or
/// [`ROSTER_RAIL_WIDTH`] when collapsed. It has no background of its own, so it shows the window chrome behind it,
/// and a `border_subtle` line on its right edge. The list scrolls when it does not fit.
#[derive(IntoElement)]
pub struct Roster {
    sections: Vec<RosterSection>,
    collapsed: bool,
    home_active: bool,
    inbox_count: usize,
    on_home: Option<Handler>,
    on_select_space: Option<SpaceHandler>,
    on_select_agent: Option<AgentHandler>,
    on_inbox: Option<Handler>,
    on_new_space: Option<Handler>,
}

impl Roster {
    /// An expanded Roster listing `sections` in order, with Home not selected and an empty Inbox.
    pub fn new(sections: Vec<RosterSection>) -> Self {
        Self {
            sections,
            collapsed: false,
            home_active: false,
            inbox_count: 0,
            on_home: None,
            on_select_space: None,
            on_select_agent: None,
            on_inbox: None,
            on_new_space: None,
        }
    }

    /// Draw the narrow rail of icon tiles instead of the full list.
    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }

    /// Select the Home row, while Home is shown.
    pub fn home_active(mut self, active: bool) -> Self {
        self.home_active = active;
        self
    }

    /// The number of pending Inbox items, shown as a warning count on the Inbox row when not zero.
    pub fn inbox_count(mut self, count: usize) -> Self {
        self.inbox_count = count;
        self
    }

    /// Call `handler` when Home is clicked.
    pub fn on_home(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_home = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with a space's id when its row (or its `+N more` row) is clicked.
    pub fn on_select_space(
        mut self,
        handler: impl Fn(&SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select_space = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the space's id and the agent's id when an agent row is clicked.
    pub fn on_select_agent(
        mut self,
        handler: impl Fn(&SharedString, &SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select_agent = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when the Inbox row is clicked.
    pub fn on_inbox(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_inbox = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when "New space" is clicked.
    pub fn on_new_space(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_new_space = Some(Rc::new(handler));
        self
    }
}

/// The element id and test selector name `name`.
fn named(name: String) -> (ElementId, String) {
    (ElementId::Name(name.clone().into()), name)
}

/// The state icon of a row: a spinner while running, else the state's icon in its colour. Spinners are kept per
/// `key` across frames so they keep turning.
fn state_icon(
    state: RowState,
    key: String,
    size: IconSize,
    theme: &Theme,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    if state == RowState::Running {
        window
            .use_keyed_state(ElementId::Name(key.into()), cx, |_, _| {
                Spinner::new().size(size).tone(Tone::Success)
            })
            .into_any_element()
    } else {
        Icon::new(state.icon())
            .size(size)
            .color(state.color(theme))
            .into_any_element()
    }
}

/// A small fully rounded count in the warning tone.
fn count_pill(count: usize, theme: &Theme) -> gpui::Div {
    let colors = theme.tone(Tone::Warning);
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(16.0))
        .min_w(px(16.0))
        .px(px(5.0))
        .rounded(Theme::RADIUS_FULL)
        .bg(colors.bg)
        .border_1()
        .border_color(colors.border)
        .text_color(colors.fg)
        .text_size(px(10.0))
        .font_weight(FontWeight::SEMIBOLD)
        .child(count.to_string())
}

/// A clickable full-width row with the hover wash, selected look and optional accent bar.
fn row_shell(
    (id, selector): (ElementId, String),
    height: Pixels,
    selected: bool,
    theme: &Theme,
) -> gpui::Stateful<gpui::Div> {
    let hover = theme.hover;
    let hover_text = theme.text;
    div()
        .id(id)
        .debug_selector(move || selector)
        .relative()
        .flex()
        .flex_none()
        .items_center()
        .gap(Theme::SPACE_2)
        .w_full()
        .h(height)
        .px(Theme::SPACE_2)
        .rounded(Theme::RADIUS_MD)
        .cursor_pointer()
        .text_color(if selected {
            theme.text
        } else {
            theme.text_muted
        })
        .when(selected, |row| row.bg(theme.selection))
        .when(!selected, |row| {
            row.hover(move |style| style.bg(hover).text_color(hover_text))
        })
}

/// The 2px accent bar at the left of the active row.
fn active_bar(theme: &Theme) -> gpui::Div {
    div()
        .absolute()
        .left_0()
        .top(px(7.0))
        .bottom(px(7.0))
        .w(px(2.0))
        .rounded(Theme::RADIUS_FULL)
        .bg(theme.accent)
}

impl Roster {
    fn nav_row(
        &self,
        name: &str,
        icon: IconName,
        label: &'static str,
        selected: bool,
        handler: Option<Handler>,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        row_shell(named(format!("roster-{name}")), ROW_HEIGHT, selected, theme)
            .text_size(Theme::TEXT_MD)
            .when(selected, |row| {
                row.font_weight(FontWeight::SEMIBOLD)
                    .child(active_bar(theme))
            })
            .child(Icon::new(icon).size(IconSize::Sm).color(if selected {
                theme.accent
            } else {
                theme.text_muted
            }))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .when_some(handler, |row, handler| {
                row.on_click(move |_, window, cx| handler(window, cx))
            })
    }

    fn space_row(
        &self,
        space: &RosterSpaceItem,
        theme: &Theme,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let group: SharedString = format!("roster-space-group-{}", space.id).into();
        let icon = state_icon(
            space.state,
            format!("roster-spinner-{}", space.id),
            IconSize::Sm,
            theme,
            window,
            cx,
        );
        let shortcut = space.shortcut.and_then(shortcut_label).map(|chord| {
            keycap(chord, theme).when(!space.active, |cap| {
                cap.opacity(0.0)
                    .group_hover(group.clone(), |style| style.opacity(1.0))
            })
        });
        let production = space
            .production
            .then(|| crate::components::Badge::new("PROD").tone(Tone::Danger));
        let pending = (space.pending > 0).then(|| count_pill(space.pending, theme));
        let age = (!space.age.is_empty()).then(|| {
            div()
                .flex_none()
                .text_size(Theme::TEXT_XS)
                .text_color(theme.text_subtle)
                .child(space.age.clone())
        });
        let tooltip = (!space.subtitle.is_empty())
            .then(|| SharedString::from(format!("{} · {}", space.title, space.subtitle)));

        let mut row = row_shell(
            named(format!("roster-space-{}", space.id)),
            ROW_HEIGHT,
            space.active,
            theme,
        )
        .group(group)
        .text_size(Theme::TEXT_MD)
        .when(space.active, |row| {
            row.font_weight(FontWeight::SEMIBOLD)
                .child(active_bar(theme))
        })
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(16.0))
                .child(icon),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .child(space.title.clone()),
        )
        .children(production)
        .children(pending)
        .children(age)
        .children(shortcut);
        if let Some(text) = tooltip {
            row = row.tooltip(move |_, cx| Tooltip::view(text.clone(), cx));
        }
        if let Some(handler) = self.on_select_space.clone() {
            let id = space.id.clone();
            row = row.on_click(move |_, window, cx| handler(&id, window, cx));
        }
        row
    }

    fn agent_row(
        &self,
        space: &SharedString,
        agent: &RosterAgentItem,
        theme: &Theme,
        window: &mut Window,
        cx: &mut App,
    ) -> impl IntoElement {
        let icon = state_icon(
            agent.state,
            format!("roster-spinner-{space}-{}", agent.id),
            IconSize::Xs,
            theme,
            window,
            cx,
        );
        let activity = (!agent.activity.is_empty()).then(|| {
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_color(theme.text_subtle)
                .child(agent.activity.clone())
        });
        let mut row = row_shell(
            named(format!("roster-agent-{space}-{}", agent.id)),
            AGENT_ROW_HEIGHT,
            false,
            theme,
        )
        .pl(AGENT_INDENT)
        .gap(Theme::SPACE_1 + px(2.0))
        .text_size(Theme::TEXT_SM)
        .child(
            div()
                .flex_none()
                .size(px(6.0))
                .rounded(Theme::RADIUS_FULL)
                .bg(agent.color),
        )
        .child(
            div()
                .flex_none()
                .max_w(px(96.0))
                .truncate()
                .child(agent.label.clone()),
        )
        .child(icon)
        .children(activity);
        if let Some(handler) = self.on_select_agent.clone() {
            let (space, agent) = (space.clone(), agent.id.clone());
            row = row.on_click(move |_, window, cx| handler(&space, &agent, window, cx));
        }
        row
    }

    fn more_row(&self, space: &SharedString, hidden: usize, theme: &Theme) -> impl IntoElement {
        let mut row = row_shell(
            named(format!("roster-more-{space}")),
            px(20.0),
            false,
            theme,
        )
        .pl(AGENT_INDENT + px(12.0))
        .text_size(Theme::TEXT_XS)
        .text_color(theme.text_subtle)
        .child(format!("+{hidden} more"));
        if let Some(handler) = self.on_select_space.clone() {
            let id = space.clone();
            row = row.on_click(move |_, window, cx| handler(&id, window, cx));
        }
        row
    }

    fn render_expanded(self, theme: &Theme, window: &mut Window, cx: &mut App) -> gpui::Div {
        let mut list = div()
            .id("roster-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(Theme::SPACE_2)
            .pt(Theme::SPACE_2)
            .pb(Theme::SPACE_2)
            .gap(px(1.0))
            .child(self.nav_row(
                "home",
                IconName::Home,
                "Home",
                self.home_active,
                self.on_home.clone(),
                theme,
            ))
            .child(
                self.nav_row(
                    "inbox",
                    IconName::Inbox,
                    "Inbox",
                    false,
                    self.on_inbox.clone(),
                    theme,
                )
                .when(self.inbox_count > 0, |row| {
                    row.child(count_pill(self.inbox_count, theme))
                }),
            );
        for section in self.sections.iter().filter(|s| !s.spaces.is_empty()) {
            list = list.child(
                div()
                    .flex_none()
                    .px(Theme::SPACE_2)
                    .pt(Theme::SPACE_4)
                    .pb(Theme::SPACE_1)
                    .text_size(Theme::TEXT_XS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text_subtle)
                    .child(section.label.clone()),
            );
            for space in &section.spaces {
                list = list.child(self.space_row(space, theme, window, cx));
                for agent in space.agents.iter().take(ROSTER_AGENT_LIMIT) {
                    list = list.child(self.agent_row(&space.id, agent, theme, window, cx));
                }
                let hidden = hidden_agent_count(space.agents.len());
                if hidden > 0 {
                    list = list.child(self.more_row(&space.id, hidden, theme));
                }
            }
        }

        let mut new_space = row_shell(
            named("roster-new-space".to_owned()),
            ROW_HEIGHT,
            false,
            theme,
        )
        .text_size(Theme::TEXT_MD)
        .child(
            Icon::new(IconName::Plus)
                .size(IconSize::Sm)
                .color(theme.text_subtle),
        )
        .child(div().flex_1().child("New space"));
        if let Some(handler) = self.on_new_space.clone() {
            new_space = new_space.on_click(move |_, window, cx| handler(window, cx));
        }

        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(ROSTER_WIDTH)
            .h_full()
            .border_r_1()
            .border_color(theme.border_subtle)
            .child(list)
            .child(
                div()
                    .flex_none()
                    .p(Theme::SPACE_2)
                    .border_t_1()
                    .border_color(theme.border_subtle)
                    .child(new_space),
            )
    }

    fn render_rail(self, theme: &Theme, window: &mut Window, cx: &mut App) -> gpui::Div {
        let tile = |name: String, selected: bool, tooltip: SharedString| {
            let (id, selector) = named(name);
            let hover = theme.hover;
            div()
                .id(id)
                .debug_selector(move || selector)
                .relative()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(TILE_SIZE)
                .rounded(Theme::RADIUS_MD)
                .cursor_pointer()
                .when(selected, |tile| tile.bg(theme.selection))
                .when(!selected, |tile| tile.hover(move |style| style.bg(hover)))
                .tooltip(move |_, cx| Tooltip::view(tooltip.clone(), cx))
        };
        let icon_color = |selected: bool| {
            if selected {
                theme.accent
            } else {
                theme.text_muted
            }
        };

        let mut home = tile(
            "roster-rail-home".into(),
            self.home_active,
            "Home ⌘0".into(),
        )
        .child(
            Icon::new(IconName::Home)
                .size(IconSize::Md)
                .color(icon_color(self.home_active)),
        );
        if let Some(handler) = self.on_home.clone() {
            home = home.on_click(move |_, window, cx| handler(window, cx));
        }
        let mut inbox = tile("roster-rail-inbox".into(), false, "Inbox".into())
            .child(
                Icon::new(IconName::Inbox)
                    .size(IconSize::Md)
                    .color(icon_color(false)),
            )
            .when(self.inbox_count > 0, |tile| {
                tile.child(
                    div()
                        .absolute()
                        .top(px(7.0))
                        .right(px(7.0))
                        .size(px(7.0))
                        .rounded(Theme::RADIUS_FULL)
                        .bg(theme.warning),
                )
            });
        if let Some(handler) = self.on_inbox.clone() {
            inbox = inbox.on_click(move |_, window, cx| handler(window, cx));
        }

        let mut list = div()
            .id("roster-rail-list")
            .flex()
            .flex_col()
            .items_center()
            .flex_1()
            .w_full()
            .min_h_0()
            .overflow_y_scroll()
            .py(Theme::SPACE_2)
            .gap(Theme::SPACE_1)
            .child(home)
            .child(inbox);
        let mut first = true;
        for space in self.sections.iter().flat_map(|section| &section.spaces) {
            if first {
                list = list.child(
                    div()
                        .flex_none()
                        .w(px(20.0))
                        .h(px(1.0))
                        .my(Theme::SPACE_1)
                        .bg(theme.border),
                );
                first = false;
            }
            let icon = state_icon(
                space.state,
                format!("roster-rail-spinner-{}", space.id),
                IconSize::Xs,
                theme,
                window,
                cx,
            );
            let mut tile = tile(
                format!("roster-rail-{}", space.id),
                space.active,
                space.title.clone(),
            )
            .text_size(Theme::TEXT_MD)
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(if space.active {
                theme.text
            } else {
                theme.text_muted
            })
            .when(space.production, |tile| {
                tile.border_1().border_color(theme.danger.alpha(0.55))
            })
            .when(space.active, |tile| {
                tile.child(active_bar(theme).left(px(-8.0)))
            })
            .child(rail_initial(&space.title))
            .child(div().absolute().right(px(3.0)).bottom(px(3.0)).child(icon));
            if let Some(handler) = self.on_select_space.clone() {
                let id = space.id.clone();
                tile = tile.on_click(move |_, window, cx| handler(&id, window, cx));
            }
            list = list.child(tile);
        }

        let mut new_space = tile("roster-rail-new-space".into(), false, "New space".into()).child(
            Icon::new(IconName::Plus)
                .size(IconSize::Md)
                .color(theme.text_subtle),
        );
        if let Some(handler) = self.on_new_space.clone() {
            new_space = new_space.on_click(move |_, window, cx| handler(window, cx));
        }

        div()
            .flex()
            .flex_col()
            .items_center()
            .flex_none()
            .w(ROSTER_RAIL_WIDTH)
            .h_full()
            .border_r_1()
            .border_color(theme.border_subtle)
            .child(list)
            .child(div().flex_none().py(Theme::SPACE_2).child(new_space))
    }
}

impl RenderOnce for Roster {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        if self.collapsed {
            self.render_rail(&theme, window, cx)
        } else {
            self.render_expanded(&theme, window, cx)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATES: [RowState; 5] = [
        RowState::NeedsYou,
        RowState::Failed,
        RowState::Running,
        RowState::Paused,
        RowState::Idle,
    ];

    #[test]
    fn states_use_their_theme_colours_and_distinct_icons() {
        for theme in [Theme::dark(), Theme::light()] {
            let colors = STATES.map(|state| state.color(&theme));
            assert_eq!(
                colors,
                [
                    theme.warning,
                    theme.danger,
                    theme.success,
                    theme.text_muted,
                    theme.text_subtle
                ]
            );
        }
        let icons = STATES.map(RowState::icon);
        for (i, a) in icons.iter().enumerate() {
            assert!(icons[i + 1..].iter().all(|b| b != a), "{a:?} repeats");
        }
    }

    #[test]
    fn agents_past_the_limit_are_counted() {
        assert_eq!(hidden_agent_count(0), 0);
        assert_eq!(hidden_agent_count(ROSTER_AGENT_LIMIT), 0);
        assert_eq!(hidden_agent_count(ROSTER_AGENT_LIMIT + 3), 3);
    }

    #[test]
    fn shortcuts_and_initials() {
        assert_eq!(shortcut_label(1).as_deref(), Some("⌘1"));
        assert_eq!(shortcut_label(9).as_deref(), Some("⌘9"));
        assert_eq!(shortcut_label(0), None);
        assert_eq!(shortcut_label(10), None);
        assert_eq!(rail_initial("checkout retry").as_ref(), "C");
        assert_eq!(rail_initial("  #fix").as_ref(), "F");
        assert_eq!(rail_initial("").as_ref(), "?");
    }
}
