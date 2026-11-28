//! [`OmpView`]: the native omp pane. It draws a read-only [`SessionModel`] snapshot (HUD, transcript, banner,
//! composer) and emits [`OmpIntent`]s; it never talks to omp itself.

use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, Context, Div, Entity, EventEmitter, ExternalPaths, FocusHandle, Focusable, FontWeight,
    Hsla, SharedString, Window, div, prelude::*, px, relative,
};
use xctx_omp::{Phase, SessionModel};
use xctx_ui::{Badge, Button, ButtonSize, ButtonVariant, IconSize, Pill, Spinner, Theme, Tone};

use crate::banner::{self, Banner, Exit, banners_for};
use crate::composer::{Composer, ComposerOutput, FileCompleter, PaneEscape, mode_border};
use crate::gate::GateState;
use crate::hud::{
    ModelChoice, TurnMeter, WorkingClock, compact_tokens, context_is_high, cost_label, rate_label,
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
    pub(crate) session: Arc<SessionModel>,
    /// The request omp waits on, while one is open (see [`crate::gate`]).
    pub(crate) gate: Option<GateState>,
    transcript: Transcript,
    composer: Entity<Composer>,
    spinner: Entity<Spinner>,
    clock: Entity<WorkingClock>,
    meter: TurnMeter,
    /// The folder the pane runs in and its git branch, shown in the footer.
    location: Option<(SharedString, Option<SharedString>)>,
    models: Option<Vec<ModelChoice>>,
    menu: Option<HudMenu>,
    exit: Option<Exit>,
    malformed: Option<String>,
    dismissed: Option<Banner>,
}

/// The most todo dots the rail draws; more become a `+N`.
const TODO_DOTS: usize = 12;

/// The colour of a todo's dot: done (completed or abandoned) in the accent, the one in progress in warning, the
/// rest in the border colour.
fn todo_dot(status: &str, theme: &Theme) -> Hsla {
    match status {
        "completed" | "abandoned" => theme.accent,
        "in_progress" => theme.warning,
        _ => theme.border,
    }
}

/// A short label in the dock. The selector is for tests.
fn readout(selector: &'static str, text: impl Into<SharedString>, color: Hsla) -> Div {
    div()
        .debug_selector(move || selector.to_owned())
        .text_size(Theme::TEXT_SM)
        .text_color(color)
        .child(text.into())
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
            gate: None,
            transcript: Transcript::new(),
            composer,
            spinner: cx.new(|_| Spinner::new().size(IconSize::Sm)),
            clock: cx.new(|_| WorkingClock::new()),
            meter: TurnMeter::default(),
            location: None,
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
        self.gate = match session.next_gate().cloned() {
            Some(request) if self.gate.as_ref().map(|g| &g.request_id) != Some(&request.id) => {
                Some(GateState::new(&request, cx))
            }
            Some(_) => self.gate.take(),
            None => None,
        };
        self.session = session;
        cx.notify();
    }

    /// Show where the pane runs: the folder (its last component is shown) and the git branch checked out there.
    pub fn set_location(&mut self, folder: &str, branch: Option<&str>, cx: &mut Context<Self>) {
        let name = std::path::Path::new(folder)
            .file_name()
            .map_or_else(|| folder.to_owned(), |n| n.to_string_lossy().into_owned());
        self.location = Some((
            name.into(),
            branch.map(|b| SharedString::from(b.to_owned())),
        ));
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

    /// The dock's top rail: the working state with Stop, the needs-you badge, the turn's todos, and the context meter.
    fn render_rail(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
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
        let todos = todo_progress(session).map(|(done, total)| {
            let statuses: Vec<&str> = session
                .todos
                .iter()
                .flat_map(|phase| &phase.tasks)
                .map(|task| task.status.as_str())
                .collect();
            div()
                .flex()
                .items_center()
                .gap(px(3.0))
                .children(statuses.iter().take(TODO_DOTS).map(|status| {
                    div()
                        .size(px(6.0))
                        .rounded_full()
                        .bg(todo_dot(status, theme))
                }))
                .children((statuses.len() > TODO_DOTS).then(|| {
                    readout(
                        "omp-hud-todos-more",
                        format!("+{}", statuses.len() - TODO_DOTS),
                        theme.text_subtle,
                    )
                }))
                .child(readout(
                    "omp-hud-todos",
                    format!("{done}/{total}"),
                    theme.text_muted,
                ))
        });
        div()
            .debug_selector(|| "omp-rail".to_owned())
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
            .children(todos)
            .child(div().flex_1())
            .children(self.render_context(theme))
    }

    /// The context meter: how full the model's window is, with the window's size. Warns from 80%.
    fn render_context(&self, theme: &Theme) -> Option<impl IntoElement> {
        let usage = self.session.context?;
        let percent = usage.percent.clamp(0.0, 100.0);
        let color = if context_is_high(&self.session) {
            theme.warning
        } else {
            theme.accent
        };
        Some(
            div()
                .debug_selector(|| "omp-hud-context".to_owned())
                .flex()
                .items_center()
                .gap(Theme::SPACE_1)
                .child(
                    div()
                        .relative()
                        .w(px(96.0))
                        .h(px(4.0))
                        .rounded_full()
                        .bg(theme.border_subtle)
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .h_full()
                                .w(relative(percent as f32 / 100.0))
                                .rounded_full()
                                .bg(color),
                        ),
                )
                .child(readout(
                    "omp-hud-context-percent",
                    format!("{percent:.0}%"),
                    color,
                ))
                .child(readout(
                    "omp-hud-context-window",
                    compact_tokens(usage.window),
                    theme.meta,
                )),
        )
    }

    /// The dock's footer: the model and thinking level to change, the folder and branch the pane runs in, and the
    /// turn's cost and speed.
    fn render_footer(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let session = &self.session;
        let model_label: SharedString = session
            .model
            .as_ref()
            .map_or_else(|| "model".into(), |m| m.label().to_owned().into());
        let effort_label: SharedString = session
            .thinking
            .clone()
            .map_or_else(|| "effort".into(), SharedString::from);
        let location = self.location.as_ref().map(|(folder, branch)| {
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_1)
                .min_w_0()
                .child(
                    readout("omp-location", folder.clone(), theme.text_muted)
                        .min_w_0()
                        .truncate(),
                )
                .children(branch.clone().map(|branch| {
                    readout("omp-branch", format!("⎇ {branch}"), theme.meta).flex_none()
                }))
        });
        div()
            .debug_selector(|| "omp-footer".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .gap(Theme::SPACE_2)
            .w_full()
            .h(px(32.0))
            .px(Theme::SPACE_3)
            .border_t_1()
            .border_color(theme.border_subtle)
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
            .child(div().flex_1().min_w_0().children(location))
            .child(readout("omp-hud-cost", cost_label(session), theme.meta))
            .children(
                self.meter
                    .rate()
                    .map(|rate| readout("omp-hud-rate", rate_label(rate), theme.meta)),
            )
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let (mode, focused) = {
            let composer = self.composer.read(cx);
            (composer.mode(cx), composer.is_focused(window, cx))
        };
        let dock = div()
            .debug_selector(|| "omp-dock".to_owned())
            .flex()
            .flex_col()
            .flex_none()
            .mx(Theme::SPACE_3)
            .mt(Theme::SPACE_2)
            .mb(Theme::SPACE_3)
            .rounded(Theme::RADIUS_LG)
            .border_1()
            .when(mode.label().is_some(), |dock| dock.border_2())
            .border_color(mode_border(mode, focused, &theme))
            .bg(theme.raised)
            .overflow_hidden()
            .child(self.render_rail(&theme, cx))
            .child(
                div()
                    .px(Theme::SPACE_2)
                    .pt(Theme::SPACE_1)
                    .child(self.composer.clone()),
            )
            .child(self.render_footer(&theme, cx));
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
            .child(
                div()
                    .debug_selector(|| "omp-transcript".to_owned())
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .py(Theme::SPACE_2)
                    .child(body),
            )
            .children(self.render_gate(&theme, cx))
            .children(banner)
            .children(self.menu.map(|menu| self.render_menu(menu, &theme, cx)))
            .child(dock)
    }
}
