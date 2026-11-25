//! Caps and small drawing helpers shared by the plugin panes.

use gpui::{Div, Hsla, SharedString, div, prelude::*, px};
use xctx_ui::{Theme, Tone, cut_line};

/// How much of a long text a pane draws, for [`excerpt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Caps {
    /// The most lines drawn.
    pub lines: usize,
    /// The most characters drawn of one line; longer lines end in "…".
    pub line_chars: usize,
    /// The most characters drawn in all; the lines past it are counted as hidden.
    pub total_chars: usize,
}

/// The part of a text a pane draws, from [`excerpt`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Excerpt {
    /// The lines drawn, each cut to [`Caps::line_chars`].
    pub lines: Vec<SharedString>,
    /// How many lines were left out.
    pub hidden: usize,
}

/// The first lines of `lines` within `caps`: at most [`Caps::lines`] lines, each cut to [`Caps::line_chars`],
/// and none once [`Caps::total_chars`] characters are drawn. The lines left out are counted, not collected.
pub fn excerpt<'a>(lines: impl IntoIterator<Item = &'a str>, caps: Caps) -> Excerpt {
    let mut drawn = Vec::new();
    let mut chars = 0;
    let mut rest = lines.into_iter();
    let mut hidden = 0;
    while let Some(line) = rest.next() {
        if drawn.len() >= caps.lines || chars >= caps.total_chars {
            hidden = 1 + rest.count();
            break;
        }
        let cut = cut_line(line, caps.line_chars);
        chars += cut.chars().count();
        drawn.push(cut);
    }
    Excerpt {
        lines: drawn,
        hidden,
    }
}

/// The text colour of `tone`: muted text for neutral, else the tone's colour.
pub fn tone_text(theme: &Theme, tone: Tone) -> Hsla {
    match tone {
        Tone::Neutral => theme.text_muted,
        tone => theme.tone(tone).fg,
    }
}

/// A small subtle section heading.
pub fn section(title: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .flex_none()
        .text_size(Theme::TEXT_XS)
        .text_color(theme.text_subtle)
        .child(title.into())
}

/// A subtle note such as "… 12 more lines", named `selector` for tests.
pub fn note(selector: &'static str, text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.to_owned())
        .flex_none()
        .px(Theme::SPACE_1)
        .text_size(Theme::TEXT_XS)
        .text_color(theme.text_subtle)
        .child(text.into())
}

/// An error line in the danger colour, named `selector` for tests.
pub fn error_line(selector: &'static str, text: SharedString, theme: &Theme) -> Div {
    div()
        .debug_selector(move || selector.to_owned())
        .flex_none()
        .text_size(Theme::TEXT_SM)
        .text_color(theme.danger)
        .child(text)
}

/// The outer frame of a pane: a column filling its parent on the surface colour.
pub fn pane_frame(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .size_full()
        .min_h_0()
        .gap(Theme::SPACE_2)
        .p(Theme::SPACE_2)
        .bg(theme.surface)
        .text_color(theme.text)
        .text_size(Theme::TEXT_SM)
}

/// A text standing in for an input the app did not pass: monospace, on a raised field.
pub fn field_text(text: SharedString, placeholder: &'static str, theme: &Theme) -> Div {
    let (text, color) = if text.is_empty() {
        (SharedString::from(placeholder), theme.text_subtle)
    } else {
        (text, theme.text)
    };
    div()
        .flex_1()
        .min_w_0()
        .h(px(24.0))
        .flex()
        .items_center()
        .px(Theme::SPACE_2)
        .rounded(Theme::RADIUS_SM)
        .border_1()
        .border_color(theme.border)
        .bg(theme.elevated)
        .font_family("Menlo")
        .text_color(color)
        .truncate()
        .child(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CAPS: Caps = Caps {
        lines: 3,
        line_chars: 4,
        total_chars: 100,
    };

    #[test]
    fn excerpts_cap_lines_and_count_the_rest() {
        let e = excerpt("a\nb\nc\nd\ne".lines(), CAPS);
        assert_eq!(e.lines.len(), 3);
        assert_eq!(e.hidden, 2);
        let e = excerpt("a\nb".lines(), CAPS);
        assert_eq!(e.hidden, 0);
        assert_eq!(excerpt(std::iter::empty(), CAPS), Excerpt::default());
    }

    #[test]
    fn excerpts_cut_long_lines_and_stop_at_the_character_cap() {
        let e = excerpt(["abcdefgh"], CAPS);
        assert_eq!(e.lines[0].as_ref(), "abc…");
        let caps = Caps {
            lines: 100,
            line_chars: 10,
            total_chars: 25,
        };
        let long = "x".repeat(50);
        let e = excerpt(std::iter::repeat_n(long.as_str(), 10), caps);
        assert_eq!(e.lines.len(), 3);
        assert_eq!(e.hidden, 7);
    }
}
