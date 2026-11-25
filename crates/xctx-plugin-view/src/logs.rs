//! The logs pane: the tail of a command's or a file's output, filtered by text and level, coloured by level
//! with a letter cue, following new lines unless paused or unfollowed.

use std::rc::Rc;

use gpui::{AnyElement, App, ScrollHandle, SharedString, Window, div, prelude::*, px};
use xctx_ui::{Badge, Button, ButtonSize, ButtonVariant, Pill, Theme, Tone, cut_line};

use crate::common::{error_line, field_text, note, pane_frame, tone_text};

/// The most lines drawn: the last ones. A note says how many there were.
pub const LOG_LINE_LIMIT: usize = 2000;

/// The most characters drawn of one log line; longer lines end in "…".
pub const LOG_LINE_CHARS: usize = 1000;

/// What the pane says when there are no lines to show.
pub const LOGS_EMPTY: &str = "No lines yet.";

/// The level of a log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LogLevel {
    /// An error.
    Error,
    /// A warning.
    Warn,
    /// Information.
    Info,
    /// Debug or trace output.
    Debug,
}

/// The levels in the order their pills are drawn, most severe first.
pub const LOG_LEVELS: [LogLevel; 4] = [
    LogLevel::Error,
    LogLevel::Warn,
    LogLevel::Info,
    LogLevel::Debug,
];

impl LogLevel {
    /// The lower-case name used in selectors (`logs-level-error`).
    pub fn name(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }

    /// The pill label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "Error",
            Self::Warn => "Warn",
            Self::Info => "Info",
            Self::Debug => "Debug",
        }
    }

    /// The one-letter cue in the gutter, so the level reads without colour.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Error => "E",
            Self::Warn => "W",
            Self::Info => "I",
            Self::Debug => "D",
        }
    }

    /// The line's tone: danger for errors, warning for warnings, neutral otherwise.
    pub fn tone(self) -> Tone {
        match self {
            Self::Error => Tone::Danger,
            Self::Warn => Tone::Warning,
            Self::Info | Self::Debug => Tone::Neutral,
        }
    }

    /// Whether a line of this level passes a filter for `filter`: at least as severe (a Warn filter shows errors
    /// and warnings). Lines with no level pass only when there is no filter; see [`level_passes`].
    pub fn at_least(self, filter: LogLevel) -> bool {
        self <= filter
    }
}

/// Whether a line with `level` passes the level filter `filter`: everything passes no filter; otherwise only
/// lines at least as severe as it.
pub fn level_passes(level: Option<LogLevel>, filter: Option<LogLevel>) -> bool {
    match (filter, level) {
        (None, _) => true,
        (Some(filter), Some(level)) => level.at_least(filter),
        (Some(_), None) => false,
    }
}

/// A guess at a line's level from its first words: `ERROR`/`ERR`/`FATAL`/`PANIC`, `WARN`/`WARNING`, `INFO`,
/// `DEBUG`/`TRACE`, as whole words in any case, optionally in brackets or followed by a colon, among the first
/// 8 words (so a timestamp may come first). `level=error` and similar are recognised too.
pub fn detect_level(line: &str) -> Option<LogLevel> {
    line.split(|c: char| c.is_whitespace() || matches!(c, '[' | ']' | '|' | '"' | ','))
        .filter(|word| !word.is_empty())
        .take(8)
        .find_map(|word| {
            let word = word.strip_prefix("level=").unwrap_or(word);
            let word = word.trim_end_matches(':');
            match word.to_ascii_lowercase().as_str() {
                "error" | "err" | "fatal" | "panic" | "crit" | "critical" => Some(LogLevel::Error),
                "warn" | "warning" => Some(LogLevel::Warn),
                "info" | "notice" => Some(LogLevel::Info),
                "debug" | "trace" => Some(LogLevel::Debug),
                _ => None,
            }
        })
}

/// One log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// The text. Already masked by the caller. Cut to [`LOG_LINE_CHARS`].
    pub text: SharedString,
    /// Its level, if known (see [`detect_level`]).
    pub level: Option<LogLevel>,
}

/// What the logs pane shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogsView {
    /// What is tailed: a command or a path. Already masked by the caller.
    pub source: SharedString,
    /// The lines that pass the filters, oldest first. The app filters; the pane draws the last
    /// [`LOG_LINE_LIMIT`].
    pub lines: Vec<LogLine>,
    /// How many lines pass the filters in all, when the app keeps fewer than that in `lines`; the note says
    /// "showing last N of M" with the larger of this and `lines.len()`.
    pub total: usize,
    /// The filter text, drawn when the app passes no filter input.
    pub filter: SharedString,
    /// The level filter, or `None` for all levels.
    pub level: Option<LogLevel>,
    /// Whether the pane keeps scrolled to the newest line.
    pub follow: bool,
    /// Whether the tail is paused (new lines are not added).
    pub paused: bool,
    /// Why tailing failed. Already masked by the caller.
    pub error: Option<SharedString>,
}

/// The note under a capped tail, such as "Showing last 2000 of 5310 lines", or `None` when every line is drawn.
pub fn tail_note(drawn: usize, total: usize) -> Option<SharedString> {
    (total > drawn).then(|| format!("Showing last {drawn} of {total} lines").into())
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type LevelHandler = Rc<dyn Fn(Option<LogLevel>, &mut Window, &mut App)>;
type BoolHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// The logs pane, drawn from a [`LogsView`].
///
/// While following, the line list scrolls to the bottom each time it is drawn. Pass the same
/// [`ScrollHandle`] every frame with [`LogsPaneView::scroll_handle`] to keep the scroll position; without one
/// the pane keeps its own, keyed by its place in the element tree (so two logs panes in one window should each
/// pass a handle, or sit under parents with distinct element ids).
///
/// Selectors for tests: `logs-level-all` and `logs-level-<name>` for each of [`LOG_LEVELS`], `logs-follow`,
/// `logs-pause`, `logs-clear`, `logs-lines`, `logs-line-<i>` (of the lines drawn), `logs-more`, `logs-empty`,
/// `logs-paused` and `logs-error`.
#[derive(IntoElement)]
pub struct LogsPaneView {
    view: LogsView,
    filter_input: Option<AnyElement>,
    scroll: Option<ScrollHandle>,
    on_level: Option<LevelHandler>,
    on_follow: Option<BoolHandler>,
    on_pause: Option<BoolHandler>,
    on_clear: Option<Handler>,
}

impl LogsPaneView {
    /// A pane for `view` with no input and no handlers.
    pub fn new(view: LogsView) -> Self {
        Self {
            view,
            filter_input: None,
            scroll: None,
            on_level: None,
            on_follow: None,
            on_pause: None,
            on_clear: None,
        }
    }

    /// Draw `element` (a prompt input, say) as the filter field instead of [`LogsView::filter`].
    pub fn filter_input(mut self, element: impl IntoElement) -> Self {
        self.filter_input = Some(element.into_any_element());
        self
    }

    /// Scroll the line list with `handle`.
    pub fn scroll_handle(mut self, handle: ScrollHandle) -> Self {
        self.scroll = Some(handle);
        self
    }

    /// Call `handler` with the level pill clicked: `None` for All.
    pub fn on_level(
        mut self,
        handler: impl Fn(Option<LogLevel>, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_level = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the new follow state when Follow is clicked.
    pub fn on_follow(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_follow = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the new paused state when Pause (or Resume) is clicked.
    pub fn on_pause(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_pause = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Clear is clicked.
    pub fn on_clear(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_clear = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for LogsPaneView {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let view = self.view;
        let scroll = match self.scroll {
            Some(handle) => handle,
            None => window
                .use_keyed_state("logs-scroll", cx, |_, _| ScrollHandle::new())
                .read(cx)
                .clone(),
        };
        if view.follow {
            scroll.scroll_to_bottom();
        }

        let levels = std::iter::once(None)
            .chain(LOG_LEVELS.iter().copied().map(Some))
            .map(|level| {
                let name = level.map_or("all", LogLevel::name);
                let label = level.map_or("All", LogLevel::label);
                let selector = format!("logs-level-{name}");
                let mut pill = Pill::new(SharedString::from(selector.clone()), label)
                    .selected(view.level == level)
                    .selector(selector);
                if let Some(handler) = self.on_level.clone() {
                    pill = pill.on_click(move |_, window, cx| handler(level, window, cx));
                }
                pill
            });
        let filter = self.filter_input.unwrap_or_else(|| {
            field_text(view.filter.clone(), "Filter", &theme).into_any_element()
        });
        let follow_on = view.follow;
        let mut follow = Button::new("logs-follow", "Follow")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Compact)
            .selected(follow_on)
            .selector("logs-follow");
        if let Some(handler) = self.on_follow.clone() {
            follow = follow.on_click(move |_, window, cx| handler(!follow_on, window, cx));
        }
        let paused = view.paused;
        let mut pause = Button::new("logs-pause", if paused { "Resume" } else { "Pause" })
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Compact)
            .selected(paused)
            .selector("logs-pause");
        if let Some(handler) = self.on_pause.clone() {
            pause = pause.on_click(move |_, window, cx| handler(!paused, window, cx));
        }
        let mut clear = Button::new("logs-clear", "Clear")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Compact)
            .selector("logs-clear");
        if let Some(handler) = self.on_clear.clone() {
            clear = clear.on_click(move |_, window, cx| handler(window, cx));
        }

        let toolbar = div()
            .flex()
            .flex_none()
            .flex_col()
            .gap(Theme::SPACE_1)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(Badge::new("LOGS"))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .font_family("Menlo")
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_muted)
                            .child(view.source.clone()),
                    )
                    .children(paused.then(|| {
                        div()
                            .debug_selector(|| "logs-paused".to_owned())
                            .child(Badge::new("PAUSED").tone(Tone::Warning))
                    }))
                    .child(follow)
                    .child(pause)
                    .child(clear),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .child(div().flex_1().min_w_0().child(filter))
                    .children(levels),
            );

        let start = view.lines.len().saturating_sub(LOG_LINE_LIMIT);
        let shown = &view.lines[start..];
        let total = view.total.max(view.lines.len());
        let more = tail_note(shown.len(), total).map(|text| note("logs-more", text, &theme));
        let rows: Vec<_> = shown
            .iter()
            .enumerate()
            .map(|(i, line)| {
                let tone = line.level.map_or(Tone::Neutral, LogLevel::tone);
                let color = match line.level {
                    Some(LogLevel::Error | LogLevel::Warn) => tone_text(&theme, tone),
                    Some(LogLevel::Debug) => theme.text_subtle,
                    _ => theme.text,
                };
                let mut row = div()
                    .debug_selector(move || format!("logs-line-{i}"))
                    .flex()
                    .flex_none()
                    .gap(Theme::SPACE_2)
                    .min_h(px(14.0))
                    .px(Theme::SPACE_2)
                    .whitespace_nowrap()
                    .child(
                        div()
                            .flex_none()
                            .w(px(10.0))
                            .text_color(tone_text(&theme, tone))
                            .child(line.level.map_or(" ", LogLevel::letter)),
                    )
                    .child(
                        div()
                            .text_color(color)
                            .child(cut_line(&line.text, LOG_LINE_CHARS)),
                    );
                if line.level == Some(LogLevel::Error) {
                    row = row.bg(theme.tone(Tone::Danger).bg);
                }
                row
            })
            .collect();
        let empty = shown
            .is_empty()
            .then(|| note("logs-empty", LOGS_EMPTY, &theme));

        let error = view
            .error
            .clone()
            .map(|text| error_line("logs-error", text, &theme));

        pane_frame(&theme)
            .child(toolbar)
            .children(error)
            .children(more)
            .child(
                div()
                    .id("logs-lines")
                    .debug_selector(|| "logs-lines".to_owned())
                    .track_scroll(&scroll)
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .overflow_scroll()
                    .py(Theme::SPACE_1)
                    .rounded(Theme::RADIUS_SM)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.bg)
                    .font_family("Menlo")
                    .text_size(Theme::TEXT_XS)
                    .children(rows)
                    .children(empty),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_detected_from_the_first_words() {
        assert_eq!(detect_level("ERROR db timeout"), Some(LogLevel::Error));
        assert_eq!(
            detect_level("2024-01-01T00:00:00Z [warn] slow query"),
            Some(LogLevel::Warn)
        );
        assert_eq!(
            detect_level("12:00:01 INFO: listening on 3000"),
            Some(LogLevel::Info)
        );
        assert_eq!(
            detect_level("ts=1 level=debug msg=x"),
            Some(LogLevel::Debug)
        );
        assert_eq!(detect_level("thread panic at main"), Some(LogLevel::Error));
        assert_eq!(detect_level("GET /errors 200"), None);
        assert_eq!(detect_level(""), None);
        assert_eq!(
            detect_level("a b c d e f g h i j error"),
            None,
            "only the first words count"
        );
    }

    #[test]
    fn level_filters_keep_lines_at_least_as_severe() {
        use LogLevel::*;
        assert!(level_passes(None, None));
        assert!(level_passes(Some(Debug), None));
        assert!(level_passes(Some(Error), Some(Warn)));
        assert!(level_passes(Some(Warn), Some(Warn)));
        assert!(!level_passes(Some(Info), Some(Warn)));
        assert!(!level_passes(None, Some(Debug)));
    }

    #[test]
    fn levels_have_letters_names_and_tones() {
        assert_eq!(LogLevel::Error.letter(), "E");
        assert_eq!(LogLevel::Warn.letter(), "W");
        assert_eq!(LogLevel::Warn.name(), "warn");
        assert_eq!(LogLevel::Error.tone(), Tone::Danger);
        assert_eq!(LogLevel::Info.tone(), Tone::Neutral);
    }

    #[test]
    fn the_tail_note_counts_hidden_lines() {
        assert_eq!(tail_note(10, 10), None);
        assert_eq!(
            tail_note(2000, 5310).as_deref(),
            Some("Showing last 2000 of 5310 lines")
        );
    }
}
