//! The fork dialog: fork a writer agent into competing attempts, each in its own worktree, with an optional
//! instruction of its own. Nothing is forked until Fork is clicked.

use std::rc::Rc;

use gpui::{AnyElement, App, FontWeight, SharedString, Window, div, prelude::*, px};

use crate::components::{Badge, Button, ButtonSize, ButtonVariant, Pill};
use crate::theme::{Theme, Tone};

/// The line under the attempts.
pub const FORK_HINT: &str = "Each attempt gets its own worktree on attempt/<slug>/<n>, its own PORT and a fork \
     of the writer's pi session. Nothing is merged until you adopt one.";

/// What an attempt row says when it has neither an input nor an instruction.
pub const FORK_NO_INSTRUCTION: &str = "Same task as the writer";

/// What the dialog says instead of the count pills when the space has no room for another attempt.
pub const FORK_FULL: &str =
    "This space already has the maximum live attempts. Discard or adopt one first.";

/// What the dialog says while a fork runs: closing it does not stop the fork.
pub const FORK_BUSY_HINT: &str =
    "Forking… Closing this dialog does not stop the fork; it reports in the status bar.";

/// What the dialog shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkDialogView {
    /// The writer agent being forked, as labelled in the space, e.g. "pi · api". Drawn as given: already masked.
    pub writer: SharedString,
    /// The repo the writer works in. Drawn as given: already masked.
    pub repo: SharedString,
    /// How many attempts to fork, from 1 to `max`.
    pub count: u32,
    /// The space's remaining capacity: how many more live attempts it may have
    /// (`xctx_attempt::MAX_ATTEMPTS` minus the live attempts it has). With 0 the dialog draws no count pills,
    /// says the space is full ([`FORK_FULL`]) and Fork is disabled.
    pub max: u32,
    /// Each attempt's instruction, by attempt (index 0 is attempt 1). Missing or empty entries mean "the same
    /// task". Shown only for rows the app gave no input for. Drawn as given: already masked.
    pub instructions: Vec<SharedString>,
    /// The fork is in progress: the count pills and Fork are disabled, Cancel reads "Close" and a hint says the
    /// fork carries on ([`FORK_BUSY_HINT`]). The dialog cannot lock the app's instruction inputs: the app must
    /// make them read-only meanwhile.
    pub busy: bool,
    /// Why the last fork failed, shown in the danger colour. Drawn as given: already masked.
    pub error: Option<SharedString>,
}

/// The Fork button's label for `count` attempts, e.g. "Fork 3 attempts".
pub fn fork_label(count: u32) -> SharedString {
    if count == 1 {
        "Fork 1 attempt".into()
    } else {
        format!("Fork {count} attempts").into()
    }
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type CountHandler = Rc<dyn Fn(u32, &mut Window, &mut App)>;

/// The fork dialog. The app owns the instruction inputs and passes them with
/// [`ForkDialog::instruction_inputs`]; rows without one show the instruction from the view.
///
/// Selectors for tests: `fork-count-<n>`, `fork-cancel`, `fork-confirm`, `fork-row-<n>`, `fork-instruction-<n>`
/// (a row's instruction text) or `fork-same-task-<n>` (its fallback text), `fork-full`, `fork-busy-hint` and
/// `fork-error`.
#[derive(IntoElement)]
pub struct ForkDialog {
    view: ForkDialogView,
    instruction_inputs: Vec<AnyElement>,
    on_count: Option<CountHandler>,
    on_cancel: Option<Handler>,
    on_fork: Option<Handler>,
}

impl ForkDialog {
    /// A dialog for `view` with no inputs and no handlers.
    pub fn new(view: ForkDialogView) -> Self {
        Self {
            view,
            instruction_inputs: Vec::new(),
            on_count: None,
            on_cancel: None,
            on_fork: None,
        }
    }

    /// Draw `inputs` (e.g. `PromptInput` entities) as the attempts' instructions, the first for attempt 1.
    /// Rows past the last input fall back to the view's instruction text; inputs past the count are not drawn.
    pub fn instruction_inputs<E: IntoElement>(
        mut self,
        inputs: impl IntoIterator<Item = E>,
    ) -> Self {
        self.instruction_inputs = inputs
            .into_iter()
            .map(IntoElement::into_any_element)
            .collect();
        self
    }

    /// Call `handler` with the count pill clicked (not while busy).
    pub fn on_count(mut self, handler: impl Fn(u32, &mut Window, &mut App) + 'static) -> Self {
        self.on_count = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Cancel (or, while busy, Close) is clicked. The fork, if running, carries on.
    pub fn on_cancel(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_cancel = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Fork is clicked (only while [`ForkDialog::can_fork`]).
    pub fn on_fork(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_fork = Some(Rc::new(handler));
        self
    }

    /// The count shown: the view's count kept within 1 and `max` (and `max` at least 1).
    pub fn count(&self) -> u32 {
        self.view.count.clamp(1, self.view.max.max(1))
    }

    /// Whether Fork is enabled: no fork is in progress and the space has room for another attempt.
    pub fn can_fork(&self) -> bool {
        !self.view.busy && self.view.max > 0
    }
}

fn label(text: &'static str, theme: &Theme) -> gpui::Div {
    div()
        .w(px(72.0))
        .flex_none()
        .text_size(Theme::TEXT_XS)
        .text_color(theme.text_subtle)
        .child(text)
}

impl RenderOnce for ForkDialog {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let count = self.count();
        let can_fork = self.can_fork();
        let busy = self.view.busy;
        let max = self.view.max;
        let full = max == 0;
        let view = self.view;

        let pills = (1..=max).map(|n| {
            let mut pill = Pill::new(SharedString::from(format!("fork-count-{n}")), n.to_string())
                .selected(n == count)
                .disabled(busy)
                .selector(format!("fork-count-{n}"));
            if let Some(handler) = self.on_count.clone().filter(|_| !busy) {
                pill = pill.on_click(move |_, window, cx| handler(n, window, cx));
            }
            pill
        });

        let mut inputs = self.instruction_inputs.into_iter();
        let row_count = if full { 0 } else { count };
        let rows: Vec<_> = (1..=row_count)
            .map(|n| {
                let body: AnyElement = match inputs.next() {
                    Some(input) => div().flex_1().min_w_0().child(input).into_any_element(),
                    None => {
                        let text = view
                            .instructions
                            .get(n as usize - 1)
                            .filter(|text| !text.trim().is_empty())
                            .cloned();
                        let (color, selector) = if text.is_some() {
                            (theme.text, format!("fork-instruction-{n}"))
                        } else {
                            (theme.text_subtle, format!("fork-same-task-{n}"))
                        };
                        div()
                            .debug_selector(move || selector)
                            .flex_1()
                            .min_w_0()
                            .text_size(Theme::TEXT_SM)
                            .text_color(color)
                            .child(text.unwrap_or_else(|| FORK_NO_INSTRUCTION.into()))
                            .into_any_element()
                    }
                };
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .py(Theme::SPACE_1)
                    .debug_selector(move || format!("fork-row-{n}"))
                    .child(
                        div()
                            .w(px(72.0))
                            .flex_none()
                            .text_size(Theme::TEXT_SM)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_muted)
                            .child(format!("Attempt {n}")),
                    )
                    .child(body)
            })
            .collect();

        let error = view.error.clone().map(|text| {
            div()
                .debug_selector(|| "fork-error".to_owned())
                .text_size(Theme::TEXT_SM)
                .text_color(theme.danger)
                .child(text)
        });

        let mut fork = Button::new(
            "fork-confirm",
            if busy {
                "Forking…".into()
            } else {
                fork_label(count)
            },
        )
        .variant(ButtonVariant::Primary)
        .size(ButtonSize::Large)
        .disabled(!can_fork)
        .selector("fork-confirm");
        if let Some(handler) = self.on_fork {
            fork = fork.on_click(move |_, window, cx| handler(window, cx));
        }
        let mut cancel = Button::new("fork-cancel", if busy { "Close" } else { "Cancel" })
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Large)
            .selector("fork-cancel");
        if let Some(handler) = self.on_cancel {
            cancel = cancel.on_click(move |_, window, cx| handler(window, cx));
        }

        div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_3)
            .w_full()
            .p(Theme::SPACE_4)
            .rounded(Theme::RADIUS_LG)
            .border_1()
            .border_color(theme.accent.alpha(0.55))
            .bg(theme.elevated)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(Badge::new("FORK").tone(Tone::Accent))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(Theme::TEXT_XL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child("Fork competing attempts"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(Theme::SPACE_2)
                    .child(label("Writer", &theme))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(Theme::TEXT_MD)
                            .text_color(theme.text)
                            .child(view.writer.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .gap(Theme::SPACE_2)
                    .child(label("Repo", &theme))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(Theme::TEXT_MD)
                            .text_color(theme.text)
                            .child(view.repo.clone()),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(label("Attempts", &theme))
                    .child(if full {
                        div()
                            .debug_selector(|| "fork-full".to_owned())
                            .flex_1()
                            .min_w_0()
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.warning)
                            .child(FORK_FULL)
                    } else {
                        div().flex().gap(Theme::SPACE_1).children(pills)
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .pt(Theme::SPACE_1)
                    .border_t_1()
                    .border_color(theme.divider)
                    .children(rows),
            )
            .children(error)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .pt(Theme::SPACE_2)
                    .border_t_1()
                    .border_color(theme.divider)
                    .child(if busy {
                        div()
                            .debug_selector(|| "fork-busy-hint".to_owned())
                            .flex_1()
                            .min_w_0()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_muted)
                            .child(FORK_BUSY_HINT)
                    } else {
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_subtle)
                            .child(FORK_HINT)
                    })
                    .child(cancel)
                    .child(fork),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view(count: u32, max: u32, busy: bool) -> ForkDialogView {
        ForkDialogView {
            writer: "pi".into(),
            repo: "api".into(),
            count,
            max,
            instructions: Vec::new(),
            busy,
            error: None,
        }
    }

    #[test]
    fn the_count_stays_within_one_and_max() {
        assert_eq!(ForkDialog::new(view(0, 4, false)).count(), 1);
        assert_eq!(ForkDialog::new(view(3, 4, false)).count(), 3);
        assert_eq!(ForkDialog::new(view(9, 4, false)).count(), 4);
        assert_eq!(ForkDialog::new(view(2, 0, false)).count(), 1);
    }

    #[test]
    fn fork_is_disabled_while_busy() {
        assert!(ForkDialog::new(view(2, 4, false)).can_fork());
        assert!(!ForkDialog::new(view(2, 4, true)).can_fork());
    }

    #[test]
    fn fork_is_disabled_when_the_space_is_full() {
        assert!(ForkDialog::new(view(1, 1, false)).can_fork());
        assert!(!ForkDialog::new(view(1, 0, false)).can_fork());
        assert!(!ForkDialog::new(view(2, 0, false)).can_fork());
    }

    #[test]
    fn the_fork_label_counts_attempts() {
        assert_eq!(fork_label(1).as_ref(), "Fork 1 attempt");
        assert_eq!(fork_label(3).as_ref(), "Fork 3 attempts");
    }
}
