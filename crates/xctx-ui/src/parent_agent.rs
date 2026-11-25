//! The Parent Agent drawer on Home: the always-on agent that drafts spaces and owns the Inbox, shown as a
//! header with its status and, when open, its terminal. It never launches on its own: its drafts wait at the
//! Plan Gate. Its no-code rule is advisory, a prompt rather than a sandbox.

use std::rc::Rc;

use gpui::{AnyElement, App, FontWeight, Pixels, SharedString, Window, div, prelude::*, px};

use crate::components::{Badge, Button, ButtonSize, ButtonVariant};
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

/// The line under the drawer's title. It says the no-code rule is only asked of the agent (it has a shell), while
/// the Plan Gate is enforced.
pub const PARENT_AGENT_ROLE: &str = "Drafts spaces from your tasks and owns the Inbox. Asked not to write code (advisory: it has a shell); nothing launches until you approve.";

/// The default height of the open drawer's terminal.
pub const DRAWER_HEIGHT: Pixels = px(280.0);

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The Parent Agent drawer.
#[derive(IntoElement)]
pub struct ParentAgentDrawer {
    status: AgentStatus,
    open: bool,
    height: Pixels,
    body: Option<AnyElement>,
    on_toggle: Option<Handler>,
    on_restart: Option<Handler>,
}

impl ParentAgentDrawer {
    /// A closed drawer for an agent in `status`.
    pub fn new(status: AgentStatus) -> Self {
        Self {
            status,
            open: false,
            height: DRAWER_HEIGHT,
            body: None,
            on_toggle: None,
            on_restart: None,
        }
    }

    /// Show the body under the header.
    pub fn open(mut self, open: bool) -> Self {
        self.open = open;
        self
    }

    /// The open drawer's body height.
    pub fn height(mut self, height: Pixels) -> Self {
        self.height = height;
        self
    }

    /// What the open drawer shows: the agent's terminal.
    pub fn body(mut self, body: impl IntoElement) -> Self {
        self.body = Some(body.into_any_element());
        self
    }

    /// Call `handler` when Show or Hide is clicked.
    pub fn on_toggle(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Some(Rc::new(handler));
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
        let restart = self
            .on_restart
            .filter(|_| self.status.can_restart())
            .map(|handler| {
                Button::new("parent-restart", "Restart")
                    .size(ButtonSize::Regular)
                    .selector("parent-restart")
                    .on_click(move |_, window, cx| handler(window, cx))
            });
        let toggle = self.on_toggle.map(|handler| {
            Button::new("parent-toggle", if self.open { "Hide" } else { "Show" })
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Regular)
                .selector("parent-toggle")
                .on_click(move |_, window, cx| handler(window, cx))
        });
        let body = self.open.then(|| {
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
            div()
                .h(self.height)
                .w_full()
                .overflow_hidden()
                .border_t_1()
                .border_color(theme.border_subtle)
                .child(content)
        });
        div()
            .flex()
            .flex_col()
            .w_full()
            .rounded(Theme::RADIUS_MD)
            .border_1()
            .border_color(theme.accent.alpha(0.4))
            .bg(theme.surface)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .px(Theme::SPACE_3)
                    .py(Theme::SPACE_2)
                    .child(
                        div()
                            .flex_none()
                            .text_size(Theme::TEXT_LG)
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
                    .children(restart)
                    .children(toggle),
            )
            .children(body)
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
