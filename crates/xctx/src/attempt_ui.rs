//! The attempt UI in the window: the fork dialog (⌘⇧F, a writer's "Fork…" in the graph panel, or the attempt
//! strip's Fork…), the attempt strip above a space's panes, which switches the pane area to an attempt's agent,
//! and the review sheet (⌘⇧R or the strip's Review) that compares a round's attempts and adopts or discards them.
//!
//! What they show is worked out by the plain functions at the top, tested without a window; the jobs they start
//! are in `attempt_jobs.rs`.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};

use gpui::{
    AnyElement, Context, Entity, Focusable, FontWeight, IntoElement, ParentElement, SharedString,
    Styled, Subscription, Window, div, prelude::*, px, relative,
};
use xctx_attempt::{Attempt, AttemptSet, AttemptState, Comparison, Repro};
use xctx_core::git::FileChange;
use xctx_core::{AttemptWorktree, MAX_ATTEMPTS, Redactor, SpaceId};
use xctx_graph::{AgentGraph, AgentNodeId};
use xctx_ui::{
    AttemptChip, AttemptChipState, AttemptStrip, DiffView, FileRow, ForkDialog, ForkDialogView,
    PromptEvent, PromptInput, ReproView, ReviewColumn, ReviewSheet, ReviewView, Theme,
};

use crate::dialogs::Dialog;
use crate::space_tab::{PaneTerminal, SpaceTab};
use crate::workspace::Workspace;

/// How many attempts the fork dialog asks for at first.
pub const DEFAULT_FORK_COUNT: u32 = 2;

/// What the diff view says while a file's diff is read.
const DIFF_LOADING: &str = "Loading the diff…";

// -------------------------------------------------------------------------------------------------------------
// What is shown, worked out without a window

/// The strip's and review sheet's state for an attempt in `state`.
pub fn chip_state(state: AttemptState) -> AttemptChipState {
    match state {
        AttemptState::Running => AttemptChipState::Running,
        AttemptState::Done => AttemptChipState::Done,
        AttemptState::Failed => AttemptChipState::Failed,
        AttemptState::Adopted => AttemptChipState::Adopted,
        AttemptState::Discarded => AttemptChipState::Discarded,
    }
}

/// What an attempt's chip says: its instruction (masked with `redactor`) when it has one, else "Attempt n".
pub fn attempt_label(attempt: &Attempt, redactor: &Redactor) -> String {
    match attempt
        .instruction
        .as_deref()
        .map(str::trim)
        .filter(|i| !i.is_empty())
    {
        Some(instruction) => redactor.redact(instruction).into_owned(),
        None => format!("Attempt {}", attempt.number()),
    }
}

/// The attempts the strip lists: every attempt of the latest round, and any still live from an earlier one.
pub fn strip_attempts(set: &AttemptSet) -> Vec<&Attempt> {
    let latest = set.latest_round();
    set.attempts
        .iter()
        .filter(|a| Some(a.round) == latest || a.state.is_live())
        .collect()
}

/// The attempt the pane area shows for a tab that selected `shown`: none (the space's own panes) when it selected
/// none, or an attempt that is gone from the records or was discarded.
pub fn shown_attempt(set: &AttemptSet, shown: Option<u32>) -> Option<u32> {
    shown.filter(|n| {
        set.get(*n)
            .is_some_and(|a| a.state != AttemptState::Discarded)
    })
}

/// The worktree of the attempt the pane area shows (see [`shown_attempt`]), whose port and branch the status bar
/// shows instead of the space's; none when it shows the space's own panes.
pub fn shown_worktree(set: &AttemptSet, shown: Option<u32>) -> Option<&AttemptWorktree> {
    let number = shown_attempt(set, shown)?;
    set.get(number).map(|attempt| &attempt.worktree)
}

/// The strip's chips for `set`, the one numbered `shown` active.
pub fn strip_chips(set: &AttemptSet, shown: Option<u32>, redactor: &Redactor) -> Vec<AttemptChip> {
    let shown = shown_attempt(set, shown);
    strip_attempts(set)
        .into_iter()
        .map(|attempt| AttemptChip {
            number: attempt.number(),
            label: attempt_label(attempt, redactor).into(),
            state: chip_state(attempt.state),
            active: shown == Some(attempt.number()),
        })
        .collect()
}

/// Whether the strip offers Review: some attempt of the latest round has reported (done or failed).
pub fn can_review(set: &AttemptSet) -> bool {
    set.latest_round().is_some_and(|round| {
        set.round(round)
            .any(|a| matches!(a.state, AttemptState::Done | AttemptState::Failed))
    })
}

fn repro_view(repro: Repro) -> ReproView {
    match repro {
        Repro::NotChecked => ReproView::NotChecked,
        Repro::Fixed => ReproView::Fixed,
        Repro::StillFails => ReproView::StillFails,
    }
}

/// The review sheet's view of `comparison` (already masked): `busy` whether an attempt job runs for the space
/// (disables every action), `working` the attempt being adopted or discarded, `confirm_discard` the attempt
/// whose Discard awaits its confirming click, `error` why the last action failed, and `diff` the file diff open.
pub fn review_view(
    comparison: &Comparison,
    busy: bool,
    working: Option<u32>,
    confirm_discard: Option<u32>,
    error: Option<String>,
    diff: Option<DiffView>,
) -> ReviewView {
    ReviewView {
        round: comparison.round,
        columns: comparison
            .columns
            .iter()
            .map(|column| ReviewColumn {
                number: column.number,
                instruction: column.instruction.clone().map(SharedString::from),
                state: chip_state(column.state),
                summary: column.summary.clone().map(SharedString::from),
                tests: column.tests.as_ref().map(|t| (t.passed, t.failed)),
                test_command: column
                    .tests
                    .as_ref()
                    .and_then(|t| t.command.clone())
                    .map(SharedString::from),
                repro: repro_view(column.repro),
                files: column.files.as_ref().map(|files| {
                    files
                        .iter()
                        .map(|f| FileRow {
                            path: f.path.clone().into(),
                            added: f.added,
                            removed: f.removed,
                        })
                        .collect()
                }),
                added: column.added,
                removed: column.removed,
            })
            .collect(),
        adopted: comparison.adopted,
        busy,
        working,
        confirm_discard,
        error: error.map(SharedString::from),
        diff,
    }
}

/// Which writer of `graph` ⌘⇧F forks: the one bound to the focused pane `focused`, else the space's only
/// writer. Otherwise the error names the writers there are.
pub fn fork_target(graph: &AgentGraph, focused: Option<u64>) -> Result<AgentNodeId, String> {
    if let Some(pane) = focused
        && let Some(writer) = graph.writers().find(|w| w.pane_id == Some(pane))
    {
        return Ok(writer.id.clone());
    }
    let writers: Vec<&AgentNodeId> = graph.writers().map(|w| &w.id).collect();
    match writers.as_slice() {
        [only] => Ok((*only).clone()),
        [] => Err("this space has no writer to fork".to_owned()),
        many => Err(format!(
            "focus the pane of the writer to fork, or use Fork… in the graph panel (⌘G); the writers are {}",
            many.iter()
                .map(|w| w.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

/// The attempt lineage of writer `agent`, for its card in the graph panel: "attempts: #1 adopted, #2 discarded".
/// `None` when it has no attempts.
pub fn attempt_lineage(set: &AttemptSet, agent: &str) -> Option<String> {
    let parts: Vec<String> = set
        .attempts
        .iter()
        .filter(|a| a.agent == agent)
        .map(|a| format!("#{} {}", a.number(), a.state.name()))
        .collect();
    (!parts.is_empty()).then(|| format!("attempts: {}", parts.join(", ")))
}

/// One instruction per attempt from the fork dialog's input texts: the first `count`, trimmed, blank ones none.
pub fn dialog_instructions(texts: &[String], count: u32) -> Vec<Option<String>> {
    (0..count as usize)
        .map(|i| {
            texts
                .get(i)
                .map(|t| t.trim())
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
        })
        .collect()
}

/// The instructions the fork dialog draws as text instead of its inputs: while a fork runs (`running`), the
/// first `count` of `submitted` (what it was started with), masked; none otherwise, since the inputs are drawn.
pub fn busy_instructions(
    running: bool,
    submitted: &[String],
    count: u32,
    redactor: &Redactor,
) -> Vec<SharedString> {
    if !running {
        return Vec::new();
    }
    submitted
        .iter()
        .take(count as usize)
        .map(|text| SharedString::from(redactor.redact(text.trim()).into_owned()))
        .collect()
}

// -------------------------------------------------------------------------------------------------------------
// Dialog state

/// The open fork dialog.
pub(crate) struct ForkState {
    pub(crate) space: SpaceId,
    agent: AgentNodeId,
    writer: String,
    repo: String,
    count: u32,
    /// One instruction input per possible attempt; the first `count` are shown.
    inputs: Vec<Entity<PromptInput>>,
    /// The instructions the running fork was started with, shown read-only in place of the inputs meanwhile.
    submitted: Vec<String>,
    error: Option<String>,
    running: bool,
    _subscriptions: Vec<Subscription>,
}

/// The open review sheet, for one round of a space's attempts.
pub(crate) struct ReviewState {
    pub(crate) space: SpaceId,
    round: u32,
    /// Each live attempt's changed files, once computed.
    changes: BTreeMap<u32, Vec<FileChange>>,
    /// The attempt the user asked to adopt or discard, while its job runs.
    acting: Option<u32>,
    /// The attempt whose Discard was clicked once and awaits "Confirm discard".
    confirm_discard: Option<u32>,
    error: Option<String>,
    diff: Option<DiffView>,
    /// Which computation of the changes is the latest; results of older ones are dropped.
    generation: u64,
}

/// A number for each computation of a review's changes, so a late result of an earlier one is dropped.
fn next_generation() -> u64 {
    static GENERATION: AtomicU64 = AtomicU64::new(1);
    GENERATION.fetch_add(1, Ordering::Relaxed)
}

impl Workspace {
    // ---------------------------------------------------------------------------------------------------------
    // Fork dialog

    /// ⌘⇧F: fork the writer of the space being shown that is bound to the focused pane, or its only writer.
    pub(crate) fn fork_from_keys(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else {
            return;
        };
        let focused = tab.layout.focused().0;
        let space = tab.space.clone();
        let target = self
            .graphs
            .load(&space)
            .and_then(|state| fork_target(state.graph(), Some(focused)));
        match target {
            Ok(agent) => self.open_fork_dialog(agent, window, cx),
            Err(e) => self.info(e, cx),
        }
    }

    /// Open the fork dialog for the writer `agent` of the space being shown, with [`DEFAULT_FORK_COUNT`]
    /// attempts and an empty instruction each. Nothing happens while another dialog is open.
    pub(crate) fn open_fork_dialog(
        &mut self,
        agent: AgentNodeId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog.is_some() {
            return;
        }
        let Some(space) = self.active_tab().map(|tab| tab.space.clone()) else {
            return;
        };
        let node = match self.graphs.load(&space) {
            Ok(state) => state.graph().node(&agent).cloned(),
            Err(e) => {
                self.error(e, cx);
                return;
            }
        };
        let Some(node) = node else {
            self.error(format!("space {} has no agent {agent}", space.id), cx);
            return;
        };
        let xctx_graph::AgentRole::Writer { repo } = &node.role else {
            self.info(
                format!("{agent} is not a writer; only writers can be forked"),
                cx,
            );
            return;
        };
        // A writer whose runner cannot fork sessions has no attempts; say why rather than fail later.
        if let Some(why) = self.fork_refusal(&space, &agent) {
            self.info(format!("{agent} cannot be forked: {why}"), cx);
            return;
        }
        let mut inputs = Vec::new();
        let mut subscriptions = Vec::new();
        for n in 1..=MAX_ATTEMPTS {
            let input = cx.new(|cx| {
                PromptInput::new(cx).placeholder(format!(
                    "Attempt {n}: what should it try? Empty for the same task"
                ))
            });
            subscriptions.push(cx.subscribe_in(
                &input,
                window,
                |this, _, event: &PromptEvent, window, cx| match event {
                    PromptEvent::Submit(_) => this.confirm_fork(window, cx),
                    PromptEvent::Cancel => this.dismiss_dialog(window, cx),
                },
            ));
            inputs.push(input);
        }
        let focus = inputs.first().map(|input| input.focus_handle(cx));
        self.dialog = Some(Dialog::Fork(ForkState {
            space: space.id.clone(),
            agent,
            writer: node.label.clone(),
            repo: repo.clone(),
            count: DEFAULT_FORK_COUNT,
            inputs,
            submitted: Vec::new(),
            error: None,
            running: false,
            _subscriptions: subscriptions,
        }));
        self.dialog_subscription = None;
        match focus {
            Some(focus) => focus.focus(window, cx),
            None => self.dialog_focus.focus(window, cx),
        }
        cx.notify();
    }

    /// A count pill of the fork dialog: show `count` instruction inputs.
    fn set_fork_count(&mut self, count: u32, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::Fork(fork)) = self.dialog.as_mut() else {
            return;
        };
        if fork.running {
            return;
        }
        fork.count = count.clamp(1, MAX_ATTEMPTS);
        // The input typed in may have been hidden; the first is always shown.
        if let Some(input) = fork.inputs.first() {
            let focus = input.focus_handle(cx);
            focus.focus(window, cx);
        }
        cx.notify();
    }

    /// Fork (or Enter in an instruction): start the fork; the dialog closes once it succeeded and shows the
    /// error if it failed.
    fn confirm_fork(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Dialog::Fork(fork)) = self.dialog.as_mut() else {
            return;
        };
        if fork.running {
            return;
        }
        let texts: Vec<String> = fork
            .inputs
            .iter()
            .map(|input| input.read(cx).value().to_owned())
            .collect();
        let instructions = dialog_instructions(&texts, fork.count);
        fork.running = true;
        fork.submitted = texts;
        fork.error = None;
        // The inputs are not drawn while the fork runs (see `render_fork_dialog`); the dialog keeps the keys, so
        // Esc still closes it.
        self.dialog_focus.focus(window, cx);
        let (space, agent) = (fork.space.clone(), fork.agent.clone());
        let for_space = space.clone();
        let started = self.fork_attempts_then(
            &space,
            &agent,
            instructions,
            move |this, result, window, cx| {
                let Some(Dialog::Fork(fork)) = this.dialog.as_mut() else {
                    return;
                };
                if fork.space != for_space || !fork.running {
                    return;
                }
                match result {
                    Ok(()) => {
                        this.dialog = None;
                        this.dialog_subscription = None;
                        if this.active_space_id().as_ref() == Some(&for_space) {
                            this.focus_active_pane(window, cx);
                        }
                    }
                    Err(e) => {
                        fork.running = false;
                        fork.error = Some(e);
                        if let Some(input) = fork.inputs.first() {
                            let focus = input.focus_handle(cx);
                            focus.focus(window, cx);
                        }
                    }
                }
                cx.notify();
            },
            window,
            cx,
        );
        if let Err(e) = started
            && let Some(Dialog::Fork(fork)) = self.dialog.as_mut()
        {
            fork.running = false;
            fork.error = Some(e);
            if let Some(input) = fork.inputs.first() {
                let focus = input.focus_handle(cx);
                focus.focus(window, cx);
            }
        }
        cx.notify();
    }

    /// The fork dialog's panel.
    pub(crate) fn render_fork_dialog(&self, fork: &ForkState, cx: &mut Context<Self>) -> gpui::Div {
        let view = ForkDialogView {
            writer: fork.writer.clone().into(),
            repo: fork.repo.clone().into(),
            count: fork.count,
            max: MAX_ATTEMPTS.saturating_sub(
                self.attempts
                    .get(&fork.space)
                    .map_or(0, |set| set.live().count() as u32),
            ),
            instructions: busy_instructions(
                fork.running,
                &fork.submitted,
                fork.count,
                &self.redactor,
            ),
            busy: fork.running,
            error: fork.error.clone().map(SharedString::from),
        };
        // While the fork runs, its instructions are drawn as text: what is typed then would not be used.
        let inputs: Vec<Entity<PromptInput>> = if fork.running {
            Vec::new()
        } else {
            fork.inputs
                .iter()
                .take(fork.count as usize)
                .cloned()
                .collect()
        };
        let entity = cx.entity().downgrade();
        let (count, cancel, confirm) = (entity.clone(), entity.clone(), entity);
        div().w(px(600.0)).shadow_lg().child(
            ForkDialog::new(view)
                .instruction_inputs(inputs)
                .on_count(move |n, window, cx| {
                    let _ = count.update(cx, |this, cx| this.set_fork_count(n, window, cx));
                })
                .on_cancel(move |window, cx| {
                    let _ = cancel.update(cx, |this, cx| this.dismiss_dialog(window, cx));
                })
                .on_fork(move |window, cx| {
                    let _ = confirm.update(cx, |this, cx| this.confirm_fork(window, cx));
                }),
        )
    }

    // ---------------------------------------------------------------------------------------------------------
    // Attempt strip

    /// Show attempt `number` of the space being shown in the pane area (its agent's terminal, with the keyboard),
    /// or with `None` the space's own panes again.
    pub(crate) fn select_attempt(
        &mut self,
        number: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.dialog.is_some() {
            return;
        }
        let Some(id) = self.active_space_id() else {
            return;
        };
        if let Some(tab) = self.tabs.get_mut(&id) {
            tab.shown_attempt = number;
        }
        self.focus_active_pane(window, cx);
        cx.notify();
    }

    /// Whether ⌘⇧F and the strip's "Fork…" (which fork the focused writer, else the only one) have a writer
    /// whose runner can fork, so they are offered. When the graph is not loaded, or the writer is ambiguous, the
    /// dialog decides and says why.
    pub(crate) fn strip_can_fork(&self, tab: &SpaceTab) -> bool {
        let Some(state) = self.graphs.get(&tab.space.id) else {
            return true;
        };
        match fork_target(state.graph(), Some(tab.layout.focused().0)) {
            Ok(agent) => !self.fork_refusals_in(state).contains_key(&agent),
            Err(_) => true,
        }
    }

    /// The pane area of `tab`: the attempt strip over the space's split view, or over the attempt it shows, when
    /// the space has attempts; else just the split view.
    pub(crate) fn render_space_body(&self, tab: &SpaceTab, cx: &mut Context<Self>) -> AnyElement {
        let Some(set) = self
            .attempts
            .get(&tab.space.id)
            .filter(|set| !set.attempts.is_empty())
        else {
            return self.render_split(tab, cx);
        };
        let shown = shown_attempt(set, tab.shown_attempt);
        let entity = cx.entity().downgrade();
        let (select, review, fork) = (entity.clone(), entity.clone(), entity);
        let strip = AttemptStrip::new(strip_chips(set, shown, &self.redactor))
            .can_review(can_review(set))
            .busy(self.attempt_jobs.contains(&tab.space.id))
            .can_fork((set.live().count() as u32) < MAX_ATTEMPTS && self.strip_can_fork(tab))
            .on_select(move |number, window, cx| {
                let _ = select.update(cx, |this, cx| this.select_attempt(number, window, cx));
            })
            .on_review(move |window, cx| {
                let _ = review.update(cx, |this, cx| this.open_review(window, cx));
            })
            .on_fork(move |window, cx| {
                let _ = fork.update(cx, |this, cx| {
                    if this.dialog.is_none() {
                        this.fork_from_keys(window, cx);
                    }
                });
            });
        let body = match shown {
            Some(number) => self.render_attempt_terminal(tab, set, number, cx),
            None => self.render_split(tab, cx),
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(strip)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    /// Attempt `number`'s agent terminal, filling the pane area under a one-line header.
    fn render_attempt_terminal(
        &self,
        tab: &SpaceTab,
        set: &AttemptSet,
        number: u32,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = Theme::of(cx);
        let attempt = set.get(number);
        let mut header = format!("Attempt {number}");
        if let Some(attempt) = attempt {
            header.push_str(&format!(
                " · {} · {} · PORT {}",
                attempt.worktree.repo, attempt.worktree.branch, attempt.worktree.port
            ));
        }
        let note = |text: String, color| {
            div()
                .size_full()
                .p(Theme::SPACE_3)
                .text_size(Theme::TEXT_SM)
                .text_color(color)
                .child(text)
                .into_any_element()
        };
        let content = match tab.attempt_terminals.get(&number) {
            Some(PaneTerminal::Running {
                view,
                title,
                exited,
                ..
            }) => {
                if let Some(title) = title.as_deref().filter(|t| !t.trim().is_empty()) {
                    header.push_str(&format!(" · {title}"));
                }
                match exited {
                    Some(Some(code)) => header.push_str(&format!(" — exited ({code})")),
                    Some(None) => header.push_str(" — exited"),
                    None => {}
                }
                div().size_full().child(view.clone()).into_any_element()
            }
            Some(PaneTerminal::Failed(message)) => note(message.clone(), theme.danger),
            None => match attempt.map(|a| a.state) {
                Some(AttemptState::Adopted) => note(
                    format!(
                        "Attempt {number} was adopted; its work is on the space's branch and the writer continues its session."
                    ),
                    theme.text_subtle,
                ),
                _ => note("starting…".to_owned(), theme.text_subtle),
            },
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex_none()
                    .px(Theme::SPACE_2)
                    .py(Theme::SPACE_1)
                    .border_b_1()
                    .border_color(theme.divider)
                    .truncate()
                    .text_size(Theme::TEXT_SM)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.text_muted)
                    .child(header),
            )
            .child(div().flex_1().min_h_0().child(content))
            .into_any_element()
    }

    // ---------------------------------------------------------------------------------------------------------
    // Review sheet

    /// ⌘⇧R or the strip's Review: open the review sheet on the latest round of the space being shown, and compute
    /// its attempts' changes in the background.
    pub(crate) fn open_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        let Some(id) = self.active_space_id() else {
            return;
        };
        let round = match self.attempt_set(&id) {
            Ok(set) => set.latest_round(),
            Err(e) => {
                self.error(e, cx);
                return;
            }
        };
        let Some(round) = round else {
            self.info(
                format!("{id} has no attempts to review; fork a writer with ⌘⇧F"),
                cx,
            );
            return;
        };
        self.dialog = Some(Dialog::Review(ReviewState {
            space: id,
            round,
            changes: BTreeMap::new(),
            acting: None,
            confirm_discard: None,
            error: None,
            diff: None,
            generation: 0,
        }));
        self.dialog_subscription = None;
        self.dialog_focus.focus(window, cx);
        self.refresh_review_changes(cx);
        cx.notify();
    }

    /// Compute the changed files of every live attempt of the open review's round, off the UI thread.
    fn refresh_review_changes(&mut self, cx: &mut Context<Self>) {
        let Some(Dialog::Review(review)) = self.dialog.as_ref() else {
            return;
        };
        let worktrees: Vec<AttemptWorktree> = self
            .attempts
            .get(&review.space)
            .map(|set| {
                set.round(review.round)
                    .filter(|a| a.state.is_live())
                    .map(|a| a.worktree.clone())
                    .collect()
            })
            .unwrap_or_default();
        let generation = next_generation();
        let space = review.space.clone();
        if let Some(Dialog::Review(review)) = self.dialog.as_mut() {
            review.generation = generation;
            review
                .changes
                .retain(|number, _| worktrees.iter().any(|w| w.number == *number));
        }
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let results = executor
                .spawn(async move {
                    worktrees
                        .iter()
                        .map(|w| (w.number, xctx_core::attempt::attempt_changes(w)))
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(Dialog::Review(review)) = this.dialog.as_mut() else {
                    return;
                };
                if review.space != space || review.generation != generation {
                    return;
                }
                let mut failures = Vec::new();
                for (number, result) in results {
                    match result {
                        Ok(files) => {
                            review.changes.insert(number, files);
                        }
                        Err(e) => failures.push(format!(
                            "could not read the changes of attempt {number}: {e}"
                        )),
                    }
                }
                if !failures.is_empty() {
                    review.error = Some(failures.join("; "));
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Adopt (`adopt`) or discard attempt `number` from the review sheet.
    /// Ask for (or, with `None`, withdraw) the confirming click on attempt `number`'s Discard.
    fn set_review_confirm_discard(&mut self, number: Option<u32>, cx: &mut Context<Self>) {
        if let Some(Dialog::Review(review)) = self.dialog.as_mut() {
            review.confirm_discard = number;
            cx.notify();
        }
    }

    fn review_action(
        &mut self,
        number: u32,
        adopt: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(Dialog::Review(review)) = self.dialog.as_mut() else {
            return;
        };
        if self.attempt_jobs.contains(&review.space) {
            return;
        }
        review.acting = Some(number);
        review.confirm_discard = None;
        review.error = None;
        let space = review.space.clone();
        let started = if adopt {
            self.adopt_attempt(&space, number, window, cx)
        } else {
            self.discard_attempt(&space, number, window, cx)
        };
        if let Err(e) = started
            && let Some(Dialog::Review(review)) = self.dialog.as_mut()
        {
            review.acting = None;
            review.error = Some(e);
        }
        cx.notify();
    }

    /// An adopt or discard of attempt `number` of space `id` ended with `outcome`: the review sheet, when open
    /// on the space, shows the error or its new state (and keeps open, so the rest can be discarded), and a strip
    /// showing an attempt that is gone goes back to the space's panes.
    pub(crate) fn attempt_job_done(
        &mut self,
        id: &SpaceId,
        number: u32,
        outcome: Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let live = self
            .attempts
            .get(id)
            .and_then(|set| set.get(number))
            .is_some_and(|a| a.state.is_live());
        if !live
            && let Some(tab) = self.tabs.get_mut(id)
            && tab.shown_attempt == Some(number)
        {
            tab.shown_attempt = None;
        }
        let mut refresh = false;
        if let Some(Dialog::Review(review)) = self.dialog.as_mut()
            && &review.space == id
        {
            review.acting = None;
            match outcome {
                Ok(()) => {
                    review.error = None;
                    if !live && review.diff.as_ref().is_some_and(|d| d.attempt == number) {
                        review.diff = None;
                    }
                    refresh = true;
                }
                Err(e) => review.error = Some(e),
            }
        }
        if refresh {
            self.refresh_review_changes(cx);
        }
        if self.dialog.is_none() && self.active_space_id().as_ref() == Some(id) {
            self.focus_active_pane(window, cx);
        }
        cx.notify();
    }

    /// A changed file clicked on the review sheet: read its diff against the fork point in the background, masked.
    fn open_review_diff(&mut self, number: u32, path: String, cx: &mut Context<Self>) {
        let Some(Dialog::Review(review)) = self.dialog.as_ref() else {
            return;
        };
        let worktree = self
            .attempts
            .get(&review.space)
            .and_then(|set| set.get(number))
            .filter(|a| a.state.is_live())
            .map(|a| a.worktree.clone());
        let space = review.space.clone();
        let Some(worktree) = worktree else {
            if let Some(Dialog::Review(review)) = self.dialog.as_mut() {
                review.error = Some(format!("attempt {number}'s worktree is gone"));
            }
            cx.notify();
            return;
        };
        if let Some(Dialog::Review(review)) = self.dialog.as_mut() {
            review.diff = Some(DiffView {
                attempt: number,
                path: path.clone().into(),
                text: DIFF_LOADING.into(),
            });
        }
        let redactor = self.redactor.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let wanted = path.clone();
            let text = executor
                .spawn(async move {
                    let tip = xctx_core::attempt::attempt_tip(&worktree)?;
                    xctx_core::git::diff_file(
                        &worktree.worktree,
                        &worktree.fork_commit,
                        &tip,
                        &wanted,
                    )
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                let Some(Dialog::Review(review)) = this.dialog.as_mut() else {
                    return;
                };
                let open = review
                    .diff
                    .as_ref()
                    .is_some_and(|d| d.attempt == number && d.path.as_ref() == path);
                if review.space != space || !open {
                    return;
                }
                match text {
                    Ok(text) => {
                        review.diff = Some(DiffView {
                            attempt: number,
                            path: path.into(),
                            text: redactor.redact(&text).into_owned().into(),
                        });
                    }
                    Err(e) => {
                        review.diff = None;
                        review.error = Some(format!(
                            "could not read the diff of {path} in attempt {number}: {e}"
                        ));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn close_review_diff(&mut self, cx: &mut Context<Self>) {
        if let Some(Dialog::Review(review)) = self.dialog.as_mut() {
            review.diff = None;
            cx.notify();
        }
    }

    /// The review sheet's panel, scrolling when it is taller than the window.
    pub(crate) fn render_review_sheet(
        &self,
        review: &ReviewState,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let comparison = match self.attempts.get(&review.space) {
            Some(set) => {
                xctx_attempt::comparison(set, review.round, &review.changes, &self.redactor)
            }
            None => Comparison {
                round: review.round,
                columns: Vec::new(),
                adopted: None,
            },
        };
        let busy = self.attempt_jobs.contains(&review.space);
        let view = review_view(
            &comparison,
            busy,
            review.acting.filter(|_| busy),
            review.confirm_discard,
            review.error.clone(),
            review.diff.clone(),
        );
        let entity = cx.entity().downgrade();
        let (adopt, request, discard, keep, file, close_diff, close) = (
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity,
        );
        div()
            .id("review-sheet-scroll")
            .w(px(1080.0))
            .max_w(relative(0.96))
            .max_h(relative(0.92))
            .overflow_y_scroll()
            .shadow_lg()
            .child(
                ReviewSheet::new(view)
                    .on_adopt(move |number, window, cx| {
                        let _ = adopt
                            .update(cx, |this, cx| this.review_action(number, true, window, cx));
                    })
                    .on_discard_request(move |number, _, cx| {
                        let _ = request.update(cx, |this, cx| {
                            this.set_review_confirm_discard(Some(number), cx)
                        });
                    })
                    .on_cancel_discard(move |_, _, cx| {
                        let _ =
                            keep.update(cx, |this, cx| this.set_review_confirm_discard(None, cx));
                    })
                    .on_discard(move |number, window, cx| {
                        let _ = discard
                            .update(cx, |this, cx| this.review_action(number, false, window, cx));
                    })
                    .on_file(move |number, path, _, cx| {
                        let path = path.to_string();
                        let _ = file.update(cx, |this, cx| this.open_review_diff(number, path, cx));
                    })
                    .on_close_diff(move |_, cx| {
                        let _ = close_diff.update(cx, |this, cx| this.close_review_diff(cx));
                    })
                    .on_close(move |window, cx| {
                        let _ = close.update(cx, |this, cx| this.dismiss_dialog(window, cx));
                    }),
            )
    }

    // ---------------------------------------------------------------------------------------------------------
    // Graph panel

    /// Each writer's attempt lineage in space `id`, for the graph panel's cards (see [`attempt_lineage`]).
    pub(crate) fn attempt_notes(&self, id: &SpaceId) -> HashMap<AgentNodeId, SharedString> {
        let Some(set) = self.attempts.get(id) else {
            return HashMap::new();
        };
        let mut agents: Vec<&str> = set.attempts.iter().map(|a| a.agent.as_str()).collect();
        agents.sort_unstable();
        agents.dedup();
        agents
            .into_iter()
            .filter_map(|agent| {
                attempt_lineage(set, agent).map(|line| (AgentNodeId::new(agent), line.into()))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use xctx_attempt::{Outcome, TestSummary};
    use xctx_graph::AgentRole;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn worktree(number: u32) -> AttemptWorktree {
        AttemptWorktree {
            number,
            repo: "api".into(),
            repo_path: "/code/api".into(),
            worktree: PathBuf::from(format!("/code/api-worktrees/s--attempt-{number}")),
            branch: format!("attempt/s/{number}"),
            fork_commit: "abc".into(),
            port: 4100 + number as u16,
        }
    }

    fn fork(set: &mut AttemptSet, instructions: Vec<Option<String>>) -> TestResult {
        let plan = set.plan_fork("writer-api", "api", instructions)?;
        let trees = plan.numbers.iter().map(|&n| worktree(n)).collect();
        set.record_fork(&plan, trees, "xctx-s-writer-api", 10)?;
        Ok(())
    }

    fn outcome(summary: &str) -> Outcome {
        Outcome {
            summary: summary.into(),
            tests: Some(TestSummary {
                command: Some("cargo test".into()),
                passed: 3,
                failed: 1,
            }),
            repro: Repro::Fixed,
        }
    }

    #[test]
    fn every_attempt_state_has_a_chip_state() {
        use AttemptChipState as C;
        assert_eq!(chip_state(AttemptState::Running), C::Running);
        assert_eq!(chip_state(AttemptState::Done), C::Done);
        assert_eq!(chip_state(AttemptState::Failed), C::Failed);
        assert_eq!(chip_state(AttemptState::Adopted), C::Adopted);
        assert_eq!(chip_state(AttemptState::Discarded), C::Discarded);
    }

    #[test]
    fn chips_list_the_latest_round_and_older_live_attempts() -> TestResult {
        let mut set = AttemptSet::new(SpaceId::new("s"));
        fork(&mut set, vec![Some("use tok-424242".into()), None])?;
        set.mark_discarded(2, 11)?;
        // Round 2 while attempt 1 of round 1 is still live.
        fork(&mut set, vec![None])?;
        let redactor = Redactor::new([("TOKEN".to_owned(), "tok-424242".to_owned())]);
        let chips = strip_chips(&set, Some(3), &redactor);
        let summary: Vec<(u32, &str, AttemptChipState, bool)> = chips
            .iter()
            .map(|c| (c.number, c.label.as_ref(), c.state, c.active))
            .collect();
        assert_eq!(
            summary,
            [
                (1, "use [REDACTED:TOKEN]", AttemptChipState::Running, false),
                (3, "Attempt 3", AttemptChipState::Running, true),
            ]
        );
        Ok(())
    }

    #[test]
    fn a_discarded_or_unknown_attempt_is_not_shown() -> TestResult {
        let mut set = AttemptSet::new(SpaceId::new("s"));
        fork(&mut set, vec![None, None])?;
        set.mark_discarded(2, 11)?;
        assert_eq!(shown_attempt(&set, Some(1)), Some(1));
        assert_eq!(shown_attempt(&set, Some(2)), None);
        assert_eq!(shown_attempt(&set, Some(9)), None);
        assert_eq!(shown_attempt(&set, None), None);
        set.mark_adopted(1, 12)?;
        assert_eq!(shown_attempt(&set, Some(1)), Some(1), "adopted still shows");
        let shown = shown_worktree(&set, Some(1)).map(|w| (w.number, w.port, w.branch.clone()));
        let attempt = set.get(1).map(|a| &a.worktree);
        assert_eq!(
            shown,
            attempt.map(|w| (1, w.port, w.branch.clone())),
            "the shown attempt's own port and branch"
        );
        assert!(shown_worktree(&set, Some(2)).is_none());
        assert!(shown_worktree(&set, None).is_none());
        Ok(())
    }

    #[test]
    fn review_shows_once_the_latest_round_has_a_report() -> TestResult {
        let mut set = AttemptSet::new(SpaceId::new("s"));
        assert!(!can_review(&set));
        fork(&mut set, vec![None, None])?;
        assert!(!can_review(&set), "all running");
        set.report(1, outcome("done"), false, 11)?;
        assert!(can_review(&set));
        fork_after_discarding(&mut set)?;
        assert!(!can_review(&set), "the new round has not reported");
        set.report(4, outcome("could not"), true, 13)?;
        assert!(can_review(&set), "a failed report counts");
        Ok(())
    }

    fn fork_after_discarding(set: &mut AttemptSet) -> TestResult {
        set.mark_discarded(1, 12)?;
        set.mark_discarded(2, 12)?;
        fork(set, vec![None, None])
    }

    #[test]
    fn the_review_view_follows_the_comparison() -> TestResult {
        let mut set = AttemptSet::new(SpaceId::new("s"));
        fork(&mut set, vec![Some("token bucket".into()), None])?;
        set.report(1, outcome("fixed it"), false, 11)?;
        let changes: BTreeMap<u32, Vec<FileChange>> = [(
            1,
            vec![
                FileChange {
                    path: "src/lib.rs".into(),
                    added: Some(5),
                    removed: Some(2),
                },
                FileChange {
                    path: "logo.png".into(),
                    added: None,
                    removed: None,
                },
            ],
        )]
        .into_iter()
        .collect();
        let comparison = xctx_attempt::comparison(&set, 1, &changes, &Redactor::default());
        let view = review_view(
            &comparison,
            true,
            Some(2),
            Some(1),
            Some("refused".into()),
            None,
        );
        assert_eq!(view.round, 1);
        assert!(view.busy);
        assert_eq!(view.working, Some(2));
        assert_eq!(view.confirm_discard, Some(1));
        assert_eq!(view.error.as_ref().map(|e| e.as_ref()), Some("refused"));
        let first = &view.columns[0];
        assert_eq!(first.state, AttemptChipState::Done);
        assert_eq!(
            first.instruction.as_ref().map(|i| i.as_ref()),
            Some("token bucket")
        );
        assert_eq!(first.summary.as_ref().map(|s| s.as_ref()), Some("fixed it"));
        assert_eq!(first.tests, Some((3, 1)));
        assert_eq!(
            first.test_command.as_ref().map(|c| c.as_ref()),
            Some("cargo test")
        );
        assert_eq!(first.repro, ReproView::Fixed);
        assert_eq!((first.added, first.removed), (5, 2));
        let files = first.files.as_ref().map(Vec::len);
        assert_eq!(files, Some(2));
        // Not computed yet.
        let second = &view.columns[1];
        assert_eq!(second.files, None);
        assert_eq!(second.summary, None);
        assert_eq!(view.adopted, None);
        Ok(())
    }

    #[test]
    fn fork_keys_pick_the_focused_writer_or_the_only_one() -> TestResult {
        let mut graph = AgentGraph::for_space(&["api".to_owned(), "web".to_owned()]);
        let api = AgentNodeId::new("writer-api");
        let web = AgentNodeId::new("writer-web");
        graph.bind_pane(&api, Some(1))?;
        graph.bind_pane(&web, Some(2))?;
        assert_eq!(fork_target(&graph, Some(2))?, web);
        assert_eq!(fork_target(&graph, Some(1))?, api);
        let unclear = fork_target(&graph, Some(7)).err().unwrap_or_default();
        assert!(
            unclear.contains("writer-api, writer-web"),
            "names the writers: {unclear}"
        );
        let single = AgentGraph::for_space(&["api".to_owned()]);
        assert_eq!(fork_target(&single, None)?, api);
        let none = AgentGraph::for_space(&[]);
        assert!(fork_target(&none, None).is_err());
        // A helper's pane is not a writer's.
        let mut helped = AgentGraph::for_space(&["api".to_owned(), "web".to_owned()]);
        let helper = helped.add_lane(
            AgentRole::Helper {
                kind: xctx_graph::HelperKind::Tests,
            },
            None,
        )?;
        helped.bind_pane(&helper, Some(3))?;
        assert!(fork_target(&helped, Some(3)).is_err());
        Ok(())
    }

    #[test]
    fn lineage_names_each_attempt_and_its_state() -> TestResult {
        let mut set = AttemptSet::new(SpaceId::new("s"));
        assert_eq!(attempt_lineage(&set, "writer-api"), None);
        fork(&mut set, vec![None, None])?;
        set.mark_adopted(1, 11)?;
        set.mark_discarded(2, 12)?;
        assert_eq!(
            attempt_lineage(&set, "writer-api").as_deref(),
            Some("attempts: #1 adopted, #2 discarded")
        );
        assert_eq!(attempt_lineage(&set, "writer-web"), None);
        Ok(())
    }

    #[test]
    fn a_running_fork_shows_its_instructions_read_only() {
        let redactor = Redactor::new([("T".to_owned(), "tok-4242".to_owned())]);
        let texts = ["use tok-4242 ".to_owned(), String::new(), "c".to_owned()];
        assert!(busy_instructions(false, &texts, 3, &redactor).is_empty());
        let shown = busy_instructions(true, &texts, 2, &redactor);
        assert_eq!(
            shown,
            [
                SharedString::from("use [REDACTED:T]"),
                SharedString::from("")
            ]
        );
    }

    #[test]
    fn dialog_instructions_keep_places_and_drop_blanks() {
        let texts = vec![
            " a ".to_owned(),
            "  ".to_owned(),
            "c".to_owned(),
            "d".to_owned(),
        ];
        assert_eq!(
            dialog_instructions(&texts, 3),
            [Some("a".into()), None, Some("c".into())]
        );
        assert_eq!(dialog_instructions(&[], 2), [None, None]);
    }
}
