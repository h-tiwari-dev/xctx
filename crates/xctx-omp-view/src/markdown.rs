//! Assistant markdown, parsed with `pulldown-cmark` into blocks the transcript draws with GPUI text. Code blocks and
//! diffs are plain monospace (diff lines take the theme's success and danger colours); there is no syntax
//! highlighting.

use std::ops::Range;

use gpui::{
    AnyElement, FontStyle, FontWeight, HighlightStyle, SharedString, StrikethroughStyle,
    StyledText, UnderlineStyle, div, prelude::*, px,
};
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use xctx_ui::{Button, ButtonSize, ButtonVariant, IconName, Theme};

use crate::changes::CopyCallback;

/// How a run of inline text is set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InlineStyle {
    /// `**strong**`.
    pub bold: bool,
    /// `*emphasis*`.
    pub italic: bool,
    /// `` `code` ``.
    pub code: bool,
    /// Link text.
    pub link: bool,
    /// `~~struck~~`.
    pub strike: bool,
}

impl InlineStyle {
    fn is_plain(self) -> bool {
        self == InlineStyle::default()
    }
}

/// A run of text with styled ranges (byte ranges into `text`, in order and never overlapping).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inline {
    /// The text.
    pub text: String,
    /// The styled ranges; the rest is plain.
    pub spans: Vec<(Range<usize>, InlineStyle)>,
}

impl Inline {
    fn push(&mut self, text: &str, style: InlineStyle) {
        if text.is_empty() {
            return;
        }
        let start = self.text.len();
        self.text.push_str(text);
        if style.is_plain() {
            return;
        }
        match self.spans.last_mut() {
            Some((range, last)) if *last == style && range.end == start => {
                range.end = self.text.len()
            }
            _ => self.spans.push((start..self.text.len(), style)),
        }
    }

    fn is_blank(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// One block of rendered markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MdBlock {
    /// A paragraph; `quoted` when it sits in a block quote.
    Paragraph {
        /// Its text.
        text: Inline,
        /// Whether it is quoted.
        quoted: bool,
    },
    /// A heading, level 1 to 6.
    Heading {
        /// The level.
        level: u8,
        /// Its text.
        text: Inline,
    },
    /// A fenced or indented code block.
    Code {
        /// The fence's language tag, empty when none.
        lang: String,
        /// The code, without the final line break.
        text: String,
    },
    /// A list item.
    Item {
        /// How deeply it is nested, 0 for a top-level item.
        depth: usize,
        /// `•`, `3.`, or a task box.
        marker: String,
        /// Its text.
        text: Inline,
    },
    /// A horizontal rule.
    Rule,
    /// A table, header row first.
    Table {
        /// The rows of cell text.
        rows: Vec<Vec<String>>,
    },
}

#[derive(Default)]
struct TableBuild {
    rows: Vec<Vec<String>>,
    row: Vec<String>,
    cell: String,
}

#[derive(Default)]
struct Builder {
    out: Vec<MdBlock>,
    inline: Inline,
    bold: u32,
    italic: u32,
    strike: u32,
    link: u32,
    quote: u32,
    heading: Option<u8>,
    lists: Vec<Option<u64>>,
    item: Option<String>,
    code: Option<(String, String)>,
    table: Option<TableBuild>,
}

impl Builder {
    fn style(&self) -> InlineStyle {
        InlineStyle {
            bold: self.bold > 0 || self.heading.is_some(),
            italic: self.italic > 0,
            code: false,
            link: self.link > 0,
            strike: self.strike > 0,
        }
    }

    fn text(&mut self, text: &str, code: bool) {
        if let Some((_, body)) = &mut self.code {
            body.push_str(text);
        } else if let Some(table) = &mut self.table {
            table.cell.push_str(text);
        } else {
            let style = InlineStyle {
                code,
                ..self.style()
            };
            self.inline.push(text, style);
        }
    }

    /// Close the pending list item, or the paragraph, with what was collected.
    fn flush(&mut self) {
        let text = std::mem::take(&mut self.inline);
        if let Some(marker) = self.item.take() {
            if !text.is_blank() {
                self.out.push(MdBlock::Item {
                    depth: self.lists.len().saturating_sub(1),
                    marker,
                    text,
                });
            }
        } else if !text.is_blank() {
            self.out.push(MdBlock::Paragraph {
                text,
                quoted: self.quote > 0,
            });
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.text(&text, false),
            Event::Code(text) | Event::InlineMath(text) => self.text(&text, true),
            Event::DisplayMath(text) => {
                self.flush();
                self.out.push(MdBlock::Code {
                    lang: "math".to_owned(),
                    text: text.trim_end().to_owned(),
                });
            }
            Event::Html(text) | Event::InlineHtml(text) => self.text(&text, false),
            Event::FootnoteReference(label) => self.text(&format!("[{label}]"), false),
            Event::SoftBreak => self.text(" ", false),
            Event::HardBreak => self.text("\n", false),
            Event::Rule => {
                self.flush();
                self.out.push(MdBlock::Rule);
            }
            Event::TaskListMarker(done) => {
                self.item = Some(if done { "☑" } else { "☐" }.to_owned());
            }
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                if self.item.is_some() && !self.inline.text.is_empty() {
                    self.inline.push("\n", InlineStyle::default());
                }
            }
            Tag::Heading { level, .. } => {
                self.flush();
                self.heading = Some(level as u8);
            }
            Tag::BlockQuote(_) => {
                self.flush();
                self.quote += 1;
            }
            Tag::CodeBlock(kind) => {
                self.flush();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => {
                        info.split_whitespace().next().unwrap_or("").to_owned()
                    }
                    CodeBlockKind::Indented => String::new(),
                };
                self.code = Some((lang, String::new()));
            }
            Tag::List(start) => {
                self.flush();
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                let marker = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        let marker = format!("{n}.");
                        *n += 1;
                        marker
                    }
                    _ => "•".to_owned(),
                };
                self.item = Some(marker);
            }
            Tag::Emphasis => self.italic += 1,
            Tag::Strong => self.bold += 1,
            Tag::Strikethrough => self.strike += 1,
            Tag::Link { .. } => self.link += 1,
            Tag::Table(_) => {
                self.flush();
                self.table = Some(TableBuild::default());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(table) = &mut self.table {
                    table.row.clear();
                }
            }
            Tag::TableCell => {
                if let Some(table) = &mut self.table {
                    table.cell.clear();
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                if self.item.is_none() {
                    self.flush();
                }
            }
            TagEnd::Heading(_) => {
                let level = self.heading.take().unwrap_or(1);
                let text = std::mem::take(&mut self.inline);
                if !text.is_blank() {
                    self.out.push(MdBlock::Heading { level, text });
                }
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                if let Some((lang, mut text)) = self.code.take() {
                    if text.ends_with('\n') {
                        text.pop();
                    }
                    self.out.push(MdBlock::Code { lang, text });
                }
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Item => self.flush(),
            TagEnd::Emphasis => self.italic = self.italic.saturating_sub(1),
            TagEnd::Strong => self.bold = self.bold.saturating_sub(1),
            TagEnd::Strikethrough => self.strike = self.strike.saturating_sub(1),
            TagEnd::Link => self.link = self.link.saturating_sub(1),
            TagEnd::TableCell => {
                if let Some(table) = &mut self.table {
                    let cell = std::mem::take(&mut table.cell);
                    table.row.push(cell);
                }
            }
            TagEnd::TableHead | TagEnd::TableRow => {
                if let Some(table) = &mut self.table {
                    let row = std::mem::take(&mut table.row);
                    table.rows.push(row);
                }
            }
            TagEnd::Table => {
                if let Some(table) = self.table.take() {
                    self.out.push(MdBlock::Table { rows: table.rows });
                }
            }
            _ => {}
        }
    }
}

/// Parse `source` (CommonMark with tables, strikethrough and task lists) into blocks.
pub fn parse(source: &str) -> Vec<MdBlock> {
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_MATH;
    let mut builder = Builder::default();
    for event in Parser::new_ext(source, options) {
        builder.event(event);
    }
    builder.flush();
    builder.out
}

/// The colour of one diff line: additions in success, removals in danger, hunk headers in the accent.
pub fn diff_line_color(line: &str, theme: &Theme) -> gpui::Hsla {
    if line.starts_with("+++") || line.starts_with("---") {
        theme.text_muted
    } else if line.starts_with('+') {
        theme.success
    } else if line.starts_with('-') {
        theme.danger
    } else if line.starts_with("@@") {
        theme.accent
    } else {
        theme.text_muted
    }
}

fn highlight(style: InlineStyle, theme: &Theme) -> HighlightStyle {
    HighlightStyle {
        color: if style.link {
            Some(theme.accent)
        } else if style.code {
            Some(theme.meta)
        } else {
            None
        },
        font_weight: style.bold.then_some(FontWeight::BOLD),
        font_style: style.italic.then_some(FontStyle::Italic),
        background_color: style.code.then_some(theme.hover),
        underline: style.link.then_some(UnderlineStyle {
            thickness: px(1.0),
            color: Some(theme.accent),
            wavy: false,
        }),
        strikethrough: style.strike.then_some(StrikethroughStyle {
            thickness: px(1.0),
            color: None,
        }),
        fade_out: None,
    }
}

fn styled(inline: &Inline, theme: &Theme) -> StyledText {
    let theme = *theme;
    StyledText::new(inline.text.clone()).with_highlights(
        inline
            .spans
            .iter()
            .map(move |(range, style)| (range.clone(), highlight(*style, &theme))),
    )
}

/// A monospace block of `text`: plain, or coloured line by line when `diff` is set.
pub fn code_block(text: &str, diff: bool, theme: &Theme) -> AnyElement {
    code_block_inner(text, diff, theme).into_any_element()
}

fn code_block_inner(text: &str, diff: bool, theme: &Theme) -> gpui::Div {
    let body = div()
        .flex()
        .flex_col()
        .w_full()
        .p(Theme::SPACE_2)
        .rounded(Theme::RADIUS_SM)
        .bg(theme.raised)
        .border_1()
        .border_color(theme.border_subtle)
        .font_family("Menlo")
        .text_size(Theme::TEXT_SM)
        .text_color(theme.text);
    if diff {
        body.children(text.lines().map(|line| {
            div()
                .text_color(diff_line_color(line, theme))
                .child(line.to_owned())
        }))
    } else {
        body.child(text.to_owned())
    }
}

/// A monospace code block with a Tern-style header bar showing `lang` and a one-click Copy button.
pub fn code_block_with_copy(
    lang: &str,
    text: &str,
    diff: bool,
    copy_key: String,
    copied: Option<&str>,
    on_copy: CopyCallback,
    theme: &Theme,
) -> AnyElement {
    let is_copied = copied == Some(copy_key.as_str());
    let label = if lang.trim().is_empty() {
        if diff { "diff" } else { "code" }
    } else {
        lang.trim()
    };
    let payload = text.to_owned();
    let selector = format!("omp-copy-{copy_key}");
    let header = div()
        .flex()
        .items_center()
        .w_full()
        .h(px(26.0))
        .px(Theme::SPACE_2)
        .border_b_1()
        .border_color(theme.border_subtle)
        .bg(theme.surface)
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family("Menlo")
                .text_size(Theme::TEXT_XS)
                .text_color(theme.text_subtle)
                .child(label.to_owned()),
        )
        .child(
            Button::new(
                SharedString::from(format!("omp-code-copy-{copy_key}")),
                if is_copied { "Copied" } else { "Copy" },
            )
            .icon(if is_copied {
                IconName::Check
            } else {
                IconName::Copy
            })
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Compact)
            .tooltip("Copy code block")
            .selector(selector)
            .on_click(move |_, _, cx| on_copy(copy_key.clone(), payload.clone(), cx)),
        );
    let body = div()
        .flex()
        .flex_col()
        .w_full()
        .p(Theme::SPACE_2)
        .font_family("Menlo")
        .text_size(Theme::TEXT_SM)
        .text_color(theme.text);
    let body = if diff {
        body.children(text.lines().map(|line| {
            div()
                .text_color(diff_line_color(line, theme))
                .child(line.to_owned())
        }))
    } else {
        body.child(text.to_owned())
    };
    div()
        .flex()
        .flex_col()
        .w_full()
        .rounded(Theme::RADIUS_SM)
        .bg(theme.raised)
        .border_1()
        .border_color(theme.border_subtle)
        .overflow_hidden()
        .child(header)
        .child(body)
        .into_any_element()
}

/// Draw parsed markdown as a column of blocks.
pub fn render(blocks: &[MdBlock], theme: &Theme) -> AnyElement {
    render_with_copy(blocks, "md", None, None, theme)
}

/// Draw parsed markdown as a column of blocks, adding a header with a Copy button to fenced code blocks when
/// `on_copy` is provided.
pub fn render_with_copy(
    blocks: &[MdBlock],
    scope: &str,
    copied: Option<&str>,
    on_copy: Option<CopyCallback>,
    theme: &Theme,
) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap(Theme::SPACE_2)
        .w_full()
        .min_w_0()
        .text_size(Theme::TEXT_MD)
        .text_color(theme.text)
        .children(blocks.iter().enumerate().map(|(i, block)| {
            render_block(
                block,
                &format!("{scope}-code-{i}"),
                copied,
                on_copy.clone(),
                theme,
            )
        }))
        .into_any_element()
}

fn render_block(
    block: &MdBlock,
    code_key: &str,
    copied: Option<&str>,
    on_copy: Option<CopyCallback>,
    theme: &Theme,
) -> AnyElement {
    match block {
        MdBlock::Paragraph { text, quoted } => div()
            .w_full()
            .when(*quoted, |d| {
                d.pl(Theme::SPACE_2)
                    .border_l_2()
                    .border_color(theme.border_strong)
                    .text_color(theme.text_muted)
            })
            .child(styled(text, theme))
            .into_any_element(),
        MdBlock::Heading { level, text } => div()
            .w_full()
            .text_size(if *level <= 2 {
                Theme::TEXT_LG
            } else {
                Theme::TEXT_MD
            })
            .font_weight(FontWeight::SEMIBOLD)
            .child(styled(text, theme))
            .into_any_element(),
        MdBlock::Code { lang, text } => match on_copy {
            Some(cb) => code_block_with_copy(
                lang,
                text,
                lang == "diff",
                code_key.to_owned(),
                copied,
                cb,
                theme,
            ),
            None => code_block(text, lang == "diff", theme),
        },
        MdBlock::Item {
            depth,
            marker,
            text,
        } => div()
            .flex()
            .gap(Theme::SPACE_1)
            .w_full()
            .pl(px(16.0 * *depth as f32))
            .child(
                div()
                    .flex_none()
                    .min_w(px(14.0))
                    .text_color(theme.text_muted)
                    .child(marker.clone()),
            )
            .child(div().flex_1().min_w_0().child(styled(text, theme)))
            .into_any_element(),
        MdBlock::Rule => div()
            .w_full()
            .h(px(1.0))
            .bg(theme.border_subtle)
            .into_any_element(),
        MdBlock::Table { rows } => div()
            .flex()
            .flex_col()
            .w_full()
            .font_family("Menlo")
            .text_size(Theme::TEXT_SM)
            .border_1()
            .border_color(theme.border_subtle)
            .rounded(Theme::RADIUS_SM)
            .children(rows.iter().enumerate().map(|(i, row)| {
                div()
                    .flex()
                    .w_full()
                    .when(i == 0, |r| {
                        r.font_weight(FontWeight::SEMIBOLD)
                            .border_b_1()
                            .border_color(theme.border_subtle)
                    })
                    .children(row.iter().map(|cell| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .px(Theme::SPACE_1)
                            .child(cell.clone())
                    }))
            }))
            .into_any_element(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inline(text: &str) -> Inline {
        Inline {
            text: text.to_owned(),
            spans: Vec::new(),
        }
    }

    #[test]
    fn paragraphs_headings_and_inline_styles() {
        let blocks =
            parse("# Title\n\nSome **bold** and `code` and [a link](http://x).\n\n> quoted");
        assert_eq!(
            blocks[0],
            MdBlock::Heading {
                level: 1,
                text: Inline {
                    text: "Title".into(),
                    spans: vec![(
                        0..5,
                        InlineStyle {
                            bold: true,
                            ..InlineStyle::default()
                        }
                    )],
                },
            }
        );
        let MdBlock::Paragraph { text, quoted } = &blocks[1] else {
            panic!("paragraph: {blocks:?}");
        };
        assert!(!quoted);
        assert_eq!(text.text, "Some bold and code and a link.");
        let styled: Vec<(&str, InlineStyle)> = text
            .spans
            .iter()
            .map(|(r, s)| (&text.text[r.clone()], *s))
            .collect();
        assert_eq!(styled[0].0, "bold");
        assert!(styled[0].1.bold);
        assert_eq!(styled[1].0, "code");
        assert!(styled[1].1.code);
        assert_eq!(styled[2].0, "a link");
        assert!(styled[2].1.link);
        assert_eq!(
            blocks[2],
            MdBlock::Paragraph {
                text: inline("quoted"),
                quoted: true
            }
        );
    }

    #[test]
    fn lists_number_nest_and_mark_tasks() {
        let blocks = parse("1. one\n2. two\n   - inner\n- [x] done\n");
        let items: Vec<(usize, &str, &str)> = blocks
            .iter()
            .filter_map(|b| match b {
                MdBlock::Item {
                    depth,
                    marker,
                    text,
                } => Some((*depth, marker.as_str(), text.text.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(
            items,
            [
                (0, "1.", "one"),
                (0, "2.", "two"),
                (1, "•", "inner"),
                (0, "☑", "done")
            ]
        );
    }

    #[test]
    fn code_blocks_keep_their_text_and_language() {
        let blocks = parse("```diff\n-old\n+new\n```\n\n    indented\n");
        assert_eq!(
            blocks,
            [
                MdBlock::Code {
                    lang: "diff".into(),
                    text: "-old\n+new".into()
                },
                MdBlock::Code {
                    lang: String::new(),
                    text: "indented".into()
                }
            ]
        );
    }

    #[test]
    fn tables_and_rules() {
        let blocks = parse("| a | b |\n|---|---|\n| 1 | 2 |\n\n---\n");
        assert_eq!(
            blocks,
            [
                MdBlock::Table {
                    rows: vec![vec!["a".into(), "b".into()], vec!["1".into(), "2".into()]]
                },
                MdBlock::Rule
            ]
        );
    }

    #[test]
    fn diff_lines_take_status_colours() {
        let theme = Theme::dark();
        assert_eq!(diff_line_color("+x", &theme), theme.success);
        assert_eq!(diff_line_color("-x", &theme), theme.danger);
        assert_eq!(diff_line_color("@@ -1 +1 @@", &theme), theme.accent);
        assert_eq!(diff_line_color("+++ b/x", &theme), theme.text_muted);
        assert_eq!(diff_line_color(" x", &theme), theme.text_muted);
    }
}
