//! The Milestone 5 tool dock in GPUI's headless test window: a click opens a closed tool and closes an open one, a
//! tool with no usable default asks in a popover, a right-click or the hover chevron opens the popover, a locked
//! tool shows its lock, the problems item lists the manifests that could not be read, and nothing reacts while busy.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, Entity, IntoElement, Modifiers, MouseButton, Render, SharedString, TestAppContext,
    VisualTestContext, Window, div, prelude::*,
};
use xctx_ui::{
    DockItem, IconName, PromptInput, TOOL_PROBLEM_LIMIT, Theme, ToolDock, ToolDockView, ToolGroup,
    ToolProblem, ToolSetting,
};

type Log = Rc<RefCell<Vec<String>>>;

fn click(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    let Some(bounds) = cx.debug_bounds(selector) else {
        return false;
    };
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
    true
}

fn right_click(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    let Some(bounds) = cx.debug_bounds(selector) else {
        return false;
    };
    cx.simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    true
}

fn drawn(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
}

/// A host like the app's: it opens a popover with the setting's default in its input, opens tools with what the
/// input holds, and toggles the problems list.
struct DockHost {
    view: ToolDockView,
    input: Entity<PromptInput>,
    log: Log,
}

impl Render for DockHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let (open, close) = (self.log.clone(), self.log.clone());
        let host = cx.entity().downgrade();
        let (submit_host, problems_host) = (host.clone(), host.clone());
        let mut dock = ToolDock::new(self.view.clone())
            .on_open(move |name, _, _| open.borrow_mut().push(format!("open {name}")))
            .on_close(move |name, _, _| close.borrow_mut().push(format!("close {name}")))
            .on_popover(move |name, _, cx| {
                let name = name.cloned();
                let _ = host.update(cx, |host, cx| host.popover(name, cx));
            })
            .on_submit(move |name, _, cx| {
                let name = name.clone();
                let _ = submit_host.update(cx, |host, cx| {
                    let value = host.input.read(cx).value().to_owned();
                    host.log.borrow_mut().push(format!("open {name}: {value}"));
                });
            })
            .on_problems(move |open, _, cx| {
                let _ = problems_host.update(cx, |host, cx| {
                    host.log.borrow_mut().push(format!("problems {open}"));
                    host.view.problems_open = open;
                    cx.notify();
                });
            });
        if self.view.popover.is_some() {
            dock = dock.setting_input(self.input.clone());
        }
        div().size_full().flex().justify_end().child(dock)
    }
}

impl DockHost {
    fn popover(&mut self, name: Option<SharedString>, cx: &mut Context<Self>) {
        let default = name.as_ref().and_then(|name| {
            self.view
                .items
                .iter()
                .find(|item| &item.name == name)
                .and_then(|item| item.setting.as_ref())
                .map(|s| s.default.to_string())
        });
        self.log.borrow_mut().push(match &name {
            Some(name) => format!("popover {name}"),
            None => "popover closed".to_owned(),
        });
        self.view.popover = name;
        if let Some(default) = default {
            self.input
                .update(cx, |input, cx| input.set_value(&default, cx));
        }
        cx.notify();
    }
}

fn item(name: &str, group: ToolGroup) -> DockItem {
    DockItem {
        name: name.to_owned().into(),
        title: name.to_uppercase().into(),
        detail: Some(format!("the {name} tool").into()),
        icon: IconName::Plugin,
        group,
        open: false,
        locked: false,
        setting: None,
        needs_input: false,
    }
}

fn with_setting(mut item: DockItem, default: &str) -> DockItem {
    item.setting = Some(ToolSetting {
        label: "Command".into(),
        default: default.to_owned().into(),
        hint: Some("blank for your shell".into()),
    });
    item.needs_input = default.is_empty();
    item
}

fn dock_view() -> ToolDockView {
    let mut browser = item("browser", ToolGroup::Panes);
    browser.open = true;
    let mut seed = item("seed", ToolGroup::Other);
    seed.locked = true;
    ToolDockView {
        items: vec![
            with_setting(item("terminal", ToolGroup::Panes), "npm run dev"),
            with_setting(item("logs", ToolGroup::Panes), ""),
            browser,
            item("files", ToolGroup::Panes),
            item("database", ToolGroup::Tools),
            item("jira", ToolGroup::Connectors),
            seed,
        ],
        problems: vec![ToolProblem {
            path: "/repo/.xctx/plugins/bad/plugin.toml".into(),
            message: "missing field `name`".into(),
        }],
        popover: None,
        problems_open: false,
        busy: false,
    }
}

fn dock_host(
    cx: &mut TestAppContext,
    view: ToolDockView,
) -> (Log, Entity<DockHost>, &mut VisualTestContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let events: Log = Rc::default();
    let log = events.clone();
    let (host, cx) = cx.add_window_view(move |_, cx| DockHost {
        view,
        input: cx.new(PromptInput::new),
        log,
    });
    cx.run_until_parked();
    (events, host, cx)
}

#[gpui::test]
fn a_click_opens_a_closed_tool_and_closes_an_open_one(cx: &mut TestAppContext) {
    let (events, _host, cx) = dock_host(cx, dock_view());
    for selector in [
        "tool-dock",
        "tool-open-browser",
        "tool-divider-0",
        "tool-divider-1",
        "tool-divider-2",
    ] {
        assert!(drawn(cx, selector), "{selector} is drawn");
    }
    // Only the open tool shows the dot; four groups, three dividers.
    assert!(!drawn(cx, "tool-open-files"));
    assert!(!drawn(cx, "tool-divider-3"));
    for selector in ["tool-files", "tool-browser", "tool-terminal", "tool-jira"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(
        *events.borrow(),
        ["open files", "close browser", "open terminal", "open jira"]
    );
}

#[gpui::test]
fn a_tool_with_no_default_asks_in_a_popover(cx: &mut TestAppContext) {
    let (events, host, cx) = dock_host(cx, dock_view());
    assert!(!drawn(cx, "tool-popover-logs"));
    assert!(click(cx, "tool-logs"));
    assert!(drawn(cx, "tool-popover-logs"));
    assert!(drawn(cx, "tool-setting-logs"));
    host.update(cx, |host, cx| {
        host.input
            .update(cx, |input, cx| input.set_value("tail -F log/app.log", cx))
    });
    cx.run_until_parked();
    assert!(click(cx, "tool-submit-logs"));
    assert_eq!(
        *events.borrow(),
        ["popover logs", "open logs: tail -F log/app.log"]
    );
}

#[gpui::test]
fn a_right_click_or_the_chevron_opens_the_popover(cx: &mut TestAppContext) {
    let (events, _host, cx) = dock_host(cx, dock_view());
    // Only tools with a setting have the chevron.
    assert!(drawn(cx, "tool-more-terminal"));
    assert!(!drawn(cx, "tool-more-files"));
    assert!(right_click(cx, "tool-terminal"));
    assert!(drawn(cx, "tool-popover-terminal"));
    // It opens with the default; Open submits it.
    assert!(click(cx, "tool-submit-terminal"));
    // A click outside closes it.
    assert!(click(cx, "tool-database"));
    assert!(!drawn(cx, "tool-popover-terminal"));
    assert!(click(cx, "tool-more-terminal"));
    assert!(drawn(cx, "tool-popover-terminal"));
    // A right-click on a tool with no setting does nothing.
    assert!(right_click(cx, "tool-files"));
    assert_eq!(
        *events.borrow(),
        [
            "popover terminal",
            "open terminal: npm run dev",
            "popover closed",
            "open database",
            "popover terminal",
            "popover closed",
        ]
    );
}

#[gpui::test]
fn an_untrusted_tool_shows_a_lock_and_still_asks_on_click(cx: &mut TestAppContext) {
    let (events, _host, cx) = dock_host(cx, dock_view());
    assert!(drawn(cx, "tool-lock-seed"));
    assert!(!drawn(cx, "tool-lock-files"));
    assert!(click(cx, "tool-seed"));
    assert_eq!(*events.borrow(), ["open seed"]);
}

#[gpui::test]
fn the_problems_item_lists_what_could_not_be_loaded(cx: &mut TestAppContext) {
    let mut view = dock_view();
    view.problems = (0..TOOL_PROBLEM_LIMIT + 1)
        .map(|i| ToolProblem {
            path: format!("/p{i}/plugin.toml").into(),
            message: "bad".into(),
        })
        .collect();
    let (events, host, cx) = dock_host(cx, view);
    assert!(drawn(cx, "tool-problems"));
    assert!(!drawn(cx, "tool-problems-list"));
    assert!(click(cx, "tool-problems"));
    assert!(drawn(cx, "tool-problems-list"));
    assert!(drawn(cx, "tool-problem-0"));
    assert!(drawn(cx, "tool-problems-more"));
    assert!(click(cx, "tool-problems"));
    assert!(!drawn(cx, "tool-problems-list"));
    assert_eq!(*events.borrow(), ["problems true", "problems false"]);
    // No problems, no item.
    host.update(cx, |host, cx| {
        host.view.problems.clear();
        cx.notify();
    });
    cx.run_until_parked();
    assert!(!drawn(cx, "tool-problems"));
}

#[gpui::test]
fn a_busy_dock_does_nothing(cx: &mut TestAppContext) {
    let mut view = dock_view();
    view.busy = true;
    let (events, _host, cx) = dock_host(cx, view);
    for selector in [
        "tool-files",
        "tool-browser",
        "tool-logs",
        "tool-seed",
        "tool-problems",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert!(right_click(cx, "tool-terminal"));
    assert!(!drawn(cx, "tool-more-terminal"));
    assert!(events.borrow().is_empty());
}
