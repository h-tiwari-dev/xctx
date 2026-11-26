//! The Milestone 5 Plugins menu in GPUI's headless test window: one click adds a plugin with no setting, a plugin
//! with a setting opens inline and mounts with what its input holds, mounted plugins offer Show and Remove,
//! nothing reacts while busy, and its badges, groups, problems, caps and error line.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, Entity, IntoElement, Modifiers, Render, SharedString, TestAppContext,
    VisualTestContext, Window, div, prelude::*,
};
use xctx_ui::{
    IconName, MountedEntry, PLUGIN_PROBLEM_LIMIT, PLUGIN_ROW_LIMIT, PluginEntry, PluginGroup,
    PluginMenu, PluginMenuView, PluginProblemRow, PluginSetting, PluginSource, PromptInput, Theme,
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

/// A host like the app's: it opens a setting row with the setting's default in its input, and mounts with what
/// the input holds.
struct MenuHost {
    view: PluginMenuView,
    input: Entity<PromptInput>,
    log: Log,
}

impl Render for MenuHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let log = self.log.clone();
        let (add, show, remove, close) = (log.clone(), log.clone(), log.clone(), log.clone());
        let host = cx.entity().downgrade();
        let submit_host = host.clone();
        let mut menu = PluginMenu::new(self.view.clone())
            .on_add(move |name, _, _| add.borrow_mut().push(format!("add {name}")))
            .on_expand(move |name, _, cx| {
                let name = name.cloned();
                let _ = host.update(cx, |host, cx| host.expand(name, cx));
            })
            .on_submit(move |name, _, cx| {
                let name = name.clone();
                let _ = submit_host.update(cx, |host, cx| {
                    let value = host.input.read(cx).value().to_owned();
                    host.log.borrow_mut().push(format!("add {name}: {value}"));
                });
            })
            .on_show(move |name, _, _| show.borrow_mut().push(format!("show {name}")))
            .on_remove(move |name, _, _| remove.borrow_mut().push(format!("remove {name}")))
            .on_close(move |_, _| close.borrow_mut().push("close".to_owned()));
        if self.view.expanded.is_some() {
            menu = menu.setting_input(self.input.clone());
        }
        div().size_full().child(menu)
    }
}

impl MenuHost {
    fn expand(&mut self, name: Option<SharedString>, cx: &mut Context<Self>) {
        let default = name.as_ref().and_then(|name| {
            self.view
                .plugins
                .iter()
                .find(|p| &p.name == name)
                .and_then(|p| p.setting.as_ref())
                .map(|s| s.default.to_string())
        });
        self.log.borrow_mut().push(match &name {
            Some(name) => format!("expand {name}"),
            None => "collapse".to_owned(),
        });
        self.view.expanded = name;
        if let Some(default) = default {
            self.input
                .update(cx, |input, cx| input.set_value(&default, cx));
        }
        cx.notify();
    }
}

fn entry(name: &str, group: PluginGroup, source: PluginSource, needs_trust: bool) -> PluginEntry {
    PluginEntry {
        name: name.to_owned().into(),
        title: name.to_uppercase().into(),
        description: format!("the {name} plugin").into(),
        icon: IconName::Plugin,
        source,
        group,
        needs_trust,
        setting: None,
    }
}

fn with_setting(mut entry: PluginEntry, default: &str) -> PluginEntry {
    entry.setting = Some(PluginSetting {
        label: "Command".into(),
        default: default.to_owned().into(),
        hint: Some("blank for your shell".into()),
    });
    entry
}

fn menu_view() -> PluginMenuView {
    PluginMenuView {
        mounted: vec![MountedEntry {
            name: "browser".into(),
            title: "Browser".into(),
            icon: IconName::ExternalLink,
            showing: Some("http://localhost:4011/".into()),
        }],
        plugins: vec![
            with_setting(
                entry("terminal", PluginGroup::Panes, PluginSource::Builtin, false),
                "npm run dev",
            ),
            entry("files", PluginGroup::Panes, PluginSource::Builtin, false),
            entry(
                "jira",
                PluginGroup::Connectors,
                PluginSource::Builtin,
                false,
            ),
            entry("mine", PluginGroup::Tools, PluginSource::User, false),
            entry("seed", PluginGroup::Other, PluginSource::Repo, true),
        ],
        expanded: None,
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
    let (host, cx) = cx.add_window_view(move |_, cx| MenuHost {
        view,
        input: cx.new(PromptInput::new),
        log,
    });
    cx.run_until_parked();
    (events, host, cx)
}

#[gpui::test]
fn one_click_adds_a_plugin_with_no_setting(cx: &mut TestAppContext) {
    let (events, _host, cx) = menu_host(cx, menu_view());
    for selector in [
        "plugin-group-panes",
        "plugin-group-tools",
        "plugin-group-connectors",
        "plugin-group-other",
        "plugin-source-mine",
        "plugin-source-seed",
        "plugin-trust-seed",
        "plugin-problem-0",
    ] {
        assert!(drawn(cx, selector), "{selector} is drawn");
    }
    // Built-ins carry no source badge, and only the untrusted plugin needs approval.
    assert!(!drawn(cx, "plugin-source-files"));
    assert!(!drawn(cx, "plugin-trust-mine"));
    for selector in ["plugin-add-files", "plugin-add-jira", "plugin-add-seed"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(*events.borrow(), ["add files", "add jira", "add seed"]);
}

#[gpui::test]
fn a_plugin_with_a_setting_opens_inline_and_mounts_with_its_value(cx: &mut TestAppContext) {
    let (events, host, cx) = menu_host(cx, menu_view());
    assert!(!drawn(cx, "plugin-setting-terminal"));
    assert!(click(cx, "plugin-add-terminal"));
    cx.run_until_parked();
    assert!(drawn(cx, "plugin-setting-terminal"));
    assert!(click(cx, "plugin-submit-terminal"));
    // What the user typed replaces the default.
    host.update(cx, |host, cx| {
        host.input
            .update(cx, |input, cx| input.set_value("k9s", cx))
    });
    cx.run_until_parked();
    assert!(click(cx, "plugin-submit-terminal"));
    // Clicking the open row again closes it.
    assert!(click(cx, "plugin-add-terminal"));
    cx.run_until_parked();
    assert!(!drawn(cx, "plugin-setting-terminal"));
    assert_eq!(
        *events.borrow(),
        [
            "expand terminal",
            "add terminal: npm run dev",
            "add terminal: k9s",
            "collapse"
        ]
    );
}

#[gpui::test]
fn mounted_plugins_can_be_shown_or_removed(cx: &mut TestAppContext) {
    let (events, host, cx) = menu_host(cx, menu_view());
    assert!(drawn(cx, "plugin-mounted-browser"));
    for selector in [
        "plugin-show-browser",
        "plugin-remove-browser",
        "plugin-menu-close",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(
        *events.borrow(),
        ["show browser", "remove browser", "close"]
    );
    // Nothing mounted: the section is gone.
    host.update(cx, |host, cx| {
        host.view.mounted.clear();
        cx.notify();
    });
    cx.run_until_parked();
    assert!(!drawn(cx, "plugin-mounted-browser"));
    assert!(!drawn(cx, "plugin-show-browser"));
}

#[gpui::test]
fn a_busy_plugins_menu_does_nothing(cx: &mut TestAppContext) {
    let mut view = menu_view();
    view.expanded = Some("terminal".into());
    let (events, host, cx) = menu_host(cx, view);
    host.update(cx, |host, cx| {
        host.view.busy = true;
        host.view.error = Some("plugin http is out of scope for production".into());
        cx.notify();
    });
    cx.run_until_parked();
    assert!(drawn(cx, "plugin-menu-error"));
    for selector in [
        "plugin-add-files",
        "plugin-add-terminal",
        "plugin-submit-terminal",
        "plugin-show-browser",
        "plugin-remove-browser",
    ] {
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
        .map(|i| {
            entry(
                &format!("p{i}"),
                PluginGroup::Other,
                PluginSource::User,
                false,
            )
        })
        .collect();
    view.mounted = (0..PLUGIN_ROW_LIMIT + 1)
        .map(|i| MountedEntry {
            name: format!("m{i}").into(),
            title: format!("M{i}").into(),
            icon: IconName::Plugin,
            showing: None,
        })
        .collect();
    view.problems = (0..PLUGIN_PROBLEM_LIMIT + 1)
        .map(|i| PluginProblemRow {
            path: format!("/p{i}/plugin.toml").into(),
            message: "bad".into(),
        })
        .collect();
    let (_events, host, cx) = menu_host(cx, view);
    assert!(drawn(cx, "plugin-more"));
    assert!(drawn(cx, "plugin-mounted-more"));
    assert!(drawn(cx, "plugin-problems-more"));
    assert!(!drawn(cx, "plugin-menu-empty"));
    host.update(cx, |host, cx| {
        host.view.plugins.clear();
        host.view.mounted.clear();
        host.view.problems.clear();
        cx.notify();
    });
    cx.run_until_parked();
    assert!(drawn(cx, "plugin-menu-empty"));
    assert!(!drawn(cx, "plugin-more"));
    assert!(!drawn(cx, "plugin-group-other"));
    assert!(!drawn(cx, "plugin-problem-0"));
}
