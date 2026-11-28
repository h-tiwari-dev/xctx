//! The composer: a multi-line [`PromptInput`] with omp's mode prefixes and their borders, the slash and `@file`
//! menus, image attachments (paste and drop), the queue chips, and the send buttons (send while idle; steer or
//! follow up while the agent runs).

use std::ops::Range;
use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use base64::Engine as _;
use gpui::{
    App, ClipboardEntry, Context, Entity, FocusHandle, Focusable, FontWeight, Hsla, ImageFormat,
    ImageSource, KeyDownEvent, SharedString, Subscription, Window, div, img, prelude::*, px,
};
use xctx_omp::{Image, Queue, SessionModel};
use xctx_ui::{
    Badge, Button, ButtonSize, ButtonVariant, IconName, PromptEvent, PromptInput, Theme, Tone,
};

use crate::draft::{
    self, DraftMode, Unsendable, WhileRunning, complete_file, file_query, intent_for, slash_query,
};
use crate::intent::OmpIntent;

/// Looks up files for `@file` completion: given what follows the `@`, the matching paths (relative to the
/// agent's worktree), best first. RPC has no completion, so the host answers from its own file index.
pub type FileCompleter = Rc<dyn Fn(&str) -> Vec<SharedString>>;

/// Esc reached the composer with nothing left for it to do (no menu, no turn, no draft): the host's own layered
/// Esc takes over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneEscape;

/// What the composer hands its owner.
#[derive(Debug)]
pub enum ComposerOutput {
    /// Something for omp.
    Intent(OmpIntent),
    /// Esc with nothing left to do here.
    Escape(PaneEscape),
}

/// Where a [`Composer`] sends what the user did. The intent is moved, so attached images are never copied.
pub type ComposerSink = Rc<dyn Fn(ComposerOutput, &mut App)>;

const IDLE_PLACEHOLDER: &str =
    "Message omp — / commands, @ files, ! bash, $ python, ? side question";
const RUNNING_PLACEHOLDER: &str = "Steer the agent (Enter) or queue a follow-up (Alt+Enter)";

/// The border colour of the composer in `mode`: success for bash, warning for python, the read-out colour for a
/// side question, else the focus outline.
pub fn mode_border(mode: DraftMode, focused: bool, theme: &Theme) -> Hsla {
    match mode {
        DraftMode::Bash { .. } => theme.success,
        DraftMode::Python { .. } => theme.warning,
        DraftMode::Btw => theme.meta,
        DraftMode::Prompt | DraftMode::Slash => theme.outline(focused),
    }
}

fn mode_tone(mode: DraftMode) -> Tone {
    match mode {
        DraftMode::Bash { .. } => Tone::Success,
        DraftMode::Python { .. } => Tone::Warning,
        _ => Tone::Neutral,
    }
}

fn mode_selector(mode: DraftMode) -> &'static str {
    match mode {
        DraftMode::Bash { .. } => "omp-mode-bash",
        DraftMode::Python { .. } => "omp-mode-python",
        DraftMode::Btw => "omp-mode-btw",
        DraftMode::Prompt | DraftMode::Slash => "omp-mode-prompt",
    }
}

/// The image formats omp's models accept, by file extension.
pub fn image_format_for(path: &Path) -> Option<ImageFormat> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some(ImageFormat::Png),
        "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
        "gif" => Some(ImageFormat::Gif),
        "webp" => Some(ImageFormat::Webp),
        _ => None,
    }
}

fn accepts(format: ImageFormat) -> bool {
    matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::Webp
    )
}

enum Menu {
    Slash(Vec<(SharedString, SharedString)>),
    File(Range<usize>),
}

/// The composer entity. Create with `cx.new(|cx| Composer::new(sink, cx))`; what the user does goes to `sink`.
pub struct Composer {
    sink: ComposerSink,
    input: Entity<PromptInput>,
    session: Arc<SessionModel>,
    images: Vec<Image>,
    previews: Vec<Arc<gpui::Image>>,
    files: Option<FileCompleter>,
    file_query: Option<String>,
    file_matches: Vec<SharedString>,
    menu_index: usize,
    menu_dismissed: bool,
    note: Option<SharedString>,
    seed: Option<String>,
    last_text: String,
    _subscriptions: [Subscription; 2],
}

impl Focusable for Composer {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.focus_handle(cx)
    }
}

impl Composer {
    /// An empty composer for an idle session that hands what the user does to `sink`.
    pub fn new(sink: ComposerSink, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            PromptInput::new(cx)
                .placeholder(IDLE_PLACEHOLDER)
                .multiline(true)
                .borderless(true)
        });
        let events = cx.subscribe(
            &input,
            |this: &mut Self, _, event: &PromptEvent, cx| match event {
                PromptEvent::Submit(_) => this.submit(WhileRunning::Steer, cx),
                PromptEvent::Cancel => this.escape(cx),
            },
        );
        let edits = cx.observe(&input, |this: &mut Self, _, cx| this.on_edit(cx));
        Composer {
            sink,
            input,
            session: Arc::new(SessionModel::new()),
            images: Vec::new(),
            previews: Vec::new(),
            files: None,
            file_query: None,
            file_matches: Vec::new(),
            menu_index: 0,
            menu_dismissed: false,
            note: None,
            seed: None,
            last_text: String::new(),
            _subscriptions: [events, edits],
        }
    }

    /// Answer `@file` completion with `completer`.
    pub fn set_file_completer(&mut self, completer: FileCompleter) {
        self.files = Some(completer);
    }

    /// Show `session`'s turn state, queue and commands. Text omp asks for (`set_editor_text`) replaces the
    /// draft once.
    pub fn set_session(&mut self, session: Arc<SessionModel>, cx: &mut Context<Self>) {
        if session.composer_seed != self.seed {
            self.seed.clone_from(&session.composer_seed);
            if let Some(text) = &session.composer_seed {
                self.input.update(cx, |input, cx| input.set_value(text, cx));
            }
        }
        let placeholder = if session.is_running() {
            RUNNING_PLACEHOLDER
        } else {
            IDLE_PLACEHOLDER
        };
        if session.is_running() != self.session.is_running() {
            self.input
                .update(cx, |input, cx| input.set_placeholder(placeholder, cx));
        }
        self.session = session;
        cx.notify();
    }

    /// The draft as typed.
    pub fn text<'a>(&self, cx: &'a App) -> &'a str {
        self.input.read(cx).value()
    }

    /// Replace the draft.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_value(text, cx));
    }

    /// The draft's mode.
    /// Whether the input has keyboard focus.
    pub fn is_focused(&self, window: &Window, cx: &App) -> bool {
        self.input.focus_handle(cx).is_focused(window)
    }

    /// The draft's mode, as the dock colours its border.
    pub fn mode(&self, cx: &App) -> DraftMode {
        DraftMode::of(self.text(cx))
    }

    /// The images attached so far.
    pub fn images(&self) -> &[Image] {
        &self.images
    }

    /// Attach an image; `format` must be one omp's models take (PNG, JPEG, GIF, WebP).
    pub fn attach(&mut self, format: ImageFormat, bytes: Vec<u8>, cx: &mut Context<Self>) {
        if !accepts(format) {
            self.note = Some("Only PNG, JPEG, GIF and WebP images can be attached".into());
            cx.notify();
            return;
        }
        self.images.push(Image {
            mime_type: format.mime_type().to_owned(),
            data: base64::engine::general_purpose::STANDARD.encode(&bytes),
        });
        self.previews
            .push(Arc::new(gpui::Image::from_bytes(format, bytes)));
        cx.notify();
    }

    /// Attach the images among dropped `paths`; anything else, or a file that cannot be read, leaves a note under
    /// the composer.
    pub fn attach_paths(&mut self, paths: &[std::path::PathBuf], cx: &mut Context<Self>) {
        for path in paths {
            let name = path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            );
            let Some(format) = image_format_for(path) else {
                self.note = Some(format!("Only images can be attached: {name}").into());
                continue;
            };
            match std::fs::read(path) {
                Ok(bytes) => self.attach(format, bytes, cx),
                Err(error) => self.note = Some(format!("Could not read {name}: {error}").into()),
            }
        }
        cx.notify();
    }

    /// The note under the composer (why a draft cannot be sent, an attachment that failed), if any.
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Take queued `message` back into the draft for editing, and tell omp to drop it from `queue`.
    pub fn edit_queued(&mut self, message: &str, queue: Queue, cx: &mut Context<Self>) {
        self.set_text(message, cx);
        self.send(
            OmpIntent::EditQueued {
                message: message.to_owned(),
                queue,
            },
            cx,
        );
    }

    fn send(&self, intent: OmpIntent, cx: &mut App) {
        (self.sink)(ComposerOutput::Intent(intent), cx);
    }

    fn on_edit(&mut self, cx: &mut Context<Self>) {
        let input = self.input.read(cx);
        let text = input.value();
        if text != self.last_text {
            self.last_text.clear();
            self.last_text.push_str(text);
            self.menu_index = 0;
            self.menu_dismissed = false;
            self.note = None;
        }
        let query = file_query(text, input.buffer().cursor()).map(|(_, q)| q);
        if query != self.file_query.as_deref() {
            self.file_query = query.map(str::to_owned);
            self.file_matches = match (&self.files, query) {
                (Some(files), Some(q)) => {
                    let mut found = files(q);
                    found.truncate(draft::MENU_LIMIT);
                    found
                }
                _ => Vec::new(),
            };
        }
        cx.notify();
    }

    fn escape(&mut self, cx: &mut Context<Self>) {
        if self.current_menu(cx).is_some() {
            self.menu_dismissed = true;
            cx.notify();
        } else if self.session.is_running() {
            self.send(OmpIntent::Abort, cx);
        } else if !self.text(cx).is_empty() || !self.images.is_empty() {
            self.input.update(cx, |input, cx| input.clear(cx));
            self.images.clear();
            self.previews.clear();
            self.note = None;
            cx.notify();
        } else {
            (self.sink)(ComposerOutput::Escape(PaneEscape), cx);
        }
    }

    fn current_menu(&self, cx: &App) -> Option<Menu> {
        if self.menu_dismissed {
            return None;
        }
        let input = self.input.read(cx);
        let text = input.value();
        if let Some(query) = slash_query(text) {
            let rows: Vec<(SharedString, SharedString)> =
                draft::slash_matches(&self.session.commands, query)
                    .into_iter()
                    .map(|c| {
                        (
                            SharedString::from(c.name.clone()),
                            SharedString::from(c.description.clone()),
                        )
                    })
                    .collect();
            return (!rows.is_empty()).then_some(Menu::Slash(rows));
        }
        let (range, _) = file_query(text, input.buffer().cursor())?;
        (!self.file_matches.is_empty()).then_some(Menu::File(range))
    }

    fn menu_len(menu: &Menu, file_matches: usize) -> usize {
        match menu {
            Menu::Slash(rows) => rows.len(),
            Menu::File(_) => file_matches,
        }
    }

    fn accept(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(menu) = self.current_menu(cx) else {
            return;
        };
        let text = match menu {
            Menu::Slash(rows) => match rows.get(index) {
                Some((name, _)) => format!("/{name} "),
                None => return,
            },
            Menu::File(range) => match self.file_matches.get(index) {
                Some(path) => complete_file(self.text(cx), range, path),
                None => return,
            },
        };
        self.set_text(&text, cx);
        self.menu_index = 0;
    }

    fn submit(&mut self, queue: WhileRunning, cx: &mut Context<Self>) {
        if self.current_menu(cx).is_some() {
            self.accept(self.menu_index, cx);
            return;
        }
        let text = self.text(cx).to_owned();
        let running = self.session.is_running();
        match intent_for(
            &text,
            running,
            queue,
            &self.session.commands,
            &mut self.images,
        ) {
            Ok(intent) => {
                if self.images.is_empty() {
                    self.previews.clear();
                }
                self.note = None;
                self.input.update(cx, |input, cx| input.clear(cx));
                self.send(intent, cx);
            }
            Err(why) => self.note = why.hint().map(SharedString::from),
        }
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let m = keystroke.modifiers;
        if keystroke.key == "enter" && m.alt && !m.platform && !m.control {
            cx.stop_propagation();
            self.submit(WhileRunning::FollowUp, cx);
            return;
        }
        let Some(menu) = self.current_menu(cx) else {
            return;
        };
        let len = Self::menu_len(&menu, self.file_matches.len());
        match keystroke.key.as_str() {
            "down" if len > 0 => self.menu_index = (self.menu_index + 1) % len,
            "up" if len > 0 => self.menu_index = (self.menu_index + len - 1) % len,
            "tab" => self.accept(self.menu_index, cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn capture_paste(
        &mut self,
        event: &KeyDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let k = &event.keystroke;
        if k.key != "v" || !k.modifiers.platform || k.modifiers.control || k.modifiers.alt {
            return;
        }
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        let images: Vec<(ImageFormat, Vec<u8>)> = item
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                ClipboardEntry::Image(image) => Some((image.format, image.bytes.clone())),
                _ => None,
            })
            .collect();
        if images.is_empty() {
            return;
        }
        cx.stop_propagation();
        for (format, bytes) in images {
            self.attach(format, bytes, cx);
        }
    }

    fn remove_image(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.images.len() {
            self.images.remove(index);
            self.previews.remove(index);
            cx.notify();
        }
    }

    fn render_menu(&self, menu: &Menu, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let (selector, rows): (&'static str, Vec<(SharedString, Option<SharedString>)>) = match menu
        {
            Menu::Slash(rows) => (
                "omp-slash-menu",
                rows.iter()
                    .map(|(name, description)| {
                        (
                            SharedString::from(format!("/{name}")),
                            Some(description.clone()),
                        )
                    })
                    .collect(),
            ),
            Menu::File(_) => (
                "omp-file-menu",
                self.file_matches
                    .iter()
                    .map(|p| (SharedString::from(format!("@{p}")), None))
                    .collect(),
            ),
        };
        div()
            .debug_selector(move || selector.to_owned())
            .flex()
            .flex_col()
            .w_full()
            .py(Theme::SPACE_1)
            .rounded(Theme::RADIUS_MD)
            .border_1()
            .border_color(theme.border_strong)
            .bg(theme.overlay)
            .children(rows.into_iter().enumerate().map(|(i, (label, detail))| {
                let selected = i == self.menu_index;
                let hover = theme.hover;
                div()
                    .id(("omp-menu-row", i))
                    .debug_selector(move || format!("omp-menu-row-{i}"))
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .px(Theme::SPACE_2)
                    .h(px(24.0))
                    .cursor_pointer()
                    .when(selected, |row| row.bg(theme.selection))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.accept(i, cx);
                    }))
                    .child(
                        div()
                            .flex_none()
                            .font_family("Menlo")
                            .text_size(Theme::TEXT_SM)
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(theme.text)
                            .child(label),
                    )
                    .children(detail.map(|d| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.text_muted)
                            .child(d)
                    }))
            }))
    }

    fn render_queue(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let queue = &self.session.queue;
        if queue.steering.is_empty() && queue.follow_up.is_empty() {
            return None;
        }
        let chips = queue
            .steering
            .iter()
            .map(|m| (m, Queue::Steering))
            .chain(queue.follow_up.iter().map(|m| (m, Queue::FollowUp)))
            .enumerate()
            .map(|(i, (message, kind))| {
                let message = message.clone();
                let (edit_msg, remove_msg, promote_msg) =
                    (message.clone(), message.clone(), message.clone());
                div()
                    .debug_selector(|| "omp-queue-chip".to_owned())
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_1)
                    .max_w(px(320.0))
                    .h(px(26.0))
                    .pl(Theme::SPACE_2)
                    .pr(Theme::SPACE_1)
                    .rounded(Theme::RADIUS_FULL)
                    .border_1()
                    .border_color(theme.border)
                    .bg(theme.raised)
                    .child(
                        Badge::new(match kind {
                            Queue::Steering => "steer",
                            Queue::FollowUp => "follow-up",
                        })
                        .tone(match kind {
                            Queue::Steering => Tone::Accent,
                            Queue::FollowUp => Tone::Neutral,
                        }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.text)
                            .child(message),
                    )
                    .child(
                        Button::new(("omp-queue-edit", i), "")
                            .icon(IconName::PenTool)
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .tooltip("Edit")
                            .selector(format!("omp-queue-edit-{i}"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.edit_queued(&edit_msg, kind, cx);
                            })),
                    )
                    .when(kind == Queue::FollowUp, |chip| {
                        chip.child(
                            Button::new(("omp-queue-promote", i), "")
                                .icon(IconName::Send)
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Compact)
                                .tooltip("Steer now")
                                .selector(format!("omp-queue-promote-{i}"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.send(
                                        OmpIntent::PromoteQueued {
                                            message: promote_msg.clone(),
                                        },
                                        cx,
                                    );
                                })),
                        )
                    })
                    .child(
                        Button::new(("omp-queue-remove", i), "")
                            .icon(IconName::Close)
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .tooltip("Remove")
                            .selector(format!("omp-queue-remove-{i}"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.send(
                                    OmpIntent::RemoveQueued {
                                        message: remove_msg.clone(),
                                        queue: kind,
                                    },
                                    cx,
                                );
                            })),
                    )
            });
        Some(
            div()
                .debug_selector(|| "omp-queue".to_owned())
                .flex()
                .flex_wrap()
                .gap(Theme::SPACE_1)
                .w_full()
                .children(chips),
        )
    }
}

impl Render for Composer {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let text = self.text(cx);
        let mode = DraftMode::of(text);
        let blank = text.trim().is_empty() && self.images.is_empty();
        let python = matches!(mode, DraftMode::Python { .. });
        let running = self.session.is_running();
        let menu = self.current_menu(cx);

        let attachments = (!self.previews.is_empty()).then(|| {
            div()
                .flex()
                .gap(Theme::SPACE_1)
                .children(self.previews.iter().enumerate().map(|(i, preview)| {
                    div()
                        .debug_selector(|| "omp-attachment".to_owned())
                        .relative()
                        .size(px(48.0))
                        .rounded(Theme::RADIUS_SM)
                        .border_1()
                        .border_color(theme.border)
                        .overflow_hidden()
                        .child(img(ImageSource::Image(preview.clone())).size_full())
                        .child(
                            div().absolute().top_0().right_0().child(
                                Button::new(("omp-attachment-remove", i), "")
                                    .icon(IconName::Close)
                                    .variant(ButtonVariant::Ghost)
                                    .size(ButtonSize::Compact)
                                    .selector(format!("omp-attachment-remove-{i}"))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.remove_image(i, cx);
                                    })),
                            ),
                        )
                }))
        });

        let unsendable = blank || python;
        let buttons = if running {
            div()
                .flex()
                .gap(Theme::SPACE_1)
                .child(
                    Button::new("omp-send-follow-up", "Follow up")
                        .size(ButtonSize::Regular)
                        .tooltip("Queue for when the turn ends (Alt+Enter)")
                        .disabled(unsendable)
                        .selector("omp-send-follow-up")
                        .on_click(
                            cx.listener(|this, _, _, cx| this.submit(WhileRunning::FollowUp, cx)),
                        ),
                )
                .child(
                    Button::new("omp-send-steer", "Steer")
                        .variant(ButtonVariant::Primary)
                        .tooltip("Interrupt at the next step (Enter)")
                        .disabled(unsendable)
                        .selector("omp-send-steer")
                        .on_click(
                            cx.listener(|this, _, _, cx| this.submit(WhileRunning::Steer, cx)),
                        ),
                )
        } else {
            div().flex().child(
                Button::new("omp-send", "Send")
                    .variant(ButtonVariant::Primary)
                    .tooltip("Send (Enter)")
                    .disabled(unsendable)
                    .selector("omp-send")
                    .on_click(cx.listener(|this, _, _, cx| this.submit(WhileRunning::Steer, cx))),
            )
        };

        let note = self.note.clone().or_else(|| {
            python
                .then(|| {
                    Unsendable::PythonNeedsTerminal
                        .hint()
                        .map(SharedString::from)
                })
                .flatten()
        });

        div()
            .debug_selector(|| "omp-composer".to_owned())
            .key_context("OmpComposer")
            .capture_key_down(cx.listener(Self::capture_paste))
            .on_key_down(cx.listener(Self::on_key_down))
            .flex()
            .flex_col()
            .gap(Theme::SPACE_1)
            .w_full()
            .children(self.render_queue(&theme, cx))
            .children(menu.as_ref().map(|m| self.render_menu(m, &theme, cx)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_1)
                    .w_full()
                    .children(mode.label().map(|label| {
                        let selector = mode_selector(mode);
                        div()
                            .debug_selector(move || selector.to_owned())
                            .pt(Theme::SPACE_1)
                            .child(Badge::new(label).tone(mode_tone(mode)))
                    }))
                    .child(self.input.clone())
                    .child(
                        div()
                            .flex()
                            .items_end()
                            .justify_between()
                            .gap(Theme::SPACE_2)
                            .w_full()
                            .child(div().flex().children(attachments))
                            .child(buttons),
                    ),
            )
            .children(note.map(|note| {
                div()
                    .debug_selector(|| "omp-composer-note".to_owned())
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_muted)
                    .child(note)
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_get_their_own_border_colours() {
        let theme = Theme::dark();
        let bash = mode_border(
            DraftMode::Bash {
                exclude_from_context: false,
            },
            true,
            &theme,
        );
        let python = mode_border(
            DraftMode::Python {
                exclude_from_context: false,
            },
            true,
            &theme,
        );
        let btw = mode_border(DraftMode::Btw, true, &theme);
        assert_eq!(
            (bash, python, btw),
            (theme.success, theme.warning, theme.meta)
        );
        assert_eq!(mode_border(DraftMode::Prompt, true, &theme), theme.accent);
        assert_eq!(mode_border(DraftMode::Prompt, false, &theme), theme.border);
    }

    #[test]
    fn only_model_image_formats_are_attached_from_files() {
        assert_eq!(image_format_for(Path::new("a.PNG")), Some(ImageFormat::Png));
        assert_eq!(
            image_format_for(Path::new("a.jpeg")),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(image_format_for(Path::new("a.svg")), None);
        assert_eq!(image_format_for(Path::new("Makefile")), None);
    }
}
