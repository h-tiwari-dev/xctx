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
use xctx_ui::{Badge, Icon, IconName, IconSize, Theme, Tone};

use crate::markdown::{self, MdBlock};
use crate::tool_card;

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

    /// The list element; `view` is notified when a card is opened or closed.
    pub fn render(&self, view: EntityId) -> AnyElement {
        let ctx = RowContext {
            model: self.model.clone(),
            rows: self.rows.clone(),
            markdown: self.markdown.clone(),
            open_tools: self.open_tools.clone(),
            open_thinking: self.open_thinking.clone(),
            state: self.state.clone(),
            view,
        };
        gpui::list(self.state.clone(), move |ix, _window, cx| ctx.row(ix, cx))
            .size_full()
            .into_any_element()
    }
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
}

impl RowContext {
    fn row(&self, ix: usize, cx: &mut App) -> AnyElement {
        let theme = Theme::of(cx);
        let Some(&key) = self.rows.get(ix) else {
            return div().into_any_element();
        };
        let content = match key {
            RowKey::User(i) => match self.model.entries.get(i) {
                Some(Entry::User {
                    text,
                    images,
                    from_agent,
                }) => user_row(text, *images, *from_agent, &theme),
                _ => div().into_any_element(),
            },
            RowKey::Notice(i) => match self.model.entries.get(i) {
                Some(Entry::Notice { level, text }) => notice_row(*level, text, &theme),
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
                    div()
                        .debug_selector(|| "omp-assistant-text".to_owned())
                        .w_full()
                        .child(markdown::render(&parsed, &theme))
                        .into_any_element()
                }
                Some(Block::Thinking(text)) => self.thinking_row(ix, key, text, &theme),
                Some(Block::ToolCall(id)) => match self.model.tools.get(id) {
                    Some(run) => {
                        let open = self.open_tools.borrow().contains(id);
                        let tools = self.open_tools.clone();
                        let state = self.state.clone();
                        let view = self.view;
                        let id = id.clone();
                        tool_card::card(
                            run,
                            open,
                            Box::new(move |_, _, cx| {
                                toggle_tool(&tools, &id);
                                state.remeasure_items(ix..ix + 1);
                                cx.notify(view);
                            }),
                            &theme,
                        )
                    }
                    None => div().into_any_element(),
                },
                Some(Block::Image) => image_tile(&theme),
                None => div().into_any_element(),
            },
        };
        div()
            .w_full()
            .px(Theme::SPACE_3)
            .py(Theme::SPACE_1)
            .child(content)
            .into_any_element()
    }

    fn thinking_row(&self, ix: usize, key: RowKey, text: &str, theme: &Theme) -> AnyElement {
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
        div()
            .debug_selector(|| "omp-thinking".to_owned())
            .flex()
            .flex_col()
            .w_full()
            .child(
                div()
                    .id(SharedString::from(format!("omp-thinking-{ix}")))
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .w_full()
                    .cursor_pointer()
                    .rounded(Theme::RADIUS_SM)
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
                            .text_size(Theme::TEXT_SM)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text_muted)
                            .child("Thinking"),
                    )
                    .when(!open, |row| {
                        row.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(Theme::TEXT_SM)
                                .italic()
                                .text_color(theme.text_subtle)
                                .child(preview),
                        )
                    }),
            )
            .when(open, |col| {
                col.child(
                    div()
                        .pl(Theme::SPACE_4)
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_muted)
                        .italic()
                        .child(text.to_owned()),
                )
            })
            .into_any_element()
    }
}

fn user_row(text: &str, images: usize, from_agent: bool, theme: &Theme) -> AnyElement {
    div()
        .debug_selector(|| "omp-user-message".to_owned())
        .flex()
        .flex_col()
        .gap(Theme::SPACE_1)
        .w_full()
        .p(Theme::SPACE_2)
        .rounded(Theme::RADIUS_MD)
        .bg(theme.raised)
        .border_1()
        .border_color(theme.border_subtle)
        .child(
            div()
                .text_size(Theme::TEXT_XS)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text_subtle)
                .child(if from_agent { "Agent" } else { "You" }),
        )
        .child(
            div()
                .text_size(Theme::TEXT_MD)
                .text_color(theme.text)
                .child(text.to_owned()),
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

fn notice_row(level: Level, text: &str, theme: &Theme) -> AnyElement {
    let tone = level_tone(level);
    let colors = theme.tone(tone);
    div()
        .debug_selector(|| "omp-notice".to_owned())
        .flex()
        .items_start()
        .gap(Theme::SPACE_2)
        .w_full()
        .text_size(Theme::TEXT_SM)
        .text_color(if tone == Tone::Neutral {
            theme.text_muted
        } else {
            colors.fg
        })
        .child(
            Icon::new(match level {
                Level::Info => IconName::Idle,
                Level::Warning => IconName::Warning,
                Level::Error => IconName::Error,
            })
            .size(IconSize::Xs)
            .tone(tone),
        )
        .child(div().flex_1().min_w_0().child(text.to_owned()))
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
