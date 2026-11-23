//! Colours, spacing, radii and font sizes shared by every xctx component.

use gpui::{App, Global, Hsla, Pixels, px, rgb};
use xctx_layout::PaneKind;

/// The colours of the xctx window. Set one as a GPUI global (`cx.set_global(Theme::dark())`) to restyle every
/// component; [`Theme::of`] falls back to [`Theme::dark`] when none is set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// Window background, behind panes and bars.
    pub bg: Hsla,
    /// Panes, bars and resting buttons.
    pub surface: Hsla,
    /// A surface under the mouse.
    pub surface_hover: Hsla,
    /// Raised surfaces: pane headers, active tabs, dialogs, the prompt.
    pub elevated: Hsla,
    /// Resting outlines.
    pub border: Hsla,
    /// The outline of the focused pane or input.
    pub border_focused: Hsla,
    /// Split dividers and separators.
    pub divider: Hsla,
    /// A divider under the mouse or being dragged.
    pub divider_hover: Hsla,
    /// Primary text.
    pub text: Hsla,
    /// Secondary text: labels, inactive tabs.
    pub text_muted: Hsla,
    /// Tertiary text: placeholders, hints.
    pub text_subtle: Hsla,
    /// The highlight colour: active items, primary buttons, the caret.
    pub accent: Hsla,
    /// A translucent accent for selected backgrounds and text selection.
    pub accent_muted: Hsla,
    /// Things that went well or are running.
    pub success: Hsla,
    /// Things that need attention.
    pub warning: Hsla,
    /// Errors and destructive actions.
    pub danger: Hsla,
}

impl Global for Theme {}

/// The meaning of a coloured element, which picks its colours from the theme with [`Theme::tone`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    /// No particular meaning.
    Neutral,
    /// Active or primary.
    Accent,
    /// Success or running.
    Success,
    /// Needs attention.
    Warning,
    /// Error or destructive.
    Danger,
}

impl Tone {
    /// The tone of a pane's role badge: agents use the accent, shells success, git panes warning.
    pub fn for_pane_kind(kind: PaneKind) -> Self {
        match kind {
            PaneKind::Agent => Self::Accent,
            PaneKind::Shell => Self::Success,
            PaneKind::Git => Self::Warning,
        }
    }
}

/// The colours of a tinted element such as a badge, from [`Theme::tone`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ToneColors {
    /// Text colour.
    pub fg: Hsla,
    /// Background fill.
    pub bg: Hsla,
    /// Outline.
    pub border: Hsla,
}

fn hex(value: u32) -> Hsla {
    rgb(value).into()
}

impl Theme {
    /// Extra-small gap: 2px.
    pub const SPACE_0_5: Pixels = px(2.0);
    /// Small gap: 4px.
    pub const SPACE_1: Pixels = px(4.0);
    /// Default gap: 8px.
    pub const SPACE_2: Pixels = px(8.0);
    /// Medium gap: 12px.
    pub const SPACE_3: Pixels = px(12.0);
    /// Large gap: 16px.
    pub const SPACE_4: Pixels = px(16.0);
    /// Large gap: 24px.
    pub const SPACE_6: Pixels = px(24.0);

    /// Corner radius of badges and small controls.
    pub const RADIUS_SM: Pixels = px(4.0);
    /// Corner radius of buttons, inputs and panes.
    pub const RADIUS_MD: Pixels = px(6.0);
    /// Corner radius of dialogs and cards.
    pub const RADIUS_LG: Pixels = px(10.0);
    /// Corner radius of pills.
    pub const RADIUS_FULL: Pixels = px(999.0);

    /// Font size of badges and hints.
    pub const TEXT_XS: Pixels = px(11.0);
    /// Font size of bars, headers and buttons.
    pub const TEXT_SM: Pixels = px(12.0);
    /// Font size of body text.
    pub const TEXT_MD: Pixels = px(13.0);
    /// Font size of the prompt.
    pub const TEXT_LG: Pixels = px(15.0);
    /// Font size of headings.
    pub const TEXT_XL: Pixels = px(20.0);

    /// Height of the tab bar.
    pub const TAB_BAR_HEIGHT: Pixels = px(38.0);
    /// Height of the status bar.
    pub const STATUS_BAR_HEIGHT: Pixels = px(24.0);
    /// Height of a pane's header strip.
    pub const PANE_HEADER_HEIGHT: Pixels = px(24.0);
    /// Thickness of the gap between split panes, which is also the divider's drawn width.
    pub const DIVIDER_THICKNESS: Pixels = px(4.0);
    /// How far a divider's grab area extends past its drawn width on each side.
    pub const DIVIDER_GRAB_SLOP: Pixels = px(3.0);

    /// The default dark theme: near-black background, neutral greys and a blue accent, tuned for contrast in
    /// long sessions.
    pub fn dark() -> Self {
        let accent = hex(0x5b8cff);
        Self {
            bg: hex(0x0d0e10),
            surface: hex(0x15161a),
            surface_hover: hex(0x1f2126),
            elevated: hex(0x1b1d21),
            border: hex(0x2a2d33),
            border_focused: accent,
            divider: hex(0x24262b),
            divider_hover: accent,
            text: hex(0xe9eaee),
            text_muted: hex(0xa4a8b0),
            text_subtle: hex(0x6c717b),
            accent,
            accent_muted: accent.alpha(0.24),
            success: hex(0x3fb950),
            warning: hex(0xd8a23a),
            danger: hex(0xf2564d),
        }
    }

    /// The theme set as a global, or [`Theme::dark`] when none is set.
    pub fn of(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or_else(Self::dark)
    }

    /// The text, fill and outline colours for `tone`. Tinted tones use the tone's colour for text over a faint
    /// fill of the same colour.
    pub fn tone(&self, tone: Tone) -> ToneColors {
        let color = match tone {
            Tone::Neutral => {
                return ToneColors {
                    fg: self.text_muted,
                    bg: self.surface_hover,
                    border: self.border,
                };
            }
            Tone::Accent => self.accent,
            Tone::Success => self.success,
            Tone::Warning => self.warning,
            Tone::Danger => self.danger,
        };
        ToneColors {
            fg: color,
            bg: color.alpha(0.14),
            border: color.alpha(0.36),
        }
    }

    /// The outline of a pane or input: [`Theme::border_focused`] when focused, else [`Theme::border`].
    pub fn outline(&self, focused: bool) -> Hsla {
        if focused {
            self.border_focused
        } else {
            self.border
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance of an opaque colour.
    fn luminance(color: Hsla) -> f32 {
        let rgba = color.to_rgb();
        let channel = |c: f32| {
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(rgba.r) + 0.7152 * channel(rgba.g) + 0.0722 * channel(rgba.b)
    }

    fn contrast(a: Hsla, b: Hsla) -> f32 {
        let (la, lb) = (luminance(a), luminance(b));
        (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
    }

    #[test]
    fn dark_text_is_readable_on_every_surface() {
        let theme = Theme::dark();
        for surface in [theme.bg, theme.surface, theme.elevated, theme.surface_hover] {
            assert!(contrast(theme.text, surface) >= 12.0);
            assert!(contrast(theme.text_muted, surface) >= 6.0);
            assert!(contrast(theme.text_subtle, surface) >= 3.0);
        }
    }

    #[test]
    fn surfaces_get_lighter_as_they_rise() {
        let theme = Theme::dark();
        assert!(luminance(theme.bg) < luminance(theme.surface));
        assert!(luminance(theme.surface) < luminance(theme.elevated));
        assert!(luminance(theme.elevated) < luminance(theme.surface_hover));
    }

    #[test]
    fn tinted_tones_use_their_colour_and_neutral_does_not() {
        let theme = Theme::dark();
        let danger = theme.tone(Tone::Danger);
        assert_eq!(danger.fg, theme.danger);
        assert!(danger.bg.a < danger.border.a && danger.border.a < 1.0);
        let neutral = theme.tone(Tone::Neutral);
        assert_eq!(neutral.fg, theme.text_muted);
        assert_eq!(neutral.border, theme.border);
    }

    #[test]
    fn pane_kinds_have_distinct_tones() {
        let tones = [PaneKind::Agent, PaneKind::Shell, PaneKind::Git].map(Tone::for_pane_kind);
        assert_eq!(tones, [Tone::Accent, Tone::Success, Tone::Warning]);
    }

    #[test]
    fn outline_follows_focus() {
        let theme = Theme::dark();
        assert_eq!(theme.outline(true), theme.border_focused);
        assert_eq!(theme.outline(false), theme.border);
    }
}
