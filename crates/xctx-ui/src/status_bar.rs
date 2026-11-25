//! The compact bar along the bottom of the window: the active space and its environment, the git branch, the
//! shape, a zoom badge, a transient message, and key hints.

use std::rc::Rc;
use std::time::Duration;

use gpui::{App, FontWeight, Hsla, SharedString, Window, div, prelude::*, px};
use xctx_layout::Shape;

use crate::components::{Badge, Button, ButtonSize, ButtonVariant};
use crate::tab_bar::shape_label;
use crate::theme::{Theme, Tone};

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

type DismissHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The bottom bar. Every item is optional and hidden when unset; the app passes what applies to the space
/// being shown.
#[derive(IntoElement)]
pub struct StatusBar {
    space: Option<SharedString>,
    mise_env: Option<SharedString>,
    port: Option<u16>,
    branch: Option<SharedString>,
    shape: Option<Shape>,
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

    /// The `MISE_ENV` the space's panes run with, when it differs from the slug.
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

/// A `label value` pair, with the value in the main text colour.
fn item(theme: &Theme, label: &'static str, value: SharedString, value_color: Hsla) -> gpui::Div {
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap(Theme::SPACE_1)
        .child(div().text_color(theme.text_subtle).child(label))
        .child(div().text_color(value_color).child(value))
}

impl RenderOnce for StatusBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);

        let space = self.space.map(|slug| {
            div()
                .flex_none()
                .text_color(theme.text)
                .font_weight(FontWeight::SEMIBOLD)
                .child(slug)
        });
        let mise_env = self
            .mise_env
            .map(|env| item(&theme, "MISE_ENV", env, theme.text_muted));
        let port = self
            .port
            .map(|port| item(&theme, "PORT", port.to_string().into(), theme.text_muted));
        let branch = self
            .branch
            .map(|branch| item(&theme, "⎇", branch, theme.text_muted));
        let shape = self
            .shape
            .map(|shape| Badge::new(shape_label(shape)).tone(Tone::Accent));
        let zoomed = self
            .zoomed
            .then(|| Badge::new("ZOOMED").tone(Tone::Warning));

        let inbox = self.inbox.map(|pending| {
            let label = format!("Inbox {pending}");
            let mut button = Button::new("status-inbox", label)
                .variant(if pending > 0 {
                    ButtonVariant::Secondary
                } else {
                    ButtonVariant::Ghost
                })
                .size(ButtonSize::Compact)
                .selected(pending > 0)
                .selector("status-inbox");
            if let Some(handler) = self.on_inbox.clone() {
                button = button.on_click(move |_, window, cx| handler(window, cx));
            }
            div().flex_none().child(button)
        });

        let banner = self.banner.map(|banner| {
            let (tone, color) = match banner.kind {
                BannerKind::Info => (Tone::Accent, theme.text),
                BannerKind::Error => (Tone::Danger, theme.danger),
            };
            let colors = theme.tone(tone);
            let dismiss = self.on_dismiss_banner.map(|handler| {
                Button::new("status-dismiss", "×")
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .on_click(move |_, window, cx| handler(window, cx))
            });
            div()
                .flex()
                .min_w_0()
                .max_w_full()
                .items_center()
                .gap(Theme::SPACE_1)
                .h(px(18.0))
                .pl(Theme::SPACE_2)
                .pr(Theme::SPACE_0_5)
                .rounded(Theme::RADIUS_SM)
                .bg(colors.bg)
                .text_color(color)
                .child(div().flex_1().min_w_0().truncate().child(banner.message))
                .children(dismiss.map(|button| div().flex_none().child(button)))
        });
        let has_banner = banner.is_some();

        // With a banner showing, the hints shrink (and truncate) before the banner does.
        let hints = (!self.hints.is_empty()).then(|| {
            div()
                .when(has_banner, |hints| hints.min_w_0().truncate())
                .when(!has_banner, |hints| hints.flex_none())
                .text_color(theme.text_subtle)
                .child(format_key_hints(&self.hints))
        });

        div()
            .flex()
            .flex_none()
            .items_center()
            .w_full()
            .h(Theme::STATUS_BAR_HEIGHT)
            .px(Theme::SPACE_2)
            .gap(Theme::SPACE_3)
            .bg(theme.chrome)
            .border_t_1()
            .border_color(theme.border_subtle)
            .text_size(Theme::TEXT_XS)
            .text_color(theme.text_muted)
            .whitespace_nowrap()
            .children(space)
            .children(mise_env)
            .children(port)
            .children(branch)
            .children(shape)
            .children(zoomed)
            .children(inbox)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .when(has_banner, |container| container.min_w(BANNER_MIN_WIDTH))
                    .justify_end()
                    .children(banner),
            )
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
        let bar = bar.space("fix-retry").port(4101).zoomed(true);
        assert_eq!(bar.port, Some(4101));
        assert!(bar.zoomed);
    }
}
