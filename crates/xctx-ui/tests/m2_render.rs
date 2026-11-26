//! The Milestone 2 components in GPUI's headless test window: the Inbox panel, the Plan Gate card, the Parent
//! Agent drawer and the status bar's Inbox count, clicked through their buttons.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, IntoElement, Modifiers, Render, SharedString, TestAppContext, VisualTestContext,
    Window, div, prelude::*,
};
use xctx_layout::Shape;
use xctx_ui::{
    AgentStatus, InboxEntry, InboxKind, InboxPanel, ParentAgentDrawer, PlanGateCard, PlanGateView,
    PlanRepo, StatusBar, Theme,
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

struct InboxHost {
    entries: Vec<InboxEntry>,
    expanded: Rc<RefCell<Option<SharedString>>>,
    log: Log,
}

impl Render for InboxHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let actions = self.log.clone();
        let toggles = self.log.clone();
        let expanded = self.expanded.clone();
        let entity = cx.entity().downgrade();
        div().size_full().p_4().child(
            InboxPanel::new()
                .entries(self.entries.clone())
                .expanded(self.expanded.borrow().clone())
                .on_action(move |id, action, _, _| log(&actions, format!("{id} {action:?}")))
                .on_toggle(move |id, _, cx| {
                    log(&toggles, format!("{id} toggle"));
                    let mut open = expanded.borrow_mut();
                    *open = if open.as_ref() == Some(id) {
                        None
                    } else {
                        Some(id.clone())
                    };
                    entity.update(cx, |_, cx| cx.notify()).ok();
                }),
        )
    }
}

fn entries() -> Vec<InboxEntry> {
    vec![
        InboxEntry::new("p1", InboxKind::PlanGate, "Launch \"rate limit\" (2 repos)")
            .detail("Task: add rate limiting\n- api: name matches"),
        InboxEntry::new(
            "w1",
            InboxKind::SpaceWiden,
            "Widen rate-limit: switch to production",
        )
        .space("rate-limit")
        .production(true),
        InboxEntry::new("e1", InboxKind::MissingEnvVar, "api needs STRIPE_KEY")
            .detail("Get it from 1Password")
            .space("rate-limit"),
        InboxEntry::new("n1", InboxKind::Notice, "tests are green").detail("42 passed"),
    ]
}

#[gpui::test]
fn inbox_actions_report_the_entry_and_what_was_chosen(cx: &mut TestAppContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| InboxHost {
        entries: entries(),
        expanded: Rc::default(),
        log: host_log,
    });
    cx.run_until_parked();

    for selector in [
        "inbox-review-p1",
        "inbox-approve-w1",
        "inbox-reject-w1",
        "inbox-resolve-e1",
        "inbox-dismiss-e1",
        "inbox-dismiss-n1",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    // Gates cannot be dismissed, and notices cannot be approved.
    assert!(cx.debug_bounds("inbox-dismiss-p1").is_none());
    assert!(cx.debug_bounds("inbox-dismiss-w1").is_none());
    assert!(cx.debug_bounds("inbox-approve-n1").is_none());

    assert_eq!(
        *events.borrow(),
        [
            "p1 Review",
            "w1 Approve",
            "w1 Reject",
            "e1 Resolve",
            "e1 Dismiss",
            "n1 Dismiss"
        ]
    );
}

#[gpui::test]
fn clicking_a_row_with_detail_toggles_it_and_buttons_do_not(cx: &mut TestAppContext) {
    let events: Log = Rc::default();
    let expanded: Rc<RefCell<Option<SharedString>>> = Rc::default();
    let (host_log, host_expanded) = (events.clone(), expanded.clone());
    let (_host, cx) = cx.add_window_view(move |_, _| InboxHost {
        entries: entries(),
        expanded: host_expanded,
        log: host_log,
    });
    cx.run_until_parked();

    let before = cx.debug_bounds("inbox-row-n1").map(|b| b.origin.y);
    assert!(click(cx, "inbox-row-e1"));
    assert_eq!(expanded.borrow().as_deref(), Some("e1"));
    cx.run_until_parked();
    let after = cx.debug_bounds("inbox-row-n1").map(|b| b.origin.y);
    assert!(after > before, "the detail pushes later rows down");

    // An action button does not also toggle its row.
    assert!(click(cx, "inbox-dismiss-n1"));
    assert_eq!(expanded.borrow().as_deref(), Some("e1"));
    // A row without detail does not toggle.
    assert!(click(cx, "inbox-row-w1"));
    assert_eq!(*events.borrow(), ["e1 toggle", "n1 Dismiss"]);
}

#[gpui::test]
fn a_danger_gate_is_approved_only_from_its_expanded_detail(cx: &mut TestAppContext) {
    let events: Log = Rc::default();
    let expanded: Rc<RefCell<Option<SharedString>>> = Rc::default();
    let (host_log, host_expanded) = (events.clone(), expanded.clone());
    let (_host, cx) = cx.add_window_view(move |_, _| InboxHost {
        entries: vec![
            InboxEntry::new("w2", InboxKind::SpaceWiden, "Widen to production")
                .production(true)
                .danger(true),
        ],
        expanded: host_expanded,
        log: host_log,
    });
    cx.run_until_parked();

    // Collapsed: Deny and Review, no Approve.
    assert!(cx.debug_bounds("inbox-approve-w2").is_none());
    assert!(cx.debug_bounds("inbox-reject-w2").is_some());
    assert!(click(cx, "inbox-open-w2"));
    assert_eq!(expanded.borrow().as_deref(), Some("w2"));
    cx.run_until_parked();

    // Expanded: Approve is there, as its own click.
    assert!(cx.debug_bounds("inbox-open-w2").is_none());
    assert!(click(cx, "inbox-approve-w2"));
    assert_eq!(*events.borrow(), ["w2 toggle", "w2 Approve"]);
}

struct EmptyInboxHost;

impl Render for EmptyInboxHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(InboxPanel::new())
    }
}

#[gpui::test]
fn an_empty_inbox_draws(cx: &mut TestAppContext) {
    let (_host, cx) = cx.add_window_view(|_, _| EmptyInboxHost);
    cx.run_until_parked();
}

// ---------------------------------------------------------------------------------------------------------------

struct PlanHost {
    plan: PlanGateView,
    busy: bool,
    log: Log,
}

impl Render for PlanHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (shape, drop, approve, reject, later) = (
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
        );
        div().size_full().p_4().child(
            PlanGateCard::new(self.plan.clone())
                .busy(self.busy)
                .add_repo(div().child("add a repo"))
                .on_shape(move |s, _, _| log(&shape, format!("shape {s:?}")))
                .on_drop_repo(move |path, _, _| log(&drop, format!("drop {path}")))
                .on_approve(move |_, _| log(&approve, "approve"))
                .on_reject(move |_, _| log(&reject, "reject"))
                .on_close(move |_, _| log(&later, "later")),
        )
    }
}

fn plan(repos: &[&str]) -> PlanGateView {
    PlanGateView {
        id: "inbox-4".into(),
        title: "add rate limiting to".into(),
        task: "add rate limiting to the api".into(),
        shape: Shape::Dev,
        repos: repos
            .iter()
            .map(|name| PlanRepo {
                name: (*name).to_owned().into(),
                path: format!("/code/{name}").into(),
                reason: "name matches".into(),
            })
            .collect(),
        rationale: Some("the limiter belongs in the api".into()),
        proposed_by: "the Parent Agent".into(),
    }
}

fn plan_host(
    cx: &mut TestAppContext,
    plan: PlanGateView,
    busy: bool,
) -> (Log, &mut VisualTestContext) {
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| PlanHost {
        plan,
        busy,
        log: host_log,
    });
    cx.run_until_parked();
    (events, cx)
}

#[gpui::test]
fn a_plan_is_edited_then_approved_or_rejected(cx: &mut TestAppContext) {
    let (events, cx) = plan_host(cx, plan(&["api", "web"]), false);
    for selector in [
        "plan-shape-Debug",
        "plan-drop-1",
        "plan-later",
        "plan-reject",
        "plan-approve",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(
        *events.borrow(),
        [
            "shape Debug",
            "drop /code/web",
            "later",
            "reject",
            "approve"
        ]
    );
}

#[gpui::test]
fn a_plan_without_repos_cannot_be_approved(cx: &mut TestAppContext) {
    let (events, cx) = plan_host(cx, plan(&[]), false);
    assert!(click(cx, "plan-approve"));
    assert!(click(cx, "plan-reject"));
    assert_eq!(*events.borrow(), ["reject"]);
}

#[gpui::test]
fn a_launching_plan_ignores_everything_but_later(cx: &mut TestAppContext) {
    let (events, cx) = plan_host(cx, plan(&["api"]), true);
    for selector in [
        "plan-shape-Review",
        "plan-drop-0",
        "plan-reject",
        "plan-approve",
        "plan-later",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(*events.borrow(), ["later"]);
}

// ---------------------------------------------------------------------------------------------------------------

struct DrawerHost {
    status: AgentStatus,
    log: Log,
}

impl Render for DrawerHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let restart = self.log.clone();
        div().size_full().p_4().child(
            ParentAgentDrawer::new(self.status.clone())
                .body(div().debug_selector(|| "parent-body".into()).size_full())
                .on_restart(move |_, _| log(&restart, "restart")),
        )
    }
}

fn drawer(cx: &mut TestAppContext, status: AgentStatus) -> (Log, &mut VisualTestContext) {
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| DrawerHost {
        status,
        log: host_log,
    });
    cx.run_until_parked();
    (events, cx)
}

#[gpui::test]
fn a_running_agent_shows_its_terminal_and_offers_no_restart(cx: &mut TestAppContext) {
    let (_events, cx) = drawer(cx, AgentStatus::Running);
    assert!(cx.debug_bounds("parent-body").is_some());
    assert!(cx.debug_bounds("parent-restart").is_none());
}

#[gpui::test]
fn a_stopped_agent_can_be_restarted(cx: &mut TestAppContext) {
    let (events, cx) = drawer(cx, AgentStatus::Exited(Some(1)));
    assert!(click(cx, "parent-restart"));
    assert_eq!(*events.borrow(), ["restart"]);
}

#[gpui::test]
fn an_unavailable_agent_says_why_instead_of_a_terminal(cx: &mut TestAppContext) {
    let (_events, cx) = drawer(cx, AgentStatus::Unavailable("omp was not found".into()));
    assert!(cx.debug_bounds("parent-body").is_none());
    assert!(cx.debug_bounds("parent-restart").is_some());
}

// ---------------------------------------------------------------------------------------------------------------

struct StatusHost {
    pending: usize,
    log: Log,
}

impl Render for StatusHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let inbox = self.log.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(div().flex_1())
            .child(
                StatusBar::new()
                    .inbox(self.pending)
                    .on_inbox(move |_, _| log(&inbox, "inbox")),
            )
    }
}

#[gpui::test]
fn the_status_bar_inbox_count_opens_the_inbox(cx: &mut TestAppContext) {
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| StatusHost {
        pending: 3,
        log: host_log,
    });
    cx.run_until_parked();
    assert!(click(cx, "status-inbox"));
    assert_eq!(*events.borrow(), ["inbox"]);
}
