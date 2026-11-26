//! The compact status line along the bottom of the window, made of segments: the active space, the git branch,
//! its mise env, its port, the agents running, the shape, a zoom badge, the Inbox count, a transient message, and
//! key hints drawn as keycaps.

use std::rc::Rc;
use std::time::Duration;

use gpui::{App, FontWeight, SharedString, Window, div, prelude::*, px};
use xctx_layout::Shape;

use crate::components::{Badge, Tooltip};
use crate::icon::{Icon, IconName, IconSize};
use crate::theme::{Theme, Tone};
use crate::title_bar::shape_label;

/// What a [`StatusBanner`] reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BannerKind {
    /// Something finished or changed; clears itself.
    Info,
    /// Something failed; stays until dismissed.
    Error,
}

/// A transient message in the status bar, such as "pushed space/fix-retry" or a git error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusBanner {
    /// What the message reports.
    pub kind: BannerKind,
    /// The message.
    pub message: SharedString,
}

impl StatusBanner {
    /// How long an info message stays before the app should clear it.
    pub const INFO_TIMEOUT: Duration = Duration::from_secs(4);

    /// An info message.
    pub fn info(message: impl Into<SharedString>) -> Self {
        Self {
            kind: BannerKind::Info,
            message: message.into(),
        }
    }

    /// An error message.
    pub fn error(message: impl Into<SharedString>) -> Self {
        Self {
            kind: BannerKind::Error,
            message: message.into(),
        }
    }

    /// How long after showing it the app should clear this banner: [`StatusBanner::INFO_TIMEOUT`] for info,
    /// `None` for errors, which stay until the user dismisses them.
    pub fn auto_clear_after(&self) -> Option<Duration> {
        match self.kind {
            BannerKind::Info => Some(Self::INFO_TIMEOUT),
            BannerKind::Error => None,
        }
    }
}

/// A key chord and what it does, shown as a hint such as `⌘D split`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyHint {
    /// The chord, in macOS symbols (`⌘D`).
    pub keys: SharedString,
    /// What it does (`split`).
    pub label: SharedString,
}

impl KeyHint {
    /// A hint for `keys` doing `label`.
    pub fn new(keys: impl Into<SharedString>, label: impl Into<SharedString>) -> Self {
        Self {
            keys: keys.into(),
            label: label.into(),
        }
    }
}

/// The hints shown when the app sets none: split, close and zoom.
pub fn default_key_hints() -> Vec<KeyHint> {
    vec![
        KeyHint::new("⌘D", "split"),
        KeyHint::new("⌘W", "close"),
        KeyHint::new("⌘⇧Z", "zoom"),
    ]
}

/// The narrowest a banner gets before the key hints give up their room, so a message never collapses to nothing.
const BANNER_MIN_WIDTH: gpui::Pixels = px(160.0);

/// Hints as one line, `⌘D split · ⌘W close · ⌘⇧Z zoom`.
pub fn format_key_hints(hints: &[KeyHint]) -> String {
    hints
        .iter()
        .map(|hint| format!("{} {}", hint.keys, hint.label))
        .collect::<Vec<_>>()
        .join(" · ")
}

/// Whether a mise env names production (any `,`-separated part is `production`, ignoring case), which the bar
/// and title bar draw in the danger tone.
pub fn is_production_env(env: &str) -> bool {
    env.split(',')
        .any(|part| part.trim().eq_ignore_ascii_case("production"))
}

/// A key chord such as `⌘K` drawn as a small keycap: [`Theme::TEXT_XS`] in `meta`, on `raised` with a `border`
/// outline. Shared by the status line, the title bar and the Roster.
pub fn keycap(keys: impl Into<SharedString>, theme: &Theme) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .h(px(16.0))
        .min_w(px(16.0))
        .px(px(4.0))
        .rounded(Theme::RADIUS_SM)
        .bg(theme.raised)
        .border_1()
        .border_color(theme.border)
        .text_size(Theme::TEXT_XS)
        .text_color(theme.meta)
        .whitespace_nowrap()
        .child(keys.into())
}

type DismissHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The bottom bar. Every item is optional and hidden when unset; the app passes what applies to the space
/// being shown. It has no background of its own (it sits on the window chrome) and a `border_subtle` top line.
/// Test selectors: `status-inbox` (the Inbox button) and `status-dismiss` (the banner's dismiss button).
#[derive(IntoElement)]
pub struct StatusBar {
    space: Option<SharedString>,
    mise_env: Option<SharedString>,
    port: Option<u16>,
    branch: Option<SharedString>,
    shape: Option<Shape>,
    agents: Option<(usize, usize)>,
    zoomed: bool,
    banner: Option<StatusBanner>,
    hints: Vec<KeyHint>,
    on_dismiss_banner: Option<DismissHandler>,
    inbox: Option<usize>,
    on_inbox: Option<DismissHandler>,
}

impl Default for StatusBar {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusBar {
    /// A bar showing only [`default_key_hints`].
    pub fn new() -> Self {
        Self {
            space: None,
            mise_env: None,
            port: None,
            branch: None,
            shape: None,
            agents: None,
            zoomed: false,
            banner: None,
            hints: default_key_hints(),
            on_dismiss_banner: None,
            inbox: None,
            on_inbox: None,
        }
    }

    /// The active space's slug.
    pub fn space(mut self, slug: impl Into<SharedString>) -> Self {
        self.space = Some(slug.into());
        self
    }

    /// The `MISE_ENV` the space's panes run with; drawn as a danger badge when it names production.
    pub fn mise_env(mut self, env: impl Into<SharedString>) -> Self {
        self.mise_env = Some(env.into());
        self
    }

    /// The space's `PORT`.
    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// The git branch of the focused pane's worktree.
    pub fn branch(mut self, branch: impl Into<SharedString>) -> Self {
        self.branch = Some(branch.into());
        self
    }

    /// The space's current shape.
    pub fn shape(mut self, shape: Shape) -> Self {
        self.shape = Some(shape);
        self
    }

    /// How many of the space's `total` agents are `running`, shown as `running/total`.
    pub fn agents(mut self, running: usize, total: usize) -> Self {
        self.agents = Some((running, total));
        self
    }

    /// Show a zoom badge.
    pub fn zoomed(mut self, zoomed: bool) -> Self {
        self.zoomed = zoomed;
        self
    }

    /// A transient message; the app clears it after [`StatusBanner::auto_clear_after`] or when dismissed.
    pub fn banner(mut self, banner: Option<StatusBanner>) -> Self {
        self.banner = banner;
        self
    }

    /// Replace the key hints.
    pub fn hints(mut self, hints: Vec<KeyHint>) -> Self {
        self.hints = hints;
        self
    }

    /// Show how many Inbox items are pending; highlighted when there are any.
    pub fn inbox(mut self, pending: usize) -> Self {
        self.inbox = Some(pending);
        self
    }

    /// Make the Inbox count a button that calls `handler`, to show the Inbox.
    pub fn on_inbox(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_inbox = Some(Rc::new(handler));
        self
    }

    /// Show a dismiss button on the banner that calls `handler`.
    pub fn on_dismiss_banner(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss_banner = Some(Rc::new(handler));
        self
    }
}

/// One segment: a row of an optional icon and text, kept whole.
fn segment() -> gpui::Div {
    div().flex().flex_none().items_center().gap(Theme::SPACE_1)
}

/// The thin line between left-hand segments.
fn divider(theme: &Theme) -> gpui::Div {
    div().flex_none().w(px(1.0)).h(px(12.0)).bg(theme.border)
}

impl RenderOnce for StatusBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let mut segments: Vec<gpui::AnyElement> = Vec::new();

        if let Some(slug) = self.space {
            segments.push(
                segment()
                    .text_color(theme.text)
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(slug)
                    .into_any_element(),
            );
        }
        if let Some(branch) = self.branch {
            segments.push(
                segment()
                    .min_w_0()
                    .flex_shrink_1()
                    .child(
                        Icon::new(IconName::Git)
                            .size(IconSize::Xs)
                            .color(theme.text_subtle),
                    )
                    .child(div().min_w_0().truncate().child(branch))
                    .into_any_element(),
            );
        }
        if let Some(env) = self.mise_env {
            let element = if is_production_env(&env) {
                Badge::new(env).tone(Tone::Danger).into_any_element()
            } else {
                segment()
                    .child(div().text_color(theme.text_subtle).child("env"))
                    .child(env)
                    .into_any_element()
            };
            segments.push(element);
        }
        if let Some(port) = self.port {
            segments.push(
                segment()
                    .text_color(theme.meta)
                    .child(format!(":{port}"))
                    .into_any_element(),
            );
        }
        if let Some((running, total)) = self.agents {
            let (icon, color) = if running > 0 {
                (IconName::Running, theme.success)
            } else {
                (IconName::Idle, theme.text_subtle)
            };
            segments.push(
                segment()
                    .child(Icon::new(icon).size(IconSize::Xs).color(color))
                    .child(
                        div()
                            .text_color(theme.meta)
                            .child(format!("{running}/{total}")),
                    )
                    .child(div().text_color(theme.text_subtle).child("agents"))
                    .into_any_element(),
            );
        }
        if let Some(shape) = self.shape {
            segments.push(segment().child(shape_label(shape)).into_any_element());
        }
        if self.zoomed {
            segments.push(
                segment()
                    .child(
                        Icon::new(IconName::Maximize)
                            .size(IconSize::Xs)
                            .color(theme.warning),
                    )
                    .child(Badge::new("ZOOMED").tone(Tone::Warning))
                    .into_any_element(),
            );
        }

        let mut left = div()
            .flex()
            .min_w_0()
            .flex_shrink_1()
            .items_center()
            .gap(Theme::SPACE_2);
        for (i, element) in segments.into_iter().enumerate() {
            if i > 0 {
                left = left.child(divider(&theme));
            }
            left = left.child(element);
        }

        let banner = self.banner.map(|banner| {
            let (tone, icon) = match banner.kind {
                BannerKind::Info => (Tone::Accent, IconName::Check),
                BannerKind::Error => (Tone::Danger, IconName::Error),
            };
            let colors = theme.tone(tone);
            let dismiss = self.on_dismiss_banner.map(|handler| {
                let hover = theme.hover;
                div()
                    .id("status-dismiss")
                    .debug_selector(|| "status-dismiss".to_owned())
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(14.0))
                    .rounded(Theme::RADIUS_FULL)
                    .cursor_pointer()
                    .hover(move |style| style.bg(hover))
                    .child(
                        Icon::new(IconName::Close)
                            .size_px(px(10.0))
                            .color(colors.fg),
                    )
                    .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        handler(window, cx);
                    })
            });
            div()
                .flex()
                .min_w_0()
                .max_w_full()
                .items_center()
                .gap(Theme::SPACE_1)
                .h(px(18.0))
                .pl(Theme::SPACE_2)
                .pr(if dismiss.is_some() {
                    Theme::SPACE_0_5
                } else {
                    Theme::SPACE_2
                })
                .rounded(Theme::RADIUS_FULL)
                .border_1()
                .border_color(colors.border)
                .bg(colors.bg)
                .text_color(match banner.kind {
                    BannerKind::Info => theme.text,
                    BannerKind::Error => theme.danger,
                })
                .child(Icon::new(icon).size_px(px(11.0)).color(colors.fg))
                .child(div().flex_1().min_w_0().truncate().child(banner.message))
                .children(dismiss)
        });
        let has_banner = banner.is_some();

        let inbox = self.inbox.map(|pending| {
            let hover = theme.hover;
            let (icon_color, text_color) = if pending > 0 {
                (theme.warning, theme.warning)
            } else {
                (theme.text_subtle, theme.text_subtle)
            };
            let mut button = div()
                .id("status-inbox")
                .debug_selector(|| "status-inbox".to_owned())
                .flex()
                .flex_none()
                .items_center()
                .gap(Theme::SPACE_1)
                .h(px(18.0))
                .px(Theme::SPACE_1 + px(2.0))
                .rounded(Theme::RADIUS_SM)
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .tooltip(|_, cx| Tooltip::view("Inbox", cx))
                .child(
                    Icon::new(IconName::Inbox)
                        .size(IconSize::Xs)
                        .color(icon_color),
                )
                .when(pending > 0, |button| {
                    button.child(
                        div()
                            .text_color(text_color)
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(pending.to_string()),
                    )
                });
            if let Some(handler) = self.on_inbox.clone() {
                button = button.on_click(move |_, window, cx| handler(window, cx));
            }
            button
        });

        // With a banner showing, the hints shrink (and clip) before the banner does.
        let hints = (!self.hints.is_empty()).then(|| {
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_3)
                .overflow_hidden()
                .when(has_banner, |hints| hints.min_w_0().flex_shrink_1())
                .when(!has_banner, |hints| hints.flex_none())
                .children(self.hints.into_iter().map(|hint| {
                    segment()
                        .child(keycap(hint.keys, &theme))
                        .child(div().text_color(theme.text_subtle).child(hint.label))
                }))
        });

        div()
            .flex()
            .flex_none()
            .items_center()
            .w_full()
            .h(Theme::STATUS_BAR_HEIGHT)
            .px(Theme::SPACE_3)
            .gap(Theme::SPACE_3)
            .border_t_1()
            .border_color(theme.border_subtle)
            .text_size(Theme::TEXT_XS)
            .text_color(theme.text_muted)
            .whitespace_nowrap()
            .child(left)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .when(has_banner, |container| container.min_w(BANNER_MIN_WIDTH))
                    .justify_end()
                    .children(banner),
            )
            .children(inbox)
            .children(hints)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hints_read_as_one_line() {
        assert_eq!(
            format_key_hints(&default_key_hints()),
            "⌘D split · ⌘W close · ⌘⇧Z zoom"
        );
        assert_eq!(format_key_hints(&[]), "");
        assert_eq!(format_key_hints(&[KeyHint::new("⌘T", "new")]), "⌘T new");
    }

    #[test]
    fn info_clears_itself_and_errors_stay() {
        assert_eq!(
            StatusBanner::info("pushed").auto_clear_after(),
            Some(StatusBanner::INFO_TIMEOUT)
        );
        assert_eq!(StatusBanner::error("push failed").auto_clear_after(), None);
        assert_eq!(StatusBanner::error("x").kind, BannerKind::Error);
    }

    #[test]
    fn a_new_bar_has_default_hints_and_nothing_else() {
        let bar = StatusBar::new();
        assert_eq!(bar.hints, default_key_hints());
        assert!(bar.space.is_none() && bar.port.is_none() && bar.banner.is_none());
        assert!(bar.agents.is_none());
        let bar = bar.space("fix-retry").port(4101).zoomed(true).agents(2, 3);
        assert_eq!(bar.port, Some(4101));
        assert_eq!(bar.agents, Some((2, 3)));
        assert!(bar.zoomed);
    }

    #[test]
    fn production_envs_are_recognised() {
        assert!(is_production_env("production"));
        assert!(is_production_env("checkout, Production"));
        assert!(!is_production_env("checkout"));
        assert!(!is_production_env("preproduction"));
        assert!(!is_production_env(""));
    }
}
