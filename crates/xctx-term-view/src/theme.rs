//! Font and colours a terminal view draws with, and how cell colours resolve against them.

use gpui::{Rgba, SharedString, px, rgb, rgba};
use xctx_term::{AnsiColor, Cell, CellFlags, Color, Rgb, TerminalConfig, default_indexed_color};

/// Preferred monospaced fonts with built-in Nerd Font / Powerline icon coverage, tried in order
/// when resolving the default terminal font against installed system fonts.
const PREFERRED_FAMILIES: &[&str] = &[
    "JetBrainsMono Nerd Font Mono",
    "JetBrainsMono Nerd Font",
    "MesloLGS Nerd Font Mono",
    "MesloLGS Nerd Font",
    "CaskaydiaCove Nerd Font Mono",
    "CaskaydiaCove Nerd Font",
    "Hack Nerd Font Mono",
    "FiraCode Nerd Font Mono",
    "SF Mono",
    "JetBrains Mono",
    "Menlo",
];

/// Fallback fonts for Nerd Font icons, Powerline glyphs, Unicode symbols, and emoji.
const DEFAULT_FALLBACKS: &[&str] = &[
    "JetBrainsMono Nerd Font Mono",
    "JetBrainsMono Nerd Font",
    "MesloLGS Nerd Font Mono",
    "CaskaydiaCove Nerd Font Mono",
    "Symbols Nerd Font Mono",
    "Symbols Nerd Font",
    "Menlo",
    "Apple Symbols",
    "Apple Color Emoji",
];

/// Minimum and maximum font sizes (in px) for Cmd+= / Cmd+- zoom.
pub const MIN_FONT_SIZE: f32 = 9.0;
pub const MAX_FONT_SIZE: f32 = 28.0;
pub const DEFAULT_FONT_SIZE: f32 = 13.0;

/// The font a terminal draws its grid with. Every cell is one advance of `'m'` wide and
/// `size * line_height` tall.
#[derive(Debug, Clone, PartialEq)]
pub struct TerminalFont {
    /// Font family. Use a monospaced font; proportional fonts are drawn on the cell grid anyway.
    pub family: SharedString,
    /// Fallback font families tried when a glyph (such as a Nerd Font icon or symbol) is absent
    /// from `family`.
    pub fallbacks: Vec<SharedString>,
    /// Font size in pixels.
    pub size: f32,
    /// Row height as a multiple of `size`.
    pub line_height: f32,
}

impl Default for TerminalFont {
    fn default() -> Self {
        Self {
            family: "Menlo".into(),
            fallbacks: DEFAULT_FALLBACKS.iter().map(|&f| f.into()).collect(),
            size: DEFAULT_FONT_SIZE,
            line_height: 1.3,
        }
    }
}

impl TerminalFont {
    /// Picks the best installed Nerd Font / monospace family when still using the default `"Menlo"`,
    /// and trims fallbacks to fonts available on this system (preserving core macOS symbol fallbacks).
    pub fn resolve_for_available_fonts(&mut self, available: &[String]) {
        if available.is_empty() {
            return;
        }
        let has = |name: &str| available.iter().any(|f| f == name);
        if self.family == "Menlo"
            && let Some(&best) = PREFERRED_FAMILIES.iter().find(|&&name| has(name))
        {
            self.family = best.into();
        }
        let mut resolved: Vec<SharedString> = self
            .fallbacks
            .iter()
            .filter(|f| f.as_ref() != self.family.as_ref() && has(f.as_ref()))
            .cloned()
            .collect();
        for &core in &["Menlo", "Apple Symbols", "Apple Color Emoji"] {
            if self.family.as_ref() != core
                && !resolved.iter().any(|f| f.as_ref() == core)
                && has(core)
            {
                resolved.push(core.into());
            }
        }
        self.fallbacks = resolved;
    }

    /// Resolves `family` and `fallbacks` against the window's installed fonts.
    pub fn resolve_for_window(&mut self, window: &gpui::Window) {
        let available = window.text_system().all_font_names();
        self.resolve_for_available_fonts(&available);
    }

    /// The GPUI font for a cell's weight and slant. Ligatures are disabled so every cell
    /// keeps its own glyph.
    pub fn gpui_font(&self, bold: bool, italic: bool) -> gpui::Font {
        let fallbacks = (!self.fallbacks.is_empty()).then(|| {
            gpui::FontFallbacks::from_fonts(
                self.fallbacks
                    .iter()
                    .map(|f| f.as_ref().to_owned())
                    .collect(),
            )
        });
        gpui::Font {
            family: self.family.clone(),
            features: gpui::FontFeatures::disable_ligatures(),
            fallbacks,
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
    /// Tint drawn over the view while files are dragged onto it; the 2px outline uses the
    /// same colour fully opaque.
    pub drop_highlight: Rgba,
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
            drop_highlight: rgba(0x2ea8ff2e),
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
        // Glyphs stay opaque even when inverse hands them a translucent default background.
        foreground.a = 1.0;
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

    #[test]
    fn default_font_prefers_installed_nerd_font_and_populates_fallbacks() {
        let mut font = TerminalFont::default();
        let available = vec![
            "Menlo".to_owned(),
            "Apple Symbols".to_owned(),
            "JetBrainsMono Nerd Font Mono".to_owned(),
            "Apple Color Emoji".to_owned(),
        ];
        font.resolve_for_available_fonts(&available);
        assert_eq!(font.family.as_ref(), "JetBrainsMono Nerd Font Mono");
        assert!(font.fallbacks.iter().any(|f| f.as_ref() == "Apple Symbols"));
        assert!(
            font.fallbacks
                .iter()
                .any(|f| f.as_ref() == "Apple Color Emoji")
        );
        assert!(font.gpui_font(false, false).fallbacks.is_some());
    }
}
