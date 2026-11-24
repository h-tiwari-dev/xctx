//! The Milestone 4 components in GPUI's headless test window: the fork dialog, the attempt strip and the review
//! sheet, clicked through their buttons, including what they must not offer.

use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    Bounds, Context, Entity, IntoElement, Modifiers, Pixels, Render, TestAppContext,
    VisualTestContext, Window, div, prelude::*, px,
};
use xctx_ui::{
    AttemptChip, AttemptChipState, AttemptStrip, DIFF_LINE_LIMIT, DiffView, FILE_ROW_LIMIT,
    FileRow, ForkDialog, ForkDialogView, ReproView, ReviewColumn, ReviewSheet, ReviewView, Theme,
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

/// Whether the element named `selector`, built at run time, was drawn.
fn drawn(cx: &mut VisualTestContext, selector: String) -> bool {
    // `debug_bounds` wants a `'static` name; a test leaks a few short strings.
    cx.debug_bounds(Box::leak(selector.into_boxed_str()))
        .is_some()
}

/// The window's bounds, from its origin.
fn window_bounds(cx: &mut VisualTestContext) -> Bounds<Pixels> {
    let size = cx.update(|window, _| window.viewport_size());
    Bounds::new(gpui::point(px(0.0), px(0.0)), size)
}

/// Whether `inner` lies wholly within `outer`.
fn within(inner: Bounds<Pixels>, outer: Bounds<Pixels>) -> bool {
    inner.origin.x >= outer.origin.x
        && inner.origin.y >= outer.origin.y
        && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width + px(0.5)
        && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height + px(0.5)
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
    let (events, _host, cx) = fork_host_entity(cx, view, inputs);
    (events, cx)
}

fn fork_host_entity(
    cx: &mut TestAppContext,
    view: ForkDialogView,
    inputs: usize,
) -> (Log, Entity<ForkHost>, &mut VisualTestContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (host, cx) = cx.add_window_view(move |_, _| ForkHost {
        view,
        inputs,
        log: host_log,
    });
    cx.run_until_parked();
    (events, host, cx)
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
fn a_busy_fork_dialog_only_closes_and_says_the_fork_carries_on(cx: &mut TestAppContext) {
    let (events, host, cx) = fork_host_entity(cx, fork_view(2, false), 0);
    // Positive control: idle, the same clicks reach the handlers and there is no busy hint.
    assert!(cx.debug_bounds("fork-busy-hint").is_none());
    for selector in ["fork-count-3", "fork-confirm"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(*events.borrow(), ["count 3", "fork"]);
    events.borrow_mut().clear();

    assert!(cx.debug_bounds("fork-error").is_none());
    host.update(cx, |host, cx| {
        host.view.busy = true;
        host.view.error = Some("could not snapshot the worktree".into());
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("fork-busy-hint").is_some());
    assert!(cx.debug_bounds("fork-error").is_some());
    for selector in ["fork-count-3", "fork-confirm", "fork-cancel"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(*events.borrow(), ["cancel"]);
}

#[gpui::test]
fn a_full_space_offers_no_count_and_no_fork(cx: &mut TestAppContext) {
    let mut view = fork_view(2, false);
    view.max = 0;
    let (events, host, cx) = fork_host_entity(cx, view, 0);
    assert!(cx.debug_bounds("fork-full").is_some());
    for n in 1..=4 {
        assert!(!drawn(cx, format!("fork-count-{n}")), "no fork-count-{n}");
    }
    assert!(cx.debug_bounds("fork-row-1").is_none());
    assert!(click(cx, "fork-confirm"));
    assert!(events.borrow().is_empty(), "{:?}", events.borrow());

    // Positive control: with room for two, two pills and Fork works.
    host.update(cx, |host, cx| {
        host.view.max = 2;
        cx.notify();
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("fork-full").is_none());
    assert!(cx.debug_bounds("fork-count-2").is_some());
    assert!(cx.debug_bounds("fork-count-3").is_none());
    assert!(click(cx, "fork-confirm"));
    assert_eq!(*events.borrow(), ["fork"]);
}

#[gpui::test]
fn rows_without_an_input_show_their_instruction_or_the_same_task(cx: &mut TestAppContext) {
    let (_events, cx) = fork_host(cx, fork_view(3, false), 0);
    assert!(cx.debug_bounds("fork-instruction-1").is_some());
    assert!(cx.debug_bounds("fork-same-task-1").is_none());
    for n in [2, 3] {
        assert!(drawn(cx, format!("fork-same-task-{n}")));
        assert!(!drawn(cx, format!("fork-instruction-{n}")));
    }
}

// ---------------------------------------------------------------------------------------------------------------

struct StripHost {
    chips: Vec<AttemptChip>,
    can_review: bool,
    fork: bool,
    busy: bool,
    can_fork: bool,
    log: Log,
}

impl Render for StripHost {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let (select, review, fork) = (self.log.clone(), self.log.clone(), self.log.clone());
        let mut strip = AttemptStrip::new(self.chips.clone())
            .can_review(self.can_review)
            .busy(self.busy)
            .can_fork(self.can_fork)
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
    [Running, Done, Failed, Discarded, Adopted]
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
    let (events, _host, cx) = strip_host_entity(cx, can_review, fork);
    (events, cx)
}

fn strip_host_entity(
    cx: &mut TestAppContext,
    can_review: bool,
    fork: bool,
) -> (Log, Entity<StripHost>, &mut VisualTestContext) {
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (host, cx) = cx.add_window_view(move |_, _| StripHost {
        chips: chips(),
        can_review,
        fork,
        busy: false,
        can_fork: true,
        log: host_log,
    });
    cx.run_until_parked();
    (events, host, cx)
}

#[gpui::test]
fn the_strip_switches_between_the_space_and_its_attempts(cx: &mut TestAppContext) {
    let (events, cx) = strip_host(cx, true, true);
    for selector in [
        "strip-space",
        "strip-attempt-1",
        "strip-attempt-3",
        "strip-attempt-4",
        "strip-attempt-5",
        "strip-review",
        "strip-fork",
    ] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    // Discarded and adopted attempts are shown but have nothing to switch to.
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
fn a_busy_or_full_strip_disables_fork_and_review(cx: &mut TestAppContext) {
    let (events, host, cx) = strip_host_entity(cx, true, true);
    host.update(cx, |host, cx| {
        host.busy = true;
        cx.notify();
    });
    cx.run_until_parked();
    for selector in ["strip-review", "strip-fork"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    // Chips still switch while busy: the control that the clicks land.
    assert!(click(cx, "strip-attempt-1"));
    assert_eq!(*events.borrow(), ["select Some(1)"]);
    events.borrow_mut().clear();

    // Not busy but full: Review works, Fork… does not.
    host.update(cx, |host, cx| {
        host.busy = false;
        host.can_fork = false;
        cx.notify();
    });
    cx.run_until_parked();
    for selector in ["strip-review", "strip-fork"] {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(*events.borrow(), ["review"]);
    events.borrow_mut().clear();

    host.update(cx, |host, cx| {
        host.can_fork = true;
        cx.notify();
    });
    cx.run_until_parked();
    assert!(click(cx, "strip-fork"));
    assert_eq!(*events.borrow(), ["fork"]);
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
        let (adopt, request, discard, keep, file, close_diff, close) = (
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
            self.log.clone(),
        );
        div().size_full().p_4().child(
            ReviewSheet::new(self.view.clone())
                .on_adopt(move |n, _, _| log(&adopt, format!("adopt {n}")))
                .on_discard_request(move |n, _, _| log(&request, format!("request discard {n}")))
                .on_discard(move |n, _, _| log(&discard, format!("discard {n}")))
                .on_cancel_discard(move |n, _, _| log(&keep, format!("keep {n}")))
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

fn review_view(states: &[AttemptChipState], adopted: Option<u32>, busy: bool) -> ReviewView {
    ReviewView {
        round: 1,
        columns: states
            .iter()
            .enumerate()
            .map(|(i, &state)| review_column(i as u32 + 1, state))
            .collect(),
        adopted,
        busy,
        working: None,
        confirm_discard: None,
        error: None,
        diff: None,
    }
}

fn review_host(cx: &mut TestAppContext, view: ReviewView) -> (Log, &mut VisualTestContext) {
    let (events, _host, cx) = review_host_entity(cx, view);
    (events, cx)
}

fn review_host_entity(
    cx: &mut TestAppContext,
    view: ReviewView,
) -> (Log, Entity<ReviewHost>, &mut VisualTestContext) {
    cx.update(|cx| cx.set_global(Theme::dark()));
    let events: Log = Rc::default();
    let host_log = events.clone();
    let (host, cx) = cx.add_window_view(move |_, _| ReviewHost {
        view,
        log: host_log,
    });
    cx.run_until_parked();
    (events, host, cx)
}

fn update_review(
    cx: &mut VisualTestContext,
    host: &Entity<ReviewHost>,
    change: impl FnOnce(&mut ReviewView),
) {
    host.update(cx, |host, cx| {
        change(&mut host.view);
        cx.notify();
    });
    cx.run_until_parked();
}

#[gpui::test]
fn the_review_sheet_adopts_discards_and_opens_files(cx: &mut TestAppContext) {
    use AttemptChipState::*;
    let (events, cx) = review_host(
        cx,
        review_view(&[Done, Failed, Running, Discarded], None, false),
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
    // A running attempt can be discarded but not adopted; a discarded one offers nothing.
    assert!(cx.debug_bounds("review-adopt-3").is_none());
    assert!(cx.debug_bounds("review-adopt-4").is_none());
    assert!(cx.debug_bounds("review-discard-4").is_none());
    // No diff is open.
    assert!(cx.debug_bounds("review-diff-close").is_none());

    assert_eq!(
        *events.borrow(),
        [
            "file 1 src/limit.rs",
            "file 2 assets/logo.png",
            "request discard 3",
            "adopt 2",
            "adopt 1",
            "close",
        ]
    );
}

#[gpui::test]
fn nothing_else_is_adopted_once_a_round_has_a_winner(cx: &mut TestAppContext) {
    use AttemptChipState::*;
    let (events, cx) = review_host(cx, review_view(&[Done, Adopted, Failed], Some(2), false));
    for selector in ["review-adopt-1", "review-adopt-2", "review-adopt-3"] {
        assert!(cx.debug_bounds(selector).is_none(), "no {selector}");
    }
    assert!(cx.debug_bounds("review-discard-2").is_none());
    // The losers are still offered for discard.
    assert!(click(cx, "review-discard-1"));
    assert!(click(cx, "review-discard-3"));
    assert_eq!(*events.borrow(), ["request discard 1", "request discard 3"]);
}

#[gpui::test]
fn an_adopted_column_blocks_adopt_without_the_adopted_field(cx: &mut TestAppContext) {
    use AttemptChipState::*;
    let (_events, cx) = review_host(cx, review_view(&[Done, Adopted], None, false));
    assert!(cx.debug_bounds("review-adopt-1").is_none());
    assert!(cx.debug_bounds("review-discard-1").is_some());
}

#[gpui::test]
fn actions_are_disabled_while_one_runs(cx: &mut TestAppContext) {
    use AttemptChipState::*;
    let mut view = review_view(&[Done, Done], None, true);
    view.working = Some(1);
    view.error = Some("merge conflict in src/limit.rs".into());
    let (events, host, cx) = review_host_entity(cx, view);
    assert!(cx.debug_bounds("review-error").is_some());
    assert!(cx.debug_bounds("review-working-1").is_some());
    assert!(cx.debug_bounds("review-working-2").is_none());
    let selectors = [
        "review-adopt-1",
        "review-adopt-2",
        "review-discard-1",
        "review-discard-2",
    ];
    for selector in selectors {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert!(events.borrow().is_empty(), "{:?}", events.borrow());

    // A confirming Discard is disabled too.
    update_review(cx, &host, |view| view.confirm_discard = Some(2));
    assert!(click(cx, "review-confirm-discard-2"));
    assert!(events.borrow().is_empty(), "{:?}", events.borrow());

    // Positive control: the same window, no longer busy, and every click lands.
    update_review(cx, &host, |view| {
        view.busy = false;
        view.working = None;
        view.confirm_discard = None;
    });
    assert!(cx.debug_bounds("review-working-1").is_none());
    for selector in selectors {
        assert!(click(cx, selector), "{selector} is drawn");
    }
    assert_eq!(
        *events.borrow(),
        [
            "adopt 1",
            "adopt 2",
            "request discard 1",
            "request discard 2"
        ]
    );
}

#[gpui::test]
fn discard_takes_a_confirming_click(cx: &mut TestAppContext) {
    use AttemptChipState::*;
    let (events, host, cx) = review_host_entity(cx, review_view(&[Done, Running], None, false));
    assert!(cx.debug_bounds("review-confirm-discard-2").is_none());
    assert!(cx.debug_bounds("review-keep-2").is_none());
    assert!(click(cx, "review-discard-2"));
    assert_eq!(*events.borrow(), ["request discard 2"]);

    // The app asks for the confirmation: only that column changes.
    update_review(cx, &host, |view| view.confirm_discard = Some(2));
    assert!(cx.debug_bounds("review-discard-2").is_none());
    assert!(cx.debug_bounds("review-discard-1").is_some());
    assert!(cx.debug_bounds("review-confirm-discard-1").is_none());
    assert!(click(cx, "review-keep-2"));
    assert!(click(cx, "review-confirm-discard-2"));
    assert_eq!(
        *events.borrow(),
        ["request discard 2", "keep 2", "discard 2"]
    );
}

#[gpui::test]
fn missing_changes_say_whether_the_worktree_is_gone(cx: &mut TestAppContext) {
    use AttemptChipState::*;
    let mut view = review_view(&[Done, Discarded, Adopted, Running], None, false);
    for column in &mut view.columns {
        column.files = None;
    }
    let (_events, cx) = review_host(cx, view);
    for n in [1, 4] {
        assert!(drawn(cx, format!("review-changes-pending-{n}")));
        assert!(!drawn(cx, format!("review-worktree-removed-{n}")));
    }
    for n in [2, 3] {
        assert!(drawn(cx, format!("review-worktree-removed-{n}")));
        assert!(!drawn(cx, format!("review-changes-pending-{n}")));
    }
}

#[gpui::test]
fn many_files_and_a_long_summary_keep_the_actions_in_reach(cx: &mut TestAppContext) {
    use AttemptChipState::*;
    let mut view = review_view(&[Done, Done], None, false);
    view.columns[0].files = Some(
        (0..FILE_ROW_LIMIT + 50)
            .map(|i| FileRow {
                path: format!("src/file_{i}.rs").into(),
                added: Some(1),
                removed: Some(0),
            })
            .collect(),
    );
    view.columns[0].summary = Some("a long summary line. ".repeat(2000).into());
    let (events, cx) = review_host(cx, view);
    let window = window_bounds(cx);

    // The rows are capped, with a note for the rest; the short column has no note (the control).
    assert!(drawn(cx, format!("review-file-1-{}", FILE_ROW_LIMIT - 1)));
    assert!(!drawn(cx, format!("review-file-1-{FILE_ROW_LIMIT}")));
    assert!(cx.debug_bounds("review-files-more-1").is_some());
    assert!(cx.debug_bounds("review-files-more-2").is_none());

    // The list and the summary are capped in height although their content is far taller.
    let (Some(list), Some(summary), Some(row)) = (
        cx.debug_bounds("review-files-1"),
        cx.debug_bounds("review-summary-1"),
        cx.debug_bounds("review-file-1-0"),
    ) else {
        panic!("the file list and the summary are drawn");
    };
    assert!(list.size.height <= px(240.5), "{:?}", list.size);
    assert!(list.size.height >= row.size.height * 5.0, "{:?}", list.size);
    assert!(summary.size.height <= px(160.5), "{:?}", summary.size);

    // Adopt and Discard are still in the window, and they work.
    for selector in ["review-adopt-1", "review-discard-1"] {
        let Some(bounds) = cx.debug_bounds(selector) else {
            panic!("{selector} is drawn");
        };
        assert!(
            within(bounds, window),
            "{selector} {bounds:?} in {window:?}"
        );
        assert!(click(cx, selector));
    }
    assert_eq!(*events.borrow(), ["adopt 1", "request discard 1"]);
}

#[gpui::test]
fn a_long_diff_is_capped_with_a_note(cx: &mut TestAppContext) {
    use AttemptChipState::*;
    let mut view = review_view(&[Done], None, false);
    view.diff = Some(DiffView {
        attempt: 1,
        path: "src/limit.rs".into(),
        text: (0..DIFF_LINE_LIMIT + 3)
            .map(|i| format!("+line {i}\n"))
            .collect::<String>()
            .into(),
    });
    let (_events, host, cx) = review_host_entity(cx, view);
    assert!(cx.debug_bounds("review-diff").is_some());
    assert!(cx.debug_bounds("review-diff-more").is_some());

    // The control: a short diff has no note.
    update_review(cx, &host, |view| {
        if let Some(diff) = view.diff.as_mut() {
            diff.text = "+one\n-two\n".into();
        }
    });
    assert!(cx.debug_bounds("review-diff").is_some());
    assert!(cx.debug_bounds("review-diff-more").is_none());
}

#[gpui::test]
fn an_open_diff_is_shown_under_the_columns_and_closes(cx: &mut TestAppContext) {
    use AttemptChipState::*;
    let mut view = review_view(&[Done, Done], None, false);
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
            busy: false,
            working: None,
            confirm_discard: None,
            error: None,
            diff: None,
        }))
    }
}

#[gpui::test]
fn an_empty_review_sheet_says_so_and_draws_without_close(cx: &mut TestAppContext) {
    let (_host, cx) = cx.add_window_view(|_, _| EmptyReviewHost);
    cx.run_until_parked();
    assert!(cx.debug_bounds("review-close").is_none());
    assert!(cx.debug_bounds("review-empty").is_some());
    assert!(cx.debug_bounds("review-error").is_none());
}

#[gpui::test]
fn a_sheet_with_columns_has_no_empty_message(cx: &mut TestAppContext) {
    use AttemptChipState::*;
    let (_events, cx) = review_host(cx, review_view(&[Done], None, false));
    assert!(cx.debug_bounds("review-column-1").is_some());
    assert!(cx.debug_bounds("review-empty").is_none());
}
