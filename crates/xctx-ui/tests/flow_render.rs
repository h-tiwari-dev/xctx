//! The Flow view's stage track and timeline in GPUI's headless test window, clicked through their selectors.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, IntoElement, ListAlignment, ListState, Modifiers, Render, TestAppContext,
    VisualTestContext, Window, div, prelude::*, px,
};
use xctx_channel::{FindingSeverity, MessageKind};
use xctx_graph::{AgentGraph, AgentRole, AutonomyLevel, HelperKind, LoopRun, LoopStage};
use xctx_nav::flow::{TimelineItem, TimelineKind};
use xctx_ui::Theme;
use xctx_ui::flow_timeline::{FlowTimeline, sync_list_state};
use xctx_ui::stage_track::StageTrack;

/// What the hosts' handlers saw, in order.
type Log = Rc<RefCell<Vec<String>>>;

fn log(log: &Log, entry: impl Into<String>) {
    log.borrow_mut().push(entry.into());
}

/// Click the element named `selector`. False when it was not drawn.
fn click(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    let Some(bounds) = cx.debug_bounds(selector) else {
        return false;
    };
    cx.simulate_click(bounds.center(), Modifiers::default());
    true
}

/// Writers for `api` and `web` and a tests helper; no probe or reviewer.
fn graph() -> AgentGraph {
    let mut graph = AgentGraph::for_space(&["api".to_owned(), "web".to_owned()]);
    graph
        .add_lane(
            AgentRole::Helper {
                kind: HelperKind::Tests,
            },
            None,
        )
        .expect("a tests helper can be added");
    graph
}

struct StageHost {
    run: LoopRun,
    log: Log,
}

impl Render for StageHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let logs: [Log; 7] = std::array::from_fn(|_| self.log.clone());
        let [start, advance, approve, reset, autonomy, select, add] = logs;
        div().w(px(1200.0)).child(
            StageTrack::new(self.run.clone(), graph())
                .on_start(move |_, _| log(&start, "start"))
                .on_advance(move |_, _| log(&advance, "advance"))
                .on_approve_next(move |_, _| log(&approve, "approve"))
                .on_reset(move |_, _| log(&reset, "reset"))
                .on_autonomy(move |level, _, _| log(&autonomy, format!("autonomy {level:?}")))
                .on_select_agent(move |id, _, _| log(&select, format!("select {id}")))
                .on_add_helper(move |kind, _, _| log(&add, format!("add {kind:?}"))),
        )
    }
}

fn stage_window(cx: &mut TestAppContext, run: LoopRun) -> (Log, &mut VisualTestContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| StageHost { run, log: host_log });
    cx.run_until_parked();
    (events, cx)
}

#[gpui::test]
fn an_idle_loop_offers_start_and_no_gate(cx: &mut TestAppContext) {
    let (events, cx) = stage_window(cx, LoopRun::new(AutonomyLevel::AskEachStep));
    assert!(cx.debug_bounds("stage-approve").is_none());
    assert!(cx.debug_bounds("stage-reset").is_none());
    assert!(click(cx, "stage-start"), "Start is drawn");
    assert!(click(cx, "stage-autonomy-1"), "Within budget is drawn");
    assert_eq!(
        *events.borrow(),
        [
            "start".to_owned(),
            "autonomy WithinBudget { max_iterations: 3, max_turns: 40 }".to_owned()
        ]
    );
}

#[gpui::test]
fn a_loop_awaiting_approval_draws_the_gate(cx: &mut TestAppContext) {
    let mut run = LoopRun::new(AutonomyLevel::AskEachStep);
    assert_eq!(run.start(), Ok(()));
    assert!(run.advance(true).is_ok());
    assert_eq!(
        *run.status(),
        xctx_graph::LoopStatus::AwaitingApproval {
            next_stage: LoopStage::Test
        }
    );
    let (events, cx) = stage_window(cx, run);
    assert!(cx.debug_bounds("stage-start").is_none());
    assert!(click(cx, "stage-approve"), "Approve is drawn");
    assert!(click(cx, "stage-reset"), "Reset is drawn");
    assert_eq!(*events.borrow(), ["approve".to_owned(), "reset".to_owned()]);
}

#[gpui::test]
fn a_running_loop_offers_mark_stage_done(cx: &mut TestAppContext) {
    let mut run = LoopRun::new(AutonomyLevel::UntilChecksPass);
    assert_eq!(run.start(), Ok(()));
    let (events, cx) = stage_window(cx, run);
    assert!(click(cx, "stage-advance"), "Mark stage done is drawn");
    assert_eq!(*events.borrow(), ["advance".to_owned()]);
}

#[gpui::test]
fn agent_chips_select_and_empty_stages_offer_a_helper(cx: &mut TestAppContext) {
    let (events, cx) = stage_window(cx, LoopRun::new(AutonomyLevel::AskEachStep));
    assert!(
        click(cx, "stage-agent-writer-api"),
        "the api writer's chip is drawn"
    );
    assert!(
        click(cx, "stage-agent-helper-tests-1"),
        "the tests chip is drawn"
    );
    assert!(cx.debug_bounds("stage-add-tests").is_none());
    assert!(click(cx, "stage-add-probe"), "Probe offers + Probe");
    assert!(click(cx, "stage-add-reviewer"), "Review offers + Reviewer");
    assert_eq!(
        *events.borrow(),
        [
            "select writer-api".to_owned(),
            "select helper-tests-1".to_owned(),
            "add Probe".to_owned(),
            "add Reviewer".to_owned(),
        ]
    );
}

// ---------------------------------------------------------------------------------------------------------------

struct TimelineHost {
    items: Vec<TimelineItem>,
    state: ListState,
    log: Log,
}

impl Render for TimelineHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        sync_list_state(&self.state, self.items.len());
        let (clear, select, resolve) = (self.log.clone(), self.log.clone(), self.log.clone());
        div().w(px(360.0)).h(px(600.0)).child(
            FlowTimeline::new(self.items.clone(), self.state.clone())
                .graph(graph())
                .focus_label(Some("api ↔ web".into()))
                .now(1_000)
                .on_clear_focus(move |_, _| log(&clear, "clear"))
                .on_select_agent(move |id, _, _| log(&select, format!("select {id}")))
                .on_resolve_finding(move |id, _, _| log(&resolve, format!("resolve {id}"))),
        )
    }
}

fn item(key: &str, actor: Option<&str>, target: Option<&str>, kind: TimelineKind) -> TimelineItem {
    TimelineItem {
        key: key.to_owned(),
        at: 900,
        actor: actor.map(str::to_owned),
        target: target.map(str::to_owned),
        kind,
        body: format!("{key} body"),
    }
}

#[gpui::test]
fn timeline_resolves_findings_and_clears_the_focus(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let events: Log = Rc::default();
    let host_log = events.clone();
    let items = vec![
        item(
            "stage-0",
            None,
            None,
            TimelineKind::Stage {
                from: LoopStage::Test,
                to: LoopStage::Write,
                passed: false,
                iteration: 2,
            },
        ),
        item(
            "message-1",
            Some("writer-api"),
            Some("writer-web"),
            TimelineKind::Message(MessageKind::Proposal),
        ),
        item(
            "finding-7",
            Some("helper-tests-1"),
            None,
            TimelineKind::Finding {
                id: 7,
                severity: FindingSeverity::Error,
                resolved: false,
            },
        ),
        item(
            "finding-8",
            Some("helper-tests-1"),
            None,
            TimelineKind::Finding {
                id: 8,
                severity: FindingSeverity::Info,
                resolved: true,
            },
        ),
    ];
    let (_host, cx) = cx.add_window_view(move |_, _| TimelineHost {
        items,
        state: ListState::new(0, ListAlignment::Top, px(200.0)),
        log: host_log,
    });
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("timeline-resolve-8").is_none(),
        "a resolved finding has no Resolve"
    );
    assert!(click(cx, "timeline-resolve-7"), "Resolve is drawn");
    assert!(
        click(cx, "timeline-target-message-1"),
        "the target is drawn"
    );
    assert!(click(cx, "timeline-clear-focus"), "the focus chip is drawn");
    assert_eq!(
        *events.borrow(),
        [
            "resolve 7".to_owned(),
            "select writer-web".to_owned(),
            "clear".to_owned()
        ]
    );
}

#[gpui::test]
fn an_empty_timeline_has_no_list(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let (_host, cx) = cx.add_window_view(move |_, _| TimelineHost {
        items: Vec::new(),
        state: ListState::new(0, ListAlignment::Top, px(200.0)),
        log: Rc::default(),
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("timeline-clear-focus").is_some());
    assert!(cx.debug_bounds("timeline-contract").is_none());
}
