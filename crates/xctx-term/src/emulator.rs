//! The terminal emulator: parses program output into a grid and answers queries.
//!
//! [`Emulator`] has no PTY or threads, so it can be driven directly in tests.
//! [`crate::TerminalSession`] wraps one behind a lock and feeds it from the PTY.

use std::process::ExitStatus;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use alacritty_terminal::event::{Event as AlacrittyEvent, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side as AlacrittySide};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::{Cell as AlacrittyCell, Flags};
use alacritty_terminal::term::color::COUNT as COLOR_COUNT;
use alacritty_terminal::term::{Config as AlacrittyConfig, Term, TermMode};
use alacritty_terminal::vte::ansi::{
    Color as AlacrittyColor, CursorShape as AlacrittyCursorShape, NamedColor, Processor,
    Rgb as AlacrittyRgb,
};

use crate::config::TerminalConfig;
use crate::snapshot::{
    AnsiColor, Cell, CellFlags, Color, CursorShape, CursorSnapshot, GridSnapshot,
    KittyKeyboardFlags, MouseEncoding, MouseReporting, Rgb, SelectionSpan, TerminalModes,
    ViewportPoint, default_indexed_color,
};

/// Something a session reports to its owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEvent {
    /// The grid, cursor, modes or selection changed; take a new snapshot.
    Wakeup,
    /// The program set (`Some`) or reset (`None`) the window title.
    TitleChanged(Option<String>),
    /// The program rang the bell.
    Bell,
    /// The program asked to put text on the clipboard (OSC 52).
    ClipboardStore(String),
    /// The program exited. `None` when the status could not be collected.
    Exited(Option<ExitStatus>),
}

/// How a selection grows from where it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectionMode {
    /// Exactly the cells between the two ends (single click and drag).
    Cell,
    /// Whole words at both ends (double click).
    Word,
    /// Whole lines at both ends (triple click).
    Line,
}

/// Which half of a cell the pointer is over; decides whether that cell is included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    /// The left half.
    Left,
    /// The right half.
    Right,
}

/// Grid dimensions passed to alacritty_terminal.
#[derive(Debug, Clone, Copy)]
struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

type ColorFormatter = Arc<dyn Fn(AlacrittyRgb) -> String + Sync + Send + 'static>;
type SizeFormatter = Arc<dyn Fn(WindowSize) -> String + Sync + Send + 'static>;

/// Output the parser produced, kept in order so query replies stay ordered (programs send a
/// device-attributes query after others as a sentinel).
enum Pending {
    Reply(String),
    ColorReply(usize, ColorFormatter),
    TextAreaSizeReply(SizeFormatter),
    Event(TerminalEvent),
}

/// Receives events from alacritty_terminal while it holds `&mut Term`; they are resolved after
/// parsing, when the emulator can read terminal state again.
#[derive(Clone, Default)]
struct EventSink {
    pending: Arc<Mutex<Vec<Pending>>>,
}

impl EventSink {
    fn push(&self, pending: Pending) {
        self.pending
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(pending);
    }

    fn take(&self) -> Vec<Pending> {
        std::mem::take(&mut *self.pending.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

impl EventListener for EventSink {
    fn send_event(&self, event: AlacrittyEvent) {
        let pending = match event {
            AlacrittyEvent::PtyWrite(text) => Pending::Reply(text),
            AlacrittyEvent::ColorRequest(index, format) => Pending::ColorReply(index, format),
            AlacrittyEvent::TextAreaSizeRequest(format) => Pending::TextAreaSizeReply(format),
            AlacrittyEvent::Title(title) => {
                Pending::Event(TerminalEvent::TitleChanged(Some(title)))
            }
            AlacrittyEvent::ResetTitle => Pending::Event(TerminalEvent::TitleChanged(None)),
            AlacrittyEvent::Bell => Pending::Event(TerminalEvent::Bell),
            AlacrittyEvent::ClipboardStore(_, text) => {
                Pending::Event(TerminalEvent::ClipboardStore(text))
            }
            _ => return,
        };
        self.push(pending);
    }
}

/// A terminal emulator with scrollback, selection and snapshotting.
pub struct Emulator {
    term: Term<EventSink>,
    parser: Processor,
    sink: EventSink,
    title: Option<String>,
    window: WindowSize,
    default_foreground: Rgb,
    default_background: Rgb,
    generation: u64,
    replies: Vec<u8>,
    events: Vec<TerminalEvent>,
}

impl Emulator {
    /// An empty emulator sized and configured from `config`.
    pub fn new(config: &TerminalConfig) -> Self {
        let (cols, rows) = clamp_size(config.cols, config.rows);
        let sink = EventSink::default();
        let term_config = AlacrittyConfig {
            scrolling_history: config.scrollback_lines,
            kitty_keyboard: true,
            ..AlacrittyConfig::default()
        };
        let term = Term::new(
            term_config,
            &GridSize {
                cols: usize::from(cols),
                rows: usize::from(rows),
            },
            sink.clone(),
        );
        Self {
            term,
            parser: Processor::new(),
            sink,
            title: None,
            window: WindowSize {
                num_cols: cols,
                num_lines: rows,
                cell_width: config.cell_width,
                cell_height: config.cell_height,
            },
            default_foreground: config.default_foreground,
            default_background: config.default_background,
            generation: 0,
            replies: Vec::new(),
            events: Vec::new(),
        }
    }

    /// Parses program output. Returns whether the visible state may have changed; false when
    /// all of `bytes` was held back by a synchronized update (mode 2026), which is shown when
    /// the update ends or times out.
    pub fn advance(&mut self, bytes: &[u8]) -> bool {
        if bytes.is_empty() {
            return false;
        }
        let buffered_before = self.parser.sync_bytes_count();
        self.parser.advance(&mut self.term, bytes);
        let had_events = self.resolve_pending();
        // Everything went into the synchronized-update buffer: nothing visible yet.
        let all_buffered = self.parser.sync_bytes_count() >= buffered_before + bytes.len();
        let changed = !all_buffered || had_events;
        if changed {
            self.generation += 1;
        }
        changed
    }

    /// When a synchronized update (mode 2026) in progress must be flushed even if the program
    /// never ends it.
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.parser.sync_timeout().sync_timeout()
    }

    /// Flushes a synchronized update whose deadline has passed. Returns whether it did.
    pub fn flush_expired_sync(&mut self, now: Instant) -> bool {
        match self.sync_deadline() {
            Some(deadline) if now >= deadline => {
                self.parser.stop_sync(&mut self.term);
                self.resolve_pending();
                self.generation += 1;
                true
            }
            _ => false,
        }
    }

    /// Takes the bytes the emulator must write back to the program (query replies).
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    /// Takes the events produced since the last call.
    pub fn take_events(&mut self) -> Vec<TerminalEvent> {
        std::mem::take(&mut self.events)
    }

    /// Changes the grid size. Columns below 2 and rows below 1 are raised to those minimums.
    pub fn resize(&mut self, cols: u16, rows: u16, cell_width: u16, cell_height: u16) {
        let (cols, rows) = clamp_size(cols, rows);
        self.window = WindowSize {
            num_cols: cols,
            num_lines: rows,
            cell_width,
            cell_height,
        };
        self.term.resize(GridSize {
            cols: usize::from(cols),
            rows: usize::from(rows),
        });
        self.generation += 1;
    }

    /// Current width in cells.
    pub fn cols(&self) -> usize {
        self.term.columns()
    }

    /// Current height in cells.
    pub fn rows(&self) -> usize {
        self.term.screen_lines()
    }

    /// Increases whenever terminal state changes.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// The window title the program set, if any.
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Modes that affect input encoding and the view.
    pub fn modes(&self) -> TerminalModes {
        modes_from(*self.term.mode())
    }

    /// Scrolls the viewport by `delta` lines: positive moves back into history, negative moves
    /// toward the live screen. Clamped to the available history.
    pub fn scroll_lines(&mut self, delta: i32) {
        let before = self.display_offset();
        self.term.scroll_display(Scroll::Delta(delta));
        if self.display_offset() != before {
            self.generation += 1;
        }
    }

    /// Returns the viewport to the live screen.
    pub fn scroll_to_bottom(&mut self) {
        let before = self.display_offset();
        self.term.scroll_display(Scroll::Bottom);
        if self.display_offset() != before {
            self.generation += 1;
        }
    }

    /// How many lines the viewport is scrolled back; 0 when following output.
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Lines of scrollback stored above the screen.
    pub fn history_size(&self) -> usize {
        self.term.grid().history_size()
    }

    /// Starts a selection at a viewport point, replacing any existing one. The selection is
    /// anchored to the content, so it stays on the same text when the viewport scrolls or
    /// output pushes lines into history.
    pub fn start_selection(&mut self, mode: SelectionMode, point: ViewportPoint, side: Side) {
        let ty = match mode {
            SelectionMode::Cell => SelectionType::Simple,
            SelectionMode::Word => SelectionType::Semantic,
            SelectionMode::Line => SelectionType::Lines,
        };
        let point = self.grid_point(point);
        self.term.selection = Some(Selection::new(ty, point, side.into()));
        self.generation += 1;
    }

    /// Moves the free end of the selection to a viewport point. Does nothing without one.
    pub fn update_selection(&mut self, point: ViewportPoint, side: Side) {
        let point = self.grid_point(point);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side.into());
            self.generation += 1;
        }
    }

    /// Removes the selection.
    pub fn clear_selection(&mut self) {
        if self.term.selection.take().is_some() {
            self.generation += 1;
        }
    }

    /// The selected text, or `None` when nothing is selected. Soft-wrapped rows are joined,
    /// hard line ends become `\n`, and trailing spaces at each line end are removed.
    pub fn selected_text(&self) -> Option<String> {
        let range = self.term.selection.as_ref()?.to_range(&self.term)?;
        Some(text_between(&self.term, range.start, range.end))
    }

    /// Copies the visible state.
    pub fn snapshot(&self) -> GridSnapshot {
        let grid = self.term.grid();
        let cols = grid.columns();
        let rows = grid.screen_lines();
        let display_offset = grid.display_offset();

        let mut cells = Vec::with_capacity(rows * cols);
        for row in 0..rows {
            let line = &grid[viewport_line(row, display_offset)];
            for col in 0..cols {
                cells.push(convert_cell(&line[Column(col)]));
            }
        }

        let renderable_cursor = self.term.renderable_content().cursor;
        let cursor_style = self.term.cursor_style();
        let cursor_row = renderable_cursor.point.line.0 + display_offset as i32;
        let visible = renderable_cursor.shape != AlacrittyCursorShape::Hidden
            && cursor_row >= 0
            && (cursor_row as usize) < rows;
        let cursor = CursorSnapshot {
            col: renderable_cursor.point.column.0,
            row: usize::try_from(cursor_row).unwrap_or(0),
            shape: match cursor_style.shape {
                AlacrittyCursorShape::Underline => CursorShape::Underline,
                AlacrittyCursorShape::Beam => CursorShape::Beam,
                AlacrittyCursorShape::HollowBlock => CursorShape::HollowBlock,
                AlacrittyCursorShape::Block | AlacrittyCursorShape::Hidden => CursorShape::Block,
            },
            visible,
            blinking: cursor_style.blinking,
        };

        let selection = self
            .term
            .selection
            .as_ref()
            .and_then(|selection| selection.to_range(&self.term))
            .and_then(|range| visible_span(range.start, range.end, display_offset, rows, cols));

        GridSnapshot {
            cols,
            rows,
            cells,
            cursor,
            title: self.title.clone(),
            modes: self.modes(),
            selection,
            display_offset,
            history_size: grid.history_size(),
            generation: self.generation,
        }
    }

    fn grid_point(&self, point: ViewportPoint) -> Point {
        let rows = self.rows();
        let cols = self.cols();
        let row = point.row.min(rows.saturating_sub(1));
        Point::new(
            viewport_line(row, self.display_offset()),
            Column(point.col.min(cols.saturating_sub(1))),
        )
    }

    /// Resolves what the parser queued; returns whether any events were produced.
    fn resolve_pending(&mut self) -> bool {
        let mut had_events = false;
        for pending in self.sink.take() {
            match pending {
                Pending::Reply(text) => self.replies.extend_from_slice(text.as_bytes()),
                Pending::ColorReply(index, format) => {
                    let rgb = self.color_for_query(index);
                    let reply = format(AlacrittyRgb {
                        r: rgb.r,
                        g: rgb.g,
                        b: rgb.b,
                    });
                    self.replies.extend_from_slice(reply.as_bytes());
                }
                Pending::TextAreaSizeReply(format) => {
                    self.replies
                        .extend_from_slice(format(self.window).as_bytes());
                }
                Pending::Event(event) => {
                    if let TerminalEvent::TitleChanged(title) = &event {
                        self.title.clone_from(title);
                    }
                    self.events.push(event);
                    had_events = true;
                }
            }
        }
        had_events
    }

    fn color_for_query(&self, index: usize) -> Rgb {
        if index < COLOR_COUNT
            && let Some(rgb) = self.term.colors()[index]
        {
            return Rgb::new(rgb.r, rgb.g, rgb.b);
        }
        match u8::try_from(index) {
            Ok(index) => default_indexed_color(index),
            Err(_) if index == NamedColor::Background as usize => self.default_background,
            Err(_) => self.default_foreground,
        }
    }
}

impl From<Side> for AlacrittySide {
    fn from(side: Side) -> Self {
        match side {
            Side::Left => AlacrittySide::Left,
            Side::Right => AlacrittySide::Right,
        }
    }
}

fn clamp_size(cols: u16, rows: u16) -> (u16, u16) {
    (cols.max(2), rows.max(1))
}

/// The grid line shown at a viewport row.
fn viewport_line(row: usize, display_offset: usize) -> Line {
    Line(row as i32 - display_offset as i32)
}

/// Clips a grid-coordinate selection to the viewport.
fn visible_span(
    start: Point,
    end: Point,
    display_offset: usize,
    rows: usize,
    cols: usize,
) -> Option<SelectionSpan> {
    let start_row = start.line.0 + display_offset as i32;
    let end_row = end.line.0 + display_offset as i32;
    let last_row = rows as i32 - 1;
    if end_row < 0 || start_row > last_row {
        return None;
    }
    let start = if start_row < 0 {
        ViewportPoint::new(0, 0)
    } else {
        ViewportPoint::new(start_row as usize, start.column.0)
    };
    let end = if end_row > last_row {
        ViewportPoint::new(rows - 1, cols - 1)
    } else {
        ViewportPoint::new(end_row as usize, end.column.0)
    };
    Some(SelectionSpan { start, end })
}

/// Text between two grid points, inclusive, joining soft-wrapped rows.
fn text_between(term: &Term<EventSink>, start: Point, end: Point) -> String {
    let grid = term.grid();
    let last_col = grid.columns() - 1;
    let mut text = String::new();
    let mut line_text = String::new();

    for line in start.line.0..=end.line.0 {
        let row = &grid[Line(line)];
        let mut first = if line == start.line.0 {
            start.column.0
        } else {
            0
        };
        let last = if line == end.line.0 {
            end.column.0.min(last_col)
        } else {
            last_col
        };
        // Starting on the right half of a wide character includes the character.
        if first > 0 && row[Column(first)].flags.contains(Flags::WIDE_CHAR_SPACER) {
            first -= 1;
        }

        let mut after_tab = false;
        for col in first..=last {
            let cell = &row[Column(col)];
            if after_tab {
                if cell.c == ' ' && col % 8 != 0 {
                    continue;
                }
                after_tab = false;
            }
            if cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            after_tab = cell.c == '\t';
            line_text.push(cell.c);
            if let Some(zerowidth) = cell.zerowidth() {
                line_text.extend(zerowidth.iter());
            }
        }

        if line == end.line.0 {
            break;
        }
        let soft_wrapped =
            last == last_col && row[Column(last_col)].flags.contains(Flags::WRAPLINE);
        if !soft_wrapped {
            text.push_str(line_text.trim_end_matches(' '));
            text.push('\n');
            line_text.clear();
        }
    }
    text.push_str(line_text.trim_end_matches(' '));
    text
}

fn convert_cell(cell: &AlacrittyCell) -> Cell {
    Cell {
        c: cell.c,
        zerowidth: cell.zerowidth().map(<[char]>::to_vec).unwrap_or_default(),
        fg: convert_color(cell.fg),
        bg: convert_color(cell.bg),
        underline_color: cell.underline_color().map(convert_color),
        flags: convert_flags(cell.flags),
    }
}

fn convert_color(color: AlacrittyColor) -> Color {
    match color {
        AlacrittyColor::Spec(rgb) => Color::Rgb(Rgb::new(rgb.r, rgb.g, rgb.b)),
        AlacrittyColor::Indexed(index) => Color::Indexed(index),
        AlacrittyColor::Named(named) => {
            let index = named as usize;
            let dim_black = NamedColor::DimBlack as usize;
            let ansi_index = if index < 16 {
                Some(index)
            } else if (dim_black..dim_black + 8).contains(&index) {
                Some(index - dim_black)
            } else {
                None
            };
            ansi_index
                .and_then(|index| u8::try_from(index).ok())
                .and_then(AnsiColor::from_index)
                .map_or(Color::Default, Color::Named)
        }
    }
}

fn convert_flags(flags: Flags) -> CellFlags {
    const MAPPING: [(Flags, CellFlags); 15] = [
        (Flags::BOLD, CellFlags::BOLD),
        (Flags::DIM, CellFlags::DIM),
        (Flags::ITALIC, CellFlags::ITALIC),
        (Flags::UNDERLINE, CellFlags::UNDERLINE),
        (Flags::DOUBLE_UNDERLINE, CellFlags::DOUBLE_UNDERLINE),
        (Flags::UNDERCURL, CellFlags::UNDERCURL),
        (Flags::DOTTED_UNDERLINE, CellFlags::DOTTED_UNDERLINE),
        (Flags::DASHED_UNDERLINE, CellFlags::DASHED_UNDERLINE),
        (Flags::STRIKEOUT, CellFlags::STRIKETHROUGH),
        (Flags::INVERSE, CellFlags::INVERSE),
        (Flags::HIDDEN, CellFlags::HIDDEN),
        (Flags::WIDE_CHAR, CellFlags::WIDE_CHAR),
        (Flags::WIDE_CHAR_SPACER, CellFlags::WIDE_CHAR_SPACER),
        (
            Flags::LEADING_WIDE_CHAR_SPACER,
            CellFlags::LEADING_WIDE_CHAR_SPACER,
        ),
        (Flags::WRAPLINE, CellFlags::WRAPLINE),
    ];
    MAPPING
        .iter()
        .filter(|(from, _)| flags.contains(*from))
        .fold(CellFlags::empty(), |acc, (_, to)| acc | *to)
}

fn modes_from(mode: TermMode) -> TerminalModes {
    let mouse_reporting = if mode.contains(TermMode::MOUSE_MOTION) {
        MouseReporting::Motion
    } else if mode.contains(TermMode::MOUSE_DRAG) {
        MouseReporting::Drag
    } else if mode.contains(TermMode::MOUSE_REPORT_CLICK) {
        MouseReporting::Click
    } else {
        MouseReporting::Off
    };
    let mouse_encoding = if mode.contains(TermMode::SGR_MOUSE) {
        MouseEncoding::Sgr
    } else if mode.contains(TermMode::UTF8_MOUSE) {
        MouseEncoding::Utf8
    } else {
        MouseEncoding::Default
    };
    let kitty_mapping = [
        (
            TermMode::DISAMBIGUATE_ESC_CODES,
            KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES,
        ),
        (
            TermMode::REPORT_EVENT_TYPES,
            KittyKeyboardFlags::REPORT_EVENT_TYPES,
        ),
        (
            TermMode::REPORT_ALTERNATE_KEYS,
            KittyKeyboardFlags::REPORT_ALTERNATE_KEYS,
        ),
        (
            TermMode::REPORT_ALL_KEYS_AS_ESC,
            KittyKeyboardFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
        ),
        (
            TermMode::REPORT_ASSOCIATED_TEXT,
            KittyKeyboardFlags::REPORT_ASSOCIATED_TEXT,
        ),
    ];
    let kitty_keyboard = kitty_mapping
        .iter()
        .filter(|(from, _)| mode.contains(*from))
        .fold(KittyKeyboardFlags::empty(), |acc, (_, to)| acc | *to);

    TerminalModes {
        show_cursor: mode.contains(TermMode::SHOW_CURSOR),
        app_cursor: mode.contains(TermMode::APP_CURSOR),
        app_keypad: mode.contains(TermMode::APP_KEYPAD),
        bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
        focus_reporting: mode.contains(TermMode::FOCUS_IN_OUT),
        alt_screen: mode.contains(TermMode::ALT_SCREEN),
        alternate_scroll: mode.contains(TermMode::ALTERNATE_SCROLL),
        line_feed_new_line: mode.contains(TermMode::LINE_FEED_NEW_LINE),
        mouse_reporting,
        mouse_encoding,
        kitty_keyboard,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emulator(cols: u16, rows: u16) -> Emulator {
        Emulator::new(&TerminalConfig::new("/bin/sh").size(cols, rows))
    }

    fn select(
        emulator: &mut Emulator,
        mode: SelectionMode,
        from: (usize, usize),
        to: (usize, usize),
    ) {
        emulator.start_selection(mode, ViewportPoint::new(from.0, from.1), Side::Left);
        emulator.update_selection(ViewportPoint::new(to.0, to.1), Side::Right);
    }

    #[test]
    fn advance_writes_text_and_truecolor() {
        let mut emulator = emulator(20, 4);
        assert!(emulator.advance(b"\x1b[38;2;255;0;128mhi\x1b[0m there"));
        let snapshot = emulator.snapshot();
        assert_eq!(snapshot.row_text(0), "hi there");
        let h = snapshot.cell(0, 0).cloned().unwrap_or_default();
        assert_eq!(h.fg, Color::Rgb(Rgb::new(255, 0, 128)));
        let t = snapshot.cell(0, 3).cloned().unwrap_or_default();
        assert_eq!(t.fg, Color::Default);
    }

    #[test]
    fn advance_maps_named_indexed_colors_and_flags() {
        let mut emulator = emulator(20, 4);
        emulator
            .advance(b"\x1b[31;1ma\x1b[0m\x1b[38;5;200;4mb\x1b[0m\x1b[2;3;7;8;9mc\x1b[0m\x1b[44md");
        let snapshot = emulator.snapshot();
        let cell = |col| snapshot.cell(0, col).cloned().unwrap_or_default();
        assert_eq!(cell(0).fg, Color::Named(AnsiColor::Red));
        assert!(cell(0).flags.contains(CellFlags::BOLD));
        assert_eq!(cell(1).fg, Color::Indexed(200));
        assert!(cell(1).flags.contains(CellFlags::UNDERLINE));
        let flags = cell(2).flags;
        for flag in [
            CellFlags::DIM,
            CellFlags::ITALIC,
            CellFlags::INVERSE,
            CellFlags::HIDDEN,
            CellFlags::STRIKETHROUGH,
        ] {
            assert!(flags.contains(flag), "missing {flag:?}");
        }
        assert_eq!(cell(3).bg, Color::Named(AnsiColor::Blue));
    }

    #[test]
    fn wide_characters_are_flagged() {
        let mut emulator = emulator(10, 2);
        emulator.advance("日本".as_bytes());
        let snapshot = emulator.snapshot();
        let flags = |col| {
            snapshot
                .cell(0, col)
                .map(|cell| cell.flags)
                .unwrap_or_default()
        };
        assert!(flags(0).contains(CellFlags::WIDE_CHAR));
        assert!(flags(1).contains(CellFlags::WIDE_CHAR_SPACER));
        assert!(flags(2).contains(CellFlags::WIDE_CHAR));
        assert_eq!(snapshot.row_text(0), "日本");
    }

    #[test]
    fn modes_follow_program_requests() {
        let mut emulator = emulator(10, 2);
        assert!(!emulator.modes().bracketed_paste);
        emulator.advance(b"\x1b[?2004h\x1b[?1h\x1b[?1002h\x1b[?1006h\x1b[?1004h\x1b[?1049h");
        let modes = emulator.modes();
        assert!(modes.bracketed_paste);
        assert!(modes.app_cursor);
        assert!(modes.focus_reporting);
        assert!(modes.alt_screen);
        assert_eq!(modes.mouse_reporting, MouseReporting::Drag);
        assert_eq!(modes.mouse_encoding, MouseEncoding::Sgr);
    }

    #[test]
    fn kitty_keyboard_flags_are_negotiated() {
        let mut emulator = emulator(10, 2);
        // The sequence `pi` sends: push flags 7, then query them.
        emulator.advance(b"\x1b[>7u\x1b[?u");
        assert_eq!(
            emulator.modes().kitty_keyboard,
            KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES
                | KittyKeyboardFlags::REPORT_EVENT_TYPES
                | KittyKeyboardFlags::REPORT_ALTERNATE_KEYS
        );
        assert_eq!(emulator.take_replies(), b"\x1b[?7u");
        emulator.advance(b"\x1b[<u");
        assert!(emulator.modes().kitty_keyboard.is_empty());
    }

    #[test]
    fn queries_are_answered_in_order() {
        let mut emulator = emulator(10, 2);
        emulator.advance(b"\x1b]11;?\x07\x1b[6n\x1b[c");
        let replies = String::from_utf8(emulator.take_replies()).unwrap_or_default();
        assert_eq!(replies, "\x1b]11;rgb:1818/1818/1818\x07\x1b[1;1R\x1b[?6c");
    }

    #[test]
    fn title_and_bell_become_events() {
        let mut emulator = emulator(10, 2);
        emulator.advance(b"\x1b]2;hello\x07\x07");
        assert_eq!(emulator.title(), Some("hello"));
        assert_eq!(
            emulator.take_events(),
            vec![
                TerminalEvent::TitleChanged(Some("hello".to_owned())),
                TerminalEvent::Bell
            ]
        );
        assert_eq!(emulator.snapshot().title.as_deref(), Some("hello"));
    }

    #[test]
    fn cursor_position_and_visibility() {
        let mut emulator = emulator(10, 4);
        emulator.advance(b"ab\r\ncd");
        let cursor = emulator.snapshot().cursor;
        assert_eq!((cursor.row, cursor.col, cursor.visible), (1, 2, true));
        emulator.advance(b"\x1b[?25l\x1b[6 q");
        let cursor = emulator.snapshot().cursor;
        assert!(!cursor.visible);
        assert_eq!(cursor.shape, CursorShape::Beam);
    }

    #[test]
    fn scrollback_and_display_offset() {
        let mut emulator = emulator(10, 3);
        for i in 0..10 {
            emulator.advance(format!("line{i}\r\n").as_bytes());
        }
        assert_eq!(emulator.history_size(), 8);
        assert_eq!(emulator.snapshot().row_text(0), "line8");
        emulator.scroll_lines(3);
        assert_eq!(emulator.display_offset(), 3);
        let snapshot = emulator.snapshot();
        assert_eq!(snapshot.row_text(0), "line5");
        assert!(!snapshot.cursor.visible);
        emulator.scroll_lines(100);
        assert_eq!(emulator.display_offset(), 8);
        emulator.scroll_to_bottom();
        assert_eq!(emulator.display_offset(), 0);
    }

    #[test]
    fn scrollback_is_bounded_by_config() {
        let mut emulator = Emulator::new(
            &TerminalConfig::new("/bin/sh")
                .size(10, 2)
                .scrollback_lines(5),
        );
        for i in 0..50 {
            emulator.advance(format!("{i}\r\n").as_bytes());
        }
        assert_eq!(emulator.history_size(), 5);
    }

    #[test]
    fn selected_text_single_line() {
        let mut emulator = emulator(20, 3);
        emulator.advance(b"hello world   ");
        select(&mut emulator, SelectionMode::Cell, (0, 6), (0, 10));
        assert_eq!(emulator.selected_text().as_deref(), Some("world"));
        let span = emulator.snapshot().selection;
        assert_eq!(
            span,
            Some(SelectionSpan {
                start: ViewportPoint::new(0, 6),
                end: ViewportPoint::new(0, 10)
            })
        );
    }

    #[test]
    fn selected_text_multi_line_trims_trailing_spaces() {
        let mut emulator = emulator(20, 3);
        emulator.advance(b"first   \r\nsecond\r\nthird");
        select(&mut emulator, SelectionMode::Cell, (0, 0), (2, 19));
        assert_eq!(
            emulator.selected_text().as_deref(),
            Some("first\nsecond\nthird")
        );
    }

    #[test]
    fn selected_text_joins_wrapped_lines() {
        let mut emulator = emulator(10, 4);
        emulator.advance(b"0123456789abcde\r\nnext");
        let snapshot = emulator.snapshot();
        assert_eq!(snapshot.row_text(0), "0123456789");
        assert!(
            snapshot
                .cell(0, 9)
                .is_some_and(|cell| cell.flags.contains(CellFlags::WRAPLINE))
        );
        select(&mut emulator, SelectionMode::Cell, (0, 0), (2, 9));
        assert_eq!(
            emulator.selected_text().as_deref(),
            Some("0123456789abcde\nnext")
        );
    }

    #[test]
    fn selected_text_keeps_space_at_wrap_boundary() {
        let mut emulator = emulator(6, 3);
        emulator.advance(b"hello world");
        select(&mut emulator, SelectionMode::Cell, (0, 0), (1, 5));
        assert_eq!(emulator.selected_text().as_deref(), Some("hello world"));
    }

    #[test]
    fn word_and_line_selection() {
        let mut emulator = emulator(30, 3);
        emulator.advance(b"cargo test --workspace\r\nsecond line");
        emulator.start_selection(SelectionMode::Word, ViewportPoint::new(0, 7), Side::Left);
        assert_eq!(emulator.selected_text().as_deref(), Some("test"));
        emulator.start_selection(SelectionMode::Line, ViewportPoint::new(1, 3), Side::Left);
        assert_eq!(emulator.selected_text().as_deref(), Some("second line"));
        emulator.clear_selection();
        assert_eq!(emulator.selected_text(), None);
        assert_eq!(emulator.snapshot().selection, None);
    }

    #[test]
    fn selection_reaches_into_scrollback() {
        let mut emulator = emulator(10, 2);
        for i in 0..6 {
            emulator.advance(format!("row{i}\r\n").as_bytes());
        }
        emulator.scroll_lines(4);
        assert_eq!(emulator.snapshot().row_text(0), "row1");
        emulator.start_selection(SelectionMode::Cell, ViewportPoint::new(0, 0), Side::Left);
        emulator.scroll_to_bottom();
        emulator.update_selection(ViewportPoint::new(0, 9), Side::Right);
        assert_eq!(
            emulator.selected_text().as_deref(),
            Some("row1\nrow2\nrow3\nrow4\nrow5")
        );
        let span = emulator.snapshot().selection;
        assert_eq!(
            span,
            Some(SelectionSpan {
                start: ViewportPoint::new(0, 0),
                end: ViewportPoint::new(0, 9)
            })
        );
    }

    #[test]
    fn selected_text_includes_wide_characters_once() {
        let mut emulator = emulator(10, 2);
        emulator.advance("a日b".as_bytes());
        select(&mut emulator, SelectionMode::Cell, (0, 0), (0, 3));
        assert_eq!(emulator.selected_text().as_deref(), Some("a日b"));
    }

    #[test]
    fn resize_changes_dimensions_and_generation() {
        let mut emulator = emulator(10, 2);
        let before = emulator.generation();
        emulator.resize(40, 12, 9, 18);
        assert_eq!((emulator.cols(), emulator.rows()), (40, 12));
        assert!(emulator.generation() > before);
        emulator.resize(0, 0, 9, 18);
        assert_eq!((emulator.cols(), emulator.rows()), (2, 1));
    }

    #[test]
    fn synchronized_update_holds_output_until_end() {
        let mut emulator = emulator(10, 2);
        emulator.advance(b"\x1b[?2026h");
        let generation = emulator.generation();
        assert!(!emulator.advance(b"hidden"));
        assert_eq!(emulator.generation(), generation);
        assert_eq!(emulator.snapshot().row_text(0), "");
        assert!(emulator.sync_deadline().is_some());
        assert!(emulator.advance(b"\x1b[?2026l"));
        assert_eq!(emulator.snapshot().row_text(0), "hidden");
    }

    #[test]
    fn synchronized_update_flushes_after_deadline() {
        let mut emulator = emulator(10, 2);
        emulator.advance(b"\x1b[?2026hstuck");
        let deadline = emulator.sync_deadline().unwrap_or_else(Instant::now);
        assert!(!emulator.flush_expired_sync(deadline - std::time::Duration::from_millis(1)));
        assert!(emulator.flush_expired_sync(deadline));
        assert_eq!(emulator.snapshot().row_text(0), "stuck");
    }
}
