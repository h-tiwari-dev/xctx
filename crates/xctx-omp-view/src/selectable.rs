//! Mouse-selectable text across transcript rows, markdown blocks, code blocks, diffs, and tool cards.
//!
//! GPUI's `StyledText` renders styled runs and exposes a [`TextLayout`] that maps pixel coordinates back to UTF-8
//! byte indices. This module builds a lightweight, shared [`TextSelection`] coordinator on top of `StyledText` so
//! users can drag-select within or across blocks, double-click to select a word/path/hash, triple-click to select
//! a line/paragraph, click markdown links, right-click to copy, and press `Cmd+C` anywhere in the pane.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AnyElement, App, DispatchPhase, EntityId, HighlightStyle, HitboxBehavior, Hsla, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, SharedString, StyledText, Window, canvas, div,
    prelude::*,
};
use xctx_ui::Theme;

use crate::changes::CopyCallback;

/// Callback invoked when the user clicks a markdown link (`url` may be an `http(s)://` URL or a local path).
pub type LinkHandler = Rc<dyn Fn(&str, &mut Window, &mut App)>;

/// Ordered identifier for one selectable text segment. Ordering matches top-to-bottom reading order so
/// cross-block and cross-row drags highlight every intervening segment cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentId {
    /// Transcript row index (`usize::MAX` for non-transcript drawers such as Changes).
    pub row: usize,
    /// Sequential segment index within the row.
    pub block: usize,
}

impl SegmentId {
    /// A segment at transcript row `row`, block `block`.
    pub const fn new(row: usize, block: usize) -> Self {
        Self { row, block }
    }
}

/// One endpoint of a text selection: a segment and a UTF-8 byte offset inside its plain text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPoint {
    /// Which segment.
    pub seg: SegmentId,
    /// Byte offset into that segment's text.
    pub offset: usize,
}

#[derive(Default)]
struct SelectionState {
    anchor: Option<TextPoint>,
    head: Option<TextPoint>,
    dragging: bool,
    pending_link: Option<String>,
    /// Plain text of rendered segments, kept so cross-block `selected_text()` can join them in order.
    segments: BTreeMap<SegmentId, String>,
}

/// Shared selection handle held by [`crate::OmpView`] and passed into transcript rows and drawers.
#[derive(Clone, Default)]
pub struct TextSelection {
    inner: Rc<RefCell<SelectionState>>,
}

impl TextSelection {
    /// Empty selection state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the current plain text of segment `seg` so copying across segments has the latest text.
    pub fn register_segment(&self, seg: SegmentId, text: &str) {
        let mut state = self.inner.borrow_mut();
        match state.segments.get_mut(&seg) {
            Some(existing) if existing == text => {}
            Some(existing) => {
                existing.clear();
                existing.push_str(text);
            }
            None => {
                state.segments.insert(seg, text.to_owned());
            }
        }
    }

    /// Drop cached segment text and clear any active selection (e.g. on session switch).
    pub fn reset(&self) {
        let mut state = self.inner.borrow_mut();
        state.anchor = None;
        state.head = None;
        state.dragging = false;
        state.pending_link = None;
        state.segments.clear();
    }

    /// Clear the active selection while keeping registered segments.
    pub fn clear(&self) {
        let mut state = self.inner.borrow_mut();
        state.anchor = None;
        state.head = None;
        state.dragging = false;
        state.pending_link = None;
    }

    /// Programmatically select `range` inside `seg` (useful for tests and double/triple-click helpers).
    pub fn set_range(&self, seg: SegmentId, range: Range<usize>) {
        let mut state = self.inner.borrow_mut();
        state.anchor = Some(TextPoint {
            seg,
            offset: range.start,
        });
        state.head = Some(TextPoint {
            seg,
            offset: range.end,
        });
        state.dragging = false;
        state.pending_link = None;
    }

    /// Whether a non-empty range is currently selected.
    pub fn has_selection(&self) -> bool {
        let state = self.inner.borrow();
        matches!((state.anchor, state.head), (Some(a), Some(b)) if a != b)
    }

    /// Ordered `(start, end)` endpoints of the active selection, if non-empty.
    fn ordered(&self) -> Option<(TextPoint, TextPoint)> {
        let state = self.inner.borrow();
        let (a, b) = (state.anchor?, state.head?);
        if a == b {
            return None;
        }
        Some(if a <= b { (a, b) } else { (b, a) })
    }

    /// Byte range selected inside `seg` (clamped to `text_len`), if any.
    pub fn range_for(&self, seg: SegmentId, text_len: usize) -> Option<Range<usize>> {
        let (start, end) = self.ordered()?;
        if seg < start.seg || seg > end.seg {
            return None;
        }
        let lo = if seg == start.seg {
            start.offset.min(text_len)
        } else {
            0
        };
        let hi = if seg == end.seg {
            end.offset.min(text_len)
        } else {
            text_len
        };
        (lo < hi).then_some(lo..hi)
    }

    /// Extract the selected text across all spanned segments, joined by newlines.
    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.ordered()?;
        let state = self.inner.borrow();
        let mut parts: Vec<&str> = Vec::new();
        for (&seg, text) in state.segments.range(start.seg..=end.seg) {
            let lo = if seg == start.seg {
                clamp_char_boundary(text, start.offset)
            } else {
                0
            };
            let hi = if seg == end.seg {
                clamp_char_boundary(text, end.offset)
            } else {
                text.len()
            };
            if lo < hi {
                parts.push(&text[lo..hi]);
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    }
}

/// Snap `offset` to the nearest valid UTF-8 character boundary `<= offset` within `text`.
pub fn clamp_char_boundary(text: &str, offset: usize) -> usize {
    let mut i = offset.min(text.len());
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Token character test for double-click selection: keeps words, identifiers, file paths, flags, and hashes whole.
fn is_token_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '~' | '@')
}

/// Byte range of the word/path token containing `offset` (or the whitespace/punctuation run if not on a token).
pub fn word_bounds(text: &str, offset: usize) -> Range<usize> {
    if text.is_empty() {
        return 0..0;
    }
    let idx = clamp_char_boundary(text, offset);
    let probe = if idx == text.len() {
        text.char_indices().next_back().map_or(0, |(i, _)| i)
    } else {
        idx
    };
    let Some(ch) = text[probe..].chars().next() else {
        return 0..0;
    };
    let cls = |c: char| -> u8 {
        if is_token_char(c) {
            0
        } else if c.is_whitespace() {
            1
        } else {
            2
        }
    };
    let target = cls(ch);
    let mut start = probe;
    for (i, c) in text[..probe].char_indices().rev() {
        if cls(c) != target {
            break;
        }
        start = i;
    }
    let mut end = probe + ch.len_utf8();
    for (i, c) in text[end..].char_indices() {
        if cls(c) != target {
            break;
        }
        end = probe + ch.len_utf8() + i + c.len_utf8();
    }
    start..end
}

/// Byte range of the single line containing `offset` (without its trailing `\n`).
pub fn line_bounds(text: &str, offset: usize) -> Range<usize> {
    if text.is_empty() {
        return 0..0;
    }
    let idx = clamp_char_boundary(text, offset);
    let start = text[..idx].rfind('\n').map_or(0, |i| i + 1);
    let end = text[idx..].find('\n').map_or(text.len(), |i| idx + i);
    start..end
}

/// Find non-overlapping case-insensitive occurrences of `query` inside `text`, returning valid UTF-8 byte ranges.
pub fn find_matches(text: &str, query: &str) -> Vec<Range<usize>> {
    let query = query.trim();
    if query.is_empty() || text.is_empty() {
        return Vec::new();
    }
    let needle: Vec<char> = query.chars().flat_map(char::to_lowercase).collect();
    if needle.is_empty() {
        return Vec::new();
    }
    // Map each lowercased character back to its original `(start_byte, end_byte)` in `text`.
    let mut lower_chars: Vec<(char, usize, usize)> = Vec::with_capacity(text.len());
    for (byte_start, ch) in text.char_indices() {
        let byte_end = byte_start + ch.len_utf8();
        for lc in ch.to_lowercase() {
            lower_chars.push((lc, byte_start, byte_end));
        }
    }
    let mut out = Vec::new();
    let n = needle.len();
    let mut i = 0;
    while i + n <= lower_chars.len() {
        if lower_chars[i..i + n]
            .iter()
            .zip(&needle)
            .all(|((c, _, _), t)| c == t)
        {
            let start = lower_chars[i].1;
            let end = lower_chars[i + n - 1].2;
            if start < end && text.is_char_boundary(start) && text.is_char_boundary(end) {
                out.push(start..end);
            }
            i += n;
        } else {
            i += 1;
        }
    }
    out
}

/// Merge base inline highlights, search match highlights, and an active selection range into strictly sorted,
/// non-overlapping UTF-8 character-boundary highlight runs suitable for GPUI's `StyledText::with_highlights`.
pub fn merge_highlights(
    text: &str,
    base: &[(Range<usize>, HighlightStyle)],
    search: &[Range<usize>],
    selection: Option<Range<usize>>,
    search_bg: Hsla,
    selection_bg: Hsla,
) -> Vec<(Range<usize>, HighlightStyle)> {
    let len = text.len();
    if len == 0 {
        return Vec::new();
    }
    let mut cuts: Vec<usize> = Vec::with_capacity(base.len() * 2 + search.len() * 2 + 4);
    cuts.push(0);
    cuts.push(len);
    for (r, _) in base {
        cuts.push(clamp_char_boundary(text, r.start));
        cuts.push(clamp_char_boundary(text, r.end));
    }
    for r in search {
        cuts.push(clamp_char_boundary(text, r.start));
        cuts.push(clamp_char_boundary(text, r.end));
    }
    if let Some(sel) = &selection {
        cuts.push(clamp_char_boundary(text, sel.start));
        cuts.push(clamp_char_boundary(text, sel.end));
    }
    cuts.sort_unstable();
    cuts.dedup();

    let mut merged: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if a >= b {
            continue;
        }
        let mut style = base
            .iter()
            .find(|(r, _)| r.start <= a && r.end >= b)
            .map(|(_, s)| *s)
            .unwrap_or_default();
        let in_sel = selection
            .as_ref()
            .is_some_and(|sel| sel.start <= a && sel.end >= b);
        let in_search = !in_sel && search.iter().any(|r| r.start <= a && r.end >= b);
        if in_sel {
            style.background_color = Some(selection_bg);
        } else if in_search {
            style.background_color = Some(search_bg);
        }
        if style == HighlightStyle::default() {
            continue;
        }
        match merged.last_mut() {
            Some((prev_range, prev_style)) if *prev_style == style && prev_range.end == a => {
                prev_range.end = b;
            }
            _ => merged.push((a..b, style)),
        }
    }
    merged
}

/// Optional interactive features threaded into selectable text rendering.
#[derive(Clone)]
pub struct SelectContext {
    /// Shared selection state.
    pub selection: TextSelection,
    /// Entity to notify when selection changes.
    pub view: EntityId,
    /// Active case-insensitive search query (empty when closed).
    pub search_query: SharedString,
    /// Called on right-click copy of an active selection.
    pub on_copy: Option<CopyCallback>,
    /// Called when a markdown link is clicked without dragging.
    pub on_link: Option<LinkHandler>,
}

/// Render selectable styled text with optional inline highlights, font-family overrides (e.g. inline code),
/// clickable links, search highlighting, and mouse-drag selection.
pub fn selectable_styled(
    seg: SegmentId,
    text: String,
    base_highlights: Vec<(Range<usize>, HighlightStyle)>,
    font_overrides: Vec<(Range<usize>, SharedString)>,
    links: Vec<(Range<usize>, String)>,
    ctx: &SelectContext,
    theme: &Theme,
) -> AnyElement {
    ctx.selection.register_segment(seg, &text);
    let sel_range = ctx
        .selection
        .range_for(seg, text.len())
        .map(|r| clamp_char_boundary(&text, r.start)..clamp_char_boundary(&text, r.end));
    let search_ranges = if ctx.search_query.trim().is_empty() {
        Vec::new()
    } else {
        find_matches(&text, &ctx.search_query)
    };
    let search_bg = theme.warning.alpha(0.32);
    let selection_bg = theme.selection;
    let merged = merge_highlights(
        &text,
        &base_highlights,
        &search_ranges,
        sel_range,
        search_bg,
        selection_bg,
    );

    let mut styled = StyledText::new(text.clone());
    if !merged.is_empty() {
        styled = styled.with_highlights(merged);
    }
    if !font_overrides.is_empty() {
        styled = styled.with_font_family_overrides(font_overrides);
    }
    let layout = styled.layout().clone();

    let sel_down = ctx.selection.clone();
    let sel_move = ctx.selection.clone();
    let sel_up = ctx.selection.clone();
    let sel_right = ctx.selection.clone();
    let view = ctx.view;
    let on_copy = ctx.on_copy.clone();
    let on_link = ctx.on_link.clone();
    let text_down = Rc::new(text);
    let links_down = Rc::new(links);

    let overlay = canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |bounds, hitbox, window, _| {
            let down_layout = layout.clone();
            let down_sel = sel_down.clone();
            let down_text = text_down.clone();
            let down_links = links_down.clone();
            let down_hitbox = hitbox.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !down_hitbox.is_hovered(window) {
                    return;
                }
                if event.button == MouseButton::Right {
                    if let Some(cb) = &on_copy
                        && let Some(copied) = sel_right.selected_text()
                    {
                        cb("selection".to_owned(), copied, cx);
                    }
                    return;
                }
                if event.button != MouseButton::Left {
                    return;
                }
                let (Ok(raw_ix) | Err(raw_ix)) = down_layout.index_for_position(event.position);
                let ix = clamp_char_boundary(&down_text, raw_ix);
                let mut state = down_sel.inner.borrow_mut();
                match event.click_count {
                    2 => {
                        let r = word_bounds(&down_text, ix);
                        state.anchor = Some(TextPoint {
                            seg,
                            offset: r.start,
                        });
                        state.head = Some(TextPoint { seg, offset: r.end });
                        state.dragging = false;
                        state.pending_link = None;
                    }
                    n if n >= 3 => {
                        let r = line_bounds(&down_text, ix);
                        state.anchor = Some(TextPoint {
                            seg,
                            offset: r.start,
                        });
                        state.head = Some(TextPoint { seg, offset: r.end });
                        state.dragging = false;
                        state.pending_link = None;
                    }
                    _ => {
                        let pt = TextPoint { seg, offset: ix };
                        if event.modifiers.shift && state.anchor.is_some() {
                            state.head = Some(pt);
                            state.pending_link = None;
                        } else {
                            state.anchor = Some(pt);
                            state.head = Some(pt);
                            state.pending_link = down_links
                                .iter()
                                .find(|(r, _)| r.start <= ix && ix < r.end)
                                .map(|(_, url)| url.clone());
                        }
                        state.dragging = true;
                    }
                }
                drop(state);
                cx.notify(view);
            });

            let move_layout = layout.clone();
            let move_sel = sel_move.clone();
            let move_text = text_down.clone();
            let move_hitbox = hitbox.clone();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || event.pressed_button != Some(MouseButton::Left)
                {
                    return;
                }
                if !move_sel.inner.borrow().dragging {
                    return;
                }
                let in_band = move_hitbox.is_hovered(window)
                    || (event.position.y >= bounds.top() && event.position.y <= bounds.bottom());
                if !in_band {
                    return;
                }
                let (Ok(raw_ix) | Err(raw_ix)) = move_layout.index_for_position(event.position);
                let ix = clamp_char_boundary(&move_text, raw_ix);
                let pt = TextPoint { seg, offset: ix };
                let changed = {
                    let mut state = move_sel.inner.borrow_mut();
                    if state.head != Some(pt) {
                        state.head = Some(pt);
                        if state.anchor != Some(pt) {
                            state.pending_link = None;
                        }
                        true
                    } else {
                        false
                    }
                };
                if changed {
                    cx.notify(view);
                }
            });

            let up_sel = sel_up.clone();
            let up_link = on_link.clone();
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                    return;
                }
                let link_to_open = {
                    let mut state = up_sel.inner.borrow_mut();
                    if !state.dragging {
                        None
                    } else {
                        state.dragging = false;
                        if state.anchor == state.head {
                            state.pending_link.take()
                        } else {
                            state.pending_link = None;
                            None
                        }
                    }
                };
                if let (Some(url), Some(handler)) = (link_to_open, &up_link) {
                    handler(&url, window, cx);
                }
            });
        },
    )
    .absolute()
    .inset_0();

    div()
        .relative()
        .w_full()
        .min_w_0()
        .cursor_text()
        .child(styled)
        .child(overlay)
        .into_any_element()
}

/// Convenience helper for plain (unstyled) selectable text.
pub fn selectable_plain(
    seg: SegmentId,
    text: String,
    ctx: &SelectContext,
    theme: &Theme,
) -> AnyElement {
    selectable_styled(seg, text, Vec::new(), Vec::new(), Vec::new(), ctx, theme)
}

#[cfg(test)]
mod tests {
    use gpui::FontWeight;

    use super::*;

    #[test]
    fn word_and_line_bounds_handle_paths_and_unicode() {
        let line = "Check harsh_resume_new.pdf at 51f8930 — done\nsecond line";
        let pdf_at = line.find("resume").unwrap();
        assert_eq!(&line[word_bounds(line, pdf_at)], "harsh_resume_new.pdf");

        let hash_at = line.find("51f").unwrap();
        assert_eq!(&line[word_bounds(line, hash_at)], "51f8930");

        let first_line = line_bounds(line, hash_at);
        assert_eq!(
            &line[first_line],
            "Check harsh_resume_new.pdf at 51f8930 — done"
        );
    }

    #[test]
    fn selection_spans_multiple_segments_in_reading_order() {
        let sel = TextSelection::new();
        let s0 = SegmentId::new(0, 0);
        let s1 = SegmentId::new(0, 1);
        let s2 = SegmentId::new(1, 0);
        sel.register_segment(s0, "First paragraph here.");
        sel.register_segment(s1, "Second paragraph.");
        sel.register_segment(s2, "Third row text.");

        // Drag backwards from row 1 to row 0: endpoints normalize cleanly.
        {
            let mut st = sel.inner.borrow_mut();
            st.anchor = Some(TextPoint { seg: s2, offset: 5 });
            st.head = Some(TextPoint { seg: s0, offset: 6 });
        }
        assert_eq!(sel.range_for(s0, 21), Some(6..21));
        assert_eq!(sel.range_for(s1, 17), Some(0..17));
        assert_eq!(sel.range_for(s2, 15), Some(0..5));
        assert_eq!(
            sel.selected_text().as_deref(),
            Some("paragraph here.\nSecond paragraph.\nThird")
        );
    }

    #[test]
    fn merge_highlights_splits_overlaps_without_unsorted_or_overlapping_runs() {
        let theme = Theme::dark();
        let text = "Hello bold world!";
        let base = vec![(
            6..10,
            HighlightStyle {
                font_weight: Some(FontWeight::BOLD),
                ..HighlightStyle::default()
            },
        )];
        let search = [0..5, 11..16];
        let merged = merge_highlights(
            text,
            &base,
            &search,
            Some(3..8),
            theme.warning,
            theme.selection,
        );
        // Must be strictly non-overlapping and sorted.
        for pair in merged.windows(2) {
            assert!(pair[0].0.end <= pair[1].0.start, "overlap: {merged:?}");
        }
        // Selection overrides search on 3..5 and adds selection bg on bold 6..8.
        let bold_selected = merged.iter().find(|(r, _)| *r == (6..8)).unwrap();
        assert_eq!(bold_selected.1.font_weight, Some(FontWeight::BOLD));
        assert_eq!(bold_selected.1.background_color, Some(theme.selection));
        let bold_rest = merged.iter().find(|(r, _)| *r == (8..10)).unwrap();
        assert_eq!(bold_selected.1.font_weight, bold_rest.1.font_weight);
        assert_eq!(bold_rest.1.background_color, None);
    }

    #[test]
    fn find_matches_is_case_insensitive_and_utf8_safe() {
        let text = "Resume — harsh_resume_new.pdf and RESUME";
        let hits = find_matches(text, "resume");
        assert_eq!(hits.len(), 3);
        assert_eq!(&text[hits[0].clone()], "Resume");
        assert_eq!(&text[hits[1].clone()], "resume");
        assert_eq!(&text[hits[2].clone()], "RESUME");
    }
}
