//! The loop bar: a space's `Write -> Test -> Probe -> Review` loop as a strip of stage pills, its status, its
//! iteration and turn counts, the autonomy selector, and the action the loop's status allows.
//!
//! It is drawn in two rows so it fits the 560px graph panel without scrolling sideways: the stages, status and
//! counter first, then the autonomy selector and the actions. Each row wraps rather than overflows, so the
//! actions are always in view.
//!
//! The bar draws a [`LoopRun`] the app passes in and reports what the user did through callbacks. It keeps no
//! state.

use std::rc::Rc;

use gpui::{App, FontWeight, SharedString, Window, div, prelude::*, px};
use xctx_graph::{AutonomyLevel, LoopRun, LoopStage, LoopStatus};

use crate::components::{Badge, Button, ButtonVariant, Pill};
use crate::theme::{Theme, Tone};

/// Every stage, in the order the bar shows them.
pub const LOOP_STAGES: [LoopStage; 4] = [
    LoopStage::Write,
    LoopStage::Test,
    LoopStage::Probe,
    LoopStage::Review,
];

/// The iterations the Within budget pill offers when the loop has no budget yet; see [`LoopBar::budget`].
pub const DEFAULT_MAX_ITERATIONS: u32 = 3;

/// The turns the Within budget pill offers when the loop has no budget yet; see [`LoopBar::budget`].
pub const DEFAULT_MAX_TURNS: u32 = 40;

/// The label of a stage pill.
pub fn loop_stage_label(stage: LoopStage) -> &'static str {
    match stage {
        LoopStage::Write => "Write",
        LoopStage::Test => "Test",
        LoopStage::Probe => "Probe",
        LoopStage::Review => "Review",
    }
}

/// The label of a loop status: `Idle`, `Running`, `Awaiting approval`, `Budget exhausted`, `Passed` or
/// `Blocked`.
pub fn loop_status_label(status: &LoopStatus) -> &'static str {
    match status {
        LoopStatus::Idle => "Idle",
        LoopStatus::Running => "Running",
        LoopStatus::AwaitingApproval { .. } => "Awaiting approval",
        LoopStatus::BudgetExhausted => "Budget exhausted",
        LoopStatus::Passed => "Passed",
        LoopStatus::Blocked { .. } => "Blocked",
    }
}

/// The badge tone of a loop status.
pub fn loop_status_tone(status: &LoopStatus) -> Tone {
    match status {
        LoopStatus::Idle => Tone::Neutral,
        LoopStatus::Running => Tone::Accent,
        LoopStatus::AwaitingApproval { .. } | LoopStatus::BudgetExhausted => Tone::Warning,
        LoopStatus::Passed => Tone::Success,
        LoopStatus::Blocked { .. } => Tone::Danger,
    }
}

/// The label of an autonomy pill: `Ask each step`, `Within budget` or `Until checks pass`.
pub fn autonomy_label(autonomy: AutonomyLevel) -> &'static str {
    match autonomy {
        AutonomyLevel::AskEachStep => "Ask each step",
        AutonomyLevel::WithinBudget { .. } => "Within budget",
        AutonomyLevel::UntilChecksPass => "Until checks pass",
    }
}

/// A short lowercase name for an autonomy level, used in test selectors: `loop-autonomy-<name>`.
pub fn autonomy_name(autonomy: AutonomyLevel) -> &'static str {
    match autonomy {
        AutonomyLevel::AskEachStep => "ask",
        AutonomyLevel::WithinBudget { .. } => "budget",
        AutonomyLevel::UntilChecksPass => "checks",
    }
}

/// The counter text: `iter 1 · 4 turns`, or `iter 1/3 · 4/40 turns` under a budget.
pub fn loop_counter(run: &LoopRun) -> String {
    let (iteration, turns) = (run.iteration(), run.turns_used());
    let noun = if turns == 1 { "turn" } else { "turns" };
    match run.autonomy() {
        AutonomyLevel::WithinBudget {
            max_iterations,
            max_turns,
        } => format!("iter {iteration}/{max_iterations} · {turns}/{max_turns} {noun}"),
        AutonomyLevel::AskEachStep | AutonomyLevel::UntilChecksPass => {
            format!("iter {iteration} · {turns} {noun}")
        }
    }
}

/// An action the loop bar offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LoopAction {
    /// Start or go on running the current stage ([`LoopRun::start`]).
    Start,
    /// Approve the stage change the loop waits for ([`LoopRun::approve_next_stage`]).
    ApproveNext,
    /// The current stage is finished ([`LoopRun::advance`]).
    Advance,
    /// Start over ([`LoopRun::reset`]).
    Reset,
}

impl LoopAction {
    /// The button label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::ApproveNext => "Approve next",
            Self::Advance => "Advance",
            Self::Reset => "Reset",
        }
    }

    /// A short lowercase name, used in test selectors: `loop-<name>`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::ApproveNext => "approve",
            Self::Advance => "advance",
            Self::Reset => "reset",
        }
    }

    /// The actions a loop in `status` allows, in the order they are drawn.
    pub fn for_status(status: &LoopStatus) -> &'static [LoopAction] {
        match status {
            LoopStatus::Idle => &[Self::Start],
            LoopStatus::Running => &[Self::Advance],
            LoopStatus::AwaitingApproval { .. } => &[Self::ApproveNext],
            LoopStatus::Passed => &[Self::Reset],
            LoopStatus::Blocked { .. } | LoopStatus::BudgetExhausted => &[Self::Reset, Self::Start],
        }
    }

    fn variant(self) -> ButtonVariant {
        match self {
            Self::Start | Self::ApproveNext | Self::Advance => ButtonVariant::Primary,
            Self::Reset => ButtonVariant::Ghost,
        }
    }
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type AutonomyHandler = Rc<dyn Fn(AutonomyLevel, &mut Window, &mut App)>;

/// The loop bar.
///
/// Test selectors: `loop-stage-<stage>` (lowercase) on the stage pills, `loop-<name>` ([`LoopAction::name`]) on
/// the action buttons and `loop-autonomy-<name>` ([`autonomy_name`]) on the autonomy pills.
#[derive(IntoElement)]
pub struct LoopBar {
    run: LoopRun,
    budget: (u32, u32),
    on_start: Option<Handler>,
    on_advance: Option<Handler>,
    on_approve_next: Option<Handler>,
    on_reset: Option<Handler>,
    on_autonomy: Option<AutonomyHandler>,
}

impl LoopBar {
    /// A bar for `run`, with no handlers. The Within budget pill offers the run's own budget when it has one,
    /// else [`DEFAULT_MAX_ITERATIONS`] and [`DEFAULT_MAX_TURNS`].
    pub fn new(run: LoopRun) -> Self {
        let budget = match run.autonomy() {
            AutonomyLevel::WithinBudget {
                max_iterations,
                max_turns,
            } => (max_iterations, max_turns),
            AutonomyLevel::AskEachStep | AutonomyLevel::UntilChecksPass => {
                (DEFAULT_MAX_ITERATIONS, DEFAULT_MAX_TURNS)
            }
        };
        Self {
            run,
            budget,
            on_start: None,
            on_advance: None,
            on_approve_next: None,
            on_reset: None,
            on_autonomy: None,
        }
    }

    /// The budget the Within budget pill passes to [`LoopBar::on_autonomy`].
    pub fn budget(mut self, max_iterations: u32, max_turns: u32) -> Self {
        self.budget = (max_iterations, max_turns);
        self
    }

    /// Call `handler` when Start is clicked.
    pub fn on_start(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_start = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Advance is clicked.
    pub fn on_advance(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_advance = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Approve next is clicked.
    pub fn on_approve_next(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_approve_next = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Reset is clicked.
    pub fn on_reset(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_reset = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the autonomy level of the pill clicked (also when it is the current one).
    pub fn on_autonomy(
        mut self,
        handler: impl Fn(AutonomyLevel, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_autonomy = Some(Rc::new(handler));
        self
    }

    fn handler(&self, action: LoopAction) -> Option<Handler> {
        match action {
            LoopAction::Start => self.on_start.clone(),
            LoopAction::ApproveNext => self.on_approve_next.clone(),
            LoopAction::Advance => self.on_advance.clone(),
            LoopAction::Reset => self.on_reset.clone(),
        }
    }
}

impl RenderOnce for LoopBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let status = self.run.status().clone();
        let current = self.run.stage();

        let mut stages = Vec::new();
        for (index, stage) in LOOP_STAGES.into_iter().enumerate() {
            if index > 0 {
                stages.push(
                    div()
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_subtle)
                        .child("→")
                        .into_any_element(),
                );
            }
            let selector = format!("loop-stage-{}", loop_stage_label(stage).to_lowercase());
            let is_current = stage == current;
            let mut pill = div()
                .id(SharedString::from(selector.clone()))
                .debug_selector(|| selector)
                .flex()
                .flex_none()
                .items_center()
                .h(px(20.0))
                .px(Theme::SPACE_2)
                .rounded(Theme::RADIUS_FULL)
                .border_1()
                .text_size(Theme::TEXT_XS)
                .child(loop_stage_label(stage));
            pill = if is_current {
                let colors = theme.tone(loop_status_tone(&status));
                pill.bg(colors.bg)
                    .border_color(colors.border)
                    .text_color(colors.fg)
                    .font_weight(FontWeight::SEMIBOLD)
            } else {
                pill.border_color(theme.border).text_color(theme.text_muted)
            };
            stages.push(pill.into_any_element());
        }

        let status_text = match &status {
            LoopStatus::AwaitingApproval { next_stage } => {
                format!("Awaiting approval → {}", loop_stage_label(*next_stage))
            }
            other => loop_status_label(other).to_owned(),
        };
        let blocked_reason = match &status {
            LoopStatus::Blocked { reason } => Some(
                div()
                    .min_w_0()
                    .max_w(px(220.0))
                    .truncate()
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.danger)
                    .child(reason.clone()),
            ),
            _ => None,
        };

        let (max_iterations, max_turns) = self.budget;
        let levels = [
            AutonomyLevel::AskEachStep,
            AutonomyLevel::WithinBudget {
                max_iterations,
                max_turns,
            },
            AutonomyLevel::UntilChecksPass,
        ];
        let current_autonomy = autonomy_name(self.run.autonomy());
        let autonomy_pills = levels.into_iter().map(|level| {
            let selector = format!("loop-autonomy-{}", autonomy_name(level));
            let mut pill = Pill::new(SharedString::from(selector.clone()), autonomy_label(level))
                .selected(autonomy_name(level) == current_autonomy)
                .selector(selector);
            if let Some(handler) = self.on_autonomy.clone() {
                pill = pill.on_click(move |_, window, cx| handler(level, window, cx));
            }
            pill
        });

        let actions = LoopAction::for_status(&status).iter().map(|&action| {
            let selector = format!("loop-{}", action.name());
            let mut button = Button::new(SharedString::from(selector.clone()), action.label())
                .variant(action.variant())
                .selector(selector);
            if let Some(handler) = self.handler(action) {
                button = button.on_click(move |_, window, cx| handler(window, cx));
            }
            button
        });

        let pipeline = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(Theme::SPACE_2)
            .min_w_0()
            .child(
                div()
                    .flex_none()
                    .text_size(Theme::TEXT_XS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text_subtle)
                    .child("LOOP"),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .children(stages),
            )
            .child(Badge::new(status_text).tone(loop_status_tone(&status)))
            .children(blocked_reason)
            .child(
                div()
                    .flex_none()
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_muted)
                    .child(loop_counter(&self.run)),
            );

        let controls = div()
            .flex()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap(Theme::SPACE_2)
            .min_w_0()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_0_5)
                    .p(Theme::SPACE_0_5)
                    .rounded(Theme::RADIUS_FULL)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.bg)
                    .children(autonomy_pills),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .children(actions),
            );

        div()
            .id("loop-bar")
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .min_w_0()
            .p(Theme::SPACE_2)
            .gap(Theme::SPACE_2)
            .bg(theme.surface)
            .border_1()
            .border_color(theme.border)
            .rounded(Theme::RADIUS_MD)
            .child(pipeline)
            .child(controls)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_follow_the_loop_status() {
        assert_eq!(
            LoopAction::for_status(&LoopStatus::Idle),
            [LoopAction::Start]
        );
        assert_eq!(
            LoopAction::for_status(&LoopStatus::Running),
            [LoopAction::Advance]
        );
        assert_eq!(
            LoopAction::for_status(&LoopStatus::AwaitingApproval {
                next_stage: LoopStage::Test
            }),
            [LoopAction::ApproveNext]
        );
        assert_eq!(
            LoopAction::for_status(&LoopStatus::Passed),
            [LoopAction::Reset]
        );
        for status in [
            LoopStatus::BudgetExhausted,
            LoopStatus::Blocked {
                reason: "tests failed".into(),
            },
        ] {
            let actions = LoopAction::for_status(&status);
            assert!(actions.contains(&LoopAction::Start) && actions.contains(&LoopAction::Reset));
        }
    }

    #[test]
    fn the_counter_shows_the_budget_only_under_one() {
        let mut run = LoopRun::new(AutonomyLevel::AskEachStep);
        assert_eq!(loop_counter(&run), "iter 0 · 0 turns");
        assert_eq!(run.start(), Ok(()));
        run.record_turn(1);
        assert_eq!(loop_counter(&run), "iter 1 · 1 turn");
        run.set_autonomy(AutonomyLevel::WithinBudget {
            max_iterations: 3,
            max_turns: 30,
        });
        run.record_turn(3);
        assert_eq!(loop_counter(&run), "iter 1/3 · 4/30 turns");
    }

    #[test]
    fn the_budget_pill_offers_the_runs_budget_or_the_default() {
        let bar = LoopBar::new(LoopRun::new(AutonomyLevel::UntilChecksPass));
        assert_eq!(bar.budget, (DEFAULT_MAX_ITERATIONS, DEFAULT_MAX_TURNS));
        let bar = LoopBar::new(LoopRun::new(AutonomyLevel::WithinBudget {
            max_iterations: 2,
            max_turns: 9,
        }));
        assert_eq!(bar.budget, (2, 9));
        assert_eq!(bar.budget(5, 50).budget, (5, 50));
    }

    #[test]
    fn status_labels_match_the_loop() {
        assert_eq!(
            loop_status_label(&LoopStatus::BudgetExhausted),
            "Budget exhausted"
        );
        assert_eq!(
            loop_status_label(&LoopStatus::AwaitingApproval {
                next_stage: LoopStage::Probe
            }),
            "Awaiting approval"
        );
        let labels: Vec<_> = LOOP_STAGES.into_iter().map(loop_stage_label).collect();
        assert_eq!(labels, ["Write", "Test", "Probe", "Review"]);
    }
}
