//! Font and colours a terminal view draws with, and how cell colours resolve against them.

use gpui::{Rgba, SharedString, px, rgb, rgba};
use xctx_term::{AnsiColor, Cell, CellFlags, Color, Rgb, TerminalConfig, default_indexed_color};

/// The font a terminal draws its grid with. Every cell is one advance of `'m'` wide and
/// `size * line_height` tall.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalFont {
    /// Font family. Use a monospaced font; proportional fonts are drawn on the cell grid anyway.
    pub family: SharedString,
    /// Font size in pixels.
    pub size: f32,
    /// Row height as a multiple of `size`.
    pub line_height: f32,
}

impl Default for TerminalFont {
    fn default() -> Self {
        Self {
            family: "Menlo".into(),
            size: 13.0,
            line_height: 1.3,
        }
    }
}

impl TerminalFont {
    /// The GPUI font for a cell's weight and slant. Ligatures are disabled so every cell
    /// keeps its own glyph.
    pub fn gpui_font(&self, bold: bool, italic: bool) -> gpui::Font {
        gpui::Font {
            family: self.family.clone(),
            features: gpui::FontFeatures::disable_ligatures(),
            fallbacks: None,
            weight: if bold {
                gpui::FontWeight::BOLD
            } else {
                gpui::FontWeight::NORMAL
            },
            style: if italic {
                gpui::FontStyle::Italic
            } else {
                gpui::FontStyle::Normal
            },
        }
    }

    /// The font size as GPUI pixels.
    pub fn size_px(&self) -> gpui::Pixels {
        px(self.size)
    }
}

/// Colours of a terminal view: the defaults, the cursor, the selection and the sixteen ANSI
/// colours. Indexed colours 16-255 always use the xterm cube and grayscale ramp.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalTheme {
    /// Text colour of cells with the default foreground.
    pub foreground: Rgba,
    /// Fill of the view and of cells with the default background.
    pub background: Rgba,
    /// Fill of the cursor.
    pub cursor: Rgba,
    /// Colour of the character under a filled block cursor.
    pub cursor_text: Rgba,
    /// Overlay drawn over selected cells; normally translucent.
    pub selection: Rgba,
    /// Thumb of the scrollback indicator shown while scrolled back.
    pub scrollback_indicator: Rgba,
    /// ANSI colours 0-15: black, red, green, yellow, blue, magenta, cyan, white, then the
    /// bright variants in the same order.
    pub ansi: [Rgba; 16],
}

impl Default for TerminalTheme {
    fn default() -> Self {
        Self {
            foreground: rgb(0xe4e4e7),
            background: rgb(0x18181b),
            cursor: rgb(0xe4e4e7),
            cursor_text: rgb(0x18181b),
            selection: rgba(0x60a5fa55),
            scrollback_indicator: rgba(0xa1a1aa66),
            ansi: [
                rgb(0x27272a),
                rgb(0xf87171),
                rgb(0x4ade80),
                rgb(0xfacc15),
                rgb(0x60a5fa),
                rgb(0xc084fc),
                rgb(0x22d3ee),
                rgb(0xd4d4d8),
                rgb(0x71717a),
                rgb(0xfca5a5),
                rgb(0x86efac),
                rgb(0xfde047),
                rgb(0x93c5fd),
                rgb(0xd8b4fe),
                rgb(0x67e8f9),
                rgb(0xfafafa),
            ],
        }
    }
}

/// The colours one cell is drawn with, after inverse and dim are applied.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellColors {
    /// Glyph colour.
    pub foreground: Rgba,
    /// Cell fill.
    pub background: Rgba,
}

/// How much of the foreground survives in dim text; the rest is the background.
const DIM_FACTOR: f32 = 0.66;

impl TerminalTheme {
    /// Copies the default foreground and background into `config`, so programs that query
    /// them (OSC 10/11, used by `nvim` to pick light or dark) see this theme.
    pub fn apply_to_config(&self, config: &mut TerminalConfig) {
        config.default_foreground = rgb_from_rgba(self.foreground);
        config.default_background = rgb_from_rgba(self.background);
    }

    /// The RGBA value of a cell colour. `Color::Default` resolves to `default`.
    pub fn resolve(&self, color: Color, default: Rgba) -> Rgba {
        match color {
            Color::Default => default,
            Color::Named(named) => self.ansi_color(named),
            Color::Indexed(index) => match AnsiColor::from_index(index) {
                Some(named) => self.ansi_color(named),
                None => rgba_from_rgb(default_indexed_color(index)),
            },
            Color::Rgb(value) => rgba_from_rgb(value),
        }
    }

    /// The theme colour for one of the sixteen named ANSI colours.
    pub fn ansi_color(&self, color: AnsiColor) -> Rgba {
        self.ansi[usize::from(color.index())]
    }

    /// The foreground and background a cell is drawn with: inverse swaps them, and dim
    /// blends the foreground toward the background.
    pub fn cell_colors(&self, cell: &Cell) -> CellColors {
        let mut foreground = self.resolve(cell.fg, self.foreground);
        let mut background = self.resolve(cell.bg, self.background);
        if cell.flags.contains(CellFlags::INVERSE) {
            std::mem::swap(&mut foreground, &mut background);
        }
        if cell.flags.contains(CellFlags::DIM) {
            foreground = mix(foreground, background, DIM_FACTOR);
        }
        CellColors {
            foreground,
            background,
        }
    }
}

/// `a * factor + b * (1 - factor)`, channel by channel, keeping `a`'s alpha.
fn mix(a: Rgba, b: Rgba, factor: f32) -> Rgba {
    let blend = |x: f32, y: f32| x * factor + y * (1.0 - factor);
    Rgba {
        r: blend(a.r, b.r),
        g: blend(a.g, b.g),
        b: blend(a.b, b.b),
        a: a.a,
    }
}

/// An opaque GPUI colour from an `xctx-term` colour.
pub fn rgba_from_rgb(value: Rgb) -> Rgba {
    rgb(u32::from(value.r) << 16 | u32::from(value.g) << 8 | u32::from(value.b))
}

/// An `xctx-term` colour from a GPUI colour, ignoring alpha.
pub fn rgb_from_rgba(value: Rgba) -> Rgb {
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb::new(channel(value.r), channel(value.g), channel(value.b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(fg: Color, bg: Color, flags: CellFlags) -> Cell {
        Cell {
            fg,
            bg,
            flags,
            ..Cell::default()
        }
    }

    #[test]
    fn resolve_truecolor_is_exact() {
        let theme = TerminalTheme::default();
        let color = theme.resolve(Color::Rgb(Rgb::new(0x12, 0x34, 0x56)), theme.foreground);
        assert_eq!(color, rgb(0x123456));
        assert_eq!(rgb_from_rgba(color), Rgb::new(0x12, 0x34, 0x56));
    }

    #[test]
    fn resolve_256_color_uses_cube_and_ramp() {
        let theme = TerminalTheme::default();
        assert_eq!(
            theme.resolve(Color::Indexed(196), theme.foreground),
            rgb(0xff0000)
        );
        assert_eq!(
            theme.resolve(Color::Indexed(232), theme.foreground),
            rgb(0x080808)
        );
        // Indexes 0-15 follow the theme, not xterm.
        assert_eq!(
            theme.resolve(Color::Indexed(1), theme.foreground),
            theme.ansi[1]
        );
    }

    #[test]
    fn resolve_named_and_default() {
        let theme = TerminalTheme::default();
        assert_eq!(
            theme.resolve(Color::Named(AnsiColor::BrightBlue), theme.foreground),
            theme.ansi[12]
        );
        assert_eq!(
            theme.resolve(Color::Default, theme.background),
            theme.background
        );
    }

    #[test]
    fn inverse_swaps_default_colors() {
        let theme = TerminalTheme::default();
        let colors = theme.cell_colors(&cell(Color::Default, Color::Default, CellFlags::INVERSE));
        assert_eq!(colors.foreground, theme.background);
        assert_eq!(colors.background, theme.foreground);

        let red_on_blue = cell(
            Color::Named(AnsiColor::Red),
            Color::Named(AnsiColor::Blue),
            CellFlags::INVERSE,
        );
        let colors = theme.cell_colors(&red_on_blue);
        assert_eq!(colors.foreground, theme.ansi[4]);
        assert_eq!(colors.background, theme.ansi[1]);
    }

    #[test]
    fn dim_blends_toward_background() {
        let theme = TerminalTheme {
            foreground: rgb(0xffffff),
            background: rgb(0x000000),
            ..TerminalTheme::default()
        };
        let colors = theme.cell_colors(&cell(Color::Default, Color::Default, CellFlags::DIM));
        assert!((colors.foreground.r - DIM_FACTOR).abs() < 1e-6);
        assert_eq!(colors.background, rgb(0x000000));
        let plain = theme.cell_colors(&cell(Color::Default, Color::Default, CellFlags::empty()));
        assert_eq!(plain.foreground, rgb(0xffffff));
    }

    #[test]
    fn apply_to_config_sets_query_colors() {
        let theme = TerminalTheme::default();
        let mut config = TerminalConfig::new("/bin/sh");
        theme.apply_to_config(&mut config);
        assert_eq!(config.default_background, Rgb::new(0x18, 0x18, 0x1b));
        assert_eq!(config.default_foreground, Rgb::new(0xe4, 0xe4, 0xe7));
    }
}
