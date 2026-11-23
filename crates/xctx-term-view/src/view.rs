//! The GPUI view that draws a terminal session and feeds it input.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use futures::StreamExt;
use futures::channel::mpsc::UnboundedReceiver;
use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, DispatchPhase, Entity, EventEmitter,
    FocusHandle, Focusable, Hitbox, HitboxBehavior, Hsla, KeyDownEvent, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollDelta, ScrollEvent, ShapedLine,
    StrikethroughStyle, Subscription, Task, TextAlign, TextRun, TouchPhase, UnderlineStyle,
    WeakEntity, Window, canvas, div, fill, outline, point, prelude::*, px, size,
};
use xctx_term::input::{
    self, MouseButton as TermMouseButton, MouseEvent as TermMouseEvent, MouseEventKind,
};
use xctx_term::{
    CellFlags, CursorShape, GridSnapshot, MouseReporting, SelectionMode, Side, TermError,
    TerminalConfig, TerminalEvent, TerminalModes, TerminalSession, ViewportPoint,
};

use crate::grid::{
    CellMetrics, TextSegment, UnderlineKind, background_runs, scrollback_thumb, selection_ranges,
    text_segments,
};
use crate::keys::key_event_for_keystroke;
use crate::theme::{TerminalFont, TerminalTheme};

/// What a [`TerminalView`] tells its parent. Subscribe with `cx.subscribe`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalViewEvent {
    /// The program set the window title; an empty string when it reset the title.
    TitleChanged(String),
    /// The program rang the bell.
    Bell,
    /// The program exited, with its exit code; `None` when it was killed by a signal or the
    /// status could not be collected. The view stays open and keeps showing the last screen.
    Exited(Option<i32>),
    /// The view gained keyboard focus.
    Focused,
}

/// A left-button selection in progress.
#[derive(Debug, Clone, Copy)]
struct SelectionDrag {
    /// Where the button went down.
    anchor: ViewportPoint,
    /// Which half of the anchor cell.
    anchor_side: Side,
    /// Whether the selection exists yet. A plain click only starts one once the pointer
    /// moves, so clicking never leaves an empty selection behind.
    started: bool,
}

/// A button press reported to the program, whose motion and release are reported too.
#[derive(Debug, Clone, Copy)]
struct ReportedPress {
    button: TermMouseButton,
    last_point: ViewportPoint,
}

/// Width of the bar and underline cursors.
const CURSOR_LINE_WIDTH: Pixels = px(2.0);

/// Width of the scrollback indicator thumb and its gap from the right edge.
const THUMB_WIDTH: Pixels = px(4.0);
const THUMB_INSET: Pixels = px(2.0);
const THUMB_MIN_HEIGHT: Pixels = px(16.0);

/// A terminal pane: draws an [`xctx_term::TerminalSession`] and sends it keys, mouse, paste,
/// focus changes and resizes.
///
/// The PTY is resized to whatever whole number of cells fits the view's bounds each time they
/// change. Mouse input goes to the program when it asked for mouse reports (hold Shift to
/// select instead); otherwise dragging selects, double-click selects a word, triple-click a
/// line, and the wheel scrolls the scrollback. Cmd+C copies the selection and Cmd+V pastes;
/// other Command chords are left for the app's key bindings.
pub struct TerminalView {
    session: Arc<TerminalSession>,
    focus_handle: FocusHandle,
    font: TerminalFont,
    theme: TerminalTheme,
    /// Cleared when a wakeup is handled, so the session sends the next one.
    wakeup_pending: Arc<AtomicBool>,
    /// Cell size used for the last frame.
    metrics: Option<CellMetrics>,
    /// Bounds of the grid in the last frame.
    bounds: Option<Bounds<Pixels>>,
    /// The size last sent to the session.
    grid_size: (u16, u16),
    selection_drag: Option<SelectionDrag>,
    reported_press: Option<ReportedPress>,
    /// The cell of the last hover motion report, so each cell is reported once.
    last_hover_point: Option<ViewportPoint>,
    /// Fraction of a line scrolled but not applied yet.
    scroll_remainder: f32,
    _event_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<TerminalViewEvent> for TerminalView {}

impl Focusable for TerminalView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl TerminalView {
    /// Starts `config.program` on a new PTY and returns a view of it.
    ///
    /// The theme's default foreground and background are copied into `config` so programs
    /// that query them see the colours the view draws with. `config.cols` and `config.rows`
    /// are only the size until the view is first laid out.
    pub fn spawn(
        mut config: TerminalConfig,
        font: TerminalFont,
        theme: TerminalTheme,
        window: &mut Window,
        cx: &mut App,
    ) -> Result<Entity<Self>, TermError> {
        theme.apply_to_config(&mut config);
        let metrics = cell_metrics(&font, window);
        let (cell_width, cell_height) = metrics.cell_size_pixels();
        config.cell_width = cell_width;
        config.cell_height = cell_height;
        let grid_size = (config.cols.max(2), config.rows.max(1));

        let (sender, receiver) = futures::channel::mpsc::unbounded();
        let wakeup_pending = Arc::new(AtomicBool::new(false));
        let session_wakeup_pending = Arc::clone(&wakeup_pending);
        let session = TerminalSession::spawn(config, move |event| {
            // Coalesce wakeups: one is enough until the view has handled it.
            if matches!(event, TerminalEvent::Wakeup)
                && session_wakeup_pending.swap(true, Ordering::AcqRel)
            {
                return;
            }
            let _ = sender.unbounded_send(event);
        })?;

        Ok(cx.new(|cx| {
            Self::new(
                Arc::new(session),
                receiver,
                wakeup_pending,
                font,
                theme,
                grid_size,
                window,
                cx,
            )
        }))
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        session: Arc<TerminalSession>,
        mut events: UnboundedReceiver<TerminalEvent>,
        wakeup_pending: Arc<AtomicBool>,
        font: TerminalFont,
        theme: TerminalTheme,
        grid_size: (u16, u16),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus_handle = cx.focus_handle();
        let subscriptions = vec![
            cx.on_focus_in(&focus_handle, window, |view, _window, cx| {
                view.focus_changed(true, cx);
            }),
            cx.on_focus_out(&focus_handle, window, |view, _event, _window, cx| {
                view.focus_changed(false, cx);
            }),
        ];

        let event_task = cx.spawn(async move |view: WeakEntity<Self>, cx| {
            while let Some(event) = events.next().await {
                let mut batch = vec![event];
                while let Ok(event) = events.try_recv() {
                    batch.push(event);
                }
                let handled = view.update(cx, |view, cx| {
                    for event in batch {
                        view.handle_terminal_event(event, cx);
                    }
                });
                if handled.is_err() {
                    break;
                }
            }
        });

        Self {
            session,
            focus_handle,
            font,
            theme,
            wakeup_pending,
            metrics: None,
            bounds: None,
            grid_size,
            selection_drag: None,
            reported_press: None,
            last_hover_point: None,
            scroll_remainder: 0.0,
            _event_task: event_task,
            _subscriptions: subscriptions,
        }
    }

    /// The session this view draws, for its pid, title, exit status or to write to it.
    pub fn session(&self) -> &Arc<TerminalSession> {
        &self.session
    }

    /// Copies the selected text to the clipboard. Returns whether there was a selection to
    /// copy.
    pub fn copy_selection(&mut self, cx: &mut Context<Self>) -> bool {
        match self.session.selected_text() {
            Some(text) if !text.is_empty() => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
                true
            }
            _ => false,
        }
    }

    /// Pastes the clipboard's text into the program, bracketed when the program asked for
    /// bracketed paste. Does nothing when the clipboard holds no text.
    pub fn paste_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let modes = self.session.modes();
        self.write_input(&input::encode_paste(&text, modes.bracketed_paste), cx);
    }

    fn handle_terminal_event(&mut self, event: TerminalEvent, cx: &mut Context<Self>) {
        match event {
            TerminalEvent::Wakeup => {
                self.wakeup_pending.store(false, Ordering::Release);
                cx.notify();
            }
            TerminalEvent::TitleChanged(title) => {
                cx.emit(TerminalViewEvent::TitleChanged(title.unwrap_or_default()));
            }
            TerminalEvent::Bell => cx.emit(TerminalViewEvent::Bell),
            TerminalEvent::ClipboardStore(text) => {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
            TerminalEvent::Exited(status) => {
                cx.emit(TerminalViewEvent::Exited(status.and_then(|s| s.code())));
                cx.notify();
            }
        }
    }

    fn focus_changed(&mut self, focused: bool, cx: &mut Context<Self>) {
        if let Some(report) = input::encode_focus(focused, &self.session.modes()) {
            // A program that has exited no longer reads input; nothing to report then.
            let _ = self.session.write(&report);
        }
        if focused {
            cx.emit(TerminalViewEvent::Focused);
        }
        cx.notify();
    }

    /// Sends typed or pasted input, returning the viewport to the live screen first.
    fn write_input(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
        if self.session.display_offset() > 0 {
            self.session.scroll_to_bottom();
            cx.notify();
        }
        // Writing fails only once the program has gone; the keystroke is dropped then.
        let _ = self.session.write(bytes);
    }

    fn write_report(&self, bytes: Option<Vec<u8>>) {
        if let Some(bytes) = bytes {
            let _ = self.session.write(&bytes);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let modifiers = keystroke.modifiers;
        if modifiers.platform {
            if !modifiers.control && !modifiers.alt && !modifiers.shift {
                match keystroke.key.as_str() {
                    "c" => {
                        if self.copy_selection(cx) {
                            cx.stop_propagation();
                        }
                    }
                    "v" => {
                        self.paste_clipboard(cx);
                        cx.stop_propagation();
                    }
                    _ => {}
                }
            }
            return;
        }

        let Some(key_event) = key_event_for_keystroke(keystroke, event.is_held) else {
            return;
        };
        let Some(bytes) = input::encode_key(&key_event, &self.session.modes()) else {
            return;
        };
        cx.stop_propagation();
        self.write_input(&bytes, cx);
    }

    fn point_for(&self, position: Point<Pixels>) -> Option<(ViewportPoint, Side)> {
        let bounds = self.bounds?;
        let metrics = self.metrics?;
        let (cols, rows) = self.grid_size;
        Some(metrics.point_for_position(
            position,
            bounds.origin,
            usize::from(cols),
            usize::from(rows),
        ))
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        let Some((point, side)) = self.point_for(event.position) else {
            return;
        };
        let modes = self.session.modes();
        if reports_mouse(&modes, event.modifiers) {
            let Some(button) = term_mouse_button(event.button) else {
                return;
            };
            let report = TermMouseEvent {
                kind: MouseEventKind::Press,
                button: Some(button),
                point,
                modifiers: term_modifiers(event.modifiers),
            };
            self.write_report(input::encode_mouse(&report, &modes));
            self.reported_press = Some(ReportedPress {
                button,
                last_point: point,
            });
            return;
        }

        if event.button != gpui::MouseButton::Left {
            return;
        }
        let mode = match event.click_count {
            0 | 1 => None,
            2 => Some(SelectionMode::Word),
            _ => Some(SelectionMode::Line),
        };
        match mode {
            Some(mode) => {
                self.session.start_selection(mode, point, side);
                self.selection_drag = Some(SelectionDrag {
                    anchor: point,
                    anchor_side: side,
                    started: true,
                });
            }
            None if event.modifiers.shift && self.session.selected_text().is_some() => {
                self.session.update_selection(point, side);
                self.selection_drag = Some(SelectionDrag {
                    anchor: point,
                    anchor_side: side,
                    started: true,
                });
            }
            None => {
                self.session.clear_selection();
                self.selection_drag = Some(SelectionDrag {
                    anchor: point,
                    anchor_side: side,
                    started: false,
                });
            }
        }
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, hovered: bool, cx: &mut Context<Self>) {
        let Some((point, side)) = self.point_for(event.position) else {
            return;
        };
        if let Some(press) = self.reported_press.as_mut() {
            if press.last_point != point {
                press.last_point = point;
                let report = TermMouseEvent {
                    kind: MouseEventKind::Motion,
                    button: Some(press.button),
                    point,
                    modifiers: term_modifiers(event.modifiers),
                };
                self.write_report(input::encode_mouse(&report, &self.session.modes()));
            }
            return;
        }

        if let Some(drag) = self.selection_drag.as_mut() {
            if event.pressed_button != Some(gpui::MouseButton::Left) {
                return;
            }
            if !drag.started {
                if point == drag.anchor && side == drag.anchor_side {
                    return;
                }
                self.session
                    .start_selection(SelectionMode::Cell, drag.anchor, drag.anchor_side);
                drag.started = true;
            }
            // Dragging past the top or bottom edge scrolls the history under the selection.
            if let Some(bounds) = self.bounds {
                if event.position.y < bounds.top() {
                    self.session.scroll_lines(1);
                } else if event.position.y > bounds.bottom() {
                    self.session.scroll_lines(-1);
                }
            }
            self.session.update_selection(point, side);
            cx.notify();
            return;
        }

        // Every pane sees every move in the window; only the hovered one reports hovering.
        if !hovered || event.modifiers.shift || self.last_hover_point == Some(point) {
            return;
        }
        let modes = self.session.modes();
        if modes.mouse_reporting != MouseReporting::Motion {
            return;
        }
        self.last_hover_point = Some(point);
        let report = TermMouseEvent {
            kind: MouseEventKind::Motion,
            button: None,
            point,
            modifiers: term_modifiers(event.modifiers),
        };
        self.write_report(input::encode_mouse(&report, &modes));
    }

    fn on_mouse_up(&mut self, event: &MouseUpEvent, _cx: &mut Context<Self>) {
        if event.button == gpui::MouseButton::Left {
            self.selection_drag = None;
        }
        let Some(press) = self.reported_press else {
            return;
        };
        if term_mouse_button(event.button) != Some(press.button) {
            return;
        }
        self.reported_press = None;
        let point = self
            .point_for(event.position)
            .map_or(press.last_point, |(point, _)| point);
        let report = TermMouseEvent {
            kind: MouseEventKind::Release,
            button: Some(press.button),
            point,
            modifiers: term_modifiers(event.modifiers),
        };
        self.write_report(input::encode_mouse(&report, &self.session.modes()));
    }

    fn on_scroll(&mut self, event: &ScrollEvent, cx: &mut Context<Self>) {
        let Some(metrics) = self.metrics else {
            return;
        };
        if event.touch_phase == TouchPhase::Started {
            self.scroll_remainder = 0.0;
        }
        let delta = match event.delta {
            ScrollDelta::Lines(lines) => lines.y,
            ScrollDelta::Pixels(pixels) => pixels.y / metrics.line_height,
        };
        self.scroll_remainder += delta;
        let whole = self.scroll_remainder.trunc();
        self.scroll_remainder -= whole;
        let lines = whole as i32;
        if lines == 0 {
            return;
        }

        let modes = self.session.modes();
        if reports_mouse(&modes, event.modifiers) {
            let Some((point, _)) = self.point_for(event.position) else {
                return;
            };
            let button = if lines > 0 {
                TermMouseButton::WheelUp
            } else {
                TermMouseButton::WheelDown
            };
            let report = TermMouseEvent {
                kind: MouseEventKind::Press,
                button: Some(button),
                point,
                modifiers: term_modifiers(event.modifiers),
            };
            if let Some(bytes) = input::encode_mouse(&report, &modes) {
                let _ = self
                    .session
                    .write(&bytes.repeat(lines.unsigned_abs() as usize));
            }
            return;
        }
        if let Some(bytes) = input::encode_scroll_as_arrows(lines, &modes) {
            let _ = self.session.write(&bytes);
            return;
        }
        self.session.scroll_lines(lines);
        cx.notify();
    }

    /// Resizes the session to fit `bounds` if needed, then lays out the frame to paint.
    fn prepare_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        metrics: CellMetrics,
        focused: bool,
        window: &mut Window,
    ) -> PreparedFrame {
        self.bounds = Some(bounds);
        self.metrics = Some(metrics);
        let grid_size = metrics.grid_size(bounds.size.width, bounds.size.height);
        if grid_size != self.grid_size {
            let (cell_width, cell_height) = metrics.cell_size_pixels();
            // Fails only if the PTY has gone away; the grid is resized regardless.
            let _ = self
                .session
                .resize(grid_size.0, grid_size.1, cell_width, cell_height);
            self.grid_size = grid_size;
        }

        let snapshot = self.session.snapshot();
        let frame = FrameLayout {
            bounds,
            metrics,
            font: &self.font,
            theme: &self.theme,
        };
        frame.prepare(&snapshot, focused, window)
    }
}

/// Whether mouse events go to the program: it asked for them and Shift is not held.
fn reports_mouse(modes: &TerminalModes, modifiers: gpui::Modifiers) -> bool {
    modes.mouse_reporting != MouseReporting::Off && !modifiers.shift
}

fn term_mouse_button(button: gpui::MouseButton) -> Option<TermMouseButton> {
    match button {
        gpui::MouseButton::Left => Some(TermMouseButton::Left),
        gpui::MouseButton::Middle => Some(TermMouseButton::Middle),
        gpui::MouseButton::Right => Some(TermMouseButton::Right),
        gpui::MouseButton::Navigate(_) => None,
    }
}

fn term_modifiers(modifiers: gpui::Modifiers) -> input::Modifiers {
    input::Modifiers {
        shift: modifiers.shift,
        ctrl: modifiers.control,
        alt: modifiers.alt,
        super_key: modifiers.platform,
    }
}

/// The cell size for `font`: one advance of `'m'` wide, `size * line_height` tall rounded
/// to whole pixels.
fn cell_metrics(font: &TerminalFont, window: &Window) -> CellMetrics {
    let text_system = window.text_system();
    let font_size = font.size_px();
    let font_id = text_system.resolve_font(&font.gpui_font(false, false));
    let cell_width = text_system
        .advance(font_id, font_size, 'm')
        .map(|advance| advance.width)
        .unwrap_or(font_size * 0.6);
    let line_height = (font_size * font.line_height.max(1.0)).round();
    CellMetrics {
        cell_width,
        line_height,
    }
}

/// How the cursor's shape is drawn.
enum CursorPaint {
    Fill(Bounds<Pixels>),
    Outline(Bounds<Pixels>),
}

/// The cursor of one frame.
struct PreparedCursor {
    paint: CursorPaint,
    color: Hsla,
    /// The glyph under a filled block cursor, redrawn on top of it.
    text: Option<(Point<Pixels>, ShapedLine)>,
}

/// Everything shaped and measured in prepaint, ready to draw.
struct PreparedFrame {
    bounds: Bounds<Pixels>,
    background: Hsla,
    fills: Vec<(Bounds<Pixels>, Hsla)>,
    selection: Vec<Bounds<Pixels>>,
    selection_color: Hsla,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Option<PreparedCursor>,
    thumb: Option<(Bounds<Pixels>, Hsla)>,
    line_height: Pixels,
}

/// The inputs to laying out one frame.
struct FrameLayout<'a> {
    bounds: Bounds<Pixels>,
    metrics: CellMetrics,
    font: &'a TerminalFont,
    theme: &'a TerminalTheme,
}

impl FrameLayout<'_> {
    fn cell_origin(&self, row: usize, col: usize) -> Point<Pixels> {
        self.bounds.origin
            + point(
                self.metrics.cell_width * col as f32,
                self.metrics.line_height * row as f32,
            )
    }

    fn cells_bounds(&self, row: usize, col: usize, len: usize) -> Bounds<Pixels> {
        Bounds::new(
            self.cell_origin(row, col),
            size(
                self.metrics.cell_width * len as f32,
                self.metrics.line_height,
            ),
        )
    }

    fn shape(&self, segment: &TextSegment, window: &mut Window) -> ShapedLine {
        let runs: Vec<TextRun> = segment
            .runs
            .iter()
            .map(|run| {
                let style = run.style;
                let color: Hsla = style.color.into();
                TextRun {
                    len: run.len,
                    font: self.font.gpui_font(style.bold, style.italic),
                    color,
                    background_color: None,
                    underline: style
                        .underline
                        .map(|(kind, underline_color)| UnderlineStyle {
                            thickness: px(1.0),
                            color: Some(underline_color.into()),
                            wavy: kind == UnderlineKind::Curly,
                        }),
                    strikethrough: style.strikethrough.then_some(StrikethroughStyle {
                        thickness: px(1.0),
                        color: Some(color),
                    }),
                }
            })
            .collect();
        let force_width = (!segment.wide).then_some(self.metrics.cell_width);
        window.text_system().shape_line(
            segment.text.clone().into(),
            self.font.size_px(),
            &runs,
            force_width,
        )
    }

    fn prepare(
        &self,
        snapshot: &GridSnapshot,
        focused: bool,
        window: &mut Window,
    ) -> PreparedFrame {
        let theme = self.theme;
        let mut fills = Vec::new();
        let mut lines = Vec::new();
        for row in 0..snapshot.rows {
            let Some(cells) = snapshot.row(row) else {
                continue;
            };
            for run in background_runs(cells, theme) {
                fills.push((
                    self.cells_bounds(row, run.start_col, run.len),
                    run.color.into(),
                ));
            }
            for segment in text_segments(cells, theme) {
                let origin = self.cell_origin(row, segment.start_col);
                lines.push((origin, self.shape(&segment, window)));
            }
        }

        let selection = snapshot
            .selection
            .map(|span| {
                selection_ranges(&span, snapshot.cols, snapshot.rows)
                    .into_iter()
                    .map(|(row, start, end)| self.cells_bounds(row, start, end - start))
                    .collect()
            })
            .unwrap_or_default();

        let cursor = self.prepare_cursor(snapshot, focused, window);

        let thumb = scrollback_thumb(
            snapshot.display_offset,
            snapshot.history_size,
            snapshot.rows,
            self.bounds.size.height,
            THUMB_MIN_HEIGHT,
        )
        .map(|(top, height)| {
            let origin = point(
                self.bounds.right() - THUMB_WIDTH - THUMB_INSET,
                self.bounds.top() + top,
            );
            (
                Bounds::new(origin, size(THUMB_WIDTH, height)),
                theme.scrollback_indicator.into(),
            )
        });

        PreparedFrame {
            bounds: self.bounds,
            background: theme.background.into(),
            fills,
            selection,
            selection_color: theme.selection.into(),
            lines,
            cursor,
            thumb,
            line_height: self.metrics.line_height,
        }
    }

    /// The cursor to draw: hidden while the program hides it or the view is scrolled back,
    /// an outline while unfocused, otherwise the shape the program asked for.
    fn prepare_cursor(
        &self,
        snapshot: &GridSnapshot,
        focused: bool,
        window: &mut Window,
    ) -> Option<PreparedCursor> {
        let cursor = snapshot.cursor;
        if !cursor.visible || snapshot.display_offset > 0 {
            return None;
        }
        let cell = snapshot.cell(cursor.row, cursor.col)?;
        let width = if cell.flags.contains(CellFlags::WIDE_CHAR) {
            2
        } else {
            1
        };
        let block = self.cells_bounds(cursor.row, cursor.col, width);
        let color: Hsla = self.theme.cursor.into();
        let shape_only = |paint| PreparedCursor {
            paint,
            color,
            text: None,
        };
        if !focused {
            return Some(shape_only(CursorPaint::Outline(block)));
        }
        let prepared = match cursor.shape {
            CursorShape::Block => {
                let mut text_cell = cell.clone();
                text_cell.fg =
                    xctx_term::Color::Rgb(crate::theme::rgb_from_rgba(self.theme.cursor_text));
                text_cell.flags.remove(CellFlags::INVERSE | CellFlags::DIM);
                let text = text_segments(std::slice::from_ref(&text_cell), self.theme)
                    .into_iter()
                    .next()
                    .map(|segment| (block.origin, self.shape(&segment, window)));
                PreparedCursor {
                    paint: CursorPaint::Fill(block),
                    color,
                    text,
                }
            }
            CursorShape::Underline => {
                let bar = Bounds::new(
                    point(block.left(), block.bottom() - CURSOR_LINE_WIDTH),
                    size(block.size.width, CURSOR_LINE_WIDTH),
                );
                shape_only(CursorPaint::Fill(bar))
            }
            CursorShape::Beam => {
                let bar = Bounds::new(block.origin, size(CURSOR_LINE_WIDTH, block.size.height));
                shape_only(CursorPaint::Fill(bar))
            }
            CursorShape::HollowBlock => shape_only(CursorPaint::Outline(block)),
        };
        Some(prepared)
    }
}

impl PreparedFrame {
    fn paint(self, window: &mut Window, cx: &mut App) {
        window.paint_quad(fill(self.bounds, self.background));
        for (bounds, color) in self.fills {
            window.paint_quad(fill(bounds, color));
        }
        for bounds in self.selection {
            window.paint_quad(fill(bounds, self.selection_color));
        }
        for (origin, line) in &self.lines {
            // A glyph that cannot be rasterized is skipped; the rest of the frame still draws.
            let _ = line.paint(*origin, self.line_height, TextAlign::Left, None, window, cx);
        }
        if let Some(cursor) = &self.cursor {
            match cursor.paint {
                CursorPaint::Fill(bounds) => window.paint_quad(fill(bounds, cursor.color)),
                CursorPaint::Outline(bounds) => {
                    window.paint_quad(outline(bounds, cursor.color, gpui::BorderStyle::Solid));
                }
            }
            if let Some((origin, line)) = &cursor.text {
                let _ = line.paint(*origin, self.line_height, TextAlign::Left, None, window, cx);
            }
        }
        if let Some((bounds, color)) = self.thumb {
            window.paint_quad(fill(bounds, color).corner_radii(THUMB_WIDTH / 2.0));
        }
    }
}

/// Registers the frame's mouse handlers, which forward to the view.
fn register_mouse_handlers(view: WeakEntity<TerminalView>, hitbox: Hitbox, window: &mut Window) {
    window.set_cursor_style(CursorStyle::IBeam, &hitbox);

    window.on_mouse_event({
        let view = view.clone();
        let hitbox = hitbox.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                let _ = view.update(cx, |view, cx| view.on_mouse_down(event, window, cx));
            }
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        let hitbox = hitbox.clone();
        move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let hovered = hitbox.is_hovered(window);
                let _ = view.update(cx, |view, cx| view.on_mouse_move(event, hovered, cx));
            }
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                let _ = view.update(cx, |view, cx| view.on_mouse_up(event, cx));
            }
        }
    });
    window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
        if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
            let _ = view.update(cx, |view, cx| view.on_scroll(event, cx));
        }
    });
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let metrics = cell_metrics(&self.font, window);
        let focused = self.focus_handle.is_focused(window);
        let view = cx.entity();
        let weak_view = cx.weak_entity();

        div()
            .track_focus(&self.focus_handle)
            .key_context("Terminal")
            .size_full()
            .bg(self.theme.background)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let frame = view.update(cx, |view, _cx| {
                            view.prepare_frame(bounds, metrics, focused, window)
                        });
                        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
                        (frame, hitbox)
                    },
                    move |_bounds, (frame, hitbox), window, cx| {
                        frame.paint(window, cx);
                        register_mouse_handlers(weak_view, hitbox, window);
                    },
                )
                .size_full(),
            )
    }
}
