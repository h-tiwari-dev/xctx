//! The Flow view's canvas, agent drawer and layout in GPUI's headless test window: a card per agent at the
//! layout's positions, selection by card, by traffic badge and by empty canvas, double-click to open, the drawer's
//! Start/Open choice, and the canvas toolbar.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gpui::{
    Context, IntoElement, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Render,
    TestAppContext, VisualTestContext, Window, div, point, prelude::*, px,
};
use xctx_graph::{AgentGraph, AgentNodeId, AgentRole, AgentState, HelperKind, LoopStage};
use xctx_nav::flow::{FlowFocus, NODE_HEIGHT, pair_key};
use xctx_ui::flow_view::{AgentDetail, FlowCanvas, FlowView};
use xctx_ui::{MotionSettings, Theme};

/// What the hosts' handlers saw, in order.
type Log = Rc<RefCell<Vec<String>>>;

fn push(log: &Log, entry: impl Into<String>) {
    log.borrow_mut().push(entry.into());
}

fn setup(cx: &mut TestAppContext) {
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        MotionSettings::set_reduced(cx, true);
    });
}

/// Click the element named `selector`. False when it was not drawn.
fn click(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    let Some(bounds) = cx.debug_bounds(selector) else {
        return false;
    };
    cx.simulate_click(bounds.center(), Modifiers::default());
    true
}

fn focus_text(focus: &Option<FlowFocus>) -> String {
    match focus {
        None => "select none".to_owned(),
        Some(FlowFocus::Agent(id)) => format!("select {id}"),
        Some(FlowFocus::Pair(a, b)) => format!("select {a}+{b}"),
    }
}

/// The orchestrator, writers for `api` (running) and `web` (paused), and a tests helper.
fn graph() -> AgentGraph {
    let mut graph = AgentGraph::for_space(&["api".to_owned(), "web".to_owned()]);
    let tests = graph.add_lane(
        AgentRole::Helper {
            kind: HelperKind::Tests,
        },
        None,
    );
    assert_eq!(
        tests.as_ref().map(AgentNodeId::as_str).ok(),
        Some("helper-tests-1")
    );
    assert!(
        graph
            .set_state(&AgentNodeId::new("writer-api"), AgentState::Running)
            .is_ok()
    );
    assert!(graph.pause(&AgentNodeId::new("writer-web")).is_ok());
    graph
}

/// A canvas filling the window, recording every selection and open.
struct CanvasHost {
    focus: Option<FlowFocus>,
    log: Log,
}

impl Render for CanvasHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (select, open) = (self.log.clone(), self.log.clone());
        let traffic: HashMap<(String, String), usize> =
            [(pair_key("orchestrator", "writer-api"), 3)]
                .into_iter()
                .collect();
        div().size_full().child(
            FlowCanvas::new("canvas", graph())
                .traffic(traffic)
                .running_stage(Some(LoopStage::Write))
                .focus(self.focus.clone())
                .notes(
                    [(
                        AgentNodeId::new("writer-api"),
                        "attempts: #1 adopted".into(),
                    )]
                    .into_iter()
                    .collect(),
                )
                .on_select(move |focus, _, _| push(&select, focus_text(&focus)))
                .on_open(move |id, _, _| push(&open, format!("open {id}"))),
        )
    }
}

fn canvas_host(cx: &mut TestAppContext, focus: Option<FlowFocus>) -> (Log, &mut VisualTestContext) {
    setup(cx);
    let log: Log = Rc::default();
    let host_log = log.clone();
    let (_host, vcx) = cx.add_window_view(move |_, _| CanvasHost {
        focus,
        log: host_log,
    });
    vcx.run_until_parked();
    (log, vcx)
}

#[gpui::test]
fn the_canvas_draws_a_card_per_agent_in_its_tier(cx: &mut TestAppContext) {
    let (_log, cx) = canvas_host(cx, None);
    let bounds = |cx: &mut VisualTestContext, selector| {
        cx.debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} is drawn"))
    };
    let orchestrator = bounds(cx, "canvas-node-orchestrator");
    let api = bounds(cx, "canvas-node-writer-api");
    let web = bounds(cx, "canvas-node-writer-web");
    let tests = bounds(cx, "canvas-node-helper-tests-1");
    for card in [orchestrator, api, web, tests] {
        assert_eq!(card.size.height, px(NODE_HEIGHT));
    }
    assert!(orchestrator.origin.y < api.origin.y);
    assert_eq!(api.origin.y, web.origin.y);
    assert!(api.origin.x < web.origin.x);
    assert!(api.origin.y < tests.origin.y);
    assert!(cx.debug_bounds("canvas-note-writer-api").is_some());
}

#[gpui::test]
fn clicking_a_card_selects_its_agent_and_double_clicking_opens_it(cx: &mut TestAppContext) {
    let (log, cx) = canvas_host(cx, None);
    assert!(click(cx, "canvas-node-writer-api"));
    assert_eq!(*log.borrow(), ["select writer-api"]);

    let Some(web) = cx.debug_bounds("canvas-node-writer-web") else {
        panic!("the web writer is drawn");
    };
    let position = web.center();
    cx.simulate_event(MouseDownEvent {
        position,
        modifiers: Modifiers::default(),
        button: MouseButton::Left,
        click_count: 2,
        first_mouse: false,
    });
    cx.simulate_event(MouseUpEvent {
        position,
        modifiers: Modifiers::default(),
        button: MouseButton::Left,
        click_count: 2,
    });
    assert_eq!(
        log.borrow().last().map(String::as_str),
        Some("open writer-web")
    );
}

#[gpui::test]
fn an_edge_with_traffic_has_a_badge_that_selects_the_pair(cx: &mut TestAppContext) {
    let (log, cx) = canvas_host(cx, None);
    assert!(
        cx.debug_bounds("canvas-edge-orchestrator-writer-web")
            .is_none()
    );
    assert!(click(cx, "canvas-edge-orchestrator-writer-api"));
    assert_eq!(*log.borrow(), ["select orchestrator+writer-api"]);
}

#[gpui::test]
fn clicking_empty_canvas_clears_the_selection(cx: &mut TestAppContext) {
    let (log, cx) = canvas_host(cx, Some(FlowFocus::Agent(AgentNodeId::new("writer-api"))));
    cx.simulate_click(point(px(4.0), px(4.0)), Modifiers::default());
    assert_eq!(*log.borrow(), ["select none"]);
    // A card blocks the empty-canvas click under it.
    assert!(click(cx, "canvas-node-orchestrator"));
    assert_eq!(*log.borrow(), ["select none", "select orchestrator"]);
}

/// A drawer for the paused web writer, which has no pane, and the Flow view around a canvas.
struct ViewHost {
    log: Log,
}

impl Render for ViewHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let log = &self.log;
        let graph = graph();
        let web = graph
            .node(&AgentNodeId::new("writer-web"))
            .cloned()
            .unwrap_or_else(|| panic!("the web writer is in the graph"));
        let color = Theme::of(cx).agent_color(1);
        let detail = {
            let (open, start, pause, resume, fork, remove, close) = (
                log.clone(),
                log.clone(),
                log.clone(),
                log.clone(),
                log.clone(),
                log.clone(),
                log.clone(),
            );
            AgentDetail::new(web, color)
                .runner("omp".into())
                .on_open(move |id, _, _| push(&open, format!("open {id}")))
                .on_start(move |id, _, _| push(&start, format!("start {id}")))
                .on_pause(move |id, _, _| push(&pause, format!("pause {id}")))
                .on_resume(move |id, _, _| push(&resume, format!("resume {id}")))
                .on_fork(move |id, _, _| push(&fork, format!("fork {id}")))
                .on_remove(move |id, _, _| push(&remove, format!("remove {id}")))
                .on_close(move |_, _| push(&close, "close"))
        };
        let (select, helper, repo) = (log.clone(), log.clone(), log.clone());
        let canvas = FlowCanvas::new("canvas", graph)
            .focus(Some(FlowFocus::Agent(AgentNodeId::new("writer-web"))))
            .on_select(move |focus, _, _| push(&select, focus_text(&focus)));
        div().size_full().child(
            FlowView::new(div().h(px(48.0)), canvas, div().size_full())
                .detail(Some(detail))
                .on_add_helper(move |kind, _, _| push(&helper, format!("add {kind:?}")))
                .on_add_repo(move |_, _| push(&repo, "add repo")),
        )
    }
}

#[gpui::test]
fn the_drawer_offers_start_for_an_agent_without_a_pane(cx: &mut TestAppContext) {
    setup(cx);
    let log: Log = Rc::default();
    let host_log = log.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| ViewHost { log: host_log });
    cx.run_until_parked();

    assert!(cx.debug_bounds("agent-detail").is_some());
    assert!(cx.debug_bounds("detail-open").is_none());
    // A paused writer resumes; it neither pauses nor is removed.
    assert!(cx.debug_bounds("detail-pause").is_none());
    assert!(cx.debug_bounds("detail-remove").is_none());
    for selector in [
        "detail-start",
        "detail-resume",
        "detail-fork",
        "detail-close",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    // Clicks in the drawer never reach the canvas under it.
    assert_eq!(
        *log.borrow(),
        [
            "start writer-web",
            "resume writer-web",
            "fork writer-web",
            "close"
        ]
    );
}

#[gpui::test]
fn the_canvas_toolbar_adds_helpers_and_repos(cx: &mut TestAppContext) {
    setup(cx);
    let log: Log = Rc::default();
    let host_log = log.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| ViewHost { log: host_log });
    cx.run_until_parked();

    for selector in [
        "flow-add-tests",
        "flow-add-reviewer",
        "flow-add-probe",
        "flow-add-browser",
        "flow-add-repo",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(
        *log.borrow(),
        [
            "add Tests",
            "add Reviewer",
            "add Probe",
            "add Browser",
            "add repo"
        ]
    );
    // The canvas sits below the track and the cards are drawn inside it.
    let Some(orchestrator) = cx.debug_bounds("canvas-node-orchestrator") else {
        panic!("the canvas draws its cards inside the view");
    };
    assert!(orchestrator.origin.y > px(48.0));
}
