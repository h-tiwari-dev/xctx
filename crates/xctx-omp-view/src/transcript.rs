//! The transcript: a [`SessionModel`]'s entries flattened into rows and drawn in a virtualised GPUI list that
//! follows the tail while the agent streams. Only rows whose content changed are re-measured on an update.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    AnyElement, App, EntityId, FollowMode, FontWeight, ListAlignment, ListState, SharedString, div,
    prelude::*, px,
};
use xctx_omp::{AssistantEntry, Block, Entry, Level, SessionModel};
use xctx_ui::{
    Badge, Button, ButtonSize, ButtonVariant, Icon, IconName, IconSize, ScrollableElement as _,
    Theme, Tone,
};

use crate::changes::CopyCallback;
use crate::markdown::{self, MdBlock};
use crate::selectable::{SegmentId, SelectContext, find_matches, selectable_plain};
use crate::tool_card::{self, OpenFileHandler};

/// Callback invoked when the user clicks Edit on a previous user message to load it into the composer.
pub type EditPromptCallback = Rc<dyn Fn(String, &mut App)>;

/// Optional interactive callbacks and selection state for [`Transcript::render_interactive`].
#[derive(Clone, Default)]
pub struct TranscriptOptions {
    /// Active "Copied" feedback key.
    pub copied: Option<String>,
    /// One-click Copy button callback.
    pub on_copy: Option<CopyCallback>,
    /// Mouse-drag selection & search highlight context.
    pub select: Option<SelectContext>,
    /// Open-file button callback on Read/Edit/Write tool cards.
    pub on_open_file: Option<OpenFileHandler>,
    /// Edit-prompt callback on user messages.
    pub on_edit_prompt: Option<EditPromptCallback>,
}

/// One row of the transcript, addressed by where its content sits in the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowKey {
    /// A user (or agent-sent) message: `entries[i]`.
    User(usize),
    /// Block `block` of the assistant message `entries[entry]`.
    Block {
        /// The entry.
        entry: usize,
        /// The block.
        block: usize,
    },
    /// How the assistant message `entries[i]` stopped, when it failed or was aborted.
    Stop(usize),
    /// A notice: `entries[i]`.
    Notice(usize),
}

fn shows_stop(a: &AssistantEntry) -> bool {
    !a.streaming
        && (a.error.is_some() || matches!(a.stop_reason.as_deref(), Some("error" | "aborted")))
}

/// Append the rows `model` shows to `out`, in transcript order. Blank text blocks (a block that has only started)
/// get no row.
pub fn build_rows(model: &SessionModel, out: &mut Vec<RowKey>) {
    for (i, entry) in model.entries.iter().enumerate() {
        match entry {
            Entry::User { .. } => out.push(RowKey::User(i)),
            Entry::Notice { .. } => out.push(RowKey::Notice(i)),
            Entry::Assistant(a) => {
                for (b, block) in a.blocks.iter().enumerate() {
                    if let Block::Text(text) = block
                        && text.trim().is_empty()
                    {
                        continue;
                    }
                    out.push(RowKey::Block { entry: i, block: b });
                }
                if shows_stop(a) {
                    out.push(RowKey::Stop(i));
                }
            }
        }
    }
}

fn assistant(model: &SessionModel, entry: usize) -> Option<&AssistantEntry> {
    match model.entries.get(entry) {
        Some(Entry::Assistant(a)) => Some(a),
        _ => None,
    }
}

fn block(model: &SessionModel, entry: usize, index: usize) -> Option<&Block> {
    assistant(model, entry)?.blocks.get(index)
}

/// Whether `key` draws the same thing in `old` as in `new`.
fn same(old: &SessionModel, new: &SessionModel, key: RowKey) -> bool {
    match key {
        RowKey::User(i) | RowKey::Notice(i) => old.entries.get(i) == new.entries.get(i),
        RowKey::Stop(i) => {
            let pick = |m: &SessionModel| {
                assistant(m, i).map(|a| (a.stop_reason.clone(), a.error.clone()))
            };
            pick(old) == pick(new)
        }
        RowKey::Block { entry, block: b } => {
            let (was, is) = (block(old, entry, b), block(new, entry, b));
            if was != is {
                return false;
            }
            match is {
                Some(Block::ToolCall(id)) => old.tools.get(id) == new.tools.get(id),
                _ => true,
            }
        }
    }
}

/// The first row that differs between the old rows of `old` and the new rows of `new`; every row before it
/// draws the same and keeps its measured height.
pub fn first_change(
    old: &SessionModel,
    old_rows: &[RowKey],
    new: &SessionModel,
    new_rows: &[RowKey],
) -> usize {
    old_rows
        .iter()
        .zip(new_rows)
        .position(|(a, b)| a != b || !same(old, new, *b))
        .unwrap_or(old_rows.len().min(new_rows.len()))
}

/// The transcript list and what it remembers between snapshots: the rows, their parsed markdown, and which tool
/// cards and thinking blocks are open.
pub struct Transcript {
    state: ListState,
    model: Arc<SessionModel>,
    rows: Rc<Vec<RowKey>>,
    scratch: Vec<RowKey>,
    markdown: Rc<RefCell<HashMap<RowKey, Rc<Vec<MdBlock>>>>>,
    open_tools: Rc<RefCell<HashSet<String>>>,
    open_thinking: Rc<RefCell<HashSet<RowKey>>>,
}

impl Transcript {
    /// An empty transcript that follows the newest row.
    pub fn new() -> Self {
        let state = ListState::new(0, ListAlignment::Top, px(400.0));
        state.set_follow_mode(FollowMode::Tail);
        Transcript {
            state,
            model: Arc::new(SessionModel::new()),
            rows: Rc::new(Vec::new()),
            scratch: Vec::new(),
            markdown: Rc::default(),
            open_tools: Rc::default(),
            open_thinking: Rc::default(),
        }
    }

    /// Show `model`: rows from the first changed one on are replaced and re-measured, the rest are kept.
    pub fn set_model(&mut self, model: Arc<SessionModel>) {
        let mut next = std::mem::take(&mut self.scratch);
        next.clear();
        build_rows(&model, &mut next);
        let at = first_change(&self.model, &self.rows, &model, &next);
        let old_len = self.rows.len();
        if at < old_len || at < next.len() {
            self.state.splice(at..old_len, next.len() - at);
            let mut cache = self.markdown.borrow_mut();
            for key in &self.rows[at..] {
                cache.remove(key);
            }
        }
        if next.len() < old_len && model.entries.len() < self.model.entries.len() {
            self.open_tools.borrow_mut().clear();
            self.open_thinking.borrow_mut().clear();
        }
        self.model = model;
        let rows = Rc::make_mut(&mut self.rows);
        std::mem::swap(rows, &mut next);
        self.scratch = next;
    }

    /// The rows shown, in order.
    pub fn rows(&self) -> &[RowKey] {
        &self.rows
    }

    /// Whether there is nothing to show.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Whether every tool call in the current session is expanded.
    pub fn all_tools_open(&self) -> bool {
        !self.model.tools.is_empty() && {
            let open = self.open_tools.borrow();
            self.model.tools.keys().all(|id| open.contains(id))
        }
    }

    /// Expand every tool card in the transcript and remeasure list heights.
    pub fn expand_all_tools(&self) {
        {
            let mut open = self.open_tools.borrow_mut();
            for id in self.model.tools.keys() {
                open.insert(id.clone());
            }
        }
        self.state.remeasure();
    }

    /// Collapse every tool card in the transcript and remeasure list heights.
    pub fn collapse_all_tools(&self) {
        self.open_tools.borrow_mut().clear();
        self.state.remeasure();
    }

    /// Whether the list is currently at (or near) the bottom.
    pub fn is_scrolled_to_end(&self) -> bool {
        self.state.is_scrolled_to_end().unwrap_or(true)
    }

    /// Scroll to the newest row in the transcript and resume tail following.
    pub fn scroll_to_end(&self) {
        self.state.scroll_to_end();
        self.state.set_follow_mode(FollowMode::Tail);
    }

    /// Scroll to reveal transcript row `ix` (for in-session search).
    pub fn scroll_to_row(&self, ix: usize) {
        self.state.scroll_to_reveal_item(ix);
    }

    /// Indices of rows whose visible text matches `query` (case-insensitive).
    pub fn search_rows(&self, query: &str) -> Vec<usize> {
        if query.trim().is_empty() {
            return Vec::new();
        }
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(ix, key)| {
                let text = row_search_text(&self.model, *key);
                (!find_matches(&text, query).is_empty()).then_some(ix)
            })
            .collect()
    }

    /// Format the full session transcript as Markdown (messages, tool summaries/diffs, and notices).
    pub fn format_markdown(&self) -> String {
        format_transcript_markdown(&self.model)
    }

    /// The last non-empty assistant text response in the transcript, if any.
    pub fn last_reply(&self) -> Option<String> {
        last_assistant_reply(&self.model)
    }

    /// The list element; `view` is notified when a card is opened or closed.
    pub fn render(&self, view: EntityId) -> AnyElement {
        self.render_interactive(view, TranscriptOptions::default())
    }

    /// The list element with one-click Copy buttons wired to `on_copy` and active `copied` feedback key.
    pub fn render_with_copy(
        &self,
        view: EntityId,
        copied: Option<String>,
        on_copy: Option<CopyCallback>,
    ) -> AnyElement {
        self.render_interactive(
            view,
            TranscriptOptions {
                copied,
                on_copy,
                ..TranscriptOptions::default()
            },
        )
    }

    /// The list element with full selection, search highlighting, copy, open-file, and edit-prompt callbacks.
    pub fn render_interactive(&self, view: EntityId, opts: TranscriptOptions) -> AnyElement {
        let ctx = RowContext {
            model: self.model.clone(),
            rows: self.rows.clone(),
            markdown: self.markdown.clone(),
            open_tools: self.open_tools.clone(),
            open_thinking: self.open_thinking.clone(),
            state: self.state.clone(),
            view,
            copied: opts.copied,
            on_copy: opts.on_copy,
            select: opts.select,
            on_open_file: opts.on_open_file,
            on_edit_prompt: opts.on_edit_prompt,
        };
        div()
            .relative()
            .size_full()
            .child(
                gpui::list(self.state.clone(), move |ix, _window, cx| ctx.row(ix, cx)).size_full(),
            )
            .vertical_scrollbar(&self.state)
            .into_any_element()
    }
}

fn row_search_text(model: &SessionModel, key: RowKey) -> String {
    match key {
        RowKey::User(i) => match model.entries.get(i) {
            Some(Entry::User { text, .. }) => text.clone(),
            _ => String::new(),
        },
        RowKey::Notice(i) => match model.entries.get(i) {
            Some(Entry::Notice { text, .. }) => text.clone(),
            _ => String::new(),
        },
        RowKey::Stop(i) => assistant(model, i)
            .and_then(|a| a.error.clone())
            .unwrap_or_default(),
        RowKey::Block { entry, block: b } => match block(model, entry, b) {
            Some(Block::Text(t) | Block::Thinking(t)) => t.clone(),
            Some(Block::ToolCall(id)) => model
                .tools
                .get(id)
                .map(|run| {
                    let mut s = format!("{} {}", tool_card::title(run), tool_card::summary(run));
                    if !run.output.is_empty() {
                        s.push('\n');
                        s.push_str(&run.output);
                    }
                    s
                })
                .unwrap_or_default(),
            _ => String::new(),
        },
    }
}

/// Format `model`'s transcript as readable Markdown for the clipboard.
pub fn format_transcript_markdown(model: &SessionModel) -> String {
    let mut out = String::new();
    for entry in &model.entries {
        match entry {
            Entry::User {
                text, from_agent, ..
            } => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str(if *from_agent {
                    "## Agent\n\n"
                } else {
                    "## You\n\n"
                });
                out.push_str(text.trim_end());
            }
            Entry::Notice { text, .. } => {
                if !out.is_empty() {
                    out.push_str("\n\n");
                }
                out.push_str("> ");
                out.push_str(text.trim());
            }
            Entry::Assistant(a) => {
                let mut parts: Vec<String> = Vec::new();
                for block in &a.blocks {
                    match block {
                        Block::Text(text) if !text.trim().is_empty() => {
                            parts.push(text.trim_end().to_owned());
                        }
                        Block::ToolCall(id) => {
                            if let Some(run) = model.tools.get(id) {
                                let head = format!(
                                    "### {} `{}`",
                                    tool_card::title(run),
                                    tool_card::summary(run)
                                );
                                if let Some(payload) = tool_card::copyable_text(run) {
                                    let lang = if tool_card::ToolKind::of(&run.name)
                                        == tool_card::ToolKind::Edit
                                    {
                                        "diff"
                                    } else {
                                        ""
                                    };
                                    parts.push(format!("{head}\n```{lang}\n{payload}\n```"));
                                } else {
                                    parts.push(head);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                if !parts.is_empty() {
                    if !out.is_empty() {
                        out.push_str("\n\n");
                    }
                    out.push_str("## Assistant\n\n");
                    out.push_str(&parts.join("\n\n"));
                }
            }
        }
    }
    out
}

/// Return the last assistant entry's text blocks joined by blank lines, if any.
pub fn last_assistant_reply(model: &SessionModel) -> Option<String> {
    for entry in model.entries.iter().rev() {
        if let Entry::Assistant(a) = entry {
            let texts: Vec<&str> = a
                .blocks
                .iter()
                .filter_map(|b| match b {
                    Block::Text(t) if !t.trim().is_empty() => Some(t.trim_end()),
                    _ => None,
                })
                .collect();
            if !texts.is_empty() {
                return Some(texts.join("\n\n"));
            }
        }
    }
    None
}

impl Default for Transcript {
    fn default() -> Self {
        Self::new()
    }
}

fn toggle_tool(open: &RefCell<HashSet<String>>, id: &str) {
    let mut open = open.borrow_mut();
    if !open.remove(id) {
        open.insert(id.to_owned());
    }
}

struct RowContext {
    model: Arc<SessionModel>,
    rows: Rc<Vec<RowKey>>,
    markdown: Rc<RefCell<HashMap<RowKey, Rc<Vec<MdBlock>>>>>,
    open_tools: Rc<RefCell<HashSet<String>>>,
    open_thinking: Rc<RefCell<HashSet<RowKey>>>,
    state: ListState,
    view: EntityId,
    copied: Option<String>,
    on_copy: Option<CopyCallback>,
    select: Option<SelectContext>,
    on_open_file: Option<OpenFileHandler>,
    on_edit_prompt: Option<EditPromptCallback>,
}

impl RowContext {
    fn copy_button(
        &self,
        copy_key: String,
        selector: String,
        tooltip: &'static str,
        payload: String,
        show_label: bool,
    ) -> Option<Button> {
        let on_copy = self.on_copy.clone()?;
        let is_copied = self.copied.as_deref() == Some(copy_key.as_str());
        let label = if is_copied {
            "Copied"
        } else if show_label {
            "Copy"
        } else {
            ""
        };
        Some(
            Button::new(SharedString::from(format!("omp-btn-{copy_key}")), label)
                .icon(if is_copied {
                    IconName::Check
                } else {
                    IconName::Copy
                })
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .tooltip(tooltip)
                .selector(selector)
                .on_click(move |_, _, cx| on_copy(copy_key.clone(), payload.clone(), cx)),
        )
    }

    fn row(&self, ix: usize, cx: &mut App) -> AnyElement {
        let theme = Theme::of(cx);
        let Some(&key) = self.rows.get(ix) else {
            return div().into_any_element();
        };
        let mut seg_ix = 0usize;
        let content = match key {
            RowKey::User(i) => match self.model.entries.get(i) {
                Some(Entry::User {
                    text,
                    images,
                    from_agent,
                }) => {
                    let copy_key = format!("user-{ix}");
                    let is_copied = self.copied.as_deref() == Some(copy_key.as_str());
                    let copy_btn = (!text.trim().is_empty())
                        .then(|| {
                            self.copy_button(
                                copy_key,
                                format!("omp-copy-user-{ix}"),
                                "Copy message",
                                text.clone(),
                                false,
                            )
                        })
                        .flatten();
                    let edit_btn = (!from_agent && !text.trim().is_empty())
                        .then(|| {
                            let on_edit = self.on_edit_prompt.clone()?;
                            let draft_text = text.clone();
                            Some(
                                Button::new(SharedString::from(format!("omp-edit-user-{ix}")), "")
                                    .icon(IconName::PenTool)
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::Compact)
                                    .tooltip("Edit in composer")
                                    .selector(format!("omp-edit-user-{ix}"))
                                    .on_click(move |_, _, cx| on_edit(draft_text.clone(), cx)),
                            )
                        })
                        .flatten();
                    let body = if let Some(sel) = &self.select {
                        let seg = SegmentId::new(ix, seg_ix);
                        seg_ix += 1;
                        selectable_plain(seg, text.clone(), sel, &theme)
                    } else {
                        text.clone().into_any_element()
                    };
                    let _ = seg_ix;
                    user_row(
                        ix,
                        body,
                        *images,
                        *from_agent,
                        is_copied,
                        edit_btn,
                        copy_btn,
                        &theme,
                    )
                }
                _ => div().into_any_element(),
            },
            RowKey::Notice(i) => match self.model.entries.get(i) {
                Some(Entry::Notice { level, text }) => {
                    let copy_key = format!("notice-{ix}");
                    let is_copied = self.copied.as_deref() == Some(copy_key.as_str());
                    let copy_btn = (!text.trim().is_empty())
                        .then(|| {
                            self.copy_button(
                                copy_key,
                                format!("omp-copy-notice-{ix}"),
                                "Copy notice",
                                text.clone(),
                                false,
                            )
                        })
                        .flatten();
                    let body = if let Some(sel) = &self.select {
                        let seg = SegmentId::new(ix, seg_ix);
                        selectable_plain(seg, text.clone(), sel, &theme)
                    } else {
                        text.clone().into_any_element()
                    };
                    notice_row(ix, *level, body, is_copied, copy_btn, &theme)
                }
                _ => div().into_any_element(),
            },
            RowKey::Stop(i) => match assistant(&self.model, i) {
                Some(a) => stop_row(a, &theme),
                None => div().into_any_element(),
            },
            RowKey::Block { entry, block: b } => match block(&self.model, entry, b) {
                Some(Block::Text(text)) => {
                    let parsed = self
                        .markdown
                        .borrow_mut()
                        .entry(key)
                        .or_insert_with(|| Rc::new(markdown::parse(text)))
                        .clone();
                    let next_same_entry = matches!(
                        self.rows.get(ix + 1),
                        Some(RowKey::Block { entry: e, .. }) if *e == entry
                    );
                    let is_turn_tail = !next_same_entry;
                    let copy_key = format!("assistant-{ix}");
                    let is_copied = self.copied.as_deref() == Some(copy_key.as_str());
                    let model_badge = is_turn_tail
                        .then(|| assistant(&self.model, entry).and_then(|a| a.model.as_deref()))
                        .flatten()
                        .map(|m| {
                            div()
                                .text_size(Theme::TEXT_XS)
                                .text_color(theme.text_subtle)
                                .child(m.to_owned())
                        });
                    let copy_btn = self.copy_button(
                        copy_key,
                        format!("omp-copy-assistant-{ix}"),
                        "Copy response",
                        text.clone(),
                        is_turn_tail,
                    );
                    let group_name: SharedString = format!("omp-assistant-group-{ix}").into();
                    let md = if let Some(sel) = &self.select {
                        markdown::render_selectable(
                            &parsed,
                            &format!("row-{ix}"),
                            self.copied.as_deref(),
                            self.on_copy.clone(),
                            (sel, ix, &mut seg_ix),
                            &theme,
                        )
                    } else {
                        markdown::render_with_copy(
                            &parsed,
                            &format!("row-{ix}"),
                            self.copied.as_deref(),
                            self.on_copy.clone(),
                            &theme,
                        )
                    };
                    let mut col = div()
                        .group(group_name.clone())
                        .relative()
                        .debug_selector(|| "omp-assistant-text".to_owned())
                        .flex()
                        .flex_col()
                        .w_full()
                        .child(md);
                    if is_turn_tail {
                        let show_bar = model_badge.is_some() || copy_btn.is_some();
                        if show_bar {
                            col = col.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(Theme::SPACE_2)
                                    .w_full()
                                    .pt(px(6.0))
                                    .when(!is_copied, |bar| {
                                        bar.opacity(0.55)
                                            .group_hover(group_name, |s| s.opacity(1.0))
                                    })
                                    .children(model_badge)
                                    .child(div().flex_1())
                                    .children(copy_btn),
                            );
                        }
                    } else if let Some(btn) = copy_btn {
                        col = col.child(
                            div()
                                .absolute()
                                .top(px(-2.0))
                                .right_0()
                                .rounded(Theme::RADIUS_SM)
                                .bg(theme.surface.alpha(0.92))
                                .when(!is_copied, |pill| {
                                    pill.opacity(0.0)
                                        .group_hover(group_name, |s| s.opacity(1.0))
                                })
                                .child(btn),
                        );
                    }
                    col.into_any_element()
                }
                Some(Block::Thinking(text)) => {
                    self.thinking_row(ix, key, text, &mut seg_ix, &theme)
                }
                Some(Block::ToolCall(id)) => match self.model.tools.get(id) {
                    Some(run) => {
                        let open = self.open_tools.borrow().contains(id);
                        let tools = self.open_tools.clone();
                        let state = self.state.clone();
                        let view = self.view;
                        let id = id.clone();
                        tool_card::card_interactive(
                            run,
                            open,
                            Box::new(move |_, _, cx| {
                                toggle_tool(&tools, &id);
                                state.remeasure_items(ix..ix + 1);
                                cx.notify(view);
                            }),
                            self.copied.as_deref(),
                            self.on_copy.clone(),
                            self.on_open_file.clone(),
                            self.select.as_ref(),
                            ix,
                            &mut seg_ix,
                            &theme,
                        )
                    }
                    None => div().into_any_element(),
                },
                Some(Block::Image) => image_tile(&theme),
                None => div().into_any_element(),
            },
        };
        let prev = ix.checked_sub(1).and_then(|p| self.rows.get(p)).copied();
        let (pad_top, pad_bottom) = row_spacing(&self.model, prev, key);
        div()
            .flex()
            .justify_center()
            .w_full()
            .px(Theme::SPACE_4)
            .pt(pad_top)
            .pb(pad_bottom)
            .child(div().w_full().max_w(px(880.0)).min_w_0().child(content))
            .into_any_element()
    }

    fn thinking_row(
        &self,
        ix: usize,
        key: RowKey,
        text: &str,
        seg_ix: &mut usize,
        theme: &Theme,
    ) -> AnyElement {
        let open = self.open_thinking.borrow().contains(&key);
        let preview: SharedString = if text.trim().is_empty() {
            "redacted".into()
        } else {
            text.lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .trim()
                .to_owned()
                .into()
        };
        let thinking = self.open_thinking.clone();
        let state = self.state.clone();
        let view = self.view;
        let hover = theme.hover;
        let copy_btn = (open && !text.trim().is_empty())
            .then(|| {
                self.copy_button(
                    format!("thinking-{ix}"),
                    format!("omp-copy-thinking-{ix}"),
                    "Copy thinking",
                    text.to_owned(),
                    false,
                )
            })
            .flatten();
        let expanded_body = open.then(|| {
            let body: AnyElement = if let Some(sel) = &self.select {
                let seg = SegmentId::new(ix, *seg_ix);
                *seg_ix += 1;
                selectable_plain(seg, text.to_owned(), sel, theme)
            } else {
                text.to_owned().into_any_element()
            };
            div()
                .pl(Theme::SPACE_3)
                .pr(Theme::SPACE_3)
                .ml(px(8.0))
                .mt(px(4.0))
                .py(px(6.0))
                .rounded_r(Theme::RADIUS_SM)
                .bg(theme.raised.alpha(0.28))
                .border_l_2()
                .border_color(theme.border)
                .text_size(Theme::TEXT_SM)
                .line_height(px(20.0))
                .text_color(theme.text_muted)
                .italic()
                .child(body)
        });
        div()
            .debug_selector(|| "omp-thinking".to_owned())
            .flex()
            .flex_col()
            .w_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .child(
                        div()
                            .id(SharedString::from(format!("omp-thinking-{ix}")))
                            .flex()
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap(px(6.0))
                            .h(px(24.0))
                            .px(px(6.0))
                            .cursor_pointer()
                            .rounded(px(6.0))
                            .hover(move |s| s.bg(hover))
                            .on_click(move |_, _, cx| {
                                {
                                    let mut open = thinking.borrow_mut();
                                    if !open.remove(&key) {
                                        open.insert(key);
                                    }
                                }
                                state.remeasure_items(ix..ix + 1);
                                cx.notify(view);
                            })
                            .child(
                                Icon::new(if open {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .size(IconSize::Xs)
                                .color(theme.text_subtle),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .text_size(Theme::TEXT_XS)
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme.text_subtle)
                                    .child("Thinking"),
                            )
                            .when(!open, |row| {
                                row.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(Theme::TEXT_XS)
                                        .italic()
                                        .text_color(theme.text_subtle.alpha(0.78))
                                        .child(preview),
                                )
                            }),
                    )
                    .children(copy_btn),
            )
            .children(expanded_body)
            .into_any_element()
    }
}

fn row_spacing(
    model: &SessionModel,
    prev: Option<RowKey>,
    curr: RowKey,
) -> (gpui::Pixels, gpui::Pixels) {
    let is_activity = |key: RowKey| match key {
        RowKey::Block { entry, block: b } => matches!(
            block(model, entry, b),
            Some(Block::ToolCall(_) | Block::Thinking(_))
        ),
        _ => false,
    };
    let is_text = |key: RowKey| match key {
        RowKey::Block { entry, block: b } => matches!(block(model, entry, b), Some(Block::Text(_))),
        _ => false,
    };
    match (prev, curr) {
        (None, RowKey::User(_)) => (px(8.0), px(4.0)),
        (Some(_), RowKey::User(_)) => (px(18.0), px(4.0)),
        (Some(p), c) if is_activity(p) && is_activity(c) => (px(2.0), px(1.0)),
        (Some(p), c) if is_activity(p) && is_text(c) => (px(10.0), px(2.0)),
        (Some(RowKey::User(_)), c) if is_text(c) => (px(10.0), px(2.0)),
        (Some(RowKey::User(_)), c) if is_activity(c) => (px(8.0), px(1.0)),
        (_, c) if is_activity(c) => (px(4.0), px(1.0)),
        _ => (px(6.0), px(3.0)),
    }
}

#[allow(clippy::too_many_arguments)]
fn user_row(
    ix: usize,
    body: AnyElement,
    images: usize,
    from_agent: bool,
    is_copied: bool,
    edit_btn: Option<Button>,
    copy_btn: Option<Button>,
    theme: &Theme,
) -> AnyElement {
    let group_name: SharedString = format!("omp-user-group-{ix}").into();
    let has_actions = edit_btn.is_some() || copy_btn.is_some();
    let tint = if from_agent {
        theme.raised.alpha(0.55)
    } else {
        theme.raised.alpha(0.88)
    };
    let border = if from_agent {
        theme.border_subtle
    } else {
        theme.border
    };
    let dot_color = if from_agent {
        theme.text_subtle
    } else {
        theme.accent
    };
    div()
        .group(group_name.clone())
        .debug_selector(|| "omp-user-message".to_owned())
        .flex()
        .flex_col()
        .gap(px(5.0))
        .w_full()
        .px(px(14.0))
        .py(px(10.0))
        .rounded(px(10.0))
        .bg(tint)
        .border_1()
        .border_color(border)
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_1)
                .w_full()
                .h(px(20.0))
                .child(div().size(px(6.0)).rounded_full().bg(dot_color))
                .child(
                    div()
                        .flex_1()
                        .pl(px(2.0))
                        .text_size(Theme::TEXT_XS)
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme.text_muted)
                        .child(if from_agent { "Agent" } else { "You" }),
                )
                .when(has_actions, |row| {
                    row.child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap(px(2.0))
                            .when(!is_copied, |actions| {
                                actions
                                    .opacity(0.0)
                                    .group_hover(group_name, |s| s.opacity(1.0))
                            })
                            .children(edit_btn)
                            .children(copy_btn),
                    )
                }),
        )
        .child(
            div()
                .text_size(px(13.5))
                .line_height(px(22.0))
                .text_color(theme.text)
                .child(body),
        )
        .when(images > 0, |col| {
            col.child(
                div()
                    .flex()
                    .gap(Theme::SPACE_1)
                    .children((0..images).map(|_| image_tile(theme))),
            )
        })
        .into_any_element()
}

/// A square tile standing for an image in the transcript. omp's frames carry image bytes, but the session model
/// keeps only that an image was there, so the tile shows no picture.
fn image_tile(theme: &Theme) -> AnyElement {
    div()
        .debug_selector(|| "omp-image".to_owned())
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .size(px(48.0))
        .rounded(Theme::RADIUS_SM)
        .border_1()
        .border_color(theme.border)
        .bg(theme.raised)
        .text_size(Theme::TEXT_XS)
        .text_color(theme.text_subtle)
        .child("image")
        .into_any_element()
}

fn level_tone(level: Level) -> Tone {
    match level {
        Level::Info => Tone::Neutral,
        Level::Warning => Tone::Warning,
        Level::Error => Tone::Danger,
    }
}

fn notice_row(
    ix: usize,
    level: Level,
    body: AnyElement,
    is_copied: bool,
    copy_btn: Option<Button>,
    theme: &Theme,
) -> AnyElement {
    let tone = level_tone(level);
    let colors = theme.tone(tone);
    let group_name: SharedString = format!("omp-notice-group-{ix}").into();
    div()
        .group(group_name.clone())
        .debug_selector(|| "omp-notice".to_owned())
        .flex()
        .items_start()
        .gap(Theme::SPACE_2)
        .w_full()
        .px(px(10.0))
        .py(px(7.0))
        .rounded(px(8.0))
        .border_1()
        .border_color(if tone == Tone::Neutral {
            theme.border_subtle.alpha(0.65)
        } else {
            colors.border
        })
        .bg(if tone == Tone::Neutral {
            theme.raised.alpha(0.4)
        } else {
            colors.bg
        })
        .text_size(Theme::TEXT_SM)
        .line_height(px(18.0))
        .text_color(if tone == Tone::Neutral {
            theme.text_muted
        } else {
            colors.fg
        })
        .child(
            div().flex_none().pt(px(3.0)).child(
                Icon::new(match level {
                    Level::Info => IconName::Idle,
                    Level::Warning => IconName::Warning,
                    Level::Error => IconName::Error,
                })
                .size(IconSize::Xs)
                .tone(tone),
            ),
        )
        .child(div().flex_1().min_w_0().child(body))
        .children(copy_btn.map(|btn| {
            div()
                .flex_none()
                .when(!is_copied, |wrap| {
                    wrap.opacity(0.0)
                        .group_hover(group_name, |s| s.opacity(1.0))
                })
                .child(btn)
        }))
        .into_any_element()
}

fn stop_row(a: &AssistantEntry, theme: &Theme) -> AnyElement {
    let aborted = a.stop_reason.as_deref() == Some("aborted") && a.error.is_none();
    let text = match (&a.error, aborted) {
        (Some(error), _) => error.clone(),
        (None, true) => "Stopped".to_owned(),
        (None, false) => "The model stopped on an error".to_owned(),
    };
    div()
        .debug_selector(|| "omp-stop".to_owned())
        .flex()
        .items_center()
        .gap(Theme::SPACE_2)
        .text_size(Theme::TEXT_SM)
        .italic()
        .text_color(theme.text_muted)
        .child(
            Badge::new(if aborted { "stopped" } else { "error" }).tone(if aborted {
                Tone::Neutral
            } else {
                Tone::Danger
            }),
        )
        .child(text)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &str) -> Block {
        Block::Text(t.to_owned())
    }

    fn model(entries: Vec<Entry>) -> SessionModel {
        let mut model = SessionModel::new();
        model.entries = entries;
        model
    }

    fn reply(blocks: Vec<Block>) -> Entry {
        Entry::Assistant(AssistantEntry {
            message_id: "m".into(),
            blocks,
            streaming: true,
            ..AssistantEntry::default()
        })
    }

    #[test]
    fn rows_skip_blank_text_and_add_a_stop_line_for_failures() {
        let user = Entry::User {
            text: "hi".into(),
            images: 0,
            from_agent: false,
        };
        let failed = Entry::Assistant(AssistantEntry {
            message_id: "m2".into(),
            blocks: vec![text("partial")],
            streaming: false,
            stop_reason: Some("error".into()),
            error: Some("overloaded".into()),
            model: None,
        });
        let m = model(vec![user, reply(vec![text(""), text("ok")]), failed]);
        let mut rows = Vec::new();
        build_rows(&m, &mut rows);
        assert_eq!(
            rows,
            [
                RowKey::User(0),
                RowKey::Block { entry: 1, block: 1 },
                RowKey::Block { entry: 2, block: 0 },
                RowKey::Stop(2)
            ]
        );
    }

    #[test]
    fn a_streaming_delta_changes_only_the_last_row() {
        let before = model(vec![reply(vec![text("one"), text("tw")])]);
        let after = model(vec![reply(vec![text("one"), text("two")])]);
        let (mut a, mut b) = (Vec::new(), Vec::new());
        build_rows(&before, &mut a);
        build_rows(&after, &mut b);
        assert_eq!(first_change(&before, &a, &after, &b), 1);
        assert_eq!(first_change(&after, &b, &after, &b), 2);
        let empty = SessionModel::default();
        assert_eq!(first_change(&after, &b, &empty, &[]), 0);
    }

    #[test]
    fn a_tool_result_changes_its_card_row() {
        use xctx_omp::{ToolRun, ToolState};
        let run = |state| ToolRun {
            id: "t".into(),
            name: "bash".into(),
            args: serde_json::Value::Null,
            state,
            output: String::new(),
            details: serde_json::Value::Null,
        };
        let mut before = model(vec![reply(vec![text("a"), Block::ToolCall("t".into())])]);
        before.tools.insert("t".into(), run(ToolState::Running));
        let mut after = before.clone();
        after.tools.insert("t".into(), run(ToolState::Done));
        let mut rows = Vec::new();
        build_rows(&after, &mut rows);
        assert_eq!(first_change(&before, &rows, &after, &rows), 1);
    }

    #[test]
    fn the_list_is_spliced_from_the_first_change() {
        let mut t = Transcript::new();
        t.set_model(Arc::new(model(vec![reply(vec![text("one")])])));
        assert_eq!(t.state.item_count(), 1);
        t.set_model(Arc::new(model(vec![reply(vec![text("one"), text("two")])])));
        assert_eq!(t.state.item_count(), 2);
        t.set_model(Arc::new(SessionModel::default()));
        assert_eq!(t.state.item_count(), 0);
        assert!(t.is_empty());
    }
}
