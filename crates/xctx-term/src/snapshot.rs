//! Owned, thread-safe copies of terminal state for drawing.

use bitflags::bitflags;

/// A 24-bit colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
}

impl Rgb {
    /// Builds a colour from its three channels.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// One of the sixteen ANSI colours a program selects by name (SGR 30-37, 40-47, 90-97, 100-107).
///
/// The view maps these to its theme; [`default_indexed_color`] gives xterm defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnsiColor {
    /// Colour 0.
    Black,
    /// Colour 1.
    Red,
    /// Colour 2.
    Green,
    /// Colour 3.
    Yellow,
    /// Colour 4.
    Blue,
    /// Colour 5.
    Magenta,
    /// Colour 6.
    Cyan,
    /// Colour 7.
    White,
    /// Colour 8.
    BrightBlack,
    /// Colour 9.
    BrightRed,
    /// Colour 10.
    BrightGreen,
    /// Colour 11.
    BrightYellow,
    /// Colour 12.
    BrightBlue,
    /// Colour 13.
    BrightMagenta,
    /// Colour 14.
    BrightCyan,
    /// Colour 15.
    BrightWhite,
}

impl AnsiColor {
    /// The palette index of this colour, 0 to 15.
    pub fn index(self) -> u8 {
        self as u8
    }

    /// The named colour for a palette index, or `None` above 15.
    pub fn from_index(index: u8) -> Option<Self> {
        use AnsiColor::*;
        const ALL: [AnsiColor; 16] = [
            Black,
            Red,
            Green,
            Yellow,
            Blue,
            Magenta,
            Cyan,
            White,
            BrightBlack,
            BrightRed,
            BrightGreen,
            BrightYellow,
            BrightBlue,
            BrightMagenta,
            BrightCyan,
            BrightWhite,
        ];
        ALL.get(usize::from(index)).copied()
    }
}

/// The colour of a cell's foreground, background or underline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Color {
    /// The terminal's default colour for this role: default foreground for `fg` and underline,
    /// default background for `bg`.
    Default,
    /// One of the sixteen named ANSI colours.
    Named(AnsiColor),
    /// A 256-colour palette index (SGR 38;5;n). Indexes 0-15 are the named colours.
    Indexed(u8),
    /// A truecolor value (SGR 38;2;r;g;b).
    Rgb(Rgb),
}

/// The xterm default RGB for a 256-colour palette index.
///
/// Indexes 16-231 are the 6x6x6 colour cube and 232-255 the grayscale ramp, which every
/// terminal agrees on. Indexes 0-15 are xterm's defaults; a view normally replaces them with
/// its theme.
pub fn default_indexed_color(index: u8) -> Rgb {
    const ANSI: [Rgb; 16] = [
        Rgb::new(0x00, 0x00, 0x00),
        Rgb::new(0xcd, 0x00, 0x00),
        Rgb::new(0x00, 0xcd, 0x00),
        Rgb::new(0xcd, 0xcd, 0x00),
        Rgb::new(0x00, 0x00, 0xee),
        Rgb::new(0xcd, 0x00, 0xcd),
        Rgb::new(0x00, 0xcd, 0xcd),
        Rgb::new(0xe5, 0xe5, 0xe5),
        Rgb::new(0x7f, 0x7f, 0x7f),
        Rgb::new(0xff, 0x00, 0x00),
        Rgb::new(0x00, 0xff, 0x00),
        Rgb::new(0xff, 0xff, 0x00),
        Rgb::new(0x5c, 0x5c, 0xff),
        Rgb::new(0xff, 0x00, 0xff),
        Rgb::new(0x00, 0xff, 0xff),
        Rgb::new(0xff, 0xff, 0xff),
    ];
    match index {
        0..=15 => ANSI[usize::from(index)],
        16..=231 => {
            let cube = index - 16;
            let level = |v: u8| if v == 0 { 0 } else { 55 + v * 40 };
            Rgb::new(level(cube / 36), level((cube / 6) % 6), level(cube % 6))
        }
        232..=255 => {
            let gray = 8 + (index - 232) * 10;
            Rgb::new(gray, gray, gray)
        }
    }
}

bitflags! {
    /// Rendering attributes of one cell.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct CellFlags: u16 {
        /// Bold (SGR 1).
        const BOLD = 1 << 0;
        /// Dim / faint (SGR 2).
        const DIM = 1 << 1;
        /// Italic (SGR 3).
        const ITALIC = 1 << 2;
        /// Single underline (SGR 4).
        const UNDERLINE = 1 << 3;
        /// Double underline (SGR 21 or 4:2).
        const DOUBLE_UNDERLINE = 1 << 4;
        /// Curly underline (SGR 4:3).
        const UNDERCURL = 1 << 5;
        /// Dotted underline (SGR 4:4).
        const DOTTED_UNDERLINE = 1 << 6;
        /// Dashed underline (SGR 4:5).
        const DASHED_UNDERLINE = 1 << 7;
        /// Strikethrough (SGR 9).
        const STRIKETHROUGH = 1 << 8;
        /// Swap foreground and background (SGR 7).
        const INVERSE = 1 << 9;
        /// Do not draw the glyph (SGR 8).
        const HIDDEN = 1 << 10;
        /// The first column of a double-width character; draw the glyph two cells wide.
        const WIDE_CHAR = 1 << 11;
        /// The second column of a double-width character; draw nothing.
        const WIDE_CHAR_SPACER = 1 << 12;
        /// An empty last column left because a wide character wrapped to the next row.
        const LEADING_WIDE_CHAR_SPACER = 1 << 13;
        /// The row continues on the next row (soft wrap). Only set on the last column.
        const WRAPLINE = 1 << 14;
    }
}

/// One cell of the visible grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    /// The character in the cell; a space when empty.
    pub c: char,
    /// Zero-width characters (combining marks, variation selectors) drawn with `c`.
    pub zerowidth: Vec<char>,
    /// Foreground colour.
    pub fg: Color,
    /// Background colour.
    pub bg: Color,
    /// Underline colour (SGR 58); `None` means use the foreground.
    pub underline_color: Option<Color>,
    /// Rendering attributes.
    pub flags: CellFlags,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            c: ' ',
            zerowidth: Vec::new(),
            fg: Color::Default,
            bg: Color::Default,
            underline_color: None,
            flags: CellFlags::empty(),
        }
    }
}

/// How the cursor is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CursorShape {
    /// A filled block.
    #[default]
    Block,
    /// A line under the cell.
    Underline,
    /// A vertical bar before the cell.
    Beam,
    /// An outlined block.
    HollowBlock,
}

/// The cursor as it should be drawn in the viewport.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CursorSnapshot {
    /// Column, 0-based.
    pub col: usize,
    /// Viewport row, 0-based. Only meaningful when `visible` is true.
    pub row: usize,
    /// Shape the program asked for (DECSCUSR).
    pub shape: CursorShape,
    /// False when the program hid the cursor (DECTCEM) or the viewport is scrolled so the cursor
    /// is off screen.
    pub visible: bool,
    /// Whether the program asked for a blinking cursor.
    pub blinking: bool,
}

/// Which mouse events the program asked to receive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MouseReporting {
    /// No reporting; the view owns the mouse (selection, scrollback).
    #[default]
    Off,
    /// Presses and releases only (mode 1000).
    Click,
    /// Presses, releases, and motion while a button is held (mode 1002).
    Drag,
    /// Presses, releases, and all motion (mode 1003).
    Motion,
}

/// How mouse reports are encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MouseEncoding {
    /// X10-style `CSI M` with byte coordinates (limited to 223 columns/rows).
    #[default]
    Default,
    /// `CSI M` with UTF-8 encoded coordinates (mode 1005).
    Utf8,
    /// `CSI < b;x;y M/m` (mode 1006).
    Sgr,
}

bitflags! {
    /// Progressive enhancement flags of the kitty keyboard protocol, as pushed by the program
    /// with `CSI > flags u`.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
    pub struct KittyKeyboardFlags: u8 {
        /// Report keys that are ambiguous in legacy encoding as `CSI u`.
        const DISAMBIGUATE_ESCAPE_CODES = 1;
        /// Report repeat and release events.
        const REPORT_EVENT_TYPES = 2;
        /// Report the shifted key alongside the base key.
        const REPORT_ALTERNATE_KEYS = 4;
        /// Report every key, including plain text, as an escape code.
        const REPORT_ALL_KEYS_AS_ESCAPE_CODES = 8;
        /// Report the text a key produces inside the escape code.
        const REPORT_ASSOCIATED_TEXT = 16;
    }
}

/// Terminal modes that change how input must be encoded or how the view behaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct TerminalModes {
    /// The program shows the cursor (DECTCEM).
    pub show_cursor: bool,
    /// Arrow keys and Home/End send `SS3` sequences (DECCKM).
    pub app_cursor: bool,
    /// The keypad is in application mode (DECKPAM).
    pub app_keypad: bool,
    /// Pastes must be wrapped in `CSI 200~` / `CSI 201~` (mode 2004).
    pub bracketed_paste: bool,
    /// The program wants `CSI I` / `CSI O` on focus changes (mode 1004).
    pub focus_reporting: bool,
    /// The alternate screen is active (full-screen programs such as `nvim`).
    pub alt_screen: bool,
    /// The wheel should send arrow keys on the alternate screen when mouse reporting is off
    /// (mode 1007).
    pub alternate_scroll: bool,
    /// Enter sends CR LF instead of CR (LNM).
    pub line_feed_new_line: bool,
    /// Which mouse events the program asked for.
    pub mouse_reporting: MouseReporting,
    /// How mouse events are encoded.
    pub mouse_encoding: MouseEncoding,
    /// Active kitty keyboard protocol flags; empty means legacy key encoding.
    pub kitty_keyboard: KittyKeyboardFlags,
}

/// A point in the visible viewport, 0-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ViewportPoint {
    /// Viewport row; 0 is the top visible row, whatever the scroll offset.
    pub row: usize,
    /// Column.
    pub col: usize,
}

impl ViewportPoint {
    /// Builds a point from a viewport row and column.
    pub const fn new(row: usize, col: usize) -> Self {
        Self { row, col }
    }
}

/// The part of the active selection that is visible, in viewport coordinates, inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectionSpan {
    /// First selected cell (row-major order).
    pub start: ViewportPoint,
    /// Last selected cell (row-major order), inclusive.
    pub end: ViewportPoint,
}

impl SelectionSpan {
    /// Whether the cell at `row`, `col` is selected.
    pub fn contains(&self, row: usize, col: usize) -> bool {
        let point = (row, col);
        (self.start.row, self.start.col) <= point && point <= (self.end.row, self.end.col)
    }
}

/// An owned copy of everything needed to draw the terminal at one moment.
///
/// Taking a snapshot holds the terminal lock only while copying; the snapshot can then be sent
/// to and read from any thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridSnapshot {
    /// Number of columns.
    pub cols: usize,
    /// Number of visible rows.
    pub rows: usize,
    /// Visible cells, row-major, `rows * cols` long.
    pub cells: Vec<Cell>,
    /// The cursor.
    pub cursor: CursorSnapshot,
    /// Window title set by the program (OSC 0/2), if any.
    pub title: Option<String>,
    /// Modes the program has set.
    pub modes: TerminalModes,
    /// The visible part of the active selection.
    pub selection: Option<SelectionSpan>,
    /// How many lines the viewport is scrolled back into history; 0 means following output.
    pub display_offset: usize,
    /// Lines of scrollback currently stored above the screen.
    pub history_size: usize,
    /// Increases whenever terminal state changes; equal generations mean equal content.
    pub generation: u64,
}

impl GridSnapshot {
    /// The cell at a viewport row and column, if inside the grid.
    pub fn cell(&self, row: usize, col: usize) -> Option<&Cell> {
        if row >= self.rows || col >= self.cols {
            return None;
        }
        self.cells.get(row * self.cols + col)
    }

    /// The cells of one viewport row.
    pub fn row(&self, row: usize) -> Option<&[Cell]> {
        if row >= self.rows {
            return None;
        }
        self.cells.get(row * self.cols..(row + 1) * self.cols)
    }

    /// The text of one viewport row with trailing spaces removed and wide-character spacers
    /// skipped.
    pub fn row_text(&self, row: usize) -> String {
        let Some(cells) = self.row(row) else {
            return String::new();
        };
        let mut text = String::new();
        for cell in cells {
            if cell
                .flags
                .intersects(CellFlags::WIDE_CHAR_SPACER | CellFlags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            text.push(cell.c);
            text.extend(cell.zerowidth.iter());
        }
        text.truncate(text.trim_end_matches(' ').len());
        text
    }

    /// All visible rows joined with `\n`, each trimmed as in [`GridSnapshot::row_text`].
    pub fn text(&self) -> String {
        (0..self.rows)
            .map(|row| self.row_text(row))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_indexed_color_cube_and_grayscale() {
        assert_eq!(default_indexed_color(16), Rgb::new(0, 0, 0));
        assert_eq!(default_indexed_color(196), Rgb::new(255, 0, 0));
        assert_eq!(default_indexed_color(231), Rgb::new(255, 255, 255));
        assert_eq!(default_indexed_color(232), Rgb::new(8, 8, 8));
        assert_eq!(default_indexed_color(255), Rgb::new(238, 238, 238));
    }

    #[test]
    fn ansi_color_index_round_trips() {
        for index in 0..16 {
            let color = AnsiColor::from_index(index);
            assert_eq!(color.map(AnsiColor::index), Some(index));
        }
        assert_eq!(AnsiColor::from_index(16), None);
    }

    #[test]
    fn selection_span_contains_is_row_major() {
        let span = SelectionSpan {
            start: ViewportPoint::new(1, 5),
            end: ViewportPoint::new(2, 2),
        };
        assert!(!span.contains(1, 4));
        assert!(span.contains(1, 5));
        assert!(span.contains(1, 79));
        assert!(span.contains(2, 0));
        assert!(span.contains(2, 2));
        assert!(!span.contains(2, 3));
    }
}
