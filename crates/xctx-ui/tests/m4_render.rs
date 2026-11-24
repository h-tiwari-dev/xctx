//! The Milestone 4 components in GPUI's headless test window: the fork dialog, the attempt strip and the review
//! sheet, clicked through their buttons, including what they must not offer.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Context, IntoElement, Modifiers, Render, TestAppContext, VisualTestContext, Window, div,
    prelude::*,
};
use xctx_ui::{
    AttemptChip, AttemptChipState, AttemptStrip, DiffView, FileRow, ForkDialog, ForkDialogView,
    ReproView, ReviewColumn, ReviewSheet, ReviewView, Theme,
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

struct ForkHost {
    view: ForkDialogView,
    inputs: usize,
    log: Log,
}

impl Render for ForkHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (count, cancel, fork) = (self.log.clone(), self.log.clone(), self.log.clone());
        let inputs = (0..self.inputs).map(|i| {
            div()
                .debug_selector(move || format!("fork-input-{}", i + 1))
                .child(format!("input {}", i + 1))
        });
        div().size_full().p_4().child(
            ForkDialog::new(self.view.clone())
                .instruction_inputs(inputs)
                .on_count(move |n, _, _| log(&count, format!("count {n}")))
                .on_cancel(move |_, _| log(&cancel, "cancel"))
                .on_fork(move |_, _| log(&fork, "fork")),
        )
    }
}

fn fork_view(count: u32, busy: bool) -> ForkDialogView {
    ForkDialogView {
        writer: "pi · api".into(),
        repo: "api".into(),
        count,
        max: 4,
        instructions: vec!["use a token bucket".into()],
        busy,
        error: busy.then(|| "could not snapshot the worktree".into()),
    }
}

fn fork_host(
    cx: &mut TestAppContext,
    view: ForkDialogView,
    inputs: usize,
) -> (Log, &mut VisualTestContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| ForkHost {
        view,
        inputs,
        log: host_log,
    });
    cx.run_until_parked();
    (events, cx)
}

#[gpui::test]
fn the_fork_dialog_picks_a_count_then_forks_or_cancels(cx: &mut TestAppContext) {
    let (events, cx) = fork_host(cx, fork_view(3, false), 1);
    for selector in [
        "fork-count-1",
        "fork-count-2",
        "fork-count-3",
        "fork-count-4",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} is drawn");
    }
    assert!(cx.debug_bounds("fork-count-5").is_none());
    // One row per attempt: the first holds the app's input, the others fall back to text.
    assert!(cx.debug_bounds("fork-input-1").is_some());
    for selector in ["fork-row-1", "fork-row-2", "fork-row-3"] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector} is drawn");
    }
    assert!(cx.debug_bounds("fork-row-4").is_none());

    for selector in [
        "fork-count-2",
        "fork-count-4",
        "fork-cancel",
        "fork-confirm",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(*events.borrow(), ["count 2", "count 4", "cancel", "fork"]);
}

#[gpui::test]
fn inputs_past_the_count_are_not_drawn(cx: &mut TestAppContext) {
    let (_events, cx) = fork_host(cx, fork_view(2, false), 4);
    assert!(cx.debug_bounds("fork-input-2").is_some());
    assert!(cx.debug_bounds("fork-input-3").is_none());
}

#[gpui::test]
fn a_busy_fork_dialog_only_cancels(cx: &mut TestAppContext) {
    let (events, cx) = fork_host(cx, fork_view(2, true), 0);
    for selector in ["fork-count-3", "fork-confirm", "fork-cancel"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(*events.borrow(), ["cancel"]);
}

// ---------------------------------------------------------------------------------------------------------------

struct StripHost {
    chips: Vec<AttemptChip>,
    can_review: bool,
    fork: bool,
    log: Log,
}

impl Render for StripHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (select, review, fork) = (self.log.clone(), self.log.clone(), self.log.clone());
        let mut strip = AttemptStrip::new(self.chips.clone())
            .can_review(self.can_review)
            .on_select(move |n, _, _| log(&select, format!("select {n:?}")))
            .on_review(move |_, _| log(&review, "review"));
        if self.fork {
            strip = strip.on_fork(move |_, _| log(&fork, "fork"));
        }
        div().size_full().child(strip)
    }
}

fn chips() -> Vec<AttemptChip> {
    use AttemptChipState::*;
    [Running, Done, Failed, Discarded]
        .into_iter()
        .enumerate()
        .map(|(i, state)| AttemptChip {
            number: i as u32 + 1,
            label: format!("Attempt {}", i + 1).into(),
            state,
            active: i == 1,
        })
        .collect()
}

fn strip_host(
    cx: &mut TestAppContext,
    can_review: bool,
    fork: bool,
) -> (Log, &mut VisualTestContext) {
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| StripHost {
        chips: chips(),
        can_review,
        fork,
        log: host_log,
    });
    cx.run_until_parked();
    (events, cx)
}

#[gpui::test]
fn the_strip_switches_between_the_space_and_its_attempts(cx: &mut TestAppContext) {
    let (events, cx) = strip_host(cx, true, true);
    for selector in [
        "strip-space",
        "strip-attempt-1",
        "strip-attempt-3",
        "strip-attempt-4",
        "strip-review",
        "strip-fork",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    // A discarded attempt is shown but has nothing to switch to.
    assert_eq!(
        *events.borrow(),
        [
            "select None",
            "select Some(1)",
            "select Some(3)",
            "review",
            "fork"
        ]
    );
}

#[gpui::test]
fn the_strip_hides_review_and_fork_unless_asked(cx: &mut TestAppContext) {
    let (events, cx) = strip_host(cx, false, false);
    assert!(cx.debug_bounds("strip-review").is_none());
    assert!(cx.debug_bounds("strip-fork").is_none());
    assert!(click(cx, "strip-attempt-2"));
    assert_eq!(*events.borrow(), ["select Some(2)"]);
}

// ---------------------------------------------------------------------------------------------------------------

struct ReviewHost {
    view: ReviewView,
    log: Log,
}

impl Render for ReviewHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (adopt, discard, file, close_diff, close) = (
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
        );
        div().size_full().p_4().child(
            ReviewSheet::new(self.view.clone())
                .on_adopt(move |n, _, _| log(&adopt, format!("adopt {n}")))
                .on_discard(move |n, _, _| log(&discard, format!("discard {n}")))
                .on_file(move |n, path, _, _| log(&file, format!("file {n} {path}")))
                .on_close_diff(move |_, _| log(&close_diff, "close diff"))
                .on_close(move |_, _| log(&close, "close")),
        )
    }
}

fn review_column(number: u32, state: AttemptChipState) -> ReviewColumn {
    ReviewColumn {
        number,
        instruction: Some(format!("solution {number}").into()),
        state,
        summary: (number != 3).then(|| "swapped the limiter for a token bucket".into()),
        tests: Some((41, number - 1)),
        test_command: Some("cargo test".into()),
        repro: if number == 1 {
            ReproView::Fixed
        } else {
            ReproView::StillFails
        },
        files: Some(vec![
            FileRow {
                path: "src/limit.rs".into(),
                added: Some(12),
                removed: Some(3),
            },
            FileRow {
                path: "assets/logo.png".into(),
                added: None,
                removed: None,
            },
        ]),
        added: 12,
        removed: 3,
    }
}

fn review_view(states: &[AttemptChipState], adopted: Option<u32>, busy: Option<u32>) -> ReviewView {
    ReviewView {
        round: 1,
        columns: states
            .iter()
            .enumerate()
            .map(|(i, &state)| review_column(i as u32 + 1, state))
            .collect(),
        adopted,
        busy,
        error: None,
        diff: None,
    }
}

fn review_host(cx: &mut TestAppContext, view: ReviewView) -> (Log, &mut VisualTestContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (_host, cx) = cx.add_window_view(move |_, _| ReviewHost {
        view,
        log: host_log,
    });
    cx.run_until_parked();
    (events, cx)
}

#[gpui::test]
fn the_review_sheet_adopts_discards_and_opens_files(cx: &mut TestAppContext) {
    use AttemptChipState::*;
    let (events, cx) = review_host(
        cx,
        review_view(&[Done, Failed, Running, Discarded], None, None),
    );

    // Equal columns, side by side.
    let first = cx.debug_bounds("review-column-1");
    let second = cx.debug_bounds("review-column-2");
    let (Some(first), Some(second)) = (first, second) else {
        panic!("the columns are drawn");
    };
    assert_eq!(first.origin.y, second.origin.y);
    assert!(second.origin.x > first.origin.x);
    assert!((first.size.width - second.size.width).abs() < gpui::px(1.0));

    for selector in [
        "review-file-1-0",
        "review-file-2-1",
        "review-discard-3",
        "review-adopt-2",
        "review-adopt-1",
        "review-close",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    // A discarded attempt offers nothing.
    assert!(cx.debug_bounds("review-adopt-4").is_none());
    assert!(cx.debug_bounds("review-discard-4").is_none());
    // No diff is open.
    assert!(cx.debug_bounds("review-diff-close").is_none());

    assert_eq!(
        *events.borrow(),
        [
            "file 1 src/limit.rs",
            "file 2 assets/logo.png",
            "discard 3",
            "adopt 2",
            "adopt 1",
            "close",
        ]
    );
}

#[gpui::test]
fn nothing_else_is_adopted_once_a_round_has_a_winner(cx: &mut TestAppContext) {
    use AttemptChipState::*;
    let (events, cx) = review_host(cx, review_view(&[Done, Adopted, Failed], Some(2), None));
    for selector in ["review-adopt-1", "review-adopt-2", "review-adopt-3"] {
        assert!(cx.debug_bounds(selector).is_none(), "no {selector}");
    }
    assert!(cx.debug_bounds("review-discard-2").is_none());
    // The losers are still offered for discard.
    assert!(click(cx, "review-discard-1"));
    assert!(click(cx, "review-discard-3"));
    assert_eq!(*events.borrow(), ["discard 1", "discard 3"]);
}

#[gpui::test]
fn actions_are_disabled_while_one_runs(cx: &mut TestAppContext) {
    use AttemptChipState::*;
    let mut view = review_view(&[Done, Done], None, Some(1));
    view.error = Some("merge conflict in src/limit.rs".into());
    let (events, cx) = review_host(cx, view);
    for selector in [
        "review-adopt-1",
        "review-adopt-2",
        "review-discard-1",
        "review-discard-2",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert!(events.borrow().is_empty(), "{:?}", events.borrow());
}

#[gpui::test]
fn an_open_diff_is_shown_under_the_columns_and_closes(cx: &mut TestAppContext) {
    use AttemptChipState::*;
    let mut view = review_view(&[Done, Done], None, None);
    view.diff = Some(DiffView {
        attempt: 1,
        path: "src/limit.rs".into(),
        text: "--- a/src/limit.rs\n+++ b/src/limit.rs\n@@ -1,2 +1,2 @@\n-old\n+new\n same\n".into(),
    });
    let (events, cx) = review_host(cx, view);
    let (Some(diff), Some(column)) = (
        cx.debug_bounds("review-diff"),
        cx.debug_bounds("review-column-1"),
    ) else {
        panic!("the diff and the columns are drawn");
    };
    assert!(diff.origin.y >= column.origin.y + column.size.height);
    assert!(click(cx, "review-diff-close"));
    assert_eq!(*events.borrow(), ["close diff"]);
}

struct EmptyReviewHost;

impl Render for EmptyReviewHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(ReviewSheet::new(ReviewView {
            round: 2,
            columns: Vec::new(),
            adopted: None,
            busy: None,
            error: None,
            diff: None,
        }))
    }
}

#[gpui::test]
fn an_empty_review_sheet_draws_without_close(cx: &mut TestAppContext) {
    let (_host, cx) = cx.add_window_view(|_, _| EmptyReviewHost);
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-close").is_none());
}
