//! Window-free geometry and batching for drawing a grid snapshot: cell metrics, pixel to
//! cell conversion, background runs, text segments, selection ranges and the scrollback
//! thumb. Kept apart from painting so it can be tested without a window.

use gpui::{Pixels, Point, Rgba, px};
use xctx_term::{Cell, CellFlags, SelectionSpan, Side, ViewportPoint};

use crate::theme::TerminalTheme;

/// The size of one grid cell in pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    /// Advance of one column.
    pub cell_width: Pixels,
    /// Height of one row.
    pub line_height: Pixels,
}

impl CellMetrics {
    /// The number of whole columns and rows that fit in `width` x `height`, at least 2x1
    /// (the smallest grid a session accepts) and at most `u16::MAX` each.
    pub fn grid_size(&self, width: Pixels, height: Pixels) -> (u16, u16) {
        let fit = |extent: Pixels, cell: Pixels, min: u16| -> u16 {
            if cell <= px(0.0) {
                return min;
            }
            let count = (extent / cell).floor();
            if count.is_nan() || count < f32::from(min) {
                min
            } else if count >= f32::from(u16::MAX) {
                u16::MAX
            } else {
                count as u16
            }
        };
        (
            fit(width, self.cell_width, 2),
            fit(height, self.line_height, 1),
        )
    }

    /// The cell size in whole pixels, as reported to programs that ask for it.
    pub fn cell_size_pixels(&self) -> (u16, u16) {
        let whole = |value: Pixels| f32::from(value).round().clamp(1.0, f32::from(u16::MAX)) as u16;
        (whole(self.cell_width), whole(self.line_height))
    }

    /// The viewport cell under `position` for a grid whose top-left corner is at `origin`,
    /// clamped to the grid, and which half of the cell the position is over. Positions left
    /// of the grid map to the left half of column 0, positions right of it to the right half
    /// of the last column.
    pub fn point_for_position(
        &self,
        position: Point<Pixels>,
        origin: Point<Pixels>,
        cols: usize,
        rows: usize,
    ) -> (ViewportPoint, Side) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let x = position.x - origin.x;
        let y = position.y - origin.y;

        let (col, side) = if x < px(0.0) || self.cell_width <= px(0.0) {
            (0, Side::Left)
        } else {
            let exact = x / self.cell_width;
            let col = exact.floor() as usize;
            if col >= cols {
                (cols - 1, Side::Right)
            } else if exact - exact.floor() < 0.5 {
                (col, Side::Left)
            } else {
                (col, Side::Right)
            }
        };
        let row = if y < px(0.0) || self.line_height <= px(0.0) {
            0
        } else {
            ((y / self.line_height).floor() as usize).min(rows - 1)
        };
        (ViewportPoint::new(row, col), side)
    }
}

/// Contiguous cells of one row with the same background colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BackgroundRun {
    /// First column.
    pub start_col: usize,
    /// Number of columns.
    pub len: usize,
    /// Fill colour.
    pub color: Rgba,
}

/// The background fills one row needs, merging neighbouring cells of the same colour.
/// Cells with the theme background are skipped, since the view fills its whole area with it.
pub fn background_runs(cells: &[Cell], theme: &TerminalTheme) -> Vec<BackgroundRun> {
    let mut runs: Vec<BackgroundRun> = Vec::new();
    for (col, cell) in cells.iter().enumerate() {
        let color = theme.cell_colors(cell).background;
        if color == theme.background {
            continue;
        }
        match runs.last_mut() {
            Some(run) if run.color == color && run.start_col + run.len == col => run.len += 1,
            _ => runs.push(BackgroundRun {
                start_col: col,
                len: 1,
                color,
            }),
        }
    }
    runs
}

/// Which line a run of text has under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnderlineKind {
    /// A straight line (single, double, dotted and dashed underlines are all drawn straight).
    Straight,
    /// A wavy line (SGR 4:3).
    Curly,
}

/// How a run of text is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle {
    /// Glyph colour.
    pub color: Rgba,
    /// Bold weight.
    pub bold: bool,
    /// Italic slant.
    pub italic: bool,
    /// The underline, with its colour.
    pub underline: Option<(UnderlineKind, Rgba)>,
    /// Whether the text is struck through (in the glyph colour).
    pub strikethrough: bool,
}

/// A stretch of `len` bytes of a segment's text drawn in one style.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StyledRun {
    /// Length in UTF-8 bytes.
    pub len: usize,
    /// Style of these bytes.
    pub style: TextStyle,
}

/// Text to shape and draw as one line starting at a column.
///
/// Narrow segments hold one glyph per column and are drawn with every glyph snapped to the
/// cell grid. A wide segment holds exactly one double-width character.
#[derive(Debug, Clone, PartialEq)]
pub struct TextSegment {
    /// Column of the first character.
    pub start_col: usize,
    /// Whether this is a single double-width character.
    pub wide: bool,
    /// The text, without newlines.
    pub text: String,
    /// Styles covering all of `text`, in order.
    pub runs: Vec<StyledRun>,
}

/// The text style of a cell, from its flags and resolved colours.
pub fn text_style(cell: &Cell, theme: &TerminalTheme) -> TextStyle {
    let flags = cell.flags;
    let color = theme.cell_colors(cell).foreground;
    let underline_kind = if flags.contains(CellFlags::UNDERCURL) {
        Some(UnderlineKind::Curly)
    } else if flags.intersects(
        CellFlags::UNDERLINE
            | CellFlags::DOUBLE_UNDERLINE
            | CellFlags::DOTTED_UNDERLINE
            | CellFlags::DASHED_UNDERLINE,
    ) {
        Some(UnderlineKind::Straight)
    } else {
        None
    };
    let underline = underline_kind.map(|kind| {
        let underline_color = cell
            .underline_color
            .map(|c| theme.resolve(c, theme.foreground))
            .unwrap_or(color);
        (kind, underline_color)
    });
    TextStyle {
        color,
        bold: flags.contains(CellFlags::BOLD),
        italic: flags.contains(CellFlags::ITALIC),
        underline,
        strikethrough: flags.contains(CellFlags::STRIKETHROUGH),
    }
}

/// Whether a cell draws nothing in the text pass: an undecorated space, or hidden text.
fn is_blank(cell: &Cell) -> bool {
    let decorated = cell.flags.intersects(
        CellFlags::UNDERLINE
            | CellFlags::DOUBLE_UNDERLINE
            | CellFlags::UNDERCURL
            | CellFlags::DOTTED_UNDERLINE
            | CellFlags::DASHED_UNDERLINE
            | CellFlags::STRIKETHROUGH,
    );
    if cell.flags.contains(CellFlags::HIDDEN) {
        return !decorated;
    }
    !decorated && (cell.c == ' ' || cell.c.is_control()) && cell.zerowidth.is_empty()
}

/// The text a cell contributes: its character and combining marks, or a space when it is
/// hidden or a control character.
fn push_cell_text(text: &mut String, cell: &Cell) {
    if cell.flags.contains(CellFlags::HIDDEN) || cell.c.is_control() {
        text.push(' ');
        return;
    }
    text.push(cell.c);
    text.extend(cell.zerowidth.iter().filter(|c| !c.is_control()));
}

/// Narrow segment being built, with the byte length of its text before trailing blanks.
struct OpenSegment {
    segment: TextSegment,
    trimmed_len: usize,
}

impl OpenSegment {
    fn finish(mut self) -> TextSegment {
        let mut remaining = self.trimmed_len;
        self.segment.text.truncate(remaining);
        self.segment.runs.retain_mut(|run| {
            if remaining == 0 {
                return false;
            }
            run.len = run.len.min(remaining);
            remaining -= run.len;
            true
        });
        self.segment
    }
}

/// The text segments of one row: neighbouring cells merged into as few lines and style runs
/// as possible. Undecorated spaces take the style of the run they sit in, so they never split
/// a run; leading and trailing spaces are dropped; each wide character is its own segment;
/// wide-character spacers draw nothing.
pub fn text_segments(cells: &[Cell], theme: &TerminalTheme) -> Vec<TextSegment> {
    let mut segments = Vec::new();
    let mut open: Option<OpenSegment> = None;

    for (col, cell) in cells.iter().enumerate() {
        let flags = cell.flags;
        if flags.intersects(CellFlags::WIDE_CHAR_SPACER | CellFlags::LEADING_WIDE_CHAR_SPACER) {
            // The spacer's column is still a column: keep narrow segments aligned by
            // closing them.
            if let Some(segment) = open.take() {
                segments.push(segment.finish());
            }
            continue;
        }

        if flags.contains(CellFlags::WIDE_CHAR) {
            if let Some(segment) = open.take() {
                segments.push(segment.finish());
            }
            if !is_blank(cell) {
                let mut text = String::new();
                push_cell_text(&mut text, cell);
                segments.push(TextSegment {
                    start_col: col,
                    wide: true,
                    runs: vec![StyledRun {
                        len: text.len(),
                        style: text_style(cell, theme),
                    }],
                    text,
                });
            }
            continue;
        }

        let blank = is_blank(cell);
        let Some(current) = open.as_mut() else {
            if blank {
                continue;
            }
            let mut text = String::new();
            push_cell_text(&mut text, cell);
            let len = text.len();
            open = Some(OpenSegment {
                segment: TextSegment {
                    start_col: col,
                    wide: false,
                    text,
                    runs: vec![StyledRun {
                        len,
                        style: text_style(cell, theme),
                    }],
                },
                trimmed_len: len,
            });
            continue;
        };

        let before = current.segment.text.len();
        push_cell_text(&mut current.segment.text, cell);
        let added = current.segment.text.len() - before;
        if blank {
            if let Some(run) = current.segment.runs.last_mut() {
                run.len += added;
            }
            continue;
        }
        let style = text_style(cell, theme);
        match current.segment.runs.last_mut() {
            Some(run) if run.style == style => run.len += added,
            _ => current.segment.runs.push(StyledRun { len: added, style }),
        }
        current.trimmed_len = current.segment.text.len();
    }

    if let Some(segment) = open.take() {
        segments.push(segment.finish());
    }
    segments
}

/// The selected column range `[start, end)` of each viewport row the selection covers.
pub fn selection_ranges(
    span: &SelectionSpan,
    cols: usize,
    rows: usize,
) -> Vec<(usize, usize, usize)> {
    if cols == 0 || rows == 0 {
        return Vec::new();
    }
    let last_row = span.end.row.min(rows - 1);
    (span.start.row..=last_row)
        .filter_map(|row| {
            let start = if row == span.start.row {
                span.start.col
            } else {
                0
            };
            let end = if row == span.end.row {
                span.end.col + 1
            } else {
                cols
            };
            let end = end.min(cols);
            (start < end).then_some((row, start, end))
        })
        .collect()
}

/// The thumb of the scrollback indicator as `(top, height)` in pixels within a track of
/// `track_height`, or `None` when following output. The thumb is at the bottom when
/// `display_offset` is 0 and at the top when the whole history is scrolled back, and never
/// shorter than `min_height`.
pub fn scrollback_thumb(
    display_offset: usize,
    history_size: usize,
    rows: usize,
    track_height: Pixels,
    min_height: Pixels,
) -> Option<(Pixels, Pixels)> {
    if display_offset == 0 || rows == 0 {
        return None;
    }
    let total = (history_size + rows) as f32;
    let height = (track_height * (rows as f32 / total))
        .max(min_height)
        .min(track_height);
    let lines_above = history_size.saturating_sub(display_offset) as f32;
    let available = track_height - height;
    let top = if history_size == 0 {
        px(0.0)
    } else {
        available * (lines_above / history_size as f32)
    };
    Some((top, height))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, rgb};
    use xctx_term::{AnsiColor, Color};

    fn metrics() -> CellMetrics {
        CellMetrics {
            cell_width: px(8.0),
            line_height: px(16.0),
        }
    }

    fn text_cells(text: &str) -> Vec<Cell> {
        text.chars()
            .map(|c| Cell {
                c,
                ..Cell::default()
            })
            .collect()
    }

    #[test]
    fn grid_size_floors_and_clamps() {
        let m = metrics();
        assert_eq!(m.grid_size(px(800.0), px(480.0)), (100, 30));
        assert_eq!(m.grid_size(px(807.9), px(495.0)), (100, 30));
        assert_eq!(m.grid_size(px(0.0), px(0.0)), (2, 1));
        assert_eq!(m.cell_size_pixels(), (8, 16));
    }

    #[test]
    fn point_for_position_maps_cells_and_sides() {
        let m = metrics();
        let origin = point(px(10.0), px(20.0));
        let at = |x: f32, y: f32| m.point_for_position(point(px(x), px(y)), origin, 80, 24);
        assert_eq!(at(10.0, 20.0), (ViewportPoint::new(0, 0), Side::Left));
        assert_eq!(at(13.9, 20.0), (ViewportPoint::new(0, 0), Side::Left));
        assert_eq!(at(14.1, 35.9), (ViewportPoint::new(0, 0), Side::Right));
        assert_eq!(
            at(10.0 + 8.0 * 5.0 + 1.0, 20.0 + 16.0 * 3.0),
            (ViewportPoint::new(3, 5), Side::Left)
        );
        // Outside the grid clamps to its edges.
        assert_eq!(at(0.0, 0.0), (ViewportPoint::new(0, 0), Side::Left));
        assert_eq!(
            at(5000.0, 5000.0),
            (ViewportPoint::new(23, 79), Side::Right)
        );
    }

    #[test]
    fn background_runs_merge_and_skip_default() {
        let theme = TerminalTheme::default();
        let mut cells = text_cells("abcdef");
        for cell in &mut cells[1..3] {
            cell.bg = Color::Named(AnsiColor::Red);
        }
        cells[3].bg = Color::Named(AnsiColor::Blue);
        cells[5].bg = Color::Named(AnsiColor::Red);
        let runs = background_runs(&cells, &theme);
        assert_eq!(
            runs,
            vec![
                BackgroundRun {
                    start_col: 1,
                    len: 2,
                    color: theme.ansi[1]
                },
                BackgroundRun {
                    start_col: 3,
                    len: 1,
                    color: theme.ansi[4]
                },
                BackgroundRun {
                    start_col: 5,
                    len: 1,
                    color: theme.ansi[1]
                },
            ]
        );
    }

    #[test]
    fn background_runs_include_inverse_default_cells() {
        let theme = TerminalTheme::default();
        let mut cells = text_cells("ab");
        cells[0].flags = CellFlags::INVERSE;
        let runs = background_runs(&cells, &theme);
        assert_eq!(
            runs,
            vec![BackgroundRun {
                start_col: 0,
                len: 1,
                color: theme.foreground
            }]
        );
    }

    #[test]
    fn text_segments_batch_styles_and_trim_blanks() {
        let theme = TerminalTheme::default();
        let mut cells = text_cells("  ab cd  ");
        cells[5].fg = Color::Rgb(xctx_term::Rgb::new(255, 0, 0));
        cells[6].fg = Color::Rgb(xctx_term::Rgb::new(255, 0, 0));
        let segments = text_segments(&cells, &theme);
        assert_eq!(segments.len(), 1);
        let segment = &segments[0];
        assert_eq!(segment.start_col, 2);
        assert!(!segment.wide);
        assert_eq!(segment.text, "ab cd");
        // The space between joins the first run instead of starting a new one.
        assert_eq!(segment.runs.len(), 2);
        assert_eq!(segment.runs[0].len, 3);
        assert_eq!(segment.runs[0].style.color, theme.foreground);
        assert_eq!(segment.runs[1].len, 2);
        assert_eq!(segment.runs[1].style.color, rgb(0xff0000));
    }

    #[test]
    fn text_segments_keep_decorated_spaces_and_flags() {
        let theme = TerminalTheme::default();
        let mut cells = text_cells("a  ");
        cells[0].flags = CellFlags::BOLD | CellFlags::ITALIC;
        cells[1].flags = CellFlags::UNDERLINE;
        cells[2].flags = CellFlags::UNDERCURL;
        cells[2].underline_color = Some(Color::Named(AnsiColor::Red));
        let segments = text_segments(&cells, &theme);
        assert_eq!(segments.len(), 1);
        let runs = &segments[0].runs;
        assert_eq!(segments[0].text, "a  ");
        assert_eq!(runs.len(), 3);
        assert!(runs[0].style.bold && runs[0].style.italic);
        assert_eq!(
            runs[1].style.underline,
            Some((UnderlineKind::Straight, theme.foreground))
        );
        assert_eq!(
            runs[2].style.underline,
            Some((UnderlineKind::Curly, theme.ansi[1]))
        );
    }

    #[test]
    fn text_segments_split_around_wide_chars() {
        let theme = TerminalTheme::default();
        let mut cells = text_cells("a界 b");
        // "界" occupies columns 1-2; its spacer replaces the space at column 2.
        cells[1].flags = CellFlags::WIDE_CHAR;
        cells[2].flags = CellFlags::WIDE_CHAR_SPACER;
        let segments = text_segments(&cells, &theme);
        let summary: Vec<_> = segments
            .iter()
            .map(|s| (s.start_col, s.wide, s.text.as_str()))
            .collect();
        assert_eq!(
            summary,
            vec![(0, false, "a"), (1, true, "界"), (3, false, "b")]
        );
        assert_eq!(segments[1].runs[0].len, "界".len());
    }

    #[test]
    fn text_segments_hide_hidden_and_keep_strikethrough_and_combining() {
        let theme = TerminalTheme::default();
        let mut cells = text_cells("xyz");
        cells[0].flags = CellFlags::HIDDEN;
        cells[1].zerowidth = vec!['\u{301}'];
        cells[2].flags = CellFlags::STRIKETHROUGH;
        let segments = text_segments(&cells, &theme);
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].start_col, 1);
        assert_eq!(segments[0].text, "y\u{301}z");
        assert!(
            segments[0]
                .runs
                .last()
                .is_some_and(|run| run.style.strikethrough)
        );
        let total: usize = segments[0].runs.iter().map(|run| run.len).sum();
        assert_eq!(total, segments[0].text.len());
    }

    #[test]
    fn text_segments_of_blank_row_are_empty() {
        let theme = TerminalTheme::default();
        assert!(text_segments(&text_cells("     "), &theme).is_empty());
    }

    #[test]
    fn selection_ranges_cover_rows() {
        let span = SelectionSpan {
            start: ViewportPoint::new(1, 5),
            end: ViewportPoint::new(3, 2),
        };
        assert_eq!(
            selection_ranges(&span, 10, 24),
            vec![(1, 5, 10), (2, 0, 10), (3, 0, 3)]
        );
        let single = SelectionSpan {
            start: ViewportPoint::new(0, 2),
            end: ViewportPoint::new(0, 4),
        };
        assert_eq!(selection_ranges(&single, 10, 24), vec![(0, 2, 5)]);
    }

    #[test]
    fn scrollback_thumb_position() {
        assert_eq!(scrollback_thumb(0, 100, 20, px(200.0), px(10.0)), None);
        // Fully scrolled back: thumb at the top.
        let (top, height) = scrollback_thumb(80, 80, 20, px(200.0), px(10.0)).unwrap_or_default();
        assert_eq!(top, px(0.0));
        assert_eq!(height, px(40.0));
        // One line back: thumb near the bottom.
        let (top, _) = scrollback_thumb(1, 80, 20, px(200.0), px(10.0)).unwrap_or_default();
        assert!(top > px(155.0) && top <= px(160.0));
        // Long history keeps the minimum height.
        let (_, height) = scrollback_thumb(5, 100_000, 20, px(200.0), px(10.0)).unwrap_or_default();
        assert_eq!(height, px(10.0));
    }
}
