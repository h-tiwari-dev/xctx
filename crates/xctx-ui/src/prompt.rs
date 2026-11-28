//! A text input for the Home task prompt, also used for indexed folders and ship PR titles.
//!
//! [`PromptBuffer`] holds the text, caret and selection and does every edit; it needs no window, so it is tested
//! directly. [`PromptInput`] is the GPUI view that draws a buffer and feeds it keys and clicks.

use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, DispatchPhase, EventEmitter, FocusHandle, Focusable,
    HighlightStyle, HitboxBehavior, KeyDownEvent, Keystroke, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, SharedString, StyledText, Window, canvas, div, fill, prelude::*,
    px, size,
};

use crate::theme::Theme;

/// The text of a prompt with its caret and selection. Offsets are byte offsets into the text and always sit
/// on UTF-8 character boundaries.
///
/// In single-line mode (the default) inserted line breaks become spaces; in multi-line mode they are kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptBuffer {
    text: String,
    cursor: usize,
    /// The other end of the selection; `None`, or equal to `cursor`, means nothing is selected.
    anchor: Option<usize>,
    multiline: bool,
}

/// How a character groups with its neighbours when moving or deleting by word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CharClass {
    Space,
    Word,
    Punctuation,
}

fn char_class(c: char) -> CharClass {
    if c.is_whitespace() {
        CharClass::Space
    } else if c.is_alphanumeric() || c == '_' {
        CharClass::Word
    } else {
        CharClass::Punctuation
    }
}

impl PromptBuffer {
    /// An empty single-line buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty buffer that keeps line breaks when `multiline` is true.
    pub fn with_multiline(multiline: bool) -> Self {
        Self {
            multiline,
            ..Self::default()
        }
    }

    /// Whether line breaks are kept.
    pub fn is_multiline(&self) -> bool {
        self.multiline
    }

    /// Switch between single-line and multi-line mode. Switching to single-line turns existing line breaks
    /// into spaces.
    pub fn set_multiline(&mut self, multiline: bool) {
        self.multiline = multiline;
        if !multiline && self.text.contains(['\n', '\r']) {
            let text = std::mem::take(&mut self.text);
            self.set_text(&text);
        }
    }

    /// The text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Whether the text is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The caret's byte offset.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The selected byte range, or `None` when nothing is selected.
    pub fn selection(&self) -> Option<Range<usize>> {
        let anchor = self.anchor?;
        (anchor != self.cursor).then(|| anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    /// The selected text, or `None` when nothing is selected.
    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|range| &self.text[range])
    }

    /// Replace the text, putting the caret at its end with nothing selected.
    pub fn set_text(&mut self, text: &str) {
        self.text = self.normalize(text);
        self.cursor = self.text.len();
        self.anchor = None;
    }

    /// Remove all text.
    pub fn clear(&mut self) {
        self.set_text("");
    }

    /// Insert `text` at the caret, replacing the selection. Control characters other than line breaks and tabs
    /// are dropped; in single-line mode line breaks become spaces.
    pub fn insert(&mut self, text: &str) {
        let text = self.normalize(text);
        let range = self.selection().unwrap_or(self.cursor..self.cursor);
        self.text.replace_range(range.clone(), &text);
        self.cursor = range.start + text.len();
        self.anchor = None;
    }

    /// Delete the selection, or the character before the caret.
    pub fn backspace(&mut self) {
        if !self.delete_selection() {
            let start = self.prev_boundary(self.cursor);
            self.delete_range(start..self.cursor);
        }
    }

    /// Delete the selection, or the character after the caret.
    pub fn delete(&mut self) {
        if !self.delete_selection() {
            let end = self.next_boundary(self.cursor);
            self.delete_range(self.cursor..end);
        }
    }

    /// Delete the selection, or from the start of the word before the caret to the caret.
    pub fn delete_word_back(&mut self) {
        if !self.delete_selection() {
            let start = self.word_start_before(self.cursor);
            self.delete_range(start..self.cursor);
        }
    }

    /// Delete the selection, or from the start of the caret's line to the caret.
    pub fn delete_to_line_start(&mut self) {
        if !self.delete_selection() {
            let start = self.line_start(self.cursor);
            self.delete_range(start..self.cursor);
        }
    }

    /// Move the caret one character left. Without `extend`, a selection collapses to its start instead; with
    /// it, the selection grows or shrinks.
    pub fn move_left(&mut self, extend: bool) {
        match self.selection() {
            Some(range) if !extend => self.place(range.start, false),
            _ => self.place(self.prev_boundary(self.cursor), extend),
        }
    }

    /// Move the caret one character right. Without `extend`, a selection collapses to its end instead.
    pub fn move_right(&mut self, extend: bool) {
        match self.selection() {
            Some(range) if !extend => self.place(range.end, false),
            _ => self.place(self.next_boundary(self.cursor), extend),
        }
    }

    /// Move the caret to the start of the word before it.
    pub fn move_word_left(&mut self, extend: bool) {
        self.place(self.word_start_before(self.cursor), extend);
    }

    /// Move the caret to the end of the word after it.
    pub fn move_word_right(&mut self, extend: bool) {
        self.place(self.word_end_after(self.cursor), extend);
    }

    /// Move the caret to the start of its line (the start of the text in single-line mode).
    pub fn move_line_start(&mut self, extend: bool) {
        self.place(self.line_start(self.cursor), extend);
    }

    /// Move the caret to the end of its line (the end of the text in single-line mode).
    pub fn move_line_end(&mut self, extend: bool) {
        self.place(self.line_end(self.cursor), extend);
    }

    /// Select all text, with the caret at the end.
    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    /// Move the caret to `offset`, clamped to the text and moved back to a character boundary. With `extend`
    /// the selection runs from where it started (or from the old caret) to the new caret.
    pub fn set_cursor(&mut self, offset: usize, extend: bool) {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        self.place(offset, extend);
    }

    fn place(&mut self, offset: usize, extend: bool) {
        if extend {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = offset;
    }

    fn normalize(&self, text: &str) -> String {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        text.chars()
            .filter_map(|c| match c {
                '\n' if self.multiline => Some('\n'),
                '\n' => Some(' '),
                '\t' => Some('\t'),
                c if c.is_control() => None,
                c => Some(c),
            })
            .collect()
    }

    /// Delete the selection if there is one; returns whether it did.
    fn delete_selection(&mut self) -> bool {
        match self.selection() {
            Some(range) => {
                self.delete_range(range);
                true
            }
            None => false,
        }
    }

    fn delete_range(&mut self, range: Range<usize>) {
        self.text.replace_range(range.clone(), "");
        self.cursor = range.start;
        self.anchor = None;
    }

    fn prev_boundary(&self, offset: usize) -> usize {
        self.text[..offset]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.text[offset..]
            .chars()
            .next()
            .map_or(offset, |c| offset + c.len_utf8())
    }

    /// Skip whitespace leftwards, then the run of same-class characters before it.
    fn word_start_before(&self, offset: usize) -> usize {
        let mut chars = self.text[..offset].char_indices().rev().peekable();
        while chars
            .next_if(|(_, c)| char_class(*c) == CharClass::Space)
            .is_some()
        {}
        let Some(&(_, first)) = chars.peek() else {
            return 0;
        };
        let class = char_class(first);
        let mut start = offset;
        while let Some((index, _)) = chars.next_if(|(_, c)| char_class(*c) == class) {
            start = index;
        }
        start
    }

    /// Skip whitespace rightwards, then the run of same-class characters after it.
    fn word_end_after(&self, offset: usize) -> usize {
        let rest = &self.text[offset..];
        let mut chars = rest.char_indices().peekable();
        while chars
            .next_if(|(_, c)| char_class(*c) == CharClass::Space)
            .is_some()
        {}
        let Some(&(_, first)) = chars.peek() else {
            return self.text.len();
        };
        let class = char_class(first);
        let mut end = offset;
        while let Some((index, c)) = chars.next_if(|(_, c)| char_class(*c) == class) {
            end = offset + index + c.len_utf8();
        }
        end
    }

    fn line_start(&self, offset: usize) -> usize {
        self.text[..offset].rfind('\n').map_or(0, |index| index + 1)
    }

    fn line_end(&self, offset: usize) -> usize {
        self.text[offset..]
            .find('\n')
            .map_or(self.text.len(), |index| offset + index)
    }
}

/// What the user asked of a [`PromptInput`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptEvent {
    /// Enter was pressed with non-blank text (or any text, with [`PromptInput::submit_blank`]), given here as
    /// typed. The input keeps the text; call [`PromptInput::clear`] once it has been used.
    Submit(String),
    /// Escape was pressed.
    Cancel,
}

/// An edit or action a keystroke maps to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PromptCommand {
    Insert(String),
    Newline,
    Submit,
    Cancel,
    Backspace,
    DeleteWordBack,
    DeleteToLineStart,
    Delete,
    Left { extend: bool },
    Right { extend: bool },
    WordLeft { extend: bool },
    WordRight { extend: bool },
    LineStart { extend: bool },
    LineEnd { extend: bool },
    SelectAll,
    Copy,
    Cut,
    Paste,
}

/// The command for `keystroke`, or `None` to let the key go on to the app's bindings.
fn command_for(keystroke: &Keystroke, multiline: bool) -> Option<PromptCommand> {
    use PromptCommand::*;
    let m = keystroke.modifiers;
    let extend = m.shift;
    let plain = !m.platform && !m.control && !m.alt;
    let command = match keystroke.key.as_str() {
        "enter" if multiline && m.shift && !m.platform && !m.control => Newline,
        "enter" if !m.control && !m.alt => Submit,
        "escape" if plain && !m.shift => Cancel,
        "backspace" if m.platform => DeleteToLineStart,
        "backspace" if m.alt => DeleteWordBack,
        "backspace" if !m.control => Backspace,
        "delete" if plain => Delete,
        "left" if m.platform => LineStart { extend },
        "left" if m.alt => WordLeft { extend },
        "left" if !m.control => Left { extend },
        "right" if m.platform => LineEnd { extend },
        "right" if m.alt => WordRight { extend },
        "right" if !m.control => Right { extend },
        "home" if !m.platform && !m.control => LineStart { extend },
        "end" if !m.platform && !m.control => LineEnd { extend },
        "a" if m.control && !m.platform && !m.alt => LineStart { extend },
        "e" if m.control && !m.platform && !m.alt => LineEnd { extend },
        "a" if m.platform && !m.control && !m.alt && !m.shift => SelectAll,
        "c" if m.platform && !m.control && !m.alt && !m.shift => Copy,
        "x" if m.platform && !m.control && !m.alt && !m.shift => Cut,
        "v" if m.platform && !m.control && !m.alt && !m.shift => Paste,
        _ if m.platform || m.control || m.function => return None,
        _ => {
            let typed = keystroke.key_char.as_ref()?;
            if typed.is_empty() || typed.chars().any(char::is_control) {
                return None;
            }
            Insert(typed.clone())
        }
    };
    Some(command)
}

/// A text input view. Create it with `cx.new(|cx| PromptInput::new(cx).placeholder("..."))`, subscribe to its
/// [`PromptEvent`]s, and focus it with [`Focusable::focus_handle`].
///
/// Keys are handled in the view's key-down listener, so the app's own key bindings (such as `cmd-w`) still
/// win over it; keys it does not use go on to the app. The caret is drawn while the input is focused.
pub struct PromptInput {
    focus_handle: FocusHandle,
    buffer: PromptBuffer,
    placeholder: SharedString,
    /// Whether a mouse drag that started in the text is extending the selection.
    selecting: bool,
    /// Whether Enter on blank text still emits [`PromptEvent::Submit`] (with an empty string).
    submit_blank: bool,
    /// Whether the input draws no outline, fill or side padding, for an input inside its own frame.
    borderless: bool,
    /// Whether the input uses body-sized text and compact vertical padding.
    compact: bool,
}

impl EventEmitter<PromptEvent> for PromptInput {}

impl Focusable for PromptInput {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl PromptInput {
    /// An empty single-line input with no placeholder.
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            buffer: PromptBuffer::new(),
            placeholder: SharedString::default(),
            selecting: false,
            submit_blank: false,
            borderless: false,
            compact: false,
        }
    }

    /// Set the text shown in the empty input.
    pub fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    /// Let Shift+Enter insert a line break (Enter still submits).
    pub fn multiline(mut self, multiline: bool) -> Self {
        self.buffer.set_multiline(multiline);
        self
    }

    /// Let Enter on blank text emit [`PromptEvent::Submit`] with an empty string, for an input whose Enter also
    /// confirms what was chosen beside it.
    pub fn submit_blank(mut self, submit_blank: bool) -> Self {
        self.submit_blank = submit_blank;
        self
    }

    /// Draw no outline, fill or side padding: for an input that sits in a frame of its own, such as the command
    /// palette's query row.
    pub fn borderless(mut self, borderless: bool) -> Self {
        self.borderless = borderless;
        self
    }

    /// Use body-sized text (`TEXT_MD`) and tighter vertical padding for inline toolbars and composers.
    pub fn compact(mut self, compact: bool) -> Self {
        self.compact = compact;
        self
    }

    /// Change the placeholder.
    pub fn set_placeholder(
        &mut self,
        placeholder: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.placeholder = placeholder.into();
        cx.notify();
    }

    /// The current text.
    pub fn value(&self) -> &str {
        self.buffer.text()
    }

    /// The buffer, for the caret and selection.
    pub fn buffer(&self) -> &PromptBuffer {
        &self.buffer
    }

    /// Replace the text, with the caret at its end.
    pub fn set_value(&mut self, value: &str, cx: &mut Context<Self>) {
        self.buffer.set_text(value);
        cx.notify();
    }

    /// Remove all text.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.buffer.clear();
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(command) = command_for(&event.keystroke, self.buffer.is_multiline()) else {
            return;
        };
        if !matches!(command, PromptCommand::Copy) {
            cx.stop_propagation();
        }
        match command {
            PromptCommand::Submit => {
                if self.submit_blank || !self.buffer.text().trim().is_empty() {
                    cx.emit(PromptEvent::Submit(self.buffer.text().to_owned()));
                }
                return;
            }
            PromptCommand::Cancel => {
                cx.emit(PromptEvent::Cancel);
                return;
            }
            PromptCommand::Copy => {
                if let Some(text) = self.buffer.selected_text() {
                    cx.stop_propagation();
                    cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
                }
                return;
            }
            PromptCommand::Cut => {
                if let Some(text) = self.buffer.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text.to_owned()));
                    self.buffer.backspace();
                }
            }
            PromptCommand::Paste => {
                if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                    self.buffer.insert(&text);
                }
            }
            PromptCommand::Insert(text) => self.buffer.insert(&text),
            PromptCommand::Newline => self.buffer.insert("\n"),
            PromptCommand::Backspace => self.buffer.backspace(),
            PromptCommand::DeleteWordBack => self.buffer.delete_word_back(),
            PromptCommand::DeleteToLineStart => self.buffer.delete_to_line_start(),
            PromptCommand::Delete => self.buffer.delete(),
            PromptCommand::Left { extend } => self.buffer.move_left(extend),
            PromptCommand::Right { extend } => self.buffer.move_right(extend),
            PromptCommand::WordLeft { extend } => self.buffer.move_word_left(extend),
            PromptCommand::WordRight { extend } => self.buffer.move_word_right(extend),
            PromptCommand::LineStart { extend } => self.buffer.move_line_start(extend),
            PromptCommand::LineEnd { extend } => self.buffer.move_line_end(extend),
            PromptCommand::SelectAll => self.buffer.select_all(),
        }
        cx.notify();
    }
}

impl Render for PromptInput {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let focused = self.focus_handle.is_focused(window);
        let caret_color = theme.accent;
        let line_height = window.line_height();

        let body = if self.buffer.is_empty() {
            div()
                .relative()
                .w_full()
                .text_color(theme.text_subtle)
                .child(self.placeholder.clone())
                .when(focused, |body| {
                    body.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .w(px(2.0))
                            .h(line_height)
                            .bg(caret_color),
                    )
                })
        } else {
            let selection = self.buffer.selection().map(|range| {
                (
                    range,
                    HighlightStyle {
                        background_color: Some(theme.selection),
                        ..HighlightStyle::default()
                    },
                )
            });
            let text = StyledText::new(self.buffer.text().to_owned()).with_highlights(selection);
            let layout = text.layout().clone();
            let cursor = self.buffer.cursor();
            let view = cx.entity().downgrade();
            let overlay = canvas(
                |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                move |_, hitbox, window, _| {
                    if focused && let Some(origin) = layout.position_for_index(cursor) {
                        window.paint_quad(fill(
                            Bounds::new(origin, size(px(2.0), layout.line_height())),
                            caret_color,
                        ));
                    }
                    let down_layout = layout.clone();
                    let down_view = view.clone();
                    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble
                            || event.button != MouseButton::Left
                            || !hitbox.is_hovered(window)
                        {
                            return;
                        }
                        let (Ok(index) | Err(index)) =
                            down_layout.index_for_position(event.position);
                        let extend = event.modifiers.shift;
                        down_view
                            .update(cx, |this, cx| {
                                this.buffer.set_cursor(index, extend);
                                this.selecting = true;
                                cx.notify();
                            })
                            .ok();
                    });
                    let move_view = view.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase != DispatchPhase::Bubble
                            || event.pressed_button != Some(MouseButton::Left)
                        {
                            return;
                        }
                        let (Ok(index) | Err(index)) = layout.index_for_position(event.position);
                        move_view
                            .update(cx, |this, cx| {
                                if this.selecting {
                                    this.buffer.set_cursor(index, true);
                                    cx.notify();
                                }
                            })
                            .ok();
                    });
                    window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                        if phase == DispatchPhase::Bubble {
                            view.update(cx, |this, _| this.selecting = false).ok();
                        }
                    });
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full();
            div()
                .relative()
                .w_full()
                .text_color(theme.text)
                .child(text)
                .child(overlay)
        };

        div()
            .id("prompt-input")
            .key_context("PromptInput")
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, window, cx| {
                    this.focus_handle.focus(window, cx);
                }),
            )
            .flex()
            .items_start()
            .w_full()
            .min_h(if self.compact { px(30.0) } else { px(40.0) })
            .when(!self.borderless, |input| {
                input
                    .px(Theme::SPACE_3)
                    .rounded(Theme::RADIUS_MD)
                    .border_1()
                    .border_color(theme.outline(focused))
                    .bg(theme.raised)
            })
            .py(if self.compact {
                px(5.0)
            } else {
                Theme::SPACE_2 + px(2.0)
            })
            .text_size(if self.compact {
                Theme::TEXT_MD
            } else {
                Theme::TEXT_LG
            })
            .cursor_text()
            .child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn buffer(text: &str) -> PromptBuffer {
        let mut buffer = PromptBuffer::new();
        buffer.set_text(text);
        buffer
    }

    #[test]
    fn insert_at_caret_and_replace_selection() {
        let mut b = buffer("helo");
        b.set_cursor(3, false);
        b.insert("l");
        assert_eq!((b.text(), b.cursor()), ("hello", 4));
        b.select_all();
        b.insert("hi");
        assert_eq!((b.text(), b.cursor(), b.selection()), ("hi", 2, None));
    }

    #[test]
    fn single_line_turns_breaks_into_spaces_and_drops_controls() {
        let mut b = PromptBuffer::new();
        b.insert("a\nb\r\nc\u{7}d");
        assert_eq!(b.text(), "a b cd");
        let mut m = PromptBuffer::with_multiline(true);
        m.insert("a\r\nb\rc");
        assert_eq!(m.text(), "a\nb\nc");
        m.set_multiline(false);
        assert_eq!(m.text(), "a b c");
    }

    #[test]
    fn backspace_and_delete_respect_utf8() {
        let mut b = buffer("aé🦀b");
        b.backspace();
        assert_eq!(b.text(), "aé🦀");
        b.backspace();
        assert_eq!((b.text(), b.cursor()), ("aé", 3));
        b.move_left(false);
        assert_eq!(b.cursor(), 1);
        b.delete();
        assert_eq!((b.text(), b.cursor()), ("a", 1));
        b.delete();
        assert_eq!(b.text(), "a");
        b.move_line_start(false);
        b.backspace();
        assert_eq!((b.text(), b.cursor()), ("a", 0));
    }

    #[test]
    fn backspace_deletes_the_selection() {
        let mut b = buffer("hello world");
        b.set_cursor(5, false);
        b.move_word_right(true);
        assert_eq!(b.selected_text(), Some(" world"));
        b.backspace();
        assert_eq!(b.text(), "hello");
    }

    #[test]
    fn delete_word_back_skips_space_then_one_word_class() {
        let mut b = buffer("fix  checkout-retry");
        b.delete_word_back();
        assert_eq!(b.text(), "fix  checkout-");
        b.delete_word_back();
        assert_eq!(b.text(), "fix  checkout");
        b.delete_word_back();
        assert_eq!(b.text(), "fix  ");
        b.delete_word_back();
        assert_eq!(b.text(), "");
        b.delete_word_back();
        assert_eq!((b.text(), b.cursor()), ("", 0));
    }

    #[test]
    fn word_moves_cross_multibyte_words() {
        let mut b = buffer("héllo wörld");
        b.move_word_left(false);
        assert_eq!(b.cursor(), "héllo ".len());
        b.move_word_left(false);
        assert_eq!(b.cursor(), 0);
        b.move_word_right(false);
        assert_eq!(b.cursor(), "héllo".len());
        b.move_word_right(false);
        assert_eq!(b.cursor(), b.text().len());
    }

    #[test]
    fn left_right_collapse_or_extend_the_selection() {
        let mut b = buffer("abcd");
        b.move_left(true);
        b.move_left(true);
        assert_eq!(b.selection(), Some(2..4));
        b.move_right(true);
        assert_eq!(b.selection(), Some(3..4));
        b.move_left(false);
        assert_eq!((b.cursor(), b.selection()), (3, None));
        b.select_all();
        b.move_left(false);
        assert_eq!(b.cursor(), 0);
        b.select_all();
        b.move_right(false);
        assert_eq!(b.cursor(), 4);
        b.move_right(false);
        assert_eq!(b.cursor(), 4);
    }

    #[test]
    fn home_and_end_work_per_line() {
        let mut b = PromptBuffer::with_multiline(true);
        b.insert("one\ntwo\nthree");
        b.set_cursor(5, false);
        b.move_line_start(false);
        assert_eq!(b.cursor(), 4);
        b.move_line_end(true);
        assert_eq!(b.selected_text(), Some("two"));
        b.set_cursor(6, false);
        b.delete_to_line_start();
        assert_eq!(b.text(), "one\no\nthree");
    }

    #[test]
    fn set_cursor_clamps_and_snaps_to_char_boundaries() {
        let mut b = buffer("a🦀");
        b.set_cursor(3, false);
        assert_eq!(b.cursor(), 1);
        b.set_cursor(99, false);
        assert_eq!(b.cursor(), 5);
        b.set_cursor(0, true);
        assert_eq!(b.selected_text(), Some("a🦀"));
    }

    #[test]
    fn clear_and_set_text_reset_the_caret() {
        let mut b = buffer("abc");
        b.select_all();
        b.clear();
        assert_eq!((b.text(), b.cursor(), b.selection()), ("", 0, None));
        assert!(b.is_empty());
    }

    fn key(key: &str, key_char: Option<&str>, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.to_owned(),
            key_char: key_char.map(str::to_owned),
        }
    }

    fn cmd() -> Modifiers {
        Modifiers {
            platform: true,
            ..Modifiers::default()
        }
    }

    #[test]
    fn keys_map_to_commands() {
        let none = Modifiers::default();
        let shift = Modifiers {
            shift: true,
            ..none
        };
        let alt = Modifiers { alt: true, ..none };
        use PromptCommand::*;
        assert_eq!(command_for(&key("enter", None, none), false), Some(Submit));
        assert_eq!(command_for(&key("enter", None, shift), false), Some(Submit));
        assert_eq!(command_for(&key("enter", None, shift), true), Some(Newline));
        assert_eq!(command_for(&key("escape", None, none), false), Some(Cancel));
        assert_eq!(
            command_for(&key("backspace", None, alt), false),
            Some(DeleteWordBack)
        );
        assert_eq!(
            command_for(&key("backspace", None, cmd()), false),
            Some(DeleteToLineStart)
        );
        assert_eq!(
            command_for(&key("left", None, cmd()), false),
            Some(LineStart { extend: false })
        );
        assert_eq!(
            command_for(
                &key(
                    "right",
                    None,
                    Modifiers {
                        shift: true,
                        ..cmd()
                    }
                ),
                false
            ),
            Some(LineEnd { extend: true })
        );
        assert_eq!(
            command_for(&key("left", None, alt), false),
            Some(WordLeft { extend: false })
        );
        assert_eq!(command_for(&key("a", None, cmd()), false), Some(SelectAll));
        assert_eq!(command_for(&key("v", None, cmd()), false), Some(Paste));
        assert_eq!(command_for(&key("c", None, cmd()), false), Some(Copy));
        assert_eq!(
            command_for(&key("e", Some("é"), alt), false),
            Some(Insert("é".to_owned()))
        );
        assert_eq!(
            command_for(&key("space", Some(" "), none), false),
            Some(Insert(" ".to_owned()))
        );
    }

    #[test]
    fn app_chords_and_unprintable_keys_pass_through() {
        let ctrl = Modifiers {
            control: true,
            ..Modifiers::default()
        };
        assert_eq!(command_for(&key("d", None, cmd()), false), None);
        assert_eq!(command_for(&key("w", Some("w"), cmd()), false), None);
        assert_eq!(command_for(&key("c", Some("c"), ctrl), false), None);
        assert_eq!(
            command_for(&key("tab", Some("\t"), Modifiers::default()), false),
            None
        );
        assert_eq!(
            command_for(&key("up", None, Modifiers::default()), false),
            None
        );
    }
}
