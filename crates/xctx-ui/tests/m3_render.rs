//! The Milestone 3 components in GPUI's headless test window: the Agent Graph panel, the Shared Space panel, the
//! loop bar and the title bar's Graph toggle, clicked through their buttons.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, IntoElement, Modifiers, Render, TestAppContext, VisualTestContext, Window, div,
    prelude::*,
};
use xctx_channel::{ChannelMessage, Contract, Finding, FindingSeverity, MessageKind, Note};
use xctx_graph::{
    AgentGraph, AgentNodeId, AgentRole, AgentState, AutonomyLevel, HelperKind, LoopRun,
};
use xctx_layout::Shape;
use xctx_ui::{
    AgentGraphView, LoopBar, SharedSpacePanel, SharedTab, SpaceHeaderInfo, Theme, TitleBar,
};

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

// ---------------------------------------------------------------------------------------------------------------

struct GraphHost {
    graph: AgentGraph,
    log: Log,
}

impl Render for GraphHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (pause, resume, talk, remove, add) = (
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
        );
        div().size_full().p_4().child(
            AgentGraphView::new(self.graph.clone())
                .on_pause(move |id, _, _| log(&pause, format!("pause {id}")))
                .on_resume(move |id, _, _| log(&resume, format!("resume {id}")))
                .on_talk(move |id, _, _| log(&talk, format!("talk {id}")))
                .on_remove(move |id, _, _| log(&remove, format!("remove {id}")))
                .on_add_helper(move |kind, _, _| log(&add, format!("add {kind:?}"))),
        )
    }
}

/// An orchestrator bound to a pane, a running writer for `api` bound to a pane, a paused writer for `web`, and a
/// running tests helper bound to a pane.
fn graph() -> AgentGraph {
    let mut graph = AgentGraph::for_space(&["api".to_owned(), "web".to_owned()]);
    let orchestrator = AgentNodeId::new("orchestrator");
    let api = AgentNodeId::new("writer-api");
    let web = AgentNodeId::new("writer-web");
    let tests = graph
        .add_lane(
            AgentRole::Helper {
                kind: HelperKind::Tests,
            },
            None,
        )
        .expect("a tests helper can be added");
    assert_eq!(tests.as_str(), "helper-tests-1");
    for (id, state, pane) in [
        (&orchestrator, AgentState::Running, Some(1)),
        (&api, AgentState::Running, Some(2)),
        (&web, AgentState::Paused, None),
        (&tests, AgentState::Running, Some(3)),
    ] {
        graph
            .set_state(id, state)
            .expect("the agent is in the graph");
        graph
            .bind_pane(id, pane)
            .expect("the agent is in the graph");
    }
    graph.record_turn(&api).expect("the agent is in the graph");
    graph
}

#[gpui::test]
fn agent_graph_buttons_report_the_agent_and_what_was_chosen(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| GraphHost {
        graph: graph(),
        log: host_log,
    });
    cx.run_until_parked();

    for selector in [
        "graph-pause-orchestrator",
        "graph-talk-orchestrator",
        "graph-pause-writer-api",
        "graph-talk-writer-api",
        "graph-resume-writer-web",
        "graph-pause-helper-tests-1",
        "graph-talk-helper-tests-1",
        "graph-remove-helper-tests-1",
        "graph-add-tests",
        "graph-add-reviewer",
        "graph-add-probe",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    // The orchestrator and writers cannot be removed, a paused agent is not paused again, a running one is not
    // resumed, and an agent with no pane cannot be talked to.
    assert!(cx.debug_bounds("graph-remove-orchestrator").is_none());
    assert!(cx.debug_bounds("graph-remove-writer-api").is_none());
    assert!(cx.debug_bounds("graph-remove-writer-web").is_none());
    assert!(cx.debug_bounds("graph-pause-writer-web").is_none());
    assert!(cx.debug_bounds("graph-resume-writer-api").is_none());
    assert!(cx.debug_bounds("graph-talk-writer-web").is_none());

    assert_eq!(
        *events.borrow(),
        [
            "pause orchestrator",
            "talk orchestrator",
            "pause writer-api",
            "talk writer-api",
            "resume writer-web",
            "pause helper-tests-1",
            "talk helper-tests-1",
            "remove helper-tests-1",
            "add Tests",
            "add Reviewer",
            "add Probe",
        ]
    );
}

#[gpui::test]
fn agent_graph_tiers_stack_orchestrator_writers_then_helpers(cx: &mut TestAppContext) {
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| GraphHost {
        graph: graph(),
        log: host_log,
    });
    cx.run_until_parked();

    let top = |cx: &mut VisualTestContext, selector| {
        cx.debug_bounds(selector)
            .map(|bounds| bounds.origin.y)
            .expect("the card is drawn")
    };
    let orchestrator = top(cx, "graph-card-orchestrator");
    let api = top(cx, "graph-card-writer-api");
    let web = top(cx, "graph-card-writer-web");
    let tests = top(cx, "graph-card-helper-tests-1");
    assert!(orchestrator < api, "writers sit under the orchestrator");
    assert_eq!(api, web, "writers share a row");
    assert!(api < tests, "helpers sit under the writers");
}

struct ForkGraphHost {
    log: Log,
}

impl Render for ForkGraphHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let fork = self.log.clone();
        let notes = [(
            AgentNodeId::new("writer-api"),
            "attempts: #1 adopted, #2 discarded".into(),
        )]
        .into_iter()
        .collect();
        div().size_full().p_4().child(
            AgentGraphView::new(graph())
                .notes(notes)
                .on_fork(move |id, _, _| log(&fork, format!("fork {id}"))),
        )
    }
}

#[gpui::test]
fn writers_offer_fork_and_show_their_attempt_lineage(cx: &mut TestAppContext) {
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| ForkGraphHost { log: host_log });
    cx.run_until_parked();
    assert!(click(cx, "graph-fork-writer-web"));
    assert!(click(cx, "graph-fork-writer-api"));
    assert!(cx.debug_bounds("graph-fork-orchestrator").is_none());
    assert!(cx.debug_bounds("graph-fork-helper-tests-1").is_none());
    assert!(cx.debug_bounds("graph-note-writer-api").is_some());
    assert!(cx.debug_bounds("graph-note-writer-web").is_none());
    assert_eq!(*events.borrow(), ["fork writer-web", "fork writer-api"]);
}

#[gpui::test]
fn without_a_fork_handler_no_fork_button_is_drawn(cx: &mut TestAppContext) {
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| GraphHost {
        graph: graph(),
        log: host_log,
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("graph-fork-writer-api").is_none());
}

struct EmptyGraphHost;

impl Render for EmptyGraphHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .child(AgentGraphView::new(AgentGraph::for_space(&[])))
    }
}

#[gpui::test]
fn a_graph_with_only_an_orchestrator_draws_and_still_offers_helpers(cx: &mut TestAppContext) {
    let (_host, cx) = cx.add_window_view(|_, _| EmptyGraphHost);
    cx.run_until_parked();
    assert!(cx.debug_bounds("graph-card-orchestrator").is_some());
    assert!(cx.debug_bounds("graph-add-probe").is_some());
}

// ---------------------------------------------------------------------------------------------------------------

struct SharedHost {
    tab: Rc<RefCell<SharedTab>>,
    contract: Option<Contract>,
    log: Log,
}

impl Render for SharedHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (tabs, resolves) = (self.log.clone(), self.log.clone());
        let tab = self.tab.clone();
        let entity = cx.entity().downgrade();
        div().size_full().p_4().child(
            SharedSpacePanel::new()
                .tab(*self.tab.borrow())
                .contract(self.contract.clone())
                .messages(messages())
                .findings(findings())
                .notes([Note {
                    id: 1,
                    author: "orchestrator".into(),
                    body: "staging uses the v2 contract".into(),
                    created_at: 3,
                }])
                .on_tab(move |next, _, cx| {
                    log(&tabs, format!("tab {next:?}"));
                    *tab.borrow_mut() = next;
                    entity.update(cx, |_, cx| cx.notify()).ok();
                })
                .on_resolve_finding(move |id, _, _| log(&resolves, format!("resolve {id}"))),
        )
    }
}

fn contract() -> Contract {
    Contract {
        version: 2,
        title: "RateLimited event".into(),
        summary: "api emits it, web shows a retry banner".into(),
        spec: "{\n  \"retry_after\": u32\n}".into(),
        author: "writer-api".into(),
        agreed_by: vec!["writer-api".into(), "writer-web".into()],
        updated_at: 2,
    }
}

fn messages() -> Vec<ChannelMessage> {
    vec![
        ChannelMessage {
            id: 1,
            from: "writer-api".into(),
            to: Some("writer-web".into()),
            kind: MessageKind::Proposal,
            body: "add retry_after to the event".into(),
            created_at: 1,
        },
        ChannelMessage {
            id: 2,
            from: "writer-web".into(),
            to: None,
            kind: MessageKind::ContractUpdate,
            body: "agreed to v2".into(),
            created_at: 2,
        },
    ]
}

fn findings() -> Vec<Finding> {
    vec![
        Finding {
            id: 7,
            author: "helper-tests-1".into(),
            severity: FindingSeverity::Error,
            repo: Some("api".into()),
            file: Some("src/limit.rs".into()),
            summary: "retry_after is missing on 429".into(),
            resolved: false,
            created_at: 1,
        },
        Finding {
            id: 8,
            author: "helper-reviewer-1".into(),
            severity: FindingSeverity::Info,
            repo: None,
            file: None,
            summary: "naming is consistent".into(),
            resolved: true,
            created_at: 2,
        },
    ]
}

fn shared_host(
    cx: &mut TestAppContext,
    contract: Option<Contract>,
) -> (Log, Rc<RefCell<SharedTab>>, &mut VisualTestContext) {
    let events: Log = Rc::default();
    let tab: Rc<RefCell<SharedTab>> = Rc::default();
    let (host_log, host_tab) = (events.clone(), tab.clone());
    let (_host, cx) = cx.add_window_view(move |_, _| SharedHost {
        tab: host_tab,
        contract,
        log: host_log,
    });
    cx.run_until_parked();
    (events, tab, cx)
}

#[gpui::test]
fn shared_space_tabs_switch_and_findings_resolve(cx: &mut TestAppContext) {
    let (events, tab, cx) = shared_host(cx, Some(contract()));
    // The Contract tab is shown first; the channel and findings are not drawn.
    assert!(cx.debug_bounds("shared-message-1").is_none());
    assert!(cx.debug_bounds("shared-finding-7").is_none());

    assert!(click(cx, "shared-tab-channel"));
    assert_eq!(*tab.borrow(), SharedTab::Channel);
    cx.run_until_parked();
    let newest = cx.debug_bounds("shared-message-2").map(|b| b.origin.y);
    let oldest = cx.debug_bounds("shared-message-1").map(|b| b.origin.y);
    assert!(newest.is_some() && oldest.is_some());
    assert!(newest < oldest, "the newest message comes first");

    assert!(click(cx, "shared-tab-findings"));
    assert_eq!(*tab.borrow(), SharedTab::FindingsNotes);
    cx.run_until_parked();
    assert!(cx.debug_bounds("shared-message-1").is_none());
    assert!(cx.debug_bounds("shared-finding-8").is_some());
    // A resolved finding offers no Resolve.
    assert!(cx.debug_bounds("shared-resolve-8").is_none());
    assert!(click(cx, "shared-resolve-7"));

    assert!(click(cx, "shared-tab-contract"));
    assert_eq!(
        *events.borrow(),
        [
            "tab Channel",
            "tab FindingsNotes",
            "resolve 7",
            "tab Contract"
        ]
    );
}

#[gpui::test]
fn shared_space_without_a_contract_draws_its_empty_state(cx: &mut TestAppContext) {
    let (events, _tab, cx) = shared_host(cx, None);
    assert!(cx.debug_bounds("shared-tab-contract").is_some());
    assert!(click(cx, "shared-tab-contract"));
    assert_eq!(*events.borrow(), ["tab Contract"]);
}

// ---------------------------------------------------------------------------------------------------------------

struct LoopHost {
    run: LoopRun,
    log: Log,
}

impl Render for LoopHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (start, advance, approve, reset, autonomy) = (
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
        );
        div().size_full().p_4().child(
            LoopBar::new(self.run.clone())
                .budget(2, 20)
                .on_start(move |_, _| log(&start, "start"))
                .on_advance(move |_, _| log(&advance, "advance"))
                .on_approve_next(move |_, _| log(&approve, "approve"))
                .on_reset(move |_, _| log(&reset, "reset"))
                .on_autonomy(move |level, _, _| log(&autonomy, format!("autonomy {level:?}"))),
        )
    }
}

/// The width of the app's agent graph panel, which the loop bar sits in.
const GRAPH_PANEL_WIDTH: f32 = 560.0;

struct NarrowLoopHost {
    run: LoopRun,
}

impl Render for NarrowLoopHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w(gpui::px(GRAPH_PANEL_WIDTH))
            .child(LoopBar::new(self.run.clone()).budget(10, 400))
    }
}

/// Whether the element named `selector` is drawn entirely within the graph panel's width.
fn fits_the_panel(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector)
        .is_some_and(|b| b.origin.x + b.size.width <= gpui::px(GRAPH_PANEL_WIDTH))
}

#[gpui::test]
fn the_loop_bar_actions_fit_the_graph_panel(cx: &mut TestAppContext) {
    // A long status badge and a budget counter, with the widest action row (Reset and Start).
    let mut blocked = LoopRun::new(AutonomyLevel::WithinBudget {
        max_iterations: 10,
        max_turns: 400,
    });
    blocked.start().expect("an idle loop starts");
    blocked.advance(true).expect("a running loop advances");
    blocked.block("the contract tests failed on retry_after in three places");

    let (_host, cx) = cx.add_window_view(move |_, _| NarrowLoopHost { run: blocked });
    cx.run_until_parked();
    for selector in [
        "loop-reset",
        "loop-start",
        "loop-autonomy-ask",
        "loop-autonomy-budget",
        "loop-autonomy-checks",
        "loop-stage-review",
    ] {
        assert!(fits_the_panel(cx, selector), "{selector} fits in 560px");
    }
}

fn loop_host(cx: &mut TestAppContext, run: LoopRun) -> (Log, &mut VisualTestContext) {
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| LoopHost { run, log: host_log });
    cx.run_until_parked();
    (events, cx)
}

#[gpui::test]
fn an_idle_loop_starts_and_picks_its_autonomy(cx: &mut TestAppContext) {
    let (events, cx) = loop_host(cx, LoopRun::new(AutonomyLevel::AskEachStep));
    assert!(cx.debug_bounds("loop-stage-write").is_some());
    assert!(cx.debug_bounds("loop-advance").is_none());
    assert!(cx.debug_bounds("loop-approve").is_none());
    assert!(cx.debug_bounds("loop-reset").is_none());
    for selector in [
        "loop-start",
        "loop-autonomy-ask",
        "loop-autonomy-budget",
        "loop-autonomy-checks",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(
        *events.borrow(),
        [
            "start",
            "autonomy AskEachStep",
            "autonomy WithinBudget { max_iterations: 2, max_turns: 20 }",
            "autonomy UntilChecksPass",
        ]
    );
}

#[gpui::test]
fn a_running_loop_advances(cx: &mut TestAppContext) {
    let mut run = LoopRun::new(AutonomyLevel::UntilChecksPass);
    run.start().expect("an idle loop starts");
    let (events, cx) = loop_host(cx, run);
    assert!(cx.debug_bounds("loop-start").is_none());
    assert!(click(cx, "loop-advance"));
    assert_eq!(*events.borrow(), ["advance"]);
}

#[gpui::test]
fn a_loop_awaiting_approval_is_approved(cx: &mut TestAppContext) {
    let mut run = LoopRun::new(AutonomyLevel::AskEachStep);
    run.start().expect("an idle loop starts");
    run.advance(true).expect("a running loop advances");
    let (events, cx) = loop_host(cx, run);
    assert!(cx.debug_bounds("loop-advance").is_none());
    assert!(click(cx, "loop-approve"));
    assert_eq!(*events.borrow(), ["approve"]);
}

#[gpui::test]
fn a_blocked_loop_resets_or_starts_again(cx: &mut TestAppContext) {
    let mut run = LoopRun::new(AutonomyLevel::UntilChecksPass);
    run.start().expect("an idle loop starts");
    run.block("tests failed");
    let (events, cx) = loop_host(cx, run);
    assert!(click(cx, "loop-reset"));
    assert!(click(cx, "loop-start"));
    assert_eq!(*events.borrow(), ["reset", "start"]);
}

#[gpui::test]
fn a_passed_loop_only_resets(cx: &mut TestAppContext) {
    let mut run = LoopRun::new(AutonomyLevel::UntilChecksPass);
    run.start().expect("an idle loop starts");
    for _ in 0..4 {
        run.advance(true).expect("a running loop advances");
    }
    let (events, cx) = loop_host(cx, run);
    assert!(cx.debug_bounds("loop-start").is_none());
    assert!(click(cx, "loop-reset"));
    assert_eq!(*events.borrow(), ["reset"]);
}

// ---------------------------------------------------------------------------------------------------------------

struct TitleHost {
    log: Log,
}

impl Render for TitleHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let graph = self.log.clone();
        let bar = TitleBar::new("add rate limiting")
            .space(SpaceHeaderInfo {
                shape: Shape::Dev,
                graph_open: true,
                production: false,
                branch: "space/rate-limit".into(),
                env: "rate-limit".into(),
            })
            .on_graph(move |_, _| log(&graph, "graph"));
        div().w(gpui::px(1400.0)).h_full().child(bar)
    }
}

#[gpui::test]
fn the_title_bar_graph_button_toggles_the_graph(cx: &mut TestAppContext) {
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| TitleHost { log: host_log });
    cx.run_until_parked();
    assert!(click(cx, "title-graph"));
    assert!(click(cx, "title-graph"));
    assert_eq!(*events.borrow(), ["graph", "graph"]);
}
