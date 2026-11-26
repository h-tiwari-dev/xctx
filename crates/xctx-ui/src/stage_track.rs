//! The stage track: the strip across the top of a space's Flow view. It draws the loop's four stages
//! (`Write → Test → Probe → Review`) as connected cards, each with the agents that work in it, marks where the
//! loop is and what it waits for, and ends with the autonomy selector, the iteration and turn read-out and the
//! one action the loop's status allows.
//!
//! The track draws a [`LoopRun`] and an [`AgentGraph`] the app passes in and reports what the user did through
//! callbacks. It keeps no state (the running stage's spinner is a keyed entity, so it keeps turning across frames).
//! It is about 96px high and wraps its controls under the stages when the window is narrow; agent chips truncate
//! rather than overflow.
//!
//! Test selectors:
//! - `stage-card-<stage>` (`write`, `test`, `probe`, `review`) on the stage cards;
//! - `stage-agent-<agent id>` on an agent's chip ([`StageTrack::on_select_agent`]);
//! - `stage-add-<kind>` (`tests`, `probe`, `reviewer`) on an empty stage's ghost chip ([`StageTrack::on_add_helper`]);
//! - `stage-approve` on the gate's Approve button while the loop awaits approval ([`StageTrack::on_approve_next`]);
//! - `stage-start` (Idle), `stage-resume` (Blocked, Budget exhausted; both call [`StageTrack::on_start`]),
//!   `stage-advance` (Running) and `stage-reset` (any started state) on the actions;
//! - `stage-autonomy-<i>` on the autonomy segments: `0` Ask each step, `1` Within budget, `2` Until checks pass.

use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, BoxShadow, ElementId, FontWeight, Hsla, SharedString, Window, div, point,
    prelude::*, px,
};
use xctx_graph::{
    AgentGraph, AgentNode, AgentNodeId, AgentState, AutonomyLevel, HelperKind, LoopRun, LoopStage,
    LoopStatus,
};
use xctx_nav::flow::{STAGES, stage_agents};

use crate::agents::{
    PaneLive, agent_identity_color, card_badge, helper_kind_label, helper_kind_name,
};
use crate::components::{Button, ButtonSize, ButtonVariant, Tooltip};
use crate::icon::{Icon, IconName, IconSize};
use crate::spinner::Spinner;
use crate::theme::{Theme, Tone};

/// The iterations Within budget offers when the loop has no budget yet.
pub const DEFAULT_MAX_ITERATIONS: u32 = 3;

/// The turns Within budget offers when the loop has no budget yet.
pub const DEFAULT_MAX_TURNS: u32 = 40;

/// The name of a loop stage: `Write`, `Test`, `Probe` or `Review`.
pub fn loop_stage_label(stage: LoopStage) -> &'static str {
    match stage {
        LoopStage::Write => "Write",
        LoopStage::Test => "Test",
        LoopStage::Probe => "Probe",
        LoopStage::Review => "Review",
    }
}

/// The name of an autonomy level: `Ask each step`, `Within budget` or `Until checks pass`.
pub fn autonomy_label(autonomy: AutonomyLevel) -> &'static str {
    match autonomy {
        AutonomyLevel::AskEachStep => "Ask each step",
        AutonomyLevel::WithinBudget { .. } => "Within budget",
        AutonomyLevel::UntilChecksPass => "Until checks pass",
    }
}

/// The loop's read-out: `iter 1 · 4 turns`, or `iter 1/3 · 4/40 turns` under a budget.
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

/// The most agent chips a stage card shows; the rest are counted in a `+N`.
pub const STAGE_CHIP_LIMIT: usize = 3;

/// The icon of a stage's card: a pen for Write, a terminal for Test, a globe for Probe, a magnifier for Review.
pub fn stage_icon(stage: LoopStage) -> IconName {
    match stage {
        LoopStage::Write => IconName::PenTool,
        LoopStage::Test => IconName::Terminal,
        LoopStage::Probe => IconName::Globe,
        LoopStage::Review => IconName::Search,
    }
}

/// The helper an empty stage offers to add: Tests for Test, Probe for Probe, Reviewer for Review. `None` for Write,
/// whose agents are the space's writers (one per repo), which the track cannot add.
pub fn stage_helper(stage: LoopStage) -> Option<HelperKind> {
    match stage {
        LoopStage::Write => None,
        LoopStage::Test => Some(HelperKind::Tests),
        LoopStage::Probe => Some(HelperKind::Probe),
        LoopStage::Review => Some(HelperKind::Reviewer),
    }
}

/// Where a stage is in the loop, as its card shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StagePhase {
    /// Not reached in this iteration (or the loop has not started).
    Pending,
    /// Its checks passed in this iteration, or the whole loop passed.
    Done,
    /// The loop's stage while the loop is idle: it runs once started.
    Ready,
    /// Its agents are working.
    Running,
    /// It finished and the loop waits for approval to move on; `passed` says whether its checks passed.
    Awaiting {
        /// Whether its checks passed (false: the loop goes back to Write).
        passed: bool,
    },
    /// The loop stopped here on a failure.
    Blocked,
    /// The loop stopped here because the budget is used up.
    Exhausted,
}

/// How `run` has got on with `stage`: the loop's own stage takes the loop's status; another stage is
/// [`StagePhase::Done`] when the loop moved on from it with passing checks in the current iteration
/// ([`LoopRun::history`]), else [`StagePhase::Pending`]. Every stage is done once the loop passed.
pub fn stage_phase(run: &LoopRun, stage: LoopStage) -> StagePhase {
    let status = run.status();
    if *status == LoopStatus::Passed {
        return StagePhase::Done;
    }
    if stage == run.stage() {
        return match status {
            LoopStatus::Idle => StagePhase::Ready,
            LoopStatus::Running => StagePhase::Running,
            LoopStatus::AwaitingApproval { next_stage } => StagePhase::Awaiting {
                // Failing any stage goes back to Write, and passing Review finishes the loop instead of waiting.
                passed: *next_stage == stage.next() && stage != LoopStage::Review,
            },
            LoopStatus::Blocked { .. } => StagePhase::Blocked,
            LoopStatus::BudgetExhausted => StagePhase::Exhausted,
            LoopStatus::Passed => StagePhase::Done,
        };
    }
    let iteration = run.iteration();
    let done = run.history().iter().any(|transition| {
        transition.iteration == iteration
            && transition.checks_passed
            && transition.to != LoopStage::Write
            && transition.from == stage
    });
    if done {
        StagePhase::Done
    } else {
        StagePhase::Pending
    }
}

/// An action the track's controls offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StageAction {
    /// Start an idle loop ([`StageTrack::on_start`]).
    Start,
    /// Go on after a block or a used-up budget ([`StageTrack::on_start`]).
    Resume,
    /// The running stage is finished and its checks passed ([`StageTrack::on_advance`]).
    Advance,
    /// Start over ([`StageTrack::on_reset`]).
    Reset,
}

impl StageAction {
    /// The button label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Start => "Start",
            Self::Resume => "Resume",
            Self::Advance => "Mark stage done",
            Self::Reset => "Reset",
        }
    }

    /// The test selector: `stage-start`, `stage-resume`, `stage-advance` or `stage-reset`.
    pub fn selector(self) -> &'static str {
        match self {
            Self::Start => "stage-start",
            Self::Resume => "stage-resume",
            Self::Advance => "stage-advance",
            Self::Reset => "stage-reset",
        }
    }

    /// The actions a loop in `status` allows, in the order they are drawn (Reset first, the primary action last).
    /// Approving a stage change is not here: the gate between the stages offers it.
    pub fn for_status(status: &LoopStatus) -> &'static [StageAction] {
        match status {
            LoopStatus::Idle => &[Self::Start],
            LoopStatus::Running => &[Self::Reset, Self::Advance],
            LoopStatus::AwaitingApproval { .. } | LoopStatus::Passed => &[Self::Reset],
            LoopStatus::Blocked { .. } | LoopStatus::BudgetExhausted => {
                &[Self::Reset, Self::Resume]
            }
        }
    }

    fn icon(self) -> IconName {
        match self {
            Self::Start | Self::Resume => IconName::Play,
            Self::Advance => IconName::Check,
            Self::Reset => IconName::Refresh,
        }
    }
}

/// The autonomy levels the segmented control offers, in order: Ask each step, Within budget (the run's own budget,
/// else [`DEFAULT_MAX_ITERATIONS`] and [`DEFAULT_MAX_TURNS`]), Until checks pass.
pub fn autonomy_choices(run: &LoopRun) -> [AutonomyLevel; 3] {
    let budget = match run.autonomy() {
        level @ AutonomyLevel::WithinBudget { .. } => level,
        AutonomyLevel::AskEachStep | AutonomyLevel::UntilChecksPass => {
            AutonomyLevel::WithinBudget {
                max_iterations: DEFAULT_MAX_ITERATIONS,
                max_turns: DEFAULT_MAX_TURNS,
            }
        }
    };
    [
        AutonomyLevel::AskEachStep,
        budget,
        AutonomyLevel::UntilChecksPass,
    ]
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type AutonomyHandler = Rc<dyn Fn(AutonomyLevel, &mut Window, &mut App)>;
type AgentHandler = Rc<dyn Fn(&AgentNodeId, &mut Window, &mut App)>;
type HelperHandler = Rc<dyn Fn(HelperKind, &mut Window, &mut App)>;

/// The stage track of a space's Flow view. See the module docs for its test selectors.
#[derive(IntoElement)]
pub struct StageTrack {
    run: LoopRun,
    graph: AgentGraph,
    live: HashMap<AgentNodeId, PaneLive>,
    selected: Option<AgentNodeId>,
    on_start: Option<Handler>,
    on_advance: Option<Handler>,
    on_approve_next: Option<Handler>,
    on_reset: Option<Handler>,
    on_autonomy: Option<AutonomyHandler>,
    on_select_agent: Option<AgentHandler>,
    on_add_helper: Option<HelperHandler>,
}

impl StageTrack {
    /// A track for the loop `run` of a space whose agents are `graph`, with no handlers.
    pub fn new(run: LoopRun, graph: AgentGraph) -> Self {
        Self {
            run,
            graph,
            live: HashMap::new(),
            selected: None,
            on_start: None,
            on_advance: None,
            on_approve_next: None,
            on_reset: None,
            on_autonomy: None,
            on_select_agent: None,
            on_add_helper: None,
        }
    }

    /// What each agent's pane does, so a chip whose graph state is still idle shows its pane's state
    /// ([`card_badge`]).
    pub fn live(mut self, live: HashMap<AgentNodeId, PaneLive>) -> Self {
        self.live = live;
        self
    }

    /// Highlight the chip of `agent`, the agent selected in the Flow view.
    pub fn selected(mut self, agent: Option<AgentNodeId>) -> Self {
        self.selected = agent;
        self
    }

    /// Call `handler` when Start (an idle loop) or Resume (a blocked loop, or one whose budget is used up) is
    /// clicked.
    pub fn on_start(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_start = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Mark stage done is clicked on a running loop: the stage finished and its checks passed.
    pub fn on_advance(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_advance = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when the gate's Approve button is clicked on a loop awaiting approval.
    pub fn on_approve_next(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_approve_next = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Reset is clicked (offered once the loop has started).
    pub fn on_reset(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_reset = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the autonomy level of the segment clicked ([`autonomy_choices`]), also when it is the
    /// current one.
    pub fn on_autonomy(
        mut self,
        handler: impl Fn(AutonomyLevel, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_autonomy = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with an agent's id when its chip is clicked.
    pub fn on_select_agent(
        mut self,
        handler: impl Fn(&AgentNodeId, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_select_agent = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the helper to add when an empty stage's ghost chip is clicked ([`stage_helper`]).
    pub fn on_add_helper(
        mut self,
        handler: impl Fn(HelperKind, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_add_helper = Some(Rc::new(handler));
        self
    }

    fn action_handler(&self, action: StageAction) -> Option<Handler> {
        match action {
            StageAction::Start | StageAction::Resume => self.on_start.clone(),
            StageAction::Advance => self.on_advance.clone(),
            StageAction::Reset => self.on_reset.clone(),
        }
    }

    fn stage_card(
        &self,
        stage: LoopStage,
        phase: StagePhase,
        theme: &Theme,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        let name = loop_stage_label(stage).to_lowercase();
        let selector = format!("stage-card-{name}");
        let (outline, glow) = match phase {
            StagePhase::Pending | StagePhase::Done => (theme.border_subtle, false),
            StagePhase::Ready => (theme.border, false),
            StagePhase::Running => (theme.accent, true),
            StagePhase::Awaiting { .. } | StagePhase::Exhausted => (theme.warning, false),
            StagePhase::Blocked => (theme.danger, false),
        };
        let indicator: Option<AnyElement> = match phase {
            StagePhase::Pending | StagePhase::Ready => None,
            StagePhase::Done | StagePhase::Awaiting { passed: true } => Some(
                Icon::new(IconName::Check)
                    .size(IconSize::Xs)
                    .color(theme.success)
                    .into_any_element(),
            ),
            StagePhase::Awaiting { passed: false } | StagePhase::Blocked => Some(
                Icon::new(IconName::Error)
                    .size(IconSize::Xs)
                    .color(theme.danger)
                    .into_any_element(),
            ),
            StagePhase::Exhausted => Some(
                Icon::new(IconName::Warning)
                    .size(IconSize::Xs)
                    .color(theme.warning)
                    .into_any_element(),
            ),
            StagePhase::Running => Some(
                window
                    .use_keyed_state(ElementId::Name("stage-track-spinner".into()), cx, |_, _| {
                        Spinner::new().size(IconSize::Xs).tone(Tone::Accent)
                    })
                    .into_any_element(),
            ),
        };
        let active = !matches!(phase, StagePhase::Pending);
        let header = div()
            .flex()
            .items_center()
            .gap(Theme::SPACE_1)
            .min_w_0()
            .h(px(16.0))
            .child(
                Icon::new(stage_icon(stage))
                    .size(IconSize::Xs)
                    .color(if active {
                        theme.text_muted
                    } else {
                        theme.text_subtle
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(Theme::TEXT_XS)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if active { theme.text } else { theme.text_muted })
                    .child(loop_stage_label(stage)),
            )
            .children(indicator);

        let agents = stage_agents(&self.graph, stage);
        let mut chips: Vec<AnyElement> = agents
            .iter()
            .take(STAGE_CHIP_LIMIT)
            .map(|node| self.agent_chip(node, theme))
            .collect();
        if agents.len() > STAGE_CHIP_LIMIT {
            chips.push(
                div()
                    .flex_none()
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.meta)
                    .child(format!("+{}", agents.len() - STAGE_CHIP_LIMIT))
                    .into_any_element(),
            );
        }
        if agents.is_empty() {
            chips.push(match stage_helper(stage) {
                Some(kind) => self.add_chip(kind, theme),
                None => div()
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_subtle)
                    .child("no writer")
                    .into_any_element(),
            });
        }

        let mut card = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(|| selector)
            .flex()
            .flex_col()
            .flex_1()
            .flex_basis(px(0.0))
            .min_w(px(96.0))
            .gap(Theme::SPACE_1 + px(2.0))
            .px(Theme::SPACE_2)
            .py(Theme::SPACE_2)
            .rounded(Theme::RADIUS_LG)
            .border_1()
            .border_color(outline)
            .bg(theme.surface)
            .child(header)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .min_w_0()
                    .h(px(22.0))
                    .overflow_hidden()
                    .children(chips),
            );
        if glow {
            card = card.shadow(vec![BoxShadow {
                color: theme.accent.alpha(0.28),
                offset: point(px(0.0), px(0.0)),
                blur_radius: px(10.0),
                spread_radius: px(1.0),
                inset: false,
            }]);
        }
        card.into_any_element()
    }

    fn agent_chip(&self, node: &AgentNode, theme: &Theme) -> AnyElement {
        let selector = format!("stage-agent-{}", node.id);
        let selected = self.selected.as_ref() == Some(&node.id);
        let identity =
            agent_identity_color(&self.graph, node.id.as_str(), theme).unwrap_or(theme.text_subtle);
        let (state_label, tone) = card_badge(&node.state, self.live.get(&node.id).copied());
        let tooltip: SharedString = format!("{} · {state_label}", node.label).into();
        let mut chip = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(|| selector)
            .flex()
            .flex_shrink_1()
            .items_center()
            .gap(Theme::SPACE_1)
            .min_w(px(36.0))
            .max_w(px(160.0))
            .h(px(22.0))
            .rounded(Theme::RADIUS_MD)
            .cursor_pointer()
            .text_size(Theme::TEXT_XS)
            .text_color(theme.text)
            .tooltip(move |_, cx| Tooltip::view(tooltip.clone(), cx))
            .child(dot(identity))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(node.label.clone()),
            )
            .child(dot(tone_color(theme, tone)));
        chip = if selected {
            chip.px(px(5.0))
                .border_2()
                .border_color(theme.accent)
                .bg(theme.selection)
        } else {
            let hover = theme.hover;
            chip.px(px(6.0))
                .border_1()
                .border_color(theme.border_subtle)
                .bg(theme.raised)
                .hover(move |style| style.bg(hover))
        };
        if node.state == AgentState::Paused {
            chip = chip.opacity(0.55);
        }
        if let Some(handler) = self.on_select_agent.clone() {
            let id = node.id.clone();
            chip = chip.on_click(move |_, window, cx| {
                cx.stop_propagation();
                handler(&id, window, cx);
            });
        }
        chip.into_any_element()
    }

    fn add_chip(&self, kind: HelperKind, theme: &Theme) -> AnyElement {
        let selector = format!("stage-add-{}", helper_kind_name(kind));
        let (hover_bg, hover_fg) = (theme.hover, theme.text);
        let mut chip = div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(|| selector)
            .flex()
            .flex_shrink_1()
            .items_center()
            .gap(Theme::SPACE_1)
            .min_w_0()
            .h(px(22.0))
            .px(px(6.0))
            .rounded(Theme::RADIUS_MD)
            .border_1()
            .border_dashed()
            .border_color(theme.border)
            .cursor_pointer()
            .text_size(Theme::TEXT_XS)
            .text_color(theme.text_subtle)
            .hover(move |style| style.bg(hover_bg).text_color(hover_fg))
            .child(
                Icon::new(IconName::Plus)
                    .size(IconSize::Xs)
                    .color(theme.text_subtle),
            )
            .child(div().min_w_0().truncate().child(helper_kind_label(kind)));
        if let Some(handler) = self.on_add_helper.clone() {
            chip = chip.on_click(move |_, window, cx| {
                cx.stop_propagation();
                handler(kind, window, cx);
            });
        }
        chip.into_any_element()
    }

    fn gate(&self, next: LoopStage, theme: &Theme) -> AnyElement {
        let colors = theme.tone(Tone::Warning);
        let mut approve = Button::new(
            "stage-approve",
            format!("Approve → {}", loop_stage_label(next)),
        )
        .variant(ButtonVariant::Primary)
        .size(ButtonSize::Compact)
        .selector("stage-approve")
        .tooltip(format!(
            "The loop waits for you before moving to {}",
            loop_stage_label(next)
        ));
        if let Some(handler) = self.on_approve_next.clone() {
            approve = approve.on_click(move |_, window, cx| handler(window, cx));
        }
        div()
            .flex()
            .flex_none()
            .items_center()
            .self_center()
            .gap(Theme::SPACE_1)
            .mx(Theme::SPACE_1)
            .p(px(3.0))
            .pl(Theme::SPACE_1 + px(2.0))
            .rounded(Theme::RADIUS_MD)
            .border_1()
            .border_color(colors.border)
            .bg(colors.bg)
            .child(
                Icon::new(IconName::Warning)
                    .size(IconSize::Xs)
                    .color(theme.warning),
            )
            .child(approve)
            .into_any_element()
    }
}

/// A 6px round dot of `color`.
fn dot(color: Hsla) -> gpui::Div {
    div()
        .flex_none()
        .size(px(6.0))
        .rounded(Theme::RADIUS_FULL)
        .bg(color)
}

/// The colour of a state dot: the tone's colour, and the quiet text colour for neutral (idle) states.
fn tone_color(theme: &Theme, tone: Tone) -> Hsla {
    match tone {
        Tone::Neutral => theme.text_subtle,
        Tone::Accent => theme.accent,
        Tone::Success => theme.success,
        Tone::Warning => theme.warning,
        Tone::Danger => theme.danger,
    }
}

/// The arrow between two stage cards, green once the stage before it is done.
fn connector(done: bool, theme: &Theme) -> AnyElement {
    let color = if done {
        theme.success.alpha(0.8)
    } else {
        theme.text_subtle
    };
    div()
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .w(px(20.0))
        .child(
            Icon::new(IconName::ChevronRight)
                .size(IconSize::Xs)
                .color(color),
        )
        .into_any_element()
}

impl RenderOnce for StageTrack {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let status = self.run.status().clone();
        let current = self.run.stage();
        let gate_next = match &status {
            LoopStatus::AwaitingApproval { next_stage } => Some(*next_stage),
            _ => None,
        };

        let mut track: Vec<AnyElement> = Vec::new();
        for (index, stage) in STAGES.into_iter().enumerate() {
            let phase = stage_phase(&self.run, stage);
            track.push(self.stage_card(stage, phase, &theme, window, cx));
            let last = index + 1 == STAGES.len();
            match gate_next {
                // The gate sits right after the stage that finished: between it and the next one, or after the
                // last card when Review sends the loop back to Write.
                Some(next) if stage == current => track.push(self.gate(next, &theme)),
                _ if !last => track.push(connector(phase == StagePhase::Done, &theme)),
                _ => {}
            }
        }

        let current_autonomy = self.run.autonomy();
        let segments = autonomy_choices(&self.run)
            .into_iter()
            .enumerate()
            .map(|(index, level)| {
                let selector = format!("stage-autonomy-{index}");
                let selected = std::mem::discriminant(&level) == std::mem::discriminant(&current_autonomy);
                let mut segment = Button::new(SharedString::from(selector.clone()), autonomy_label(level))
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .selected(selected)
                    .selector(selector);
                if let AutonomyLevel::WithinBudget {
                    max_iterations,
                    max_turns,
                } = level
                {
                    segment = segment.tooltip(format!(
                        "Run without asking for up to {max_iterations} iterations or {max_turns} turns"
                    ));
                }
                if let Some(handler) = self.on_autonomy.clone() {
                    segment = segment.on_click(move |_, window, cx| handler(level, window, cx));
                }
                segment
            });

        let actions = StageAction::for_status(&status).iter().map(|&action| {
            let variant = match (action, &status) {
                (StageAction::Reset, LoopStatus::Passed) => ButtonVariant::Secondary,
                (StageAction::Reset, _) => ButtonVariant::Ghost,
                _ => ButtonVariant::Primary,
            };
            let mut button = Button::new(action.selector(), action.label())
                .variant(variant)
                .icon(action.icon())
                .selector(action.selector());
            if let Some(handler) = self.action_handler(action) {
                button = button.on_click(move |_, window, cx| handler(window, cx));
            }
            button
        });

        let controls = div()
            .flex()
            .flex_col()
            .flex_none()
            .items_end()
            .gap(Theme::SPACE_1 + px(2.0))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_0_5)
                    .p(Theme::SPACE_0_5)
                    .rounded(Theme::RADIUS_MD)
                    .border_1()
                    .border_color(theme.border_subtle)
                    .bg(theme.surface)
                    .children(segments),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(
                        div()
                            .flex_none()
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.meta)
                            .child(loop_counter(&self.run)),
                    )
                    .children(actions),
            );

        let note = match &status {
            LoopStatus::Blocked { reason } => Some((IconName::Error, theme.danger, reason.clone())),
            LoopStatus::BudgetExhausted => Some((
                IconName::Warning,
                theme.warning,
                "Budget used up: raise it or pick another autonomy level, then Resume".to_owned(),
            )),
            _ => None,
        }
        .map(|(icon, color, text)| {
            div()
                .id("stage-note")
                .flex()
                .items_center()
                .gap(Theme::SPACE_1)
                .min_w_0()
                .text_size(Theme::TEXT_XS)
                .text_color(color)
                .child(Icon::new(icon).size(IconSize::Xs).color(color))
                .child(div().flex_1().min_w_0().truncate().child(text))
        });

        div()
            .id("stage-track")
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .min_w_0()
            .px(Theme::SPACE_3)
            .py(Theme::SPACE_2)
            .gap(Theme::SPACE_1 + px(2.0))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap(Theme::SPACE_3)
                    .min_w_0()
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .items_stretch()
                            .min_w(px(440.0))
                            .children(track),
                    )
                    .child(controls),
            )
            .children(note)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idle_loop_is_ready_at_write_with_the_rest_pending() {
        let run = LoopRun::new(AutonomyLevel::AskEachStep);
        let phases: Vec<_> = STAGES.into_iter().map(|s| stage_phase(&run, s)).collect();
        assert_eq!(
            phases,
            [
                StagePhase::Ready,
                StagePhase::Pending,
                StagePhase::Pending,
                StagePhase::Pending
            ]
        );
        assert_eq!(StageAction::for_status(run.status()), [StageAction::Start]);
    }

    #[test]
    fn passed_stages_of_this_iteration_are_done() {
        let mut run = LoopRun::new(AutonomyLevel::UntilChecksPass);
        assert_eq!(run.start(), Ok(()));
        assert!(run.advance(true).is_ok());
        assert!(run.advance(true).is_ok());
        assert_eq!(stage_phase(&run, LoopStage::Write), StagePhase::Done);
        assert_eq!(stage_phase(&run, LoopStage::Test), StagePhase::Done);
        assert_eq!(stage_phase(&run, LoopStage::Probe), StagePhase::Running);
        assert_eq!(stage_phase(&run, LoopStage::Review), StagePhase::Pending);

        // Failing Probe starts iteration 2 at Write: nothing is done in it yet.
        assert!(run.advance(false).is_ok());
        assert_eq!(run.iteration(), 2);
        assert_eq!(stage_phase(&run, LoopStage::Write), StagePhase::Running);
        assert_eq!(stage_phase(&run, LoopStage::Test), StagePhase::Pending);
    }

    #[test]
    fn awaiting_approval_says_whether_the_checks_passed() {
        let mut run = LoopRun::new(AutonomyLevel::AskEachStep);
        assert_eq!(run.start(), Ok(()));
        assert!(run.advance(true).is_ok());
        assert_eq!(
            stage_phase(&run, LoopStage::Write),
            StagePhase::Awaiting { passed: true }
        );
        assert!(run.approve_next_stage().is_ok());
        assert!(run.advance(false).is_ok());
        assert_eq!(
            stage_phase(&run, LoopStage::Test),
            StagePhase::Awaiting { passed: false }
        );
        assert_eq!(StageAction::for_status(run.status()), [StageAction::Reset]);
    }

    #[test]
    fn a_passed_loop_is_done_everywhere_and_a_blocked_one_resumes() {
        let mut run = LoopRun::new(AutonomyLevel::UntilChecksPass);
        assert_eq!(run.start(), Ok(()));
        run.block("tests failed");
        assert_eq!(stage_phase(&run, LoopStage::Write), StagePhase::Blocked);
        assert_eq!(
            StageAction::for_status(run.status()),
            [StageAction::Reset, StageAction::Resume]
        );
        assert_eq!(run.start(), Ok(()));
        for _ in 0..4 {
            assert!(run.advance(true).is_ok());
        }
        assert_eq!(*run.status(), LoopStatus::Passed);
        assert!(
            STAGES
                .into_iter()
                .all(|s| stage_phase(&run, s) == StagePhase::Done)
        );
    }

    #[test]
    fn the_budget_segment_offers_the_runs_budget_or_the_default() {
        let run = LoopRun::new(AutonomyLevel::AskEachStep);
        assert_eq!(
            autonomy_choices(&run)[1],
            AutonomyLevel::WithinBudget {
                max_iterations: DEFAULT_MAX_ITERATIONS,
                max_turns: DEFAULT_MAX_TURNS
            }
        );
        let own = AutonomyLevel::WithinBudget {
            max_iterations: 5,
            max_turns: 9,
        };
        assert_eq!(autonomy_choices(&LoopRun::new(own))[1], own);
    }

    #[test]
    fn empty_stages_offer_their_helper() {
        assert_eq!(stage_helper(LoopStage::Write), None);
        assert_eq!(stage_helper(LoopStage::Test), Some(HelperKind::Tests));
        assert_eq!(stage_helper(LoopStage::Probe), Some(HelperKind::Probe));
        assert_eq!(stage_helper(LoopStage::Review), Some(HelperKind::Reviewer));
    }
}
