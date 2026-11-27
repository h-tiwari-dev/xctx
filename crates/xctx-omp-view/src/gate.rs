//! The request omp waits on, drawn above the composer: a tool approval, a `select`, a `confirm`, an `input`, an
//! `editor` or an `ask` form. The answer leaves as [`OmpIntent::Answer`]. A form is checked first (see
//! [`xctx_omp::form::Form::answer`]), so an answer omp would reject is never sent.

use std::collections::BTreeMap;

use gpui::{
    AnyElement, Context, Entity, FontWeight, IntoElement, ParentElement, SharedString, Styled,
    Subscription, div, prelude::*,
};
use xctx_omp::UiMethod;
use xctx_omp::UiRequest;
use xctx_omp::form::{Answers, ChoiceControl, FieldError, FieldKind, Form, Item, TextKind};
use xctx_ui::{Button, ButtonSize, ButtonVariant, Pill, PromptEvent, PromptInput, Theme};

use crate::intent::{GateAnswer, OmpIntent};
use crate::view::OmpView;

/// What the gate on screen holds while the user answers it.
pub(crate) struct GateState {
    /// The request's id, as omp sent it.
    pub(crate) request_id: String,
    /// The parsed form, for a `form` request.
    form: Option<Form>,
    /// The answers so far: a form's defaults to begin with, then what the user chose.
    answers: Answers,
    /// The text inputs, by field name (`""` for an `input` or `editor`).
    inputs: BTreeMap<String, Entity<PromptInput>>,
    /// The "Other" free-text inputs of choice fields, by field name.
    others: BTreeMap<String, Entity<PromptInput>>,
    /// Why the form could not be sent, by field.
    errors: Vec<FieldError>,
    /// Keeps the Enter subscriptions of single-answer inputs alive.
    _subscriptions: Vec<Subscription>,
}

impl GateState {
    /// The gate for `request`, with its inputs filled from the request's own defaults.
    pub(crate) fn new(request: &UiRequest, cx: &mut Context<OmpView>) -> GateState {
        let mut gate = GateState {
            request_id: request.id.clone(),
            form: None,
            answers: Answers::default(),
            inputs: BTreeMap::new(),
            others: BTreeMap::new(),
            errors: Vec::new(),
            _subscriptions: Vec::new(),
        };
        match &request.method {
            // A secret is never echoed: `PromptInput` has no masking, so it is answered in the terminal.
            UiMethod::Input {
                placeholder,
                secret: false,
                ..
            } => {
                let input = single_input(placeholder, false, "", cx);
                gate.subscribe_submit(&input, cx);
                gate.inputs.insert(String::new(), input);
            }
            UiMethod::Editor { prefill, .. } => {
                let input = single_input("", true, prefill, cx);
                gate.subscribe_submit(&input, cx);
                gate.inputs.insert(String::new(), input);
            }
            UiMethod::Form { html } => {
                let form = Form::parse(html);
                gate.answers = Answers::defaults(&form);
                for field in &form.fields {
                    match &field.kind {
                        FieldKind::Text(text) => {
                            let input = single_input(
                                &text.placeholder,
                                text.kind == TextKind::Multiline,
                                &text.value,
                                cx,
                            );
                            gate.inputs.insert(field.name.clone(), input);
                        }
                        FieldKind::Choice(choice) if choice.other => {
                            let input = single_input("Other", false, "", cx);
                            gate.others.insert(field.name.clone(), input);
                        }
                        FieldKind::Choice(_) | FieldKind::Hidden(_) => {}
                    }
                }
                gate.form = Some(form);
            }
            _ => {}
        }
        gate
    }

    /// Enter in `input` answers the request with its text.
    fn subscribe_submit(&mut self, input: &Entity<PromptInput>, cx: &mut Context<OmpView>) {
        self._subscriptions
            .push(cx.subscribe(input, |this, _, event: &PromptEvent, cx| {
                if let PromptEvent::Submit(text) = event {
                    this.answer(GateAnswer::Value(text.clone()), cx);
                }
            }));
    }
}

/// A text input for a gate, starting at `value`.
fn single_input(
    placeholder: &str,
    multiline: bool,
    value: &str,
    cx: &mut Context<OmpView>,
) -> Entity<PromptInput> {
    let placeholder = placeholder.to_owned();
    let value = value.to_owned();
    cx.new(|cx| {
        let mut input = PromptInput::new(cx)
            .placeholder(placeholder)
            .multiline(multiline);
        input.set_value(&value, cx);
        input
    })
}

impl OmpView {
    /// Send the user's answer to the request on screen, and clear the gate until omp says what is next.
    pub(crate) fn answer(&mut self, answer: GateAnswer, cx: &mut Context<Self>) {
        let Some(gate) = self.gate.as_ref() else {
            return;
        };
        cx.emit(OmpIntent::Answer {
            request_id: gate.request_id.clone(),
            answer,
        });
        cx.notify();
    }

    /// Choose `value` of the choice field `name`: one option for a radio or drop-down, a toggle for a checkbox.
    fn toggle_choice(&mut self, name: &str, value: &str, multiple: bool, cx: &mut Context<Self>) {
        let Some(gate) = self.gate.as_mut() else {
            return;
        };
        let chosen = gate.answers.chosen.entry(name.to_owned()).or_default();
        if !multiple {
            chosen.clear();
            chosen.push(value.to_owned());
        } else if let Some(at) = chosen.iter().position(|v| v == value) {
            chosen.remove(at);
        } else {
            chosen.push(value.to_owned());
        }
        cx.notify();
    }

    /// Read what the form's inputs hold, check it as omp would, and send it, or show what fails.
    fn submit_form(&mut self, cx: &mut Context<Self>) {
        let Some(gate) = self.gate.as_mut() else {
            return;
        };
        for (name, input) in &gate.inputs {
            gate.answers
                .text
                .insert(name.clone(), input.read(cx).value().to_owned());
        }
        for (name, input) in &gate.others {
            gate.answers
                .other
                .insert(name.clone(), input.read(cx).value().to_owned());
        }
        let Some(form) = gate.form.as_ref() else {
            return;
        };
        match form.answer(&gate.answers) {
            Ok(data) => {
                gate.errors.clear();
                self.answer(GateAnswer::Form(data), cx);
            }
            Err(errors) => {
                gate.errors = errors;
                cx.notify();
            }
        }
    }

    /// The gate on screen, above the composer, when omp waits on the user.
    pub(crate) fn render_gate(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let gate = self.gate.as_ref()?;
        let request = self.session.next_gate()?;
        let body: AnyElement = match &request.method {
            _ if request.is_approval() => {
                let approval = request.approval()?;
                self.approval_body(
                    &approval.tool,
                    &approval.detail,
                    &approval.options,
                    cx,
                    theme,
                )
            }
            UiMethod::Select { title, options } => self.select_body(title, options, cx, theme),
            UiMethod::Confirm { title, message } => div()
                .flex()
                .flex_col()
                .gap(Theme::SPACE_2)
                .child(text_line(title, theme.text, true))
                .child(text_line(message, theme.text_muted, false))
                .child(
                    div()
                        .flex()
                        .gap(Theme::SPACE_2)
                        .child(
                            Button::new("gate-yes", "Yes")
                                .variant(ButtonVariant::Primary)
                                .size(ButtonSize::Compact)
                                .selector("gate-yes")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.answer(GateAnswer::Confirmed(true), cx)
                                })),
                        )
                        .child(
                            Button::new("gate-no", "No")
                                .variant(ButtonVariant::Secondary)
                                .size(ButtonSize::Compact)
                                .selector("gate-no")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.answer(GateAnswer::Confirmed(false), cx)
                                })),
                        ),
                )
                .into_any_element(),
            UiMethod::Input {
                title,
                secret: false,
                ..
            }
            | UiMethod::Editor { title, .. } => {
                let input = gate.inputs.get("").cloned();
                let mut column = div()
                    .flex()
                    .flex_col()
                    .gap(Theme::SPACE_2)
                    .child(text_line(title, theme.text, true));
                if let Some(input) = input {
                    column = column.child(input);
                }
                column
                    .child(
                        div().flex().gap(Theme::SPACE_2).child(
                            Button::new("gate-submit", "Submit")
                                .variant(ButtonVariant::Primary)
                                .size(ButtonSize::Compact)
                                .selector("gate-submit")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let text = this
                                        .gate
                                        .as_ref()
                                        .and_then(|g| g.inputs.get(""))
                                        .map(|i| i.read(cx).value().to_owned())
                                        .unwrap_or_default();
                                    this.answer(GateAnswer::Value(text), cx)
                                })),
                        ),
                    )
                    .into_any_element()
            }
            UiMethod::Form { .. } => self.form_body(gate, cx, theme),
            // Not drawn here yet: the question stays open until it is answered in the terminal or cancelled.
            _ => div()
                .flex()
                .flex_col()
                .gap(Theme::SPACE_2)
                .child(text_line(
                    "This question needs the terminal. Open in terminal answers it.",
                    theme.text_muted,
                    false,
                ))
                .into_any_element(),
        };
        let timeout = request
            .timeout_ms
            .map(|ms| format!("times out in {}s", ms.div_ceil(1000)));
        Some(
            div()
                .debug_selector(|| "omp-gate".to_owned())
                .flex()
                .flex_col()
                .flex_none()
                .gap(Theme::SPACE_2)
                .px(Theme::SPACE_3)
                .py(Theme::SPACE_2)
                .border_t_1()
                .border_color(theme.border_subtle)
                .bg(theme.raised)
                .child(body)
                .children(timeout.map(|t| text_line(t, theme.text_subtle, false)))
                .child(
                    div().flex().justify_end().child(
                        Button::new("gate-cancel", "Cancel")
                            .variant(ButtonVariant::Ghost)
                            .size(ButtonSize::Compact)
                            .selector("gate-cancel")
                            .on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.answer(GateAnswer::Cancelled, cx)
                                }),
                            ),
                    ),
                )
                .into_any_element(),
        )
    }

    fn approval_body(
        &self,
        tool: &str,
        detail: &str,
        options: &[String],
        cx: &mut Context<Self>,
        theme: &Theme,
    ) -> AnyElement {
        let buttons =
            options.iter().enumerate().map(|(i, option)| {
                let value = option.clone();
                let deny = xctx_omp::Permission::is_deny(option);
                Button::new(
                    SharedString::from(format!("gate-option-{i}")),
                    option.clone(),
                )
                .variant(if deny {
                    ButtonVariant::Danger
                } else {
                    ButtonVariant::Secondary
                })
                .size(ButtonSize::Compact)
                .selector(format!("gate-option-{i}"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.answer(GateAnswer::Value(value.clone()), cx)
                }))
            });
        div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_2)
            .child(text_line(&format!("Allow tool: {tool}"), theme.text, true))
            .child(
                div()
                    .font_family("Menlo")
                    .text_size(Theme::TEXT_SM)
                    .text_color(theme.text_muted)
                    .child(detail.to_owned()),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(Theme::SPACE_2)
                    .children(buttons),
            )
            .into_any_element()
    }

    fn select_body(
        &self,
        title: &str,
        options: &[String],
        cx: &mut Context<Self>,
        theme: &Theme,
    ) -> AnyElement {
        let buttons =
            options.iter().enumerate().map(|(i, option)| {
                let value = option.clone();
                Button::new(
                    SharedString::from(format!("gate-option-{i}")),
                    option.clone(),
                )
                .variant(ButtonVariant::Secondary)
                .size(ButtonSize::Compact)
                .selector(format!("gate-option-{i}"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.answer(GateAnswer::Value(value.clone()), cx)
                }))
            });
        div()
            .flex()
            .flex_col()
            .gap(Theme::SPACE_2)
            .child(text_line(title, theme.text, true))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(Theme::SPACE_2)
                    .children(buttons),
            )
            .into_any_element()
    }

    fn form_body(&self, gate: &GateState, cx: &mut Context<Self>, theme: &Theme) -> AnyElement {
        let Some(form) = gate.form.as_ref() else {
            return div().into_any_element();
        };
        let mut column = div().flex().flex_col().gap(Theme::SPACE_2);
        for item in &form.items {
            match item {
                Item::Note(note) => column = column.child(text_line(note, theme.text_muted, false)),
                Item::Field(at) => {
                    let field = &form.fields[*at];
                    let label = if field.required {
                        format!("{} *", field.label)
                    } else {
                        field.label.clone()
                    };
                    column = column.child(text_line(&label, theme.text, true));
                    match &field.kind {
                        FieldKind::Text(_) => {
                            if let Some(input) = gate.inputs.get(&field.name) {
                                column = column.child(input.clone());
                            }
                        }
                        FieldKind::Choice(choice) => {
                            let multiple =
                                choice.multiple || choice.control == ChoiceControl::Checkbox;
                            let chosen = gate.answers.chosen.get(&field.name);
                            let pills = choice.options.iter().enumerate().map(|(i, option)| {
                                let (name, value) = (field.name.clone(), option.value.clone());
                                let selected = chosen.is_some_and(|c| c.contains(&option.value));
                                Pill::new(
                                    SharedString::from(format!("gate-{}-{i}", field.name)),
                                    option.label.clone(),
                                )
                                .selected(selected)
                                .disabled(option.disabled)
                                .selector(format!("gate-{}-{i}", field.name))
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.toggle_choice(&name, &value, multiple, cx)
                                    },
                                ))
                            });
                            column = column.child(
                                div().flex().flex_wrap().gap(Theme::SPACE_1).children(pills),
                            );
                            if let Some(other) = gate.others.get(&field.name) {
                                column = column.child(other.clone());
                            }
                        }
                        FieldKind::Hidden(_) => {}
                    }
                }
            }
        }
        for error in &gate.errors {
            column = column.child(
                div()
                    .text_size(Theme::TEXT_SM)
                    .text_color(theme.danger)
                    .child(format!("{}: {}", error.name, error.message)),
            );
        }
        column
            .child(
                div().flex().gap(Theme::SPACE_2).child(
                    Button::new("gate-submit", "Submit")
                        .variant(ButtonVariant::Primary)
                        .size(ButtonSize::Compact)
                        .selector("gate-submit")
                        .on_click(cx.listener(|this, _, _, cx| this.submit_form(cx))),
                ),
            )
            .into_any_element()
    }
}

/// A line of gate text: a title in semibold when `strong`, else plain.
fn text_line(text: impl Into<SharedString>, color: gpui::Hsla, strong: bool) -> impl IntoElement {
    div()
        .text_size(Theme::TEXT_SM)
        .text_color(color)
        .font_weight(if strong {
            FontWeight::SEMIBOLD
        } else {
            FontWeight::NORMAL
        })
        .child(text.into())
}
