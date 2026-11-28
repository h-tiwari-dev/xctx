//! The Parent Agent panel on Home: the always-on agent you tell what you are working on, shown as a header with
//! its status above its terminal. It never launches on its own: its drafts wait as a Plan Gate card under it.
//! Its no-code rule is advisory, a prompt rather than a sandbox.

use std::rc::Rc;

use gpui::{AnyElement, App, FontWeight, Pixels, SharedString, Window, div, prelude::*, px};

use crate::components::{Badge, Button, ButtonSize};
use crate::icon::{Icon, IconName};
use crate::theme::{Theme, Tone};

/// Where the Parent Agent's session is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentStatus {
    /// Its program is starting.
    Starting,
    /// It is running.
    Running,
    /// Its program exited, with its exit code when it had one.
    Exited(Option<i32>),
    /// It cannot run, and why (for example, `pi` is not installed).
    Unavailable(SharedString),
}

impl AgentStatus {
    /// The badge text.
    pub fn label(&self) -> SharedString {
        match self {
            Self::Starting => "STARTING".into(),
            Self::Running => "RUNNING".into(),
            Self::Exited(Some(code)) => format!("EXITED {code}").into(),
            Self::Exited(None) => "EXITED".into(),
            Self::Unavailable(_) => "UNAVAILABLE".into(),
        }
    }

    /// The badge tone.
    pub fn tone(&self) -> Tone {
        match self {
            Self::Starting => Tone::Accent,
            Self::Running => Tone::Success,
            Self::Exited(_) => Tone::Warning,
            Self::Unavailable(_) => Tone::Danger,
        }
    }

    /// Whether the agent can be started again.
    pub fn can_restart(&self) -> bool {
        matches!(self, Self::Exited(_) | Self::Unavailable(_))
    }
}

/// The line under the panel's title. It says the no-code rule is only asked of the agent (it has a shell), while
/// the Plan Gate is enforced.
pub const PARENT_AGENT_ROLE: &str = "Drafts a space for your task; nothing launches until you press Launch. Asked not to write code (advisory).";

/// The default (minimum) height of the panel's terminal; Home passes a taller one that grows with the window.
pub const DRAWER_HEIGHT: Pixels = px(260.0);

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The Parent Agent drawer: Home's composer card, a slim header (icon, name, status, Restart when stopped) over
/// the agent's terminal.
#[derive(IntoElement)]
pub struct ParentAgentDrawer {
    status: AgentStatus,
    height: Pixels,
    focused: bool,
    embedded: bool,
    body: Option<AnyElement>,
    on_restart: Option<Handler>,
}

impl ParentAgentDrawer {
    /// A panel for an agent in `status`.
    pub fn new(status: AgentStatus) -> Self {
        Self {
            status,
            height: DRAWER_HEIGHT,
            focused: false,
            embedded: false,
            body: None,
            on_restart: None,
        }
    }

    /// The terminal's height.
    pub fn height(mut self, height: Pixels) -> Self {
        self.height = height;
        self
    }

    /// Whether the agent's terminal has keyboard focus: the card's border turns `accent`.
    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }

    /// Draw inside another card, without a second outline or shadow.
    pub fn embedded(mut self, embedded: bool) -> Self {
        self.embedded = embedded;
        self
    }

    /// What the panel shows under its header: the agent's terminal.
    pub fn body(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self
    }

    /// Offer Restart once the agent has exited or could not start, calling `handler`.
    pub fn on_restart(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_restart = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for ParentAgentDrawer {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let show_header = !self.embedded || self.status.can_restart();
        let restart = self
            .on_restart
            .filter(|_| self.status.can_restart())
            .map(|handler| {
                Button::new("parent-restart", "Restart")
                    .icon(IconName::Refresh)
                    .size(ButtonSize::Compact)
                    .selector("parent-restart")
                    .on_click(move |_, window, cx| handler(window, cx))
            });
        let content = match (&self.status, self.body) {
            (AgentStatus::Unavailable(why), _) => div()
                .p(Theme::SPACE_3)
                .text_size(Theme::TEXT_SM)
                .text_color(theme.danger)
                .child(why.clone())
                .into_any_element(),
            (_, Some(body)) => body,
            (_, None) => div()
                .p(Theme::SPACE_3)
                .text_size(Theme::TEXT_SM)
                .text_color(theme.text_muted)
                .child("Starting the Parent Agent…")
                .into_any_element(),
        };
        let body = div()
            .h(self.height)
            .w_full()
            .overflow_hidden()
            .border_t_1()
            .border_color(theme.border_subtle)
            .child(content);
        let header = div()
            .flex()
            .items_center()
            .gap(Theme::SPACE_2)
            .h(px(36.0))
            .px(Theme::SPACE_3)
            .bg(theme.raised)
            .child(Icon::new(IconName::Orchestrator).color(theme.accent))
            .child(
                div()
                    .flex_none()
                    .text_size(Theme::TEXT_MD)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child("Parent Agent"),
            )
            .child(Badge::new(self.status.label()).tone(self.status.tone()))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_subtle)
                    .child(PARENT_AGENT_ROLE),
            )
            .children(restart);
        div()
            .flex()
            .flex_col()
            .w_full()
            .bg(theme.surface)
            .overflow_hidden()
            .when(!self.embedded, |panel| {
                panel
                    .rounded(Theme::RADIUS_LG)
                    .border_1()
                    .border_color(if self.focused {
                        theme.accent
                    } else {
                        theme.border
                    })
                    .shadow_md()
            })
            .children(show_header.then_some(header))
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_stopped_agent_restarts() {
        assert!(!AgentStatus::Running.can_restart());
        assert!(!AgentStatus::Starting.can_restart());
        assert!(AgentStatus::Exited(Some(1)).can_restart());
        assert!(AgentStatus::Unavailable("pi not found".into()).can_restart());
        assert_eq!(AgentStatus::Exited(Some(2)).label().as_ref(), "EXITED 2");
    }
}
