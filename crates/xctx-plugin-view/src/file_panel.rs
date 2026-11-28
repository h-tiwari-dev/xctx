//! Standalone file viewer panel: renders a single file inside its own split pane, delinked from the
//! side file tree, matching xctx's native `Theme::surface` / `Theme::border_subtle` design language.

use std::rc::Rc;

use gpui::{AnyElement, App, FontWeight, SharedString, StyledText, Window, div, prelude::*, px};
use xctx_ui::{
    Button, ButtonSize, ButtonVariant, IconName, ScrollableElement as _, Theme, Tone, cut_line,
    highlight_lines, language_for_path,
};

use crate::common::{note, tone_text};
use crate::files::{
    FILES_BINARY, FILES_LINE_CHARS, FILES_LINE_LIMIT, FileMark, PreviewBody, diff_color,
    markdown_block, size_label,
};

/// State for a standalone file viewer panel (`PaneKind::File`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilePanelData {
    /// Relative (or display) path of the file.
    pub path: SharedString,
    /// Size in bytes once read.
    pub size: Option<u64>,
    /// Git status mark for this file, if changed.
    pub mark: Option<FileMark>,
    /// Optional truncation note (when the file exceeded the preview byte cap).
    pub cut: Option<SharedString>,
    /// Whether this file has a git diff that can be toggled.
    pub can_diff: bool,
    /// Whether the panel is currently showing the unified diff.
    pub diff: bool,
    /// Whether this file is Markdown (so Rendered / Source toggle is shown).
    pub is_markdown: bool,
    /// Whether Markdown is currently shown as raw source instead of rendered blocks.
    pub raw_markdown: bool,
    /// Optional 1-based line number to highlight (e.g., from Cmd+Click on `src/main.rs:42`).
    pub target_line: Option<usize>,
    /// Content body (lines, rendered markdown, PDF document surface, binary, loading, or error).
    pub body: PreviewBody,
}

/// Actions emitted by toolbar buttons inside [`FilePanelView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilePanelAction {
    /// Toggle unified git diff mode.
    ToggleDiff(bool),
    /// Toggle raw source vs rendered Markdown preview.
    ToggleRawMarkdown(bool),
    /// Reveal and select this file in the side Files explorer.
    RevealInSidebar,
    /// Copy the file path to the clipboard.
    CopyPath,
    /// Copy the file's text content (or diff) to the clipboard.
    CopyContent,
    /// Reload the file from disk.
    Reload,
}

type ActionHandler = Rc<dyn Fn(FilePanelAction, &mut Window, &mut App)>;

/// Standalone file viewer panel drawn inside a `PaneKind::File` pane.
#[derive(IntoElement)]
pub struct FilePanelView {
    data: FilePanelData,
    document: Option<AnyElement>,
    on_action: Option<ActionHandler>,
}

impl FilePanelView {
    /// Create a file panel view for `data`.
    pub fn new(data: FilePanelData) -> Self {
        Self {
            data,
            document: None,
            on_action: None,
        }
    }

    /// Supply the native document surface (e.g., worktree-restricted PDF WebView) when `body` is
    /// [`PreviewBody::Document`].
    pub fn document(mut self, document: Option<AnyElement>) -> Self {
        self.document = document;
        self
    }

    /// Handle toolbar button clicks.
    pub fn on_action(
        mut self,
        handler: impl Fn(FilePanelAction, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_action = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for FilePanelView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let data = self.data;
        let emit: ActionHandler = self.on_action.unwrap_or_else(|| Rc::new(|_, _, _| {}));

        let (dir_prefix, file_name) = match data.path.rsplit_once('/') {
            Some((dir, name)) => (Some(format!("{dir}/")), name.to_owned()),
            None => (None, data.path.as_ref().to_owned()),
        };

        let line_count = match &data.body {
            PreviewBody::Lines(lines) => Some(lines.len()),
            _ => None,
        };

        let mut meta_parts: Vec<String> = Vec::new();
        if let Some(n) = line_count {
            meta_parts.push(if n == 1 {
                "1 line".to_owned()
            } else {
                format!("{n} lines")
            });
        }
        if let Some(size) = data.size {
            meta_parts.push(size_label(size).to_string());
        }
        let meta = (!meta_parts.is_empty()).then(|| meta_parts.join(" · "));

        let diff_on = data.diff;
        let raw_md = data.raw_markdown;
        let can_copy_content =
            matches!(data.body, PreviewBody::Lines(_) | PreviewBody::Markdown(_));

        let toolbar = div()
            .debug_selector(|| "file-panel-toolbar".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .gap(Theme::SPACE_2)
            .h(px(34.0))
            .px(Theme::SPACE_3)
            .bg(theme.surface)
            .border_b_1()
            .border_color(theme.border_subtle)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .child(
                        div()
                            .debug_selector(|| "file-panel-path".to_owned())
                            .flex()
                            .min_w_0()
                            .truncate()
                            .font_family("Menlo")
                            .text_size(Theme::TEXT_XS)
                            .children(
                                dir_prefix
                                    .map(|prefix| div().text_color(theme.text_muted).child(prefix)),
                            )
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text)
                                    .child(file_name),
                            ),
                    )
                    .when_some(data.mark, |el, mark| {
                        el.child(
                            div()
                                .flex_none()
                                .px(px(5.0))
                                .py(px(1.0))
                                .rounded(Theme::RADIUS_SM)
                                .bg(theme.tone(mark.tone()).bg)
                                .text_size(Theme::TEXT_XS)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(tone_text(&theme, mark.tone()))
                                .child(mark.letter()),
                        )
                    })
                    .when_some(meta, |el, meta| {
                        el.child(
                            div()
                                .flex_none()
                                .text_size(Theme::TEXT_XS)
                                .text_color(theme.text_subtle)
                                .child(meta),
                        )
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .when(data.is_markdown && !diff_on, |el| {
                        let emit = emit.clone();
                        el.child(
                            Button::new(
                                "file-panel-md-toggle",
                                if raw_md { "Rendered" } else { "Source" },
                            )
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .selected(raw_md)
                            .selector("file-panel-md-toggle")
                            .on_click(move |_, window, cx| {
                                emit(FilePanelAction::ToggleRawMarkdown(!raw_md), window, cx)
                            }),
                        )
                    })
                    .when(data.can_diff, |el| {
                        let emit = emit.clone();
                        el.child(
                            Button::new("file-panel-diff", "Diff")
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Compact)
                                .selected(diff_on)
                                .selector("file-panel-diff")
                                .on_click(move |_, window, cx| {
                                    emit(FilePanelAction::ToggleDiff(!diff_on), window, cx)
                                }),
                        )
                    })
                    .child({
                        let emit = emit.clone();
                        Button::new("file-panel-reveal", "")
                            .icon(IconName::Folder)
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .selector("file-panel-reveal")
                            .tooltip("Reveal in Files sidebar")
                            .on_click(move |_, window, cx| {
                                emit(FilePanelAction::RevealInSidebar, window, cx)
                            })
                    })
                    .child({
                        let emit = emit.clone();
                        Button::new("file-panel-copy-path", "Path")
                            .icon(IconName::Copy)
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .selector("file-panel-copy-path")
                            .tooltip("Copy file path")
                            .on_click(move |_, window, cx| {
                                emit(FilePanelAction::CopyPath, window, cx)
                            })
                    })
                    .when(can_copy_content, |el| {
                        let emit = emit.clone();
                        el.child(
                            Button::new("file-panel-copy", "")
                                .icon(IconName::Copy)
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Compact)
                                .selector("file-panel-copy")
                                .tooltip("Copy file contents")
                                .on_click(move |_, window, cx| {
                                    emit(FilePanelAction::CopyContent, window, cx)
                                }),
                        )
                    })
                    .child({
                        let emit = emit.clone();
                        Button::new("file-panel-reload", "")
                            .icon(IconName::Refresh)
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .selector("file-panel-reload")
                            .tooltip("Reload file")
                            .on_click(move |_, window, cx| {
                                emit(FilePanelAction::Reload, window, cx)
                            })
                    }),
            );

        let cut = data.cut.clone().map(|text| {
            div()
                .flex_none()
                .px(Theme::SPACE_3)
                .py(Theme::SPACE_1)
                .border_b_1()
                .border_color(theme.border_subtle)
                .bg(theme.raised)
                .child(note("file-panel-cut", text, &theme))
        });

        let body_element: AnyElement = match data.body {
            PreviewBody::Document => div()
                .debug_selector(|| "file-panel-document".to_owned())
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .bg(theme.surface)
                .child(self.document.unwrap_or_else(|| {
                    centered_note(
                        "file-panel-document-unavailable",
                        "Document preview is unavailable.",
                        &theme,
                    )
                }))
                .into_any_element(),
            PreviewBody::Empty => centered_note("file-panel-empty", "No file selected.", &theme),
            PreviewBody::Loading => centered_note("file-panel-loading", "Loading file…", &theme),
            PreviewBody::Binary => centered_note(
                "file-panel-binary",
                &format!("Binary file ({FILES_BINARY}) — cannot display as text."),
                &theme,
            ),
            PreviewBody::Message(why) => centered_note("file-panel-message", &why, &theme),
            PreviewBody::Markdown(blocks) => div()
                .id(SharedString::from(format!("file-panel-md-{}", data.path)))
                .debug_selector(|| "file-panel-markdown".to_owned())
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .py(Theme::SPACE_3)
                .bg(theme.surface)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .w_full()
                        .max_w(px(780.0))
                        .children(blocks.iter().map(|block| markdown_block(block, &theme))),
                )
                .overflow_y_scrollbar()
                .into_any_element(),
            PreviewBody::Lines(lines) => {
                let total = lines.len().min(FILES_LINE_LIMIT);
                let gutter_digits = total.max(1).to_string().len().max(3);
                let gutter_w = px((gutter_digits as f32) * 8.0 + 18.0);
                let target_line = data.target_line;
                let line_highlights = (!diff_on)
                    .then(|| language_for_path(&data.path))
                    .flatten()
                    .map(|lang| highlight_lines(lang, &lines[..total], &theme));
                let mut rows: Vec<AnyElement> = lines
                    .iter()
                    .take(FILES_LINE_LIMIT)
                    .enumerate()
                    .map(|(i, line)| {
                        let line_num = i + 1;
                        let is_target = !diff_on && target_line == Some(line_num);
                        let color = if diff_on {
                            diff_color(line, &theme)
                        } else {
                            theme.text
                        };
                        let row_bg = if is_target {
                            Some(theme.selection)
                        } else if diff_on && line.starts_with('+') && !line.starts_with("+++") {
                            Some(theme.tone(Tone::Success).bg)
                        } else if diff_on && line.starts_with('-') && !line.starts_with("---") {
                            Some(theme.tone(Tone::Danger).bg)
                        } else {
                            None
                        };
                        let mut row = div()
                            .flex()
                            .flex_none()
                            .items_start()
                            .min_h(px(19.0))
                            .line_height(px(19.0))
                            .pr(Theme::SPACE_4)
                            .whitespace_nowrap();
                        if let Some(bg) = row_bg {
                            row = row.bg(bg);
                        }
                        if is_target {
                            row = row.border_l_2().border_color(theme.accent);
                        }
                        if !diff_on {
                            row = row.child(
                                div()
                                    .flex_none()
                                    .w(gutter_w)
                                    .pr(Theme::SPACE_3)
                                    .mr(Theme::SPACE_3)
                                    .border_r_1()
                                    .border_color(theme.border_subtle)
                                    .text_right()
                                    .text_color(if is_target {
                                        theme.accent
                                    } else {
                                        theme.text_subtle
                                    })
                                    .child(format!("{line_num}")),
                            );
                        } else {
                            row = row.pl(Theme::SPACE_3);
                        }
                        let shown = cut_line(line, FILES_LINE_CHARS);
                        let line_el: AnyElement = if let Some(spans) =
                            line_highlights.as_ref().and_then(|all| all.get(i))
                            && !spans.is_empty()
                        {
                            let max_len = shown.len();
                            let clipped: Vec<_> = spans
                                .iter()
                                .filter_map(|(r, style)| {
                                    if r.start >= max_len {
                                        return None;
                                    }
                                    let mut end = r.end.min(max_len);
                                    while end > r.start && !shown.is_char_boundary(end) {
                                        end -= 1;
                                    }
                                    (r.start < end && shown.is_char_boundary(r.start))
                                        .then_some((r.start..end, *style))
                                })
                                .collect();
                            if clipped.is_empty() {
                                div().text_color(color).child(shown).into_any_element()
                            } else {
                                div()
                                    .text_color(color)
                                    .child(StyledText::new(shown).with_highlights(clipped))
                                    .into_any_element()
                            }
                        } else {
                            div().text_color(color).child(shown).into_any_element()
                        };
                        row.child(line_el).into_any_element()
                    })
                    .collect();
                if lines.len() > FILES_LINE_LIMIT {
                    rows.push(
                        div()
                            .px(Theme::SPACE_4)
                            .py(Theme::SPACE_2)
                            .child(note(
                                "file-panel-lines-more",
                                format!("… {} more lines", lines.len() - FILES_LINE_LIMIT),
                                &theme,
                            ))
                            .into_any_element(),
                    );
                }
                div()
                    .id(SharedString::from(format!("file-panel-code-{}", data.path)))
                    .debug_selector(|| "file-panel-code".to_owned())
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .py(Theme::SPACE_2)
                    .bg(theme.surface)
                    .font_family("Menlo")
                    .text_size(px(12.5))
                    .children(rows)
                    .overflow_y_scrollbar()
                    .into_any_element()
            }
        };

        div()
            .debug_selector(|| "file-panel".to_owned())
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .bg(theme.surface)
            .child(toolbar)
            .children(cut)
            .child(body_element)
    }
}

fn centered_note(selector: &'static str, text: &str, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .p(Theme::SPACE_4)
        .bg(theme.surface)
        .child(note(selector, text.to_owned(), theme))
        .into_any_element()
}
