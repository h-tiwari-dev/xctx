//! Colours, spacing, radii and font sizes shared by every xctx component.

use gpui::{App, Global, Hsla, Pixels, px, rgb};
use xctx_layout::PaneKind;

/// The colours of the xctx window. Set one as a GPUI global (`cx.set_global(Theme::dark())`) to restyle every
/// component; [`Theme::of`] falls back to [`Theme::dark`] when none is set.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// Window background, Roster, title/tab area, status line and split gaps.
    pub chrome: Hsla,
    /// Panes, the terminal background, cards and Inspector body.
    pub surface: Hsla,
    /// Pane headers, inputs, resting buttons and hovered rows.
    pub raised: Hsla,
    /// Dialogs, the palette, the Inbox overlay, toasts and menus.
    pub overlay: Hsla,
    /// Dividers inside a surface and split dividers at rest.
    pub border_subtle: Hsla,
    /// Outlines of cards, inputs and panes.
    pub border: Hsla,
    /// Outlines of overlays and hovered inputs.
    pub border_strong: Hsla,
    /// Translucent backdrop behind an overlay.
    pub scrim: Hsla,
    /// Primary text.
    pub text: Hsla,
    /// Secondary text: labels, inactive rows and secondary lines.
    pub text_muted: Hsla,
    /// Tertiary text: placeholders, key hints and timestamps only.
    pub text_subtle: Hsla,
    /// Primary buttons, links, the focus ring, the caret and the selected item's marker.
    pub accent: Hsla,
    /// A primary button under the mouse.
    pub accent_hover: Hsla,
    /// Text on an `accent`, `success` or `danger` fill.
    pub on_accent: Hsla,
    /// Selected row background and text selection.
    pub selection: Hsla,
    /// A translucent wash under a hovered row or ghost button; reads the same on any surface, opaque or not.
    pub hover: Hsla,
    /// Read-outs: port, budget, cost, counts and key chords.
    pub meta: Hsla,
    /// Running, passed and agreed states.
    pub success: Hsla,
    /// Needs you, paused and waiting-for-approval states.
    pub warning: Hsla,
    /// Failed, blocked, production and destructive actions.
    pub danger: Hsla,
    /// Six identity colours assigned to non-orchestrator agents in creation order.
    pub agent: [Hsla; 6],
    /// The 16 ANSI terminal colours (0-7 normal, 8-15 bright).
    pub ansi: [Hsla; 16],
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
    /// The tone of a pane's role badge: agents use the accent, shells success, git panes warning, plugin panes
    /// neutral.
    pub fn for_pane_kind(kind: PaneKind) -> Self {
        match kind {
            PaneKind::Agent => Self::Accent,
            PaneKind::Shell => Self::Success,
            PaneKind::Git => Self::Warning,
            PaneKind::Plugin | PaneKind::File => Self::Neutral,
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

    /// Height of the window's title bar row: the traffic lights, the Roster toggle and the space header.
    pub const TITLE_BAR_HEIGHT: Pixels = px(40.0);
    /// Room left of the title bar's content for the window's traffic lights.
    pub const TRAFFIC_LIGHT_INSET: Pixels = px(78.0);
    /// Where the traffic lights sit, from the window's top-left corner, so they are centred in the title bar.
    pub const TRAFFIC_LIGHT_POSITION: (Pixels, Pixels) = (px(14.0), px(13.0));
    /// Height of the status bar.
    pub const STATUS_BAR_HEIGHT: Pixels = px(24.0);
    /// Height of a pane's header strip.
    pub const PANE_HEADER_HEIGHT: Pixels = px(28.0);
    /// Thickness of the gap between split panes, where a divider is grabbed; a hovered or dragged divider draws a
    /// 2px line in its middle.
    pub const DIVIDER_THICKNESS: Pixels = px(6.0);
    /// How far a divider's grab area extends past its drawn width on each side.
    pub const DIVIDER_GRAB_SLOP: Pixels = px(3.0);

    /// The default dark theme: cool blue-grey neutrals, a bright blue accent and gold read-outs.
    pub fn dark() -> Self {
        let accent = hex(0x2ea8ff);
        Self {
            chrome: hex(0x0f1216),
            surface: hex(0x151820),
            raised: hex(0x1c2029),
            overlay: hex(0x232834),
            border_subtle: hex(0x1f252d),
            border: hex(0x2a3038),
            border_strong: hex(0x3b424d),
            scrim: hex(0x000000).alpha(0.50),
            text: hex(0xe8ecf4),
            text_muted: hex(0x9ca3b0),
            text_subtle: hex(0x6b7280),
            accent,
            accent_hover: hex(0x47b2ff),
            on_accent: hex(0x0f1216),
            selection: accent.alpha(0.16),
            hover: hex(0xe8ecf4).alpha(0.07),
            meta: hex(0xd4c090),
            success: hex(0x3ddc84),
            warning: hex(0xffb347),
            danger: hex(0xff5c6a),
            agent: [
                hex(0xb392f0),
                hex(0x39d0c4),
                hex(0xf47fb8),
                hex(0xa8d672),
                hex(0x8aa4ff),
                hex(0xff9e64),
            ],
            ansi: [
                hex(0x3b424d),
                hex(0xff5c6a),
                hex(0x3ddc84),
                hex(0xf0c040),
                hex(0x2ea8ff),
                hex(0xd06fed),
                hex(0x13d1cd),
                hex(0xc3c9d3),
                hex(0x6b7280),
                hex(0xff8a94),
                hex(0x7dedb0),
                hex(0xfddb72),
                hex(0x6ec9ff),
                hex(0xe69dfc),
                hex(0x75efea),
                hex(0xf7f9fc),
            ],
        }
    }

    /// The light theme: cool blue-grey surfaces, a deep blue accent and ochre read-outs.
    pub fn light() -> Self {
        let accent = hex(0x006db3);
        Self {
            chrome: hex(0xeceef2),
            surface: hex(0xf8f9fb),
            raised: hex(0xffffff),
            overlay: hex(0xffffff),
            border_subtle: hex(0xe4e7ec),
            border: hex(0xd3d8e0),
            border_strong: hex(0xb4bac6),
            scrim: hex(0x1a1e26).alpha(0.30),
            text: hex(0x1a1e26),
            text_muted: hex(0x545b69),
            text_subtle: hex(0x737a89),
            accent,
            accent_hover: hex(0x005f9c),
            on_accent: hex(0xffffff),
            selection: accent.alpha(0.14),
            hover: hex(0x1a1e26).alpha(0.06),
            meta: hex(0x7a6118),
            success: hex(0x157a46),
            warning: hex(0x9a5a00),
            danger: hex(0xc6283a),
            agent: [
                hex(0x6f42c1),
                hex(0x0b7d76),
                hex(0xb83280),
                hex(0x4d7c0f),
                hex(0x3b5bdb),
                hex(0xb45309),
            ],
            ansi: [
                hex(0x1a1e26),
                hex(0xc6283a),
                hex(0x157a46),
                hex(0x8a6a00),
                hex(0x0073bd),
                hex(0x9b36b7),
                hex(0x0b7d86),
                hex(0x545b69),
                hex(0x737a89),
                hex(0xe0465a),
                hex(0x1f9d5c),
                hex(0xa67c00),
                hex(0x2a8fd6),
                hex(0xb65fd0),
                hex(0x1397a1),
                hex(0x9aa1ae),
            ],
        }
    }

    /// The theme set as a global, or [`Theme::dark`] when none is set.
    pub fn of(cx: &App) -> Self {
        cx.try_global::<Self>().copied().unwrap_or_else(Self::dark)
    }

    /// The identity colour for the `index`-th non-orchestrator agent in creation order.
    pub fn agent_color(&self, index: usize) -> Hsla {
        self.agent[index % self.agent.len()]
    }

    /// The text, fill and outline colours for `tone`. Tinted tones use the tone's colour for text over a faint
    /// fill of the same colour.
    pub fn tone(&self, tone: Tone) -> ToneColors {
        let color = match tone {
            Tone::Neutral => {
                return ToneColors {
                    fg: self.text_muted,
                    bg: self.raised,
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
            bg: color.alpha(0.12),
            border: color.alpha(0.40),
        }
    }

    /// The outline of a pane or input: [`Theme::accent`] when focused, else [`Theme::border`].
    pub fn outline(&self, focused: bool) -> Hsla {
        if focused { self.accent } else { self.border }
    }

    /// This theme for a translucent, blurred window: the window chrome and the panes let the blurred desktop
    /// through (chrome more than panes, so the Roster reads as a sidebar), while raised controls, overlays and
    /// text stay opaque so everything stays legible. The terminal takes its background from `surface`, so it
    /// turns translucent with the panes.
    pub fn translucent(self) -> Self {
        Self {
            chrome: self.chrome.alpha(Self::TRANSLUCENT_CHROME_ALPHA),
            surface: self.surface.alpha(Self::TRANSLUCENT_SURFACE_ALPHA),
            border_subtle: self.border_subtle.alpha(0.7),
            ..self
        }
    }

    /// Opacity of `chrome` in a [`Theme::translucent`] theme.
    pub const TRANSLUCENT_CHROME_ALPHA: f32 = 0.62;
    /// Opacity of `surface` in a [`Theme::translucent`] theme.
    pub const TRANSLUCENT_SURFACE_ALPHA: f32 = 0.74;

    /// Whether this theme draws its chrome translucent (see [`Theme::translucent`]).
    pub fn is_translucent(&self) -> bool {
        self.chrome.a < 1.0
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
        for surface in [theme.chrome, theme.surface, theme.raised, theme.overlay] {
            assert!(contrast(theme.text, surface) >= 12.0);
            assert!(contrast(theme.text_muted, surface) >= 5.5);
            assert!(contrast(theme.text_subtle, surface) >= 3.0);
        }
    }

    #[test]
    fn text_and_status_colours_meet_contrast_on_surface() {
        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            for (label, color) in [
                ("text", theme.text),
                ("text_muted", theme.text_muted),
                ("accent", theme.accent),
                ("meta", theme.meta),
                ("success", theme.success),
                ("warning", theme.warning),
                ("danger", theme.danger),
            ] {
                let ratio = contrast(color, theme.surface);
                assert!(
                    ratio >= 4.5,
                    "{name} {label} contrast against surface is {ratio:.2} (< 4.5)"
                );
            }
            let subtle_ratio = contrast(theme.text_subtle, theme.surface);
            assert!(
                subtle_ratio >= 3.0,
                "{name} text_subtle contrast against surface is {subtle_ratio:.2} (< 3.0)"
            );
        }
    }

    #[test]
    fn agent_colours_are_distinct_from_status_and_accent() {
        for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
            let reserved = [theme.danger, theme.warning, theme.success, theme.accent];
            for (i, &agent_color) in theme.agent.iter().enumerate() {
                for &r in &reserved {
                    assert_ne!(
                        agent_color, r,
                        "{name} agent[{i}] collides with a status or accent colour"
                    );
                }
            }
            assert_eq!(theme.agent_color(6), theme.agent[0]);
        }
    }

    #[test]
    fn surfaces_get_lighter_as_they_rise() {
        let theme = Theme::dark();
        assert!(luminance(theme.chrome) < luminance(theme.surface));
        assert!(luminance(theme.surface) < luminance(theme.raised));
        assert!(luminance(theme.raised) < luminance(theme.overlay));
    }

    #[test]
    fn tinted_tones_use_their_colour_and_neutral_does_not() {
        let theme = Theme::dark();
        let danger = theme.tone(Tone::Danger);
        assert_eq!(danger.fg, theme.danger);
        assert!(danger.bg.a < danger.border.a && danger.border.a < 1.0);
        let neutral = theme.tone(Tone::Neutral);
        assert_eq!(neutral.fg, theme.text_muted);
        assert_eq!(neutral.bg, theme.raised);
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
        assert_eq!(theme.outline(true), theme.accent);
        assert_eq!(theme.outline(false), theme.border);
    }
}
