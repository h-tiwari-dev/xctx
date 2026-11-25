//! The repo picker of "+ Repo" and the Plan Gate in GPUI's headless test window: rows report their folder when
//! clicked, picked rows carry a check mark, and nothing reacts while it is disabled.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, Entity, IntoElement, Modifiers, Render, TestAppContext, VisualTestContext, Window,
    div, prelude::*,
};
use xctx_ui::{RepoChoice, RepoPicker, Theme};

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

struct PickerHost {
    choices: Vec<RepoChoice>,
    disabled: bool,
    log: Log,
}

impl Render for PickerHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (toggle, browse) = (self.log.clone(), self.log.clone());
        div().size_full().child(
            RepoPicker::new("test-repos", self.choices.clone())
                .empty("nothing to pick")
                .disabled(self.disabled)
                .on_toggle(move |path, _, _| toggle.borrow_mut().push(path.to_string()))
                .on_browse(move |_, _| browse.borrow_mut().push("browse".to_owned())),
        )
    }
}

fn choice(name: &str, picked: bool) -> RepoChoice {
    RepoChoice {
        name: name.to_owned().into(),
        path: format!("/code/{name}").into(),
        picked,
    }
}

fn picker_host(
    cx: &mut TestAppContext,
    choices: Vec<RepoChoice>,
) -> (Log, Entity<PickerHost>, &mut VisualTestContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let events: Log = Rc::default();
    let log = events.clone();
    let (host, cx) = cx.add_window_view(move |_, _| PickerHost {
        choices,
        disabled: false,
        log,
    });
    cx.run_until_parked();
    (events, host, cx)
}

#[gpui::test]
fn rows_report_their_folder_and_show_what_is_picked(cx: &mut TestAppContext) {
    let (events, _host, cx) = picker_host(cx, vec![choice("web", true), choice("api", false)]);
    assert!(drawn(cx, "repo-picked-web"));
    assert!(!drawn(cx, "repo-picked-api"));
    assert!(!drawn(cx, "repo-picker-empty"));
    assert!(click(cx, "repo-choice-api"));
    assert!(click(cx, "repo-choice-web"));
    assert!(click(cx, "repo-picker-browse"));
    assert_eq!(*events.borrow(), ["/code/api", "/code/web", "browse"]);
}

#[gpui::test]
fn a_disabled_picker_picks_nothing(cx: &mut TestAppContext) {
    let (events, host, cx) = picker_host(cx, vec![choice("api", false)]);
    host.update(cx, |host, cx| {
        host.disabled = true;
        cx.notify();
    });
    cx.run_until_parked();
    assert!(click(cx, "repo-choice-api"));
    assert!(click(cx, "repo-picker-browse"));
    assert!(events.borrow().is_empty());
}

#[gpui::test]
fn an_empty_picker_says_why(cx: &mut TestAppContext) {
    let (_events, _host, cx) = picker_host(cx, Vec::new());
    assert!(drawn(cx, "repo-picker-empty"));
}
