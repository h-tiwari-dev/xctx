//! The files pane: a read-only explorer of a repo's worktree. The tree of folders and files on the left (each
//! file with its git change as a coloured letter, each folder marked when something under it changed), the
//! selected file's text or diff on the right, and Refresh and "Changed only" in the header row.
//!
//! The pane reads nothing itself: the app lists folders, reads files and runs git, and fills a [`FilesView`].

use std::rc::Rc;
use std::sync::Arc;

use gpui::{AnyElement, App, FontWeight, SharedString, Window, div, prelude::*, px};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use xctx_ui::{Badge, Button, ButtonSize, ButtonVariant, Pill, Theme, Tone, cut_line};

use crate::common::{error_line, note, pane_frame, tone_text};

/// The most tree rows drawn; a note counts the rest.
pub const FILES_ROW_LIMIT: usize = 3000;

/// The most preview lines drawn; a note counts the rest.
pub const FILES_LINE_LIMIT: usize = 2000;

/// The most characters drawn of one preview line; longer lines end in "…".
pub const FILES_LINE_CHARS: usize = 400;

/// The width of the tree.
pub const FILES_TREE_WIDTH: gpui::Pixels = px(260.0);

/// What the preview says before a file is chosen.
pub const FILES_NO_SELECTION: &str = "Choose a file to see it here.";

/// What the preview says of a binary file.
pub const FILES_BINARY: &str = "binary file";

/// What the tree says when "Changed only" hides everything.
pub const FILES_NO_CHANGES: &str = "Nothing changed in the folders that are open.";

/// A file's change in the worktree, from `git status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileMark {
    /// Changed (`M`).
    Modified,
    /// New in the index (`A`).
    Added,
    /// Deleted (`D`).
    Deleted,
    /// Renamed (`R`).
    Renamed,
    /// Not tracked by git (`??`).
    Untracked,
    /// Both sides changed it in a merge (`U`).
    Conflicted,
}

impl FileMark {
    /// The letter drawn beside the file, so the change reads without colour.
    pub fn letter(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Untracked => "?",
            Self::Conflicted => "U",
        }
    }

    /// The letter's tone: warning for changed, success for new, danger for deleted and conflicted.
    pub fn tone(self) -> Tone {
        match self {
            Self::Modified | Self::Renamed => Tone::Warning,
            Self::Added | Self::Untracked => Tone::Success,
            Self::Deleted | Self::Conflicted => Tone::Danger,
        }
    }
}

/// What one tree row is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileRowKind {
    /// A folder.
    Folder {
        /// Whether it is open (its rows follow).
        open: bool,
        /// Whether something under it changed.
        changed: bool,
        /// A dependency or build-output folder (`node_modules`, `target`, ...), drawn muted.
        heavy: bool,
    },
    /// A file.
    File {
        /// Its change, if it has one.
        mark: Option<FileMark>,
    },
    /// "… N more" entries of the folder above that are not listed.
    More(usize),
    /// The folder above is being read.
    Loading,
    /// The folder above could not be read, or was refused: why. Already masked by the caller.
    Refused(SharedString),
}

/// One row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    /// The entry's path relative to the worktree, reported by the click handlers.
    pub path: SharedString,
    /// The name drawn.
    pub name: SharedString,
    /// How deep it is: 0 for the worktree's own entries.
    pub depth: usize,
    /// What it is.
    pub kind: FileRowKind,
}

/// What the preview area holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PreviewBody {
    /// No file is chosen.
    #[default]
    Empty,
    /// The file (or its diff) is being read.
    Loading,
    /// Text lines (a file's, or a diff's when [`FilesPreview::diff`]). Already masked by the caller.
    Lines(Vec<SharedString>),
    /// Markdown parsed once when the file preview changes and drawn as native themed blocks.
    Markdown(Arc<[MarkdownBlock]>),
    /// The file is binary.
    Binary,
    /// A native document surface supplied by the app (currently a PDF in a local-only WebKit view).
    Document,
    /// The file cannot be shown: why. Already masked by the caller.
    Message(SharedString),
}

/// One native block of a rendered Markdown document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MarkdownBlock {
    /// A heading, level 1 through 6.
    Heading(u8, SharedString),
    /// A paragraph.
    Paragraph(SharedString),
    /// A list item, with its zero-based nesting depth.
    ListItem(usize, SharedString),
    /// A quote.
    Quote(SharedString),
    /// A fenced or indented code block, with its optional language.
    Code(Option<SharedString>, SharedString),
    /// A horizontal rule.
    Rule,
}

#[derive(Clone)]
enum MarkdownCurrent {
    Heading(u8),
    Paragraph,
    ListItem(usize),
    Quote,
    Code(Option<SharedString>),
}

/// Parse `text` into the native blocks the Files pane draws. Inline emphasis and links retain their readable
/// text; document structure, lists, quotes and code blocks retain their own layout.
pub fn markdown_blocks(text: &str) -> Arc<[MarkdownBlock]> {
    let options = Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TABLES
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_FOOTNOTES;
    let mut blocks = Vec::new();
    let mut current: Option<MarkdownCurrent> = None;
    let mut content = String::new();
    let mut list_depth = 0usize;
    let mut quote_depth = 0usize;

    let flush = |current: &mut Option<MarkdownCurrent>,
                 content: &mut String,
                 blocks: &mut Vec<MarkdownBlock>| {
        let Some(kind) = current.take() else {
            content.clear();
            return;
        };
        let text = content.trim().to_owned();
        content.clear();
        if text.is_empty() {
            return;
        }
        let text: SharedString = text.into();
        blocks.push(match kind {
            MarkdownCurrent::Heading(level) => MarkdownBlock::Heading(level, text),
            MarkdownCurrent::Paragraph => MarkdownBlock::Paragraph(text),
            MarkdownCurrent::ListItem(depth) => MarkdownBlock::ListItem(depth, text),
            MarkdownCurrent::Quote => MarkdownBlock::Quote(text),
            MarkdownCurrent::Code(language) => MarkdownBlock::Code(language, text),
        });
    };

    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                flush(&mut current, &mut content, &mut blocks);
                current = Some(MarkdownCurrent::Heading(heading_level(level)));
            }
            Event::End(TagEnd::Heading(_)) => flush(&mut current, &mut content, &mut blocks),
            Event::Start(Tag::List(_)) => list_depth += 1,
            Event::End(TagEnd::List(_)) => list_depth = list_depth.saturating_sub(1),
            Event::Start(Tag::Item) => {
                flush(&mut current, &mut content, &mut blocks);
                current = Some(MarkdownCurrent::ListItem(list_depth.saturating_sub(1)));
            }
            Event::End(TagEnd::Item) => flush(&mut current, &mut content, &mut blocks),
            Event::Start(Tag::BlockQuote(_)) => {
                quote_depth += 1;
                flush(&mut current, &mut content, &mut blocks);
                current = Some(MarkdownCurrent::Quote);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                flush(&mut current, &mut content, &mut blocks);
                quote_depth = quote_depth.saturating_sub(1);
            }
            Event::Start(Tag::CodeBlock(kind)) => {
                flush(&mut current, &mut content, &mut blocks);
                let language = match kind {
                    CodeBlockKind::Indented => None,
                    CodeBlockKind::Fenced(language) if language.is_empty() => None,
                    CodeBlockKind::Fenced(language) => Some(language.to_string().into()),
                };
                current = Some(MarkdownCurrent::Code(language));
            }
            Event::End(TagEnd::CodeBlock) => flush(&mut current, &mut content, &mut blocks),
            Event::Start(Tag::Paragraph) => {
                if current.is_none() {
                    current = Some(if quote_depth > 0 {
                        MarkdownCurrent::Quote
                    } else {
                        MarkdownCurrent::Paragraph
                    });
                }
            }
            Event::End(TagEnd::Paragraph) => {
                if !matches!(current, Some(MarkdownCurrent::ListItem(_))) {
                    flush(&mut current, &mut content, &mut blocks);
                }
            }
            Event::Rule => {
                flush(&mut current, &mut content, &mut blocks);
                blocks.push(MarkdownBlock::Rule);
            }
            Event::Text(text) | Event::Code(text) => content.push_str(&text),
            Event::SoftBreak => content.push(' '),
            Event::HardBreak => content.push('\n'),
            Event::TaskListMarker(done) => {
                content.push_str(if done { "☑ " } else { "☐ " });
            }
            _ => {}
        }
    }
    flush(&mut current, &mut content, &mut blocks);
    blocks.into()
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// The selected file as the preview shows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilesPreview {
    /// Its path relative to the worktree; empty when none is chosen.
    pub path: SharedString,
    /// Its size in bytes, when known.
    pub size: Option<u64>,
    /// What is drawn.
    pub body: PreviewBody,
    /// Whether the body is the file's diff rather than its text.
    pub diff: bool,
    /// Whether the file has a change, so Diff can be turned on.
    pub can_diff: bool,
    /// Why the text is cut, such as "Showing the first 2000 lines", when it is.
    pub cut: Option<SharedString>,
}

/// What the files pane shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FilesView {
    /// The worktree drawn, as a path. Already masked by the caller.
    pub root: SharedString,
    /// The tree's rows in order.
    pub rows: Vec<FileRow>,
    /// Whether only changed paths are listed.
    pub changed_only: bool,
    /// Whether a folder, the git status or the file is being read.
    pub busy: bool,
    /// Why git status failed (not a git repository, say). Already masked by the caller.
    pub status_error: Option<SharedString>,
    /// The selected file; its row is highlighted.
    pub preview: FilesPreview,
}

/// A size such as `512 B`, `4.2 KiB` or `1.3 MiB`.
pub fn size_label(bytes: u64) -> SharedString {
    const KIB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KIB {
        format!("{bytes} B").into()
    } else if b < KIB * KIB {
        format!("{:.1} KiB", b / KIB).into()
    } else if b < KIB * KIB * KIB {
        format!("{:.1} MiB", b / (KIB * KIB)).into()
    } else {
        format!("{:.1} GiB", b / (KIB * KIB * KIB)).into()
    }
}

type Handler = Rc<dyn Fn(&mut Window, &mut App)>;
type PathHandler = Rc<dyn Fn(SharedString, &mut Window, &mut App)>;
type BoolHandler = Rc<dyn Fn(bool, &mut Window, &mut App)>;

/// The files pane, drawn from a [`FilesView`].
///
/// Selectors for tests: `files-refresh`, `files-changed-only`, `files-diff`, `files-tree`, `files-row-<i>` (of
/// the rows drawn), `files-rows-more`, `files-no-changes`, `files-status-error`, `files-preview`,
/// `files-preview-path`, `files-preview-empty`, `files-loading`, `files-binary`, `files-message` and `files-cut`.
#[derive(IntoElement)]
pub struct FilesPaneView {
    view: FilesView,
    document: Option<AnyElement>,
    on_folder: Option<PathHandler>,
    on_file: Option<PathHandler>,
    on_refresh: Option<Handler>,
    on_changed_only: Option<BoolHandler>,
    on_diff: Option<BoolHandler>,
}

impl FilesPaneView {
    /// A pane for `view` with no handlers.
    pub fn new(view: FilesView) -> Self {
        Self {
            view,
            document: None,
            on_folder: None,
            on_file: None,
            on_refresh: None,
            on_changed_only: None,
            on_diff: None,
        }
    }

    /// Put a native document surface in the preview when its body is [`PreviewBody::Document`].
    pub fn document(mut self, document: impl IntoElement) -> Self {
        self.document = Some(document.into_any_element());
        self
    }

    /// Call `handler` with a folder's path when its row is clicked (to open or close it).
    pub fn on_folder(
        mut self,
        handler: impl Fn(SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_folder = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with a file's path when its row is clicked.
    pub fn on_file(
        mut self,
        handler: impl Fn(SharedString, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_file = Some(Rc::new(handler));
        self
    }

    /// Call `handler` when Refresh is clicked.
    pub fn on_refresh(mut self, handler: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_refresh = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the new state when "Changed only" is clicked.
    pub fn on_changed_only(
        mut self,
        handler: impl Fn(bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_changed_only = Some(Rc::new(handler));
        self
    }

    /// Call `handler` with the new state when Diff is clicked (only drawn for a changed file).
    pub fn on_diff(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_diff = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for FilesPaneView {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = Theme::of(cx);
        let view = self.view;

        let changed_only = view.changed_only;
        let mut changed = Pill::new("files-changed-only", "Changed only")
            .selected(changed_only)
            .selector("files-changed-only");
        if let Some(handler) = self.on_changed_only.clone() {
            changed = changed.on_click(move |_, window, cx| handler(!changed_only, window, cx));
        }
        let mut refresh = Button::new("files-refresh", "Refresh")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Compact)
            .selector("files-refresh");
        if let Some(handler) = self.on_refresh.clone() {
            refresh = refresh.on_click(move |_, window, cx| handler(window, cx));
        }
        let header = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_2)
            .child(Badge::new("FILES"))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family("Menlo")
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_muted)
                    .child(view.root.clone()),
            )
            .children(view.busy.then(|| {
                div()
                    .flex_none()
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_subtle)
                    .child("Reading…")
            }))
            .child(changed)
            .child(refresh);

        let status_error = view
            .status_error
            .clone()
            .map(|text| error_line("files-status-error", text, &theme));

        // The tree.
        let shown = view.rows.len().min(FILES_ROW_LIMIT);
        let rows: Vec<AnyElement> = view
            .rows
            .iter()
            .take(FILES_ROW_LIMIT)
            .enumerate()
            .map(|(i, row)| {
                tree_row(
                    i,
                    row,
                    !view.preview.path.is_empty() && row.path == view.preview.path,
                    &self.on_folder,
                    &self.on_file,
                    &theme,
                )
            })
            .collect();
        let rows_more = (view.rows.len() > shown).then(|| {
            note(
                "files-rows-more",
                format!(
                    "… {} more rows; close some folders",
                    view.rows.len() - shown
                ),
                &theme,
            )
        });
        let no_changes = (changed_only && view.rows.is_empty())
            .then(|| note("files-no-changes", FILES_NO_CHANGES, &theme));
        let tree = div()
            .id("files-tree")
            .debug_selector(|| "files-tree".to_owned())
            .flex()
            .flex_col()
            .flex_none()
            .w(FILES_TREE_WIDTH)
            .h_full()
            .min_h_0()
            .overflow_y_scroll()
            .py(Theme::SPACE_1)
            .rounded(Theme::RADIUS_SM)
            .border_1()
            .border_color(theme.border)
            .bg(theme.chrome)
            .text_size(Theme::TEXT_XS)
            .children(rows)
            .children(rows_more)
            .children(no_changes);

        let preview = preview(view.preview, self.document, self.on_diff, &theme);

        pane_frame(&theme)
            .child(header)
            .children(status_error)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap(Theme::SPACE_2)
                    .child(tree)
                    .child(preview),
            )
    }
}

fn tree_row(
    i: usize,
    row: &FileRow,
    selected: bool,
    on_folder: &Option<PathHandler>,
    on_file: &Option<PathHandler>,
    theme: &Theme,
) -> AnyElement {
    let selector = format!("files-row-{i}");
    let indent = px(8.0 + 12.0 * row.depth as f32);
    let base = div()
        .id(SharedString::from(selector.clone()))
        .debug_selector(move || selector)
        .flex()
        .flex_none()
        .items_center()
        .gap(Theme::SPACE_1)
        .h(px(20.0))
        .pl(indent)
        .pr(Theme::SPACE_2)
        .whitespace_nowrap();
    let name = |color| {
        div()
            .flex_1()
            .min_w_0()
            .truncate()
            .text_color(color)
            .child(row.name.clone())
    };
    let gutter =
        |text: &'static str, color| div().flex_none().w(px(12.0)).text_color(color).child(text);
    let hover = theme.raised;
    match &row.kind {
        FileRowKind::Folder {
            open,
            changed,
            heavy,
        } => {
            let color = if *heavy {
                theme.text_subtle
            } else {
                theme.text
            };
            let mut element = base
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .child(gutter(if *open { "▾" } else { "▸" }, theme.text_subtle))
                .child(name(color).font_weight(FontWeight::MEDIUM))
                .children(changed.then(|| {
                    div()
                        .flex_none()
                        .text_color(tone_text(theme, Tone::Warning))
                        .child("•")
                }));
            if let Some(handler) = on_folder.clone() {
                let path = row.path.clone();
                element = element.on_click(move |_, window, cx| handler(path.clone(), window, cx));
            }
            element.into_any_element()
        }
        FileRowKind::File { mark } => {
            let color = match mark {
                Some(FileMark::Deleted) => theme.text_subtle,
                Some(mark) => tone_text(theme, mark.tone()),
                None => theme.text,
            };
            let mut element = base
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .child(gutter(" ", theme.text_subtle))
                .child(name(color))
                .children(mark.map(|mark| {
                    div()
                        .flex_none()
                        .font_family("Menlo")
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(tone_text(theme, mark.tone()))
                        .child(mark.letter())
                }));
            if selected {
                element = element.bg(theme.selection);
            }
            if let Some(handler) = on_file.clone() {
                let path = row.path.clone();
                element = element.on_click(move |_, window, cx| handler(path.clone(), window, cx));
            }
            element.into_any_element()
        }
        FileRowKind::More(n) => base
            .text_color(theme.text_subtle)
            .child(format!("… {n} more"))
            .into_any_element(),
        FileRowKind::Loading => base
            .text_color(theme.text_subtle)
            .child("Reading…")
            .into_any_element(),
        FileRowKind::Refused(why) => base
            .text_color(theme.danger)
            .child(div().truncate().child(why.clone()))
            .into_any_element(),
    }
}

fn preview(
    preview: FilesPreview,
    document: Option<AnyElement>,
    on_diff: Option<BoolHandler>,
    theme: &Theme,
) -> AnyElement {
    let diff_on = preview.diff;
    let diff = preview.can_diff.then(|| {
        let mut button = Button::new("files-diff", "Diff")
            .variant(ButtonVariant::Ghost)
            .size(ButtonSize::Compact)
            .selected(diff_on)
            .selector("files-diff");
        if let Some(handler) = on_diff {
            button = button.on_click(move |_, window, cx| handler(!diff_on, window, cx));
        }
        button
    });
    let heading = (!preview.path.is_empty()).then(|| {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_2)
            .child(
                div()
                    .debug_selector(|| "files-preview-path".to_owned())
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family("Menlo")
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text)
                    .child(preview.path.clone()),
            )
            .children(preview.size.map(|size| {
                div()
                    .flex_none()
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_subtle)
                    .child(size_label(size))
            }))
            .children(diff)
    });
    let cut = preview
        .cut
        .clone()
        .map(|text| note("files-cut", text, theme));

    let body: Vec<AnyElement> = match preview.body {
        PreviewBody::Empty => {
            vec![note("files-preview-empty", FILES_NO_SELECTION, theme).into_any_element()]
        }
        PreviewBody::Loading => vec![note("files-loading", "Reading…", theme).into_any_element()],
        PreviewBody::Binary => vec![note("files-binary", FILES_BINARY, theme).into_any_element()],
        PreviewBody::Document => vec![document.unwrap_or_else(|| {
            note(
                "files-document-unavailable",
                "The document preview is unavailable.",
                theme,
            )
            .into_any_element()
        })],
        PreviewBody::Message(why) => vec![
            div()
                .debug_selector(|| "files-message".to_owned())
                .px(Theme::SPACE_2)
                .text_size(Theme::TEXT_SM)
                .text_color(theme.text_muted)
                .child(why)
                .into_any_element(),
        ],
        PreviewBody::Markdown(blocks) => blocks
            .iter()
            .map(|block| markdown_block(block, theme))
            .collect(),
        PreviewBody::Lines(lines) => {
            let width = lines.len().min(FILES_LINE_LIMIT).to_string().len();
            let mut drawn: Vec<AnyElement> = lines
                .iter()
                .take(FILES_LINE_LIMIT)
                .enumerate()
                .map(|(i, line)| {
                    let color = if diff_on {
                        diff_color(line, theme)
                    } else {
                        theme.text
                    };
                    let mut row = div()
                        .flex()
                        .flex_none()
                        .gap(Theme::SPACE_2)
                        .min_h(px(14.0))
                        .px(Theme::SPACE_2)
                        .whitespace_nowrap();
                    if !diff_on {
                        row = row.child(
                            div()
                                .flex_none()
                                .text_color(theme.text_subtle)
                                .child(format!("{:>width$}", i + 1)),
                        );
                    }
                    row.child(
                        div()
                            .text_color(color)
                            .child(cut_line(line, FILES_LINE_CHARS)),
                    )
                    .into_any_element()
                })
                .collect();
            if lines.len() > FILES_LINE_LIMIT {
                drawn.push(
                    note(
                        "files-lines-more",
                        format!("… {} more lines", lines.len() - FILES_LINE_LIMIT),
                        theme,
                    )
                    .into_any_element(),
                );
            }
            drawn
        }
    };

    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h_0()
        .gap(Theme::SPACE_1)
        .children(heading)
        .children(cut)
        .child(
            div()
                // Keyed by the file, so another file opens at its top.
                .id(SharedString::from(format!(
                    "files-preview-{}",
                    preview.path
                )))
                .debug_selector(|| "files-preview".to_owned())
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .overflow_scroll()
                .py(Theme::SPACE_1)
                .rounded(Theme::RADIUS_SM)
                .border_1()
                .border_color(theme.border)
                .bg(theme.chrome)
                .font_family("Menlo")
                .text_size(Theme::TEXT_XS)
                .children(body),
        )
        .into_any_element()
}

pub(crate) fn markdown_block(block: &MarkdownBlock, theme: &Theme) -> AnyElement {
    match block {
        MarkdownBlock::Heading(level, text) => {
            let size = match level {
                1 => Theme::TEXT_XL,
                2 => Theme::TEXT_LG,
                _ => Theme::TEXT_MD,
            };
            div()
                .flex_none()
                .min_w_0()
                .mt(if *level <= 2 {
                    Theme::SPACE_3
                } else {
                    Theme::SPACE_2
                })
                .px(Theme::SPACE_4)
                .pb(Theme::SPACE_1)
                .border_b_1()
                .border_color(theme.border_subtle)
                .font_weight(FontWeight::SEMIBOLD)
                .text_size(size)
                .text_color(theme.text)
                .child(text.clone())
                .into_any_element()
        }
        MarkdownBlock::Paragraph(text) => div()
            .flex_none()
            .min_w_0()
            .px(Theme::SPACE_4)
            .py(Theme::SPACE_1)
            .text_size(Theme::TEXT_SM)
            .text_color(theme.text)
            .child(text.clone())
            .into_any_element(),
        MarkdownBlock::ListItem(depth, text) => div()
            .flex()
            .flex_none()
            .min_w_0()
            .gap(Theme::SPACE_2)
            .pl(px(16.0 + 16.0 * *depth as f32))
            .pr(Theme::SPACE_4)
            .py(px(2.0))
            .text_size(Theme::TEXT_SM)
            .text_color(theme.text)
            .child(div().flex_none().text_color(theme.accent).child("•"))
            .child(div().min_w_0().child(text.clone()))
            .into_any_element(),
        MarkdownBlock::Quote(text) => div()
            .flex_none()
            .min_w_0()
            .mx(Theme::SPACE_4)
            .my(Theme::SPACE_1)
            .pl(Theme::SPACE_3)
            .py(Theme::SPACE_2)
            .border_l_2()
            .border_color(theme.accent)
            .text_size(Theme::TEXT_SM)
            .text_color(theme.text_muted)
            .child(text.clone())
            .into_any_element(),
        MarkdownBlock::Code(language, text) => div()
            .flex()
            .flex_col()
            .flex_none()
            .min_w_0()
            .mx(Theme::SPACE_4)
            .my(Theme::SPACE_2)
            .rounded(Theme::RADIUS_MD)
            .border_1()
            .border_color(theme.border)
            .bg(theme.raised)
            .children(language.clone().map(|language| {
                div()
                    .px(Theme::SPACE_3)
                    .py(Theme::SPACE_1)
                    .border_b_1()
                    .border_color(theme.border_subtle)
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_subtle)
                    .child(language)
            }))
            .child(
                div()
                    .p(Theme::SPACE_3)
                    .whitespace_nowrap()
                    .font_family("Menlo")
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text)
                    .child(text.clone()),
            )
            .into_any_element(),
        MarkdownBlock::Rule => div()
            .flex_none()
            .h(px(1.0))
            .mx(Theme::SPACE_4)
            .my(Theme::SPACE_3)
            .bg(theme.border)
            .into_any_element(),
    }
}

/// The colour of a diff line: additions in success, removals in danger, hunk headers in the accent.
pub(crate) fn diff_color(line: &str, theme: &Theme) -> gpui::Hsla {
    if line.starts_with("+++") || line.starts_with("---") {
        theme.text_muted
    } else if line.starts_with('+') {
        theme.success
    } else if line.starts_with('-') {
        theme.danger
    } else if line.starts_with("@@") {
        theme.accent
    } else {
        theme.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_have_letters_and_tones() {
        assert_eq!(FileMark::Modified.letter(), "M");
        assert_eq!(FileMark::Untracked.letter(), "?");
        assert_eq!(FileMark::Renamed.letter(), "R");
        assert_eq!(FileMark::Deleted.tone(), Tone::Danger);
        assert_eq!(FileMark::Added.tone(), Tone::Success);
    }

    #[test]
    fn sizes_read_in_binary_units() {
        assert_eq!(size_label(512).as_ref(), "512 B");
        assert_eq!(size_label(4300).as_ref(), "4.2 KiB");
        assert_eq!(size_label(3 * 1024 * 1024).as_ref(), "3.0 MiB");
    }

    #[test]
    fn markdown_keeps_document_structure() {
        let blocks = markdown_blocks(
            "# Read me\n\nA useful paragraph.\n\n- first\n- second\n\n```rust\nfn main() {}\n```\n",
        );
        assert!(matches!(
            &blocks[0],
            MarkdownBlock::Heading(1, text) if text.as_ref() == "Read me"
        ));
        assert!(matches!(
            &blocks[1],
            MarkdownBlock::Paragraph(text) if text.as_ref() == "A useful paragraph."
        ));
        assert!(matches!(&blocks[2], MarkdownBlock::ListItem(0, _)));
        assert!(matches!(
            &blocks[4],
            MarkdownBlock::Code(Some(language), text)
                if language.as_ref() == "rust" && text.contains("fn main")
        ));
    }
}
