//! The Milestone 5 Plugins menu in GPUI's headless test window: mounting and unmounting through its buttons,
//! nothing while busy, its badges, problems, caps and error line.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, Entity, IntoElement, Modifiers, Render, TestAppContext, VisualTestContext, Window,
    div, prelude::*,
};
use xctx_ui::{
    PLUGIN_PROBLEM_LIMIT, PLUGIN_ROW_LIMIT, PluginEntry, PluginMenu, PluginMenuView,
    PluginProblemRow, PluginSource, Theme,
};

type Log = Rc<RefCell<Vec<String>>>;

fn click(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    let Some(bounds) = cx.debug_bounds(selector) else {
        return false;
    };
    cx.simulate_click(bounds.center(), Modifiers::default());
    true
}

fn drawn(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
}

struct MenuHost {
    view: PluginMenuView,
    log: Log,
}

impl Render for MenuHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (mount, unmount, close) = (self.log.clone(), self.log.clone(), self.log.clone());
        div().size_full().child(
            PluginMenu::new(self.view.clone())
                .on_mount(move |name, _, _| mount.borrow_mut().push(format!("mount {name}")))
                .on_unmount(move |name, _, _| unmount.borrow_mut().push(format!("unmount {name}")))
                .on_close(move |_, _| close.borrow_mut().push("close".to_owned())),
        )
    }
}

fn entry(name: &str, source: PluginSource, needs_trust: bool, mounted: bool) -> PluginEntry {
    PluginEntry {
        name: name.to_owned().into(),
        description: format!("the {name} plugin").into(),
        source,
        pane: Some("http".into()),
        needs_trust,
        mounted,
    }
}

fn menu_view() -> PluginMenuView {
    PluginMenuView {
        plugins: vec![
            entry("http", PluginSource::Builtin, false, false),
            entry("logs", PluginSource::Builtin, false, true),
            entry("seed", PluginSource::Repo, true, false),
        ],
        problems: vec![PluginProblemRow {
            path: "/repo/.xctx/plugins/bad/plugin.toml".into(),
            message: "missing field `name`".into(),
        }],
        busy: false,
        error: None,
    }
}

fn menu_host(
    cx: &mut TestAppContext,
    view: PluginMenuView,
) -> (Log, Entity<MenuHost>, &mut VisualTestContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let events: Log = Rc::default();
    let log = events.clone();
    let (host, cx) = cx.add_window_view(move |_, _| MenuHost { view, log });
    cx.run_until_parked();
    (events, host, cx)
}

#[gpui::test]
fn the_plugins_menu_mounts_and_unmounts(cx: &mut TestAppContext) {
    let (events, _host, cx) = menu_host(cx, menu_view());
    for selector in [
        "plugin-row-http",
        "plugin-mounted-logs",
        "plugin-trust-seed",
        "plugin-problem-0",
    ] {
        assert!(drawn(cx, selector), "{selector} is drawn");
    }
    assert!(!drawn(cx, "plugin-unmount-http"));
    assert!(!drawn(cx, "plugin-mount-logs"));
    assert!(!drawn(cx, "plugin-trust-http"));
    for selector in [
        "plugin-mount-http",
        "plugin-unmount-logs",
        "plugin-mount-seed",
        "plugin-menu-close",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(
        *events.borrow(),
        ["mount http", "unmount logs", "mount seed", "close"]
    );
}

#[gpui::test]
fn a_busy_plugins_menu_mounts_nothing(cx: &mut TestAppContext) {
    let (events, host, cx) = menu_host(cx, menu_view());
    host.update(cx, |host, cx| {
        host.view.busy = true;
        host.view.error = Some("plugin http is out of scope for production".into());
        cx.notify();
    });
    cx.run_until_parked();
    assert!(drawn(cx, "plugin-menu-error"));
    for selector in ["plugin-mount-http", "plugin-unmount-logs"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    // Positive control: Close still works in the same window.
    assert!(click(cx, "plugin-menu-close"));
    assert_eq!(*events.borrow(), ["close"]);
}

#[gpui::test]
fn the_plugins_menu_caps_long_lists_and_says_when_empty(cx: &mut TestAppContext) {
    let mut view = menu_view();
    view.plugins = (0..PLUGIN_ROW_LIMIT + 2)
        .map(|i| entry(&format!("p{i}"), PluginSource::User, false, false))
        .collect();
    view.problems = (0..PLUGIN_PROBLEM_LIMIT + 1)
        .map(|i| PluginProblemRow {
            path: format!("/p{i}/plugin.toml").into(),
            message: "bad".into(),
        })
        .collect();
    let (_events, host, cx) = menu_host(cx, view);
    assert!(drawn(cx, "plugin-more"));
    assert!(drawn(cx, "plugin-problems-more"));
    assert!(!drawn(cx, "plugin-menu-empty"));
    host.update(cx, |host, cx| {
        host.view.plugins.clear();
        host.view.problems.clear();
        cx.notify();
    });
    cx.run_until_parked();
    assert!(drawn(cx, "plugin-menu-empty"));
    assert!(!drawn(cx, "plugin-more"));
    assert!(!drawn(cx, "plugin-problem-0"));
}
