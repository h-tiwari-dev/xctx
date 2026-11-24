//! The review sheet: one round of competing attempts side by side, by outcome (summary, tests, repro, diffstat
//! and changed files), with each file's diff a click away, and Discard and Adopt per attempt. The user decides;
//! the sheet only offers what the attempt's state allows.

use std::rc::Rc;

use gpui::{App, FontWeight, Hsla, SharedString, Window, div, prelude::*, px};

use crate::attempt_strip::AttemptChipState;
use crate::components::{Badge, Button, ButtonSize, ButtonVariant};
use crate::theme::{Theme, Tone};

/// Whether an attempt's fix makes the original repro pass, as its agent reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ReproView {
    /// Not reported.
    #[default]
    NotChecked,
    /// The repro passes now.
    Fixed,
    /// The repro still fails.
    StillFails,
}

impl ReproView {
    /// The badge text.
    pub fn label(self) -> &'static str {
        match self {
            Self::NotChecked => "REPRO NOT CHECKED",
            Self::Fixed => "REPRO FIXED",
            Self::StillFails => "REPRO STILL FAILS",
        }
    }

    /// The badge tone: fixed is success, still failing danger, not checked neutral.
    pub fn tone(self) -> Tone {
        match self {
            Self::NotChecked => Tone::Neutral,
            Self::Fixed => Tone::Success,
            Self::StillFails => Tone::Danger,
        }
    }
}

/// One changed file of an attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    /// The path, relative to the repo; passed back by [`ReviewSheet::on_file`].
    pub path: SharedString,
    /// Lines added, or `None` for a binary file.
    pub added: Option<u32>,
    /// Lines removed, or `None` for a binary file.
    pub removed: Option<u32>,
}

/// One attempt's column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewColumn {
    /// The attempt's number, from 1.
    pub number: u32,
    /// Its own instruction, if it was given one.
    pub instruction: Option<SharedString>,
    /// Its state, which decides the actions offered.
    pub state: AttemptChipState,
    /// The agent's own summary, already redacted.
    pub summary: Option<SharedString>,
    /// Tests passed and failed, if the agent ran any.
    pub tests: Option<(u32, u32)>,
    /// The test command it ran.
    pub test_command: Option<SharedString>,
    /// The repro status.
    pub repro: ReproView,
    /// The changed files against the fork point, or `None` while not computed.
    pub files: Option<Vec<FileRow>>,
    /// Lines added in total.
    pub added: u32,
    /// Lines removed in total.
    pub removed: u32,
}

/// One file's diff, shown under the columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffView {
    /// The attempt it belongs to.
    pub attempt: u32,
    /// The file.
    pub path: SharedString,
    /// The unified diff, already redacted.
    pub text: SharedString,
}

/// What the sheet shows: one round of attempts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewView {
    /// The round (fork) number.
    pub round: u32,
    /// One column per attempt, in the order shown.
    pub columns: Vec<ReviewColumn>,
    /// The attempt of this round already adopted, if any. No other can be adopted then.
    pub adopted: Option<u32>,
    /// The attempt being adopted or discarded; every action is disabled meanwhile.
    pub busy: Option<u32>,
    /// Why the last action failed, shown in the danger colour.
    pub error: Option<SharedString>,
    /// The file diff open, if any.
    pub diff: Option<DiffView>,
}

/// What the summary says before the agent reported one.
pub const REVIEW_NO_SUMMARY: &str = "Not reported yet";

/// The most diff lines drawn; the rest are counted in a note.
pub const DIFF_LINE_LIMIT: usize = 2000;

/// Whether the Adopt button shows for an attempt in `state` in a round whose adopted attempt is `adopted`:
/// only live attempts (a failed one too: the user decides), and none once the round has adopted one.
pub fn shows_adopt(state: AttemptChipState, adopted: Option<u32>) -> bool {
    state.is_live() && adopted.is_none()
}

/// Whether the Discard button shows for an attempt in `state`: only live attempts.
pub fn shows_discard(state: AttemptChipState) -> bool {
    state.is_live()
}

/// The tests line, e.g. "41 passed, 1 failed", "12 passed", or "Tests not run".
pub fn tests_line(tests: Option<(u32, u32)>) -> SharedString {
    match tests {
        None => "Tests not run".into(),
        Some((passed, 0)) => format!("{passed} passed").into(),
        Some((passed, failed)) => format!("{passed} passed, {failed} failed").into(),
    }
}

/// The tests line's tone: danger when any failed, success when some passed, else neutral.
pub fn tests_tone(tests: Option<(u32, u32)>) -> Tone {
    match tests {
        Some((_, failed)) if failed > 0 => Tone::Danger,
        Some((passed, _)) if passed > 0 => Tone::Success,
        _ => Tone::Neutral,
    }
}

/// A diffstat such as "+12 −3".
pub fn diffstat_label(added: u32, removed: u32) -> SharedString {
    format!("+{added} \u{2212}{removed}").into()
}

/// The tint of a unified diff line: success for an added line, danger for a removed one, `None` for context,
/// hunk headers and the `+++`/`---` file headers.
pub fn diff_line_tone(line: &str) -> Option<Tone> {
    if line.starts_with("+++") || line.starts_with("---") {
        None
    } else if line.starts_with('+') {
        Some(Tone::Success)
    } else if line.starts_with('-') {
        Some(Tone::Danger)
    } else {
        None
    }
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type AttemptHandler = Rc<dyn Fn(u32, &mut Window, &mut App)>;
type FileHandler = Rc<dyn Fn(u32, &SharedString, &mut Window, &mut App)>;

/// The review sheet for one round.
///
/// Selectors for tests: `review-adopt-<n>`, `review-discard-<n>`, `review-file-<n>-<index>`,
/// `review-diff-close`, `review-close`, and `review-column-<n>` for each column.
#[derive(IntoElement)]
pub struct ReviewSheet {
    view: ReviewView,
    on_adopt: Option<AttemptHandler>,
    on_discard: Option<AttemptHandler>,
    on_file: Option<FileHandler>,
    on_close_diff: Option<Handler>,
    on_close: Option<Handler>,
}

impl ReviewSheet {
    /// A sheet for `view` with no handlers.
    pub fn new(view: ReviewView) -> Self {
        Self {
            view,
            on_adopt: None,
            on_discard: None,
            on_file: None,
            on_close_diff: None,
            on_close: None,
        }
    }

    /// Call `handler` with the attempt's number when its Adopt is clicked.
    pub fn on_adopt(mut self, handler: impl Fn(u32, &mut Window, &mut App) + 'static) -> Self {
        self.on_adopt = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the attempt's number when its Discard is clicked.
    pub fn on_discard(mut self, handler: impl Fn(u32, &mut Window, &mut App) + 'static) -> Self {
        self.on_discard = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the attempt's number and the file's path when a changed file is clicked.
    pub fn on_file(
        mut self,
        handler: impl Fn(u32, &SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_file = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when the diff's Close is clicked.
    pub fn on_close_diff(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close_diff = Some(Rc::new(handler));
        self
    }

    /// Show a Close button for the sheet and call `handler` when it is clicked.
    pub fn on_close(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_close = Some(Rc::new(handler));
        self
    }

    /// Whether attempt `number`'s Adopt is drawn and enabled.
    pub fn can_adopt(&self, number: u32) -> bool {
        self.view.busy.is_none()
            && self
                .column(number)
                .is_some_and(|column| shows_adopt(column.state, self.view.adopted))
    }

    /// Whether attempt `number`'s Discard is drawn and enabled.
    pub fn can_discard(&self, number: u32) -> bool {
        self.view.busy.is_none()
            && self
                .column(number)
                .is_some_and(|column| shows_discard(column.state))
    }

    fn column(&self, number: u32) -> Option<&ReviewColumn> {
        self.view
            .columns
            .iter()
            .find(|column| column.number == number)
    }
}

fn section(title: &'static str, theme: &Theme) -> gpui::Div {
    div()
        .text_size(Theme::TEXT_XS)
        .text_color(theme.text_subtle)
        .child(title)
}

fn tone_color(theme: &Theme, tone: Tone) -> Hsla {
    match tone {
        Tone::Neutral => theme.text_muted,
        tone => theme.tone(tone).fg,
    }
}

impl RenderOnce for ReviewSheet {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let view = self.view;
        let busy = view.busy;
        let open = view
            .diff
            .as_ref()
            .map(|diff| (diff.attempt, diff.path.clone()));

        let columns: Vec<_> = view
            .columns
            .iter()
            .map(|column| {
                let number = column.number;

                let instruction = column.instruction.clone().map(|text| {
                    div()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_muted)
                        .child(text)
                });
                let working =
                    (busy == Some(number)).then(|| Badge::new("WORKING…").tone(Tone::Warning));
                let header = div()
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_1)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(Theme::SPACE_2)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(Theme::TEXT_LG)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme.text)
                                    .child(format!("Attempt {number}")),
                            )
                            .children(working)
                            .child(Badge::new(column.state.label()).tone(column.state.tone())),
                    )
                    .children(instruction);

                let (summary, summary_color) = match column.summary.clone() {
                    Some(text) => (text, theme.text),
                    None => (REVIEW_NO_SUMMARY.into(), theme.text_subtle),
                };

                let test_command = column.test_command.clone().map(|command| {
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family("Menlo")
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_subtle)
                        .child(command)
                });
                let outcome = div()
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_1)
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .gap(Theme::SPACE_2)
                            .child(
                                div()
                                    .text_size(Theme::TEXT_SM)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(tone_color(&theme, tests_tone(column.tests)))
                                    .child(tests_line(column.tests)),
                            )
                            .child(Badge::new(column.repro.label()).tone(column.repro.tone())),
                    )
                    .children(test_command);

                let files: gpui::AnyElement = match &column.files {
                    None => div()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_subtle)
                        .child("Changes not computed yet")
                        .into_any_element(),
                    Some(files) if files.is_empty() => div()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_subtle)
                        .child("No changes")
                        .into_any_element(),
                    Some(files) => div()
                        .flex()
                        .flex_col()
                        .children(files.iter().enumerate().map(|(index, file)| {
                            let selector = format!("review-file-{number}-{index}");
                            let selected = open.as_ref().is_some_and(|(attempt, path)| {
                                *attempt == number && *path == file.path
                            });
                            let stat = match (file.added, file.removed) {
                                (Some(added), Some(removed)) => diffstat_label(added, removed),
                                _ => "binary".into(),
                            };
                            let hover = theme.surface_hover;
                            let mut row = div()
                                .id(SharedString::from(selector.clone()))
                                .debug_selector(move || selector)
                                .flex()
                                .items_center()
                                .gap(Theme::SPACE_2)
                                .px(Theme::SPACE_1)
                                .h(px(20.0))
                                .rounded(Theme::RADIUS_SM)
                                .cursor_pointer()
                                .hover(move |style| style.bg(hover))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .font_family("Menlo")
                                        .text_size(Theme::TEXT_XS)
                                        .text_color(theme.text)
                                        .child(file.path.clone()),
                                )
                                .child(
                                    div()
                                        .flex_none()
                                        .font_family("Menlo")
                                        .text_size(Theme::TEXT_XS)
                                        .text_color(theme.text_subtle)
                                        .child(stat),
                                );
                            if selected {
                                row = row.bg(theme.accent_muted);
                            }
                            if let Some(handler) = self.on_file.clone() {
                                let path = file.path.clone();
                                row = row.on_click(move |_, window, cx| {
                                    handler(number, &path, window, cx)
                                });
                            }
                            row
                        }))
                        .into_any_element(),
                };

                let discard = shows_discard(column.state).then(|| {
                    let mut button = Button::new(
                        SharedString::from(format!("review-discard-{number}")),
                        "Discard",
                    )
                    .variant(ButtonVariant::Danger)
                    .disabled(busy.is_some())
                    .selector(format!("review-discard-{number}"));
                    if let Some(handler) = self.on_discard.clone() {
                        button = button.on_click(move |_, window, cx| handler(number, window, cx));
                    }
                    button
                });
                let adopt = shows_adopt(column.state, view.adopted).then(|| {
                    let mut button = Button::new(
                        SharedString::from(format!("review-adopt-{number}")),
                        "Adopt",
                    )
                    .variant(ButtonVariant::Primary)
                    .disabled(busy.is_some())
                    .selector(format!("review-adopt-{number}"));
                    if let Some(handler) = self.on_adopt.clone() {
                        button = button.on_click(move |_, window, cx| handler(number, window, cx));
                    }
                    button
                });

                let border = match column.state {
                    AttemptChipState::Adopted => theme.success.alpha(0.55),
                    _ => theme.border,
                };
                let mut element = div()
                    .debug_selector(move || format!("review-column-{number}"))
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(Theme::SPACE_3)
                    .p(Theme::SPACE_3)
                    .rounded(Theme::RADIUS_MD)
                    .border_1()
                    .border_color(border)
                    .bg(theme.surface)
                    .child(header)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(Theme::SPACE_1)
                            .child(section("Summary", &theme))
                            .child(
                                div()
                                    .text_size(Theme::TEXT_SM)
                                    .text_color(summary_color)
                                    .child(summary),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(Theme::SPACE_1)
                            .child(section("Tests", &theme))
                            .child(outcome),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(Theme::SPACE_1)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(Theme::SPACE_2)
                                    .child(section("Changes", &theme))
                                    .child(
                                        div()
                                            .font_family("Menlo")
                                            .text_size(Theme::TEXT_XS)
                                            .text_color(theme.text_muted)
                                            .child(diffstat_label(column.added, column.removed)),
                                    ),
                            )
                            .child(files),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap(Theme::SPACE_2)
                            .pt(Theme::SPACE_2)
                            .border_t_1()
                            .border_color(theme.divider)
                            .children(discard)
                            .children(adopt),
                    );
                if column.state == AttemptChipState::Discarded {
                    element = element.opacity(0.5);
                }
                element
            })
            .collect();

        let empty = view.columns.is_empty().then(|| {
            div()
                .text_size(Theme::TEXT_SM)
                .text_color(theme.text_subtle)
                .child("This round has no attempts.")
        });

        let diff = view.diff.clone().map(|diff| {
            let lines: Vec<&str> = diff.text.lines().collect();
            let hidden = lines.len().saturating_sub(DIFF_LINE_LIMIT);
            let rows: Vec<_> = lines
                .iter()
                .take(DIFF_LINE_LIMIT)
                .map(|line| {
                    let mut row = div()
                        .min_h(px(14.0))
                        .px(Theme::SPACE_2)
                        .whitespace_nowrap()
                        .text_color(theme.text)
                        .child((*line).to_owned());
                    if let Some(tone) = diff_line_tone(line) {
                        let colors = theme.tone(tone);
                        row = row.text_color(colors.fg).bg(colors.bg);
                    }
                    row
                })
                .collect();
            let more = (hidden > 0).then(|| {
                div()
                    .px(Theme::SPACE_2)
                    .text_color(theme.text_subtle)
                    .child(format!("… {hidden} more lines"))
            });
            let mut close = Button::new("review-diff-close", "Close")
                .variant(ButtonVariant::Ghost)
                .selector("review-diff-close");
            if let Some(handler) = self.on_close_diff.clone() {
                close = close.on_click(move |_, window, cx| handler(window, cx));
            }
            div()
                .flex()
                .flex_col()
                .gap(Theme::SPACE_1)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(Theme::SPACE_2)
                        .child(Badge::new(format!("ATTEMPT {}", diff.attempt)).tone(Tone::Accent))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family("Menlo")
                                .text_size(Theme::TEXT_SM)
                                .text_color(theme.text)
                                .child(diff.path.clone()),
                        )
                        .child(close),
                )
                .child(
                    div()
                        .id("review-diff")
                        .debug_selector(|| "review-diff".to_owned())
                        .flex()
                        .flex_col()
                        .max_h(px(360.0))
                        .overflow_scroll()
                        .py(Theme::SPACE_1)
                        .rounded(Theme::RADIUS_SM)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.bg)
                        .font_family("Menlo")
                        .text_size(Theme::TEXT_XS)
                        .children(rows)
                        .children(more),
                )
        });

        let error = view.error.clone().map(|text| {
            div()
                .text_size(Theme::TEXT_SM)
                .text_color(theme.danger)
                .child(text)
        });

        let close = self.on_close.map(|handler| {
            Button::new("review-close", "Close")
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Large)
                .selector("review-close")
                .on_click(move |_, window, cx| handler(window, cx))
        });

        let subtitle = match view.adopted {
            Some(number) => format!("Attempt {number} was adopted"),
            None => "Discard the dead ends and adopt the winner".to_owned(),
        };

        div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_3)
            .w_full()
            .p(Theme::SPACE_4)
            .rounded(Theme::RADIUS_LG)
            .border_1()
            .border_color(theme.accent.alpha(0.55))
            .bg(theme.elevated)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(Badge::new("REVIEW").tone(Tone::Accent))
                    .child(
                        div()
                            .flex_none()
                            .text_size(Theme::TEXT_XL)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text)
                            .child(format!("Round {}", view.round)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.text_subtle)
                            .child(subtitle),
                    )
                    .children(close),
            )
            .children(error)
            .child(
                div()
                    .flex()
                    .items_stretch()
                    .gap(Theme::SPACE_3)
                    .w_full()
                    .children(columns)
                    .children(empty),
            )
            .children(diff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AttemptChipState::*;

    fn column(number: u32, state: AttemptChipState) -> ReviewColumn {
        ReviewColumn {
            number,
            instruction: None,
            state,
            summary: None,
            tests: None,
            test_command: None,
            repro: ReproView::NotChecked,
            files: None,
            added: 0,
            removed: 0,
        }
    }

    fn sheet(states: &[AttemptChipState], adopted: Option<u32>, busy: Option<u32>) -> ReviewSheet {
        ReviewSheet::new(ReviewView {
            round: 1,
            columns: states
                .iter()
                .enumerate()
                .map(|(i, &state)| column(i as u32 + 1, state))
                .collect(),
            adopted,
            busy,
            error: None,
            diff: None,
        })
    }

    #[test]
    fn only_live_attempts_are_offered_actions() {
        for state in [Running, Done, Failed] {
            assert!(shows_adopt(state, None), "{state:?}");
            assert!(shows_discard(state), "{state:?}");
        }
        for state in [Adopted, Discarded] {
            assert!(!shows_adopt(state, None), "{state:?}");
            assert!(!shows_discard(state), "{state:?}");
        }
    }

    #[test]
    fn nothing_is_adopted_once_the_round_has_a_winner() {
        for state in [Running, Done, Failed] {
            assert!(!shows_adopt(state, Some(2)), "{state:?}");
        }
        let sheet = sheet(&[Done, Adopted, Failed], Some(2), None);
        assert!(!sheet.can_adopt(1));
        assert!(!sheet.can_adopt(3));
        assert!(sheet.can_discard(1));
        assert!(sheet.can_discard(3));
        assert!(!sheet.can_discard(2));
    }

    #[test]
    fn a_failed_attempt_can_still_be_adopted() {
        assert!(sheet(&[Failed], None, None).can_adopt(1));
    }

    #[test]
    fn nothing_is_enabled_while_an_action_runs() {
        let sheet = sheet(&[Done, Done], None, Some(1));
        for number in [1, 2] {
            assert!(!sheet.can_adopt(number));
            assert!(!sheet.can_discard(number));
        }
        assert!(!ReviewSheet::new(sheet.view.clone()).can_adopt(9));
    }

    #[test]
    fn the_tests_line_says_what_ran() {
        assert_eq!(tests_line(None).as_ref(), "Tests not run");
        assert_eq!(tests_line(Some((12, 0))).as_ref(), "12 passed");
        assert_eq!(tests_line(Some((41, 1))).as_ref(), "41 passed, 1 failed");
        assert_eq!(tests_tone(None), Tone::Neutral);
        assert_eq!(tests_tone(Some((0, 0))), Tone::Neutral);
        assert_eq!(tests_tone(Some((12, 0))), Tone::Success);
        assert_eq!(tests_tone(Some((41, 1))), Tone::Danger);
    }

    #[test]
    fn repro_and_diffstat_labels() {
        assert_eq!(ReproView::Fixed.tone(), Tone::Success);
        assert_eq!(ReproView::StillFails.tone(), Tone::Danger);
        assert_eq!(ReproView::NotChecked.tone(), Tone::Neutral);
        assert_eq!(diffstat_label(12, 3).as_ref(), "+12 \u{2212}3");
    }

    #[test]
    fn diff_lines_are_tinted_by_their_sign() {
        assert_eq!(diff_line_tone("+let x = 1;"), Some(Tone::Success));
        assert_eq!(diff_line_tone("-let x = 0;"), Some(Tone::Danger));
        assert_eq!(diff_line_tone(" context"), None);
        assert_eq!(diff_line_tone("@@ -1,3 +1,4 @@"), None);
        assert_eq!(diff_line_tone("+++ b/src/lib.rs"), None);
        assert_eq!(diff_line_tone("--- a/src/lib.rs"), None);
    }
}
