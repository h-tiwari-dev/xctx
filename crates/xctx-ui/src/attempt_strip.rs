//! The attempt strip: a row in a space tab listing the space itself and its competing attempts, each with its
//! state, so the user can switch the panes between them, open the review sheet, or fork more attempts.

use std::rc::Rc;

use gpui::{App, FontWeight, SharedString, Window, div, prelude::*, px};

use crate::components::{Badge, Button, ButtonSize, ButtonVariant};
use crate::theme::{Theme, Tone};

/// The state of one attempt, as the strip and the review sheet show it. The app maps
/// `xctx_attempt::AttemptState` onto it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttemptChipState {
    /// Its agent is still working.
    Running,
    /// Its agent reported an outcome.
    Done,
    /// Its agent reported a failure or stopped.
    Failed,
    /// It was merged into the space.
    Adopted,
    /// It was thrown away; its worktree and branch are gone.
    Discarded,
}

impl AttemptChipState {
    /// The badge text, e.g. "RUNNING".
    pub fn label(self) -> &'static str {
        match self {
            Self::Running => "RUNNING",
            Self::Done => "DONE",
            Self::Failed => "FAILED",
            Self::Adopted => "ADOPTED",
            Self::Discarded => "DISCARDED",
        }
    }

    /// The badge tone: running is the accent, done and adopted success, failed danger, discarded neutral.
    pub fn tone(self) -> Tone {
        match self {
            Self::Running => Tone::Accent,
            Self::Done | Self::Adopted => Tone::Success,
            Self::Failed => Tone::Danger,
            Self::Discarded => Tone::Neutral,
        }
    }

    /// Whether the attempt still has a worktree and an agent, so it can be shown, adopted or discarded:
    /// running, done and failed attempts are live.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Running | Self::Done | Self::Failed)
    }

    /// Whether the strip lets the user switch the panes to it: only live attempts. An adopted or discarded
    /// attempt's worktree and agent are gone, so there is nothing left to show.
    pub fn can_select(self) -> bool {
        self.is_live()
    }
}

/// The longest chip label, in characters, before [`truncate_label`] cuts it.
pub const CHIP_LABEL_MAX: usize = 28;

/// `text` cut to at most `max` characters, ending in "…" when it was cut.
pub fn truncate_label(text: &str, max: usize) -> SharedString {
    let text = text.trim();
    if text.chars().count() <= max {
        return SharedString::from(text.to_owned());
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    SharedString::from(format!("{}…", kept.trim_end()))
}

/// One attempt in the strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptChip {
    /// The attempt's number within its space, from 1.
    pub number: u32,
    /// What the chip says, e.g. "Attempt 2" or the attempt's instruction. Cut to [`CHIP_LABEL_MAX`] characters.
    /// Drawn as given: already masked.
    pub label: SharedString,
    /// Its state.
    pub state: AttemptChipState,
    /// Whether the panes show this attempt now. The active chip (or the Space chip) is marked with a leading
    /// "▸" as well as the accent.
    pub active: bool,
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type SelectHandler = Rc<dyn Fn(Option<u32>, &mut Window, &mut App)>;

/// The strip of a space's attempts. The leading "Space" chip stands for the space itself and is active when no
/// attempt is. Clicking a chip calls [`AttemptStrip::on_select`] (only on live attempts, see
/// [`AttemptChipState::can_select`]); Review shows only with [`AttemptStrip::can_review`], and "Fork…" only with
/// [`AttemptStrip::on_fork`]. While [`AttemptStrip::busy`] both are disabled; "Fork…" is also disabled without
/// [`AttemptStrip::can_fork`].
///
/// Selectors for tests: `strip-space`, `strip-attempt-<n>`, `strip-review`, `strip-fork`.
#[derive(IntoElement)]
pub struct AttemptStrip {
    chips: Vec<AttemptChip>,
    can_review: bool,
    busy: bool,
    can_fork: bool,
    on_select: Option<SelectHandler>,
    on_review: Option<Handler>,
    on_fork: Option<Handler>,
}

impl AttemptStrip {
    /// A strip of `chips`, in the order given, with no handlers and no Review button.
    pub fn new(chips: Vec<AttemptChip>) -> Self {
        Self {
            chips,
            can_review: false,
            busy: false,
            can_fork: true,
            on_select: None,
            on_review: None,
            on_fork: None,
        }
    }

    /// Show the Review button, e.g. when the space has attempts to compare.
    pub fn can_review(mut self, can_review: bool) -> Self {
        self.can_review = can_review;
        self
    }

    /// Disable Review and "Fork…" while an attempt job (a fork, adopt or discard) runs for the space.
    pub fn busy(mut self, busy: bool) -> Self {
        self.busy = busy;
        self
    }

    /// Whether the space has room for another attempt (true by default); "Fork…" is disabled when it has not.
    pub fn can_fork(mut self, can_fork: bool) -> Self {
        self.can_fork = can_fork;
        self
    }

    /// Call `handler` with the chip clicked: `None` for the space itself, else the attempt's number.
    pub fn on_select(
        mut self,
        handler: impl Fn(Option<u32>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Review is clicked (not while busy).
    pub fn on_review(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_review = Some(Rc::new(handler));
        self
    }

    /// Show a "Fork…" button and call `handler` when it is clicked (not while busy or without room).
    pub fn on_fork(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_fork = Some(Rc::new(handler));
        self
    }

    /// Whether the "Space" chip is active: no attempt is.
    pub fn space_active(&self) -> bool {
        !self.chips.iter().any(|chip| chip.active)
    }
}

fn chip_shell(id: SharedString, active: bool, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    let (bg, border) = if active {
        (theme.accent_muted, theme.accent.alpha(0.55))
    } else {
        (theme.elevated, theme.border)
    };
    div()
        .id(id)
        .flex()
        .flex_none()
        .items_center()
        .gap(Theme::SPACE_1)
        .h(px(24.0))
        .px(Theme::SPACE_2)
        .rounded(Theme::RADIUS_FULL)
        .border_1()
        .border_color(border)
        .bg(bg)
        .text_size(Theme::TEXT_SM)
        .whitespace_nowrap()
}

impl RenderOnce for AttemptStrip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let space_active = self.space_active();

        let mut space = chip_shell("strip-space".into(), space_active, &theme)
            .debug_selector(|| "strip-space".to_owned())
            .text_color(if space_active {
                theme.text
            } else {
                theme.text_muted
            })
            .font_weight(if space_active {
                FontWeight::SEMIBOLD
            } else {
                FontWeight::MEDIUM
            })
            .child(if space_active { "▸ Space" } else { "Space" });
        if let Some(handler) = self.on_select.clone() {
            let hover = theme.surface_hover;
            space = space
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .on_click(move |_, window, cx| handler(None, window, cx));
        }

        let chips: Vec<_> = self
            .chips
            .iter()
            .map(|chip| {
                let number = chip.number;
                let selector = format!("strip-attempt-{number}");
                let weight = if chip.active || chip.state == AttemptChipState::Adopted {
                    FontWeight::SEMIBOLD
                } else {
                    FontWeight::MEDIUM
                };
                let mut element = chip_shell(selector.clone().into(), chip.active, &theme)
                    .debug_selector(move || selector)
                    .children(
                        chip.active
                            .then(|| div().text_color(theme.accent).child("▸")),
                    )
                    .child(
                        div()
                            .text_color(theme.text_subtle)
                            .text_size(Theme::TEXT_XS)
                            .child(format!("#{number}")),
                    )
                    .child(
                        div()
                            .font_weight(weight)
                            .text_color(if chip.active {
                                theme.text
                            } else {
                                theme.text_muted
                            })
                            .child(truncate_label(&chip.label, CHIP_LABEL_MAX)),
                    )
                    .child(Badge::new(chip.state.label()).tone(chip.state.tone()));
                if !chip.state.is_live() {
                    element = element.opacity(if chip.state == AttemptChipState::Discarded {
                        0.5
                    } else {
                        0.8
                    });
                }
                if let Some(handler) = self.on_select.clone().filter(|_| chip.state.can_select()) {
                    let hover = theme.surface_hover;
                    element = element
                        .cursor_pointer()
                        .hover(move |style| style.bg(hover))
                        .on_click(move |_, window, cx| handler(Some(number), window, cx));
                }
                element
            })
            .collect();

        let busy = self.busy;
        let review = self.on_review.filter(|_| self.can_review).map(|handler| {
            Button::new("strip-review", "Review")
                .variant(ButtonVariant::Primary)
                .size(ButtonSize::Regular)
                .disabled(busy)
                .selector("strip-review")
                .on_click(move |_, window, cx| handler(window, cx))
        });
        let can_fork = self.can_fork;
        let fork = self.on_fork.map(|handler| {
            Button::new("strip-fork", "Fork…")
                .variant(ButtonVariant::Ghost)
                .disabled(busy || !can_fork)
                .selector("strip-fork")
                .on_click(move |_, window, cx| handler(window, cx))
        });

        div()
            .flex()
            .items_center()
            .gap(Theme::SPACE_2)
            .w_full()
            .px(Theme::SPACE_2)
            .py(Theme::SPACE_1)
            .border_b_1()
            .border_color(theme.divider)
            .bg(theme.surface)
            .child(
                div()
                    .id("strip-chips")
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .overflow_x_scroll()
                    .child(space)
                    .children(chips),
            )
            .children(fork)
            .children(review)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_states_are_running_done_and_failed() {
        use AttemptChipState::*;
        for state in [Running, Done, Failed] {
            assert!(state.is_live(), "{state:?}");
            assert!(state.can_select());
        }
        for state in [Adopted, Discarded] {
            assert!(!state.is_live(), "{state:?}");
            assert!(!state.can_select(), "{state:?}");
        }
    }

    #[test]
    fn tones_follow_the_state() {
        use AttemptChipState::*;
        assert_eq!(Running.tone(), Tone::Accent);
        assert_eq!(Done.tone(), Tone::Success);
        assert_eq!(Adopted.tone(), Tone::Success);
        assert_eq!(Failed.tone(), Tone::Danger);
        assert_eq!(Discarded.tone(), Tone::Neutral);
    }

    #[test]
    fn labels_are_cut_on_characters_with_an_ellipsis() {
        assert_eq!(truncate_label("  Attempt 2 ", 28).as_ref(), "Attempt 2");
        assert_eq!(truncate_label("use a token bucket", 8).as_ref(), "use a t…");
        assert_eq!(truncate_label("use a token", 7).as_ref(), "use a…");
        assert_eq!(truncate_label("ééééé", 3).as_ref(), "éé…");
        assert_eq!(truncate_label("abc", 3).as_ref(), "abc");
    }

    #[test]
    fn the_space_is_active_when_no_attempt_is() {
        let chip = |number, active| AttemptChip {
            number,
            label: "Attempt".into(),
            state: AttemptChipState::Running,
            active,
        };
        assert!(AttemptStrip::new(vec![]).space_active());
        assert!(AttemptStrip::new(vec![chip(1, false), chip(2, false)]).space_active());
        assert!(!AttemptStrip::new(vec![chip(1, false), chip(2, true)]).space_active());
    }
}
