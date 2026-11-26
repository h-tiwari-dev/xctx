//! [`OmpView`]: the native omp pane. It draws a read-only [`SessionModel`] snapshot (HUD, transcript, banner,
//! composer) and emits [`OmpIntent`]s; it never talks to omp itself.

use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, Context, Entity, EventEmitter, ExternalPaths, FocusHandle, Focusable, FontWeight,
    SharedString, Window, div, prelude::*, px,
};
use xctx_omp::{Phase, SessionModel};
use xctx_ui::{Badge, Button, ButtonSize, ButtonVariant, IconSize, Pill, Spinner, Theme, Tone};

use crate::banner::{self, Banner, Exit, banners_for};
use crate::composer::{Composer, ComposerOutput, FileCompleter, PaneEscape};
use crate::hud::{
    ModelChoice, TurnMeter, WorkingClock, context_is_high, context_label, cost_label, rate_label,
    todo_progress,
};
use crate::intent::OmpIntent;
use crate::transcript::Transcript;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HudMenu {
    Model,
    Effort,
}

/// The native omp pane.
///
/// Create it with `cx.new(|cx| OmpView::new(session, cx))`, subscribe to its [`OmpIntent`]s (send each one's
/// [`OmpIntent::command`] to the `OmpClient`) and to [`PaneEscape`] (Esc the pane did not use), and call
/// [`OmpView::set_session`] with a fresh snapshot whenever the model changed. Tell it about the client with
/// [`OmpView::client_exited`] and [`OmpView::malformed_frame`], and fill the model menu with
/// [`OmpView::set_model_choices`] once the host has the answer to [`OmpIntent::ListModels`].
pub struct OmpView {
    session: Arc<SessionModel>,
    transcript: Transcript,
    composer: Entity<Composer>,
    spinner: Entity<Spinner>,
    clock: Entity<WorkingClock>,
    meter: TurnMeter,
    models: Option<Vec<ModelChoice>>,
    menu: Option<HudMenu>,
    exit: Option<Exit>,
    malformed: Option<String>,
    dismissed: Option<Banner>,
}

impl EventEmitter<OmpIntent> for OmpView {}
impl EventEmitter<PaneEscape> for OmpView {}

impl Focusable for OmpView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

impl OmpView {
    /// A pane showing `session`.
    pub fn new(session: Arc<SessionModel>, cx: &mut Context<Self>) -> Self {
        let view = cx.weak_entity();
        let composer = cx.new(|cx| {
            Composer::new(
                std::rc::Rc::new(move |output, cx| {
                    view.update(cx, |_, cx| match output {
                        ComposerOutput::Intent(intent) => cx.emit(intent),
                        ComposerOutput::Escape(escape) => cx.emit(escape),
                    })
                    .ok();
                }),
                cx,
            )
        });
        let mut this = OmpView {
            session: Arc::new(SessionModel::new()),
            transcript: Transcript::new(),
            composer,
            spinner: cx.new(|_| Spinner::new().size(IconSize::Sm)),
            clock: cx.new(|_| WorkingClock::new()),
            meter: TurnMeter::default(),
            models: None,
            menu: None,
            exit: None,
            malformed: None,
            dismissed: None,
        };
        this.set_session(session, cx);
        this
    }

    /// Show a new snapshot of the session.
    pub fn set_session(&mut self, session: Arc<SessionModel>, cx: &mut Context<Self>) {
        self.meter.observe(&session, Instant::now());
        let started = self.meter.started();
        self.clock
            .update(cx, |clock, cx| clock.set_started(started, cx));
        let running = session.is_running();
        self.spinner
            .update(cx, |spinner, cx| spinner.set_active(running, cx));
        self.transcript.set_model(session.clone());
        self.composer
            .update(cx, |composer, cx| composer.set_session(session.clone(), cx));
        self.session = session;
        cx.notify();
    }

    /// Answer `@file` completion with `completer` (RPC has none of its own).
    pub fn set_file_completer(&mut self, completer: FileCompleter, cx: &mut Context<Self>) {
        self.composer
            .update(cx, |composer, _| composer.set_file_completer(completer));
    }

    /// Fill the model menu, from [`ModelChoice::from_picker`] on the `get_model_picker` response.
    pub fn set_model_choices(&mut self, choices: Vec<ModelChoice>, cx: &mut Context<Self>) {
        self.models = Some(choices);
        cx.notify();
    }

    /// The omp child exited (the client's `Incoming::Exited`): show it on the banner.
    pub fn client_exited(&mut self, code: Option<i32>, stderr: &str, cx: &mut Context<Self>) {
        self.exit = Some(Exit {
            code,
            stderr: stderr.to_owned(),
        });
        cx.notify();
    }

    /// omp wrote something the client could not decode (`Incoming::Malformed`): show it on the banner.
    pub fn malformed_frame(&mut self, detail: &str, cx: &mut Context<Self>) {
        self.malformed = Some(detail.to_owned());
        cx.notify();
    }

    /// The composer, to focus it or read the draft.
    pub fn composer(&self) -> &Entity<Composer> {
        &self.composer
    }

    /// The transcript rows on screen, in order.
    pub fn transcript(&self) -> &Transcript {
        &self.transcript
    }

    /// The banner shown now, if any: the most serious current problem except the one last dismissed.
    pub fn banner(&self) -> Option<Banner> {
        banners_for(&self.session, self.exit.as_ref(), self.malformed.as_deref())
            .into_iter()
            .find(|b| self.dismissed.as_ref() != Some(b))
    }

    fn toggle_menu(&mut self, menu: HudMenu, cx: &mut Context<Self>) {
        if self.menu == Some(menu) {
            self.menu = None;
        } else {
            self.menu = Some(menu);
            if menu == HudMenu::Model && self.models.is_none() {
                self.models = Some(Vec::new());
                cx.emit(OmpIntent::ListModels);
            }
        }
        cx.notify();
    }

    fn render_hud(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let session = &self.session;
        let running = session.is_running();
        let working = running.then(|| {
            div()
                .debug_selector(|| "omp-working".to_owned())
                .flex()
                .items_center()
                .gap(Theme::SPACE_1)
                .child(self.spinner.clone())
                .child(
                    div()
                        .text_size(Theme::TEXT_SM)
                        .text_color(theme.text_muted)
                        .child(if session.compacting {
                            "Compacting"
                        } else {
                            "Working"
                        }),
                )
                .child(self.clock.clone())
                .child(
                    Button::new("omp-stop", "Stop")
                        .variant(ButtonVariant::Danger)
                        .size(ButtonSize::Compact)
                        .tooltip("Stop the turn (Esc)")
                        .selector("omp-stop")
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(OmpIntent::Abort))),
                )
        });
        let needs_you = (session.phase() == Phase::NeedsYou)
            .then(|| Badge::new("needs you").tone(Tone::Warning));
        let model_label: SharedString = session
            .model
            .as_ref()
            .map_or_else(|| "model".into(), |m| m.label().to_owned().into());
        let effort_label: SharedString = session
            .thinking
            .clone()
            .map_or_else(|| "effort".into(), SharedString::from);
        let readout = |selector: &'static str, text: String, color| {
            div()
                .debug_selector(move || selector.to_owned())
                .flex_none()
                .text_size(Theme::TEXT_SM)
                .text_color(color)
                .child(text)
        };
        div()
            .debug_selector(|| "omp-hud".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_2)
            .w_full()
            .h(px(32.0))
            .px(Theme::SPACE_3)
            .border_b_1()
            .border_color(theme.border_subtle)
            .children(working)
            .children(needs_you)
            .child(
                Button::new("omp-hud-model", model_label)
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .selected(self.menu == Some(HudMenu::Model))
                    .tooltip("Model")
                    .selector("omp-hud-model")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(HudMenu::Model, cx))),
            )
            .child(
                Button::new("omp-hud-effort", effort_label)
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .selected(self.menu == Some(HudMenu::Effort))
                    .disabled(session.thinking_levels.is_empty())
                    .tooltip("Thinking level")
                    .selector("omp-hud-effort")
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(HudMenu::Effort, cx))),
            )
            .child(div().flex_1())
            .children(context_label(session).map(|label| {
                let color = if context_is_high(session) {
                    theme.warning
                } else {
                    theme.meta
                };
                readout("omp-hud-context", label, color)
            }))
            .child(readout("omp-hud-cost", cost_label(session), theme.meta))
            .children(
                self.meter
                    .rate()
                    .map(|rate| readout("omp-hud-rate", rate_label(rate), theme.meta)),
            )
            .children(todo_progress(session).map(|(done, total)| {
                readout(
                    "omp-hud-todos",
                    format!("todo {done}/{total}"),
                    theme.text_muted,
                )
            }))
            .children(session.session.name.clone().map(|name| {
                readout("omp-hud-session", name, theme.text_muted)
                    .max_w(px(160.0))
                    .truncate()
            }))
    }

    fn render_menu(
        &self,
        menu: HudMenu,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let row = div()
            .debug_selector(move || {
                match menu {
                    HudMenu::Model => "omp-model-menu",
                    HudMenu::Effort => "omp-effort-menu",
                }
                .to_owned()
            })
            .flex()
            .flex_wrap()
            .items_center()
            .gap(Theme::SPACE_1)
            .w_full()
            .px(Theme::SPACE_3)
            .py(Theme::SPACE_1)
            .border_b_1()
            .border_color(theme.border_subtle)
            .bg(theme.raised);
        match menu {
            HudMenu::Effort => {
                let current = self.session.thinking.as_deref();
                row.children(self.session.thinking_levels.iter().map(|level| {
                    let pick = level.clone();
                    Pill::new(
                        SharedString::from(format!("omp-effort-{level}")),
                        level.clone(),
                    )
                    .selected(current == Some(level.as_str()))
                    .selector(format!("omp-effort-{level}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        cx.emit(OmpIntent::SetThinkingLevel {
                            level: pick.clone(),
                        });
                        cx.notify();
                    }))
                    .into_any_element()
                }))
            }
            HudMenu::Model => {
                let choices = self.models.as_deref().unwrap_or_default();
                if choices.is_empty() {
                    return row.child(
                        div()
                            .text_size(Theme::TEXT_SM)
                            .text_color(theme.text_subtle)
                            .child("Loading models…"),
                    );
                }
                let current = self.session.model.as_ref();
                row.children(choices.iter().enumerate().map(|(i, choice)| {
                    let selected =
                        current.is_some_and(|m| m.provider == choice.provider && m.id == choice.id);
                    let (provider, id) = (choice.provider.clone(), choice.id.clone());
                    Pill::new(("omp-model", i), choice.label().to_owned())
                        .selected(selected)
                        .selector(format!("omp-model-{i}"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.menu = None;
                            cx.emit(OmpIntent::SetModel {
                                provider: provider.clone(),
                                model_id: id.clone(),
                            });
                            cx.notify();
                        }))
                        .into_any_element()
                }))
            }
        }
    }

    fn render_empty(theme: &Theme) -> impl IntoElement {
        let hint = |keys: &'static str, what: &'static str| {
            div()
                .flex()
                .gap(Theme::SPACE_2)
                .child(
                    div()
                        .w(px(28.0))
                        .font_family("Menlo")
                        .text_color(theme.meta)
                        .child(keys),
                )
                .child(div().text_color(theme.text_muted).child(what))
        };
        div()
            .debug_selector(|| "omp-empty".to_owned())
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(Theme::SPACE_3)
            .size_full()
            .child(
                div()
                    .text_size(Theme::TEXT_XL)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child("What should the agent do?"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_1)
                    .text_size(Theme::TEXT_SM)
                    .child(hint("/", "slash commands"))
                    .child(hint("@", "reference a file"))
                    .child(hint(
                        "!",
                        "run a shell command (!! keeps it out of context)",
                    ))
                    .child(hint("?", "ask a side question"))
                    .child(hint("⌥↵", "queue a follow-up while the agent works")),
            )
    }
}

impl Render for OmpView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = Theme::of(cx);
        let body = if self.transcript.is_empty() {
            Self::render_empty(&theme).into_any_element()
        } else {
            self.transcript.render(cx.entity_id())
        };
        let banner = self.banner().map(|b| {
            let shown = b.clone();
            banner::render(
                &b,
                Box::new(cx.listener(move |this, _, _, cx| {
                    this.dismissed = Some(shown.clone());
                    cx.notify();
                })),
                &theme,
            )
        });
        let accent = theme.accent;
        div()
            .debug_selector(|| "omp-view".to_owned())
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.surface)
            .text_color(theme.text)
            .border_1()
            .border_color(gpui::transparent_black())
            .drag_over::<ExternalPaths>(move |style, _, _, _| style.border_color(accent))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                this.composer
                    .update(cx, |composer, cx| composer.attach_paths(paths.paths(), cx));
            }))
            .child(self.render_hud(&theme, cx))
            .children(self.menu.map(|menu| self.render_menu(menu, &theme, cx)))
            .child(
                div()
                    .debug_selector(|| "omp-transcript".to_owned())
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .py(Theme::SPACE_2)
                    .child(body),
            )
            .children(banner)
            .child(div().pt(Theme::SPACE_2).child(self.composer.clone()))
    }
}
