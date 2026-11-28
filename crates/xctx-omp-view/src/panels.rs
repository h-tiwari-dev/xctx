//! Collapsible session drawers inspired by Tern's native omp UI:
//!
//! - **Subagent tray & peek** (`omp.tray.*`): live status, tool count, tokens, duration, recent output with Copy,
//!   Cancel, and Steer for running subagents.
//! - **Side questions (`btw`)** (`omp.btw.*`): streaming markdown answers to `?` questions with 1-click Copy.
//! - **Todo drawer** (`omp.todo.*`): phases and task checklist with Copy as Markdown.
//! - **Session info & actions** (`omp.info.*`): session ID/path (with Copy), stats, Copy transcript, Copy last
//!   reply, Compact, Fork, and New session.

use std::rc::Rc;

use gpui::{AnyElement, App, Entity, FontWeight, Hsla, SharedString, Window, div, prelude::*, px};
use serde_json::Value;
use xctx_omp::{BtwRecord, SessionModel, Subagent, TodoPhase};
use xctx_ui::{
    Badge, Button, ButtonSize, ButtonVariant, Icon, IconName, IconSize, PromptInput, Theme, Tone,
};

use crate::changes::{CopyCallback, VoidCallback};
use crate::hud::{compact_tokens, cost_label, format_elapsed};
use crate::intent::OmpIntent;
use crate::markdown;

/// Callback invoked to refresh the pane's list of persisted sessions from disk.
pub type SessionsRefresher = Rc<dyn Fn(&mut App)>;

/// Callback invoked when the user asks to open a new independent agent session side-by-side.
pub type SplitAgentHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// One persisted session available to view or switch to in an [`crate::OmpView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionEntry {
    /// Session id.
    pub id: SharedString,
    /// Full path to the `.jsonl` file passed to [`OmpIntent::SwitchSession`].
    pub path: SharedString,
    /// Display title (explicit title, else first prompt snippet, else short id).
    pub title: SharedString,
    /// Optional first user prompt preview when distinct from `title`.
    pub preview: Option<SharedString>,
    /// Number of user and assistant turns recorded in this session.
    pub message_count: usize,
    /// Short human-readable timestamp (e.g. `"2m ago"`, `"Nov 26"`).
    pub updated_label: SharedString,
    /// Whether this session is currently active in the pane.
    pub active: bool,
}

pub(crate) type SelectSubagent = Rc<dyn Fn(Option<String>, &mut App)>;
pub(crate) type IntentCallback = Rc<dyn Fn(OmpIntent, &mut App)>;

/// Format the session's `goal` JSON value into a single short display line, if present.
pub fn goal_summary(goal: &Value) -> Option<String> {
    let raw = match goal {
        Value::String(s) => s.as_str(),
        Value::Object(map) => ["title", "goal", "summary", "text", "description"]
            .iter()
            .find_map(|k| map.get(*k).and_then(Value::as_str))?,
        _ => return None,
    };
    let line = raw.lines().find(|l| !l.trim().is_empty())?.trim();
    (!line.is_empty()).then(|| line.to_owned())
}

/// Format all todo phases and items as a Markdown task checklist suitable for copying.
pub fn todos_markdown(phases: &[TodoPhase]) -> String {
    let mut out = String::new();
    for phase in phases {
        if !out.is_empty() {
            out.push('\n');
        }
        if !phase.name.trim().is_empty() {
            out.push_str(&format!("## {}\n", phase.name.trim()));
        }
        for task in &phase.tasks {
            let box_mark = match task.status.as_str() {
                "completed" => "[x]",
                "in_progress" => "[-]",
                "abandoned" => "[~]",
                _ => "[ ]",
            };
            out.push_str(&format!("- {box_mark} {}\n", task.content));
        }
    }
    out
}

/// Whether `subagent` is currently active (can be cancelled or steered).
pub fn subagent_is_active(subagent: &Subagent) -> bool {
    matches!(subagent.status.as_str(), "started" | "running")
}

fn subagent_tone(status: &str) -> Tone {
    match status {
        "started" | "running" => Tone::Accent,
        "completed" => Tone::Success,
        "failed" | "aborted" => Tone::Danger,
        _ => Tone::Neutral,
    }
}

fn subagent_dot(status: &str, theme: &Theme) -> Hsla {
    match status {
        "started" | "running" => theme.accent,
        "completed" => theme.success,
        "failed" | "aborted" => theme.danger,
        _ => theme.text_subtle,
    }
}

/// Inputs for rendering the Subagent Tray & Peek drawer.
pub(crate) struct SubagentTrayProps<'a> {
    pub session: &'a SessionModel,
    pub selected: Option<&'a str>,
    pub steer_input: Entity<PromptInput>,
    pub copied: Option<&'a str>,
    pub on_select: SelectSubagent,
    pub on_intent: IntentCallback,
    pub on_copy: CopyCallback,
    pub on_close: VoidCallback,
}

/// Render the collapsible Subagent Tray above the dock.
pub(crate) fn render_subagent_tray(props: SubagentTrayProps<'_>, theme: &Theme) -> AnyElement {
    let active_count = props
        .session
        .subagents
        .values()
        .filter(|s| subagent_is_active(s))
        .count();
    let total = props.session.subagents.len();
    let on_close = props.on_close.clone();
    let hover = theme.hover;
    let selected_bg = theme.selection;

    let header =
        div()
            .flex()
            .items_center()
            .gap(Theme::SPACE_2)
            .w_full()
            .h(px(28.0))
            .px(Theme::SPACE_3)
            .border_b_1()
            .border_color(theme.border_subtle)
            .child(Icon::new(IconName::Orchestrator).size(IconSize::Xs).color(
                if active_count > 0 {
                    theme.accent
                } else {
                    theme.text_muted
                },
            ))
            .child(
                div()
                    .text_size(Theme::TEXT_SM)
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(format!("Subagents ({total})")),
            )
            .when(active_count > 0, |row| {
                row.child(Badge::new(format!("{active_count} running")).tone(Tone::Accent))
            })
            .child(div().flex_1())
            .child(
                Button::new("omp-tray-close", "")
                    .icon(IconName::Close)
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .tooltip("Hide subagent tray")
                    .selector("omp-tray-close")
                    .on_click(move |_, _, cx| on_close(cx)),
            );

    let rows = div()
        .id("omp-subagent-list")
        .flex()
        .flex_col()
        .w_full()
        .max_h(px(140.0))
        .overflow_y_scroll()
        .children(props.session.subagents.values().map(|sub| {
            let is_sel = props.selected == Some(sub.id.as_str());
            let id = sub.id.clone();
            let toggle_id = if is_sel { None } else { Some(id.clone()) };
            let on_select = props.on_select.clone();
            let sel_name = format!("omp-subagent-{}", sub.id);
            let elapsed = format_elapsed(std::time::Duration::from_millis(sub.duration_ms));
            let active = subagent_is_active(sub);
            let on_cancel = props.on_intent.clone();
            let cancel_id = sub.id.clone();
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_1)
                .w_full()
                .px(Theme::SPACE_2)
                .when(is_sel, |row| row.bg(selected_bg))
                .child(
                    div()
                        .id(SharedString::from(format!("omp-subagent-row-{}", sub.id)))
                        .debug_selector(move || sel_name)
                        .flex()
                        .flex_1()
                        .min_w_0()
                        .items_center()
                        .gap(Theme::SPACE_2)
                        .px(Theme::SPACE_1)
                        .py(px(4.0))
                        .rounded(Theme::RADIUS_SM)
                        .cursor_pointer()
                        .hover(move |s| s.bg(hover))
                        .on_click(move |_, _, cx| on_select(toggle_id.clone(), cx))
                        .child(
                            div()
                                .flex_none()
                                .size(px(7.0))
                                .rounded_full()
                                .bg(subagent_dot(&sub.status, theme)),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_family("Menlo")
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(Theme::TEXT_XS)
                                .text_color(theme.text)
                                .child(sub.id.clone()),
                        )
                        .when(!sub.agent.is_empty(), |row| {
                            row.child(
                                Badge::new(sub.agent.clone()).tone(subagent_tone(&sub.status)),
                            )
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_size(Theme::TEXT_XS)
                                .text_color(theme.text_muted)
                                .child(if !sub.current_tool.is_empty() && active {
                                    sub.current_tool.clone()
                                } else {
                                    sub.task.clone()
                                }),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_family("Menlo")
                                .text_size(Theme::TEXT_XS)
                                .text_color(theme.text_subtle)
                                .child(format!(
                                    "{} tools · {} · {elapsed}",
                                    sub.tool_count,
                                    compact_tokens(sub.tokens)
                                )),
                        ),
                )
                .when(active, |row| {
                    row.child(
                        Button::new(
                            SharedString::from(format!("omp-sub-cancel-{}", sub.id)),
                            "Cancel",
                        )
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Compact)
                        .tooltip("Cancel this subagent")
                        .selector(format!("omp-subagent-cancel-{}", sub.id))
                        .on_click(move |_, _, cx| {
                            on_cancel(
                                OmpIntent::CancelSubagent {
                                    id: cancel_id.clone(),
                                },
                                cx,
                            );
                        }),
                    )
                })
        }));

    let peek = props
        .selected
        .and_then(|id| props.session.subagents.get(id))
        .map(|sub| {
            let output_text = sub.recent_output.join("\n");
            let has_output = !output_text.trim().is_empty();
            let copy_key = format!("subagent-{}", sub.id);
            let copied = props.copied == Some(copy_key.as_str());
            let on_copy = props.on_copy.clone();
            let active = subagent_is_active(sub);
            let steer_input = props.steer_input.clone();
            let steer_input_send = props.steer_input.clone();
            let steer_id = sub.id.clone();
            let on_steer = props.on_intent.clone();

            div()
                .debug_selector(|| "omp-subagent-peek".to_owned())
                .flex()
                .flex_col()
                .gap(Theme::SPACE_1)
                .w_full()
                .px(Theme::SPACE_3)
                .py(Theme::SPACE_2)
                .border_t_1()
                .border_color(theme.border_subtle)
                .bg(theme.surface)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(Theme::SPACE_2)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(Theme::TEXT_XS)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(if sub.task.is_empty() {
                                    sub.id.clone()
                                } else {
                                    sub.task.clone()
                                }),
                        )
                        .child(Badge::new(sub.status.clone()).tone(subagent_tone(&sub.status)))
                        .when(has_output, |row| {
                            row.child(
                                Button::new(
                                    "omp-subagent-copy-output",
                                    if copied { "Copied" } else { "Copy output" },
                                )
                                .icon(if copied {
                                    IconName::Check
                                } else {
                                    IconName::Copy
                                })
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Compact)
                                .selector("omp-subagent-copy-output")
                                .on_click(move |_, _, cx| {
                                    on_copy(copy_key.clone(), output_text.clone(), cx);
                                }),
                            )
                        }),
                )
                .when(has_output, |col| {
                    col.child(
                        div()
                            .id("omp-subagent-output")
                            .max_h(px(120.0))
                            .overflow_y_scroll()
                            .p(Theme::SPACE_2)
                            .rounded(Theme::RADIUS_SM)
                            .bg(theme.raised)
                            .border_1()
                            .border_color(theme.border_subtle)
                            .font_family("Menlo")
                            .text_size(Theme::TEXT_XS)
                            .text_color(theme.text_muted)
                            .child(sub.recent_output.join("\n")),
                    )
                })
                .when(active, |col| {
                    col.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(Theme::SPACE_2)
                            .pt(Theme::SPACE_1)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .px(Theme::SPACE_2)
                                    .py(px(2.0))
                                    .rounded(Theme::RADIUS_SM)
                                    .bg(theme.raised)
                                    .border_1()
                                    .border_color(theme.border_subtle)
                                    .child(steer_input),
                            )
                            .child(
                                Button::new("omp-subagent-steer", "Steer")
                                    .variant(ButtonVariant::Primary)
                                    .size(ButtonSize::Compact)
                                    .selector("omp-subagent-steer")
                                    .on_click(move |_, _, cx| {
                                        let msg =
                                            steer_input_send.read(cx).value().trim().to_owned();
                                        if msg.is_empty() {
                                            return;
                                        }
                                        steer_input_send.update(cx, |input, cx| input.clear(cx));
                                        on_steer(
                                            OmpIntent::SteerSubagent {
                                                id: steer_id.clone(),
                                                message: msg,
                                            },
                                            cx,
                                        );
                                    }),
                            ),
                    )
                })
        });

    div()
        .debug_selector(|| "omp-subagent-tray".to_owned())
        .flex()
        .flex_col()
        .flex_none()
        .w_full()
        .border_t_1()
        .border_color(theme.border_subtle)
        .bg(theme.raised)
        .child(header)
        .child(rows)
        .children(peek)
        .into_any_element()
}

fn btw_tone(status: &str) -> Tone {
    match status {
        "running" => Tone::Accent,
        "done" => Tone::Success,
        "error" => Tone::Danger,
        _ => Tone::Neutral,
    }
}

/// Render the collapsible Side Questions (`btw`) panel above the dock.
pub(crate) fn render_btw_panel(
    records: &[BtwRecord],
    copied: Option<&str>,
    on_copy: CopyCallback,
    on_close: Rc<dyn Fn(&mut App)>,
    theme: &Theme,
) -> AnyElement {
    let header = div()
        .flex()
        .items_center()
        .gap(Theme::SPACE_2)
        .w_full()
        .h(px(28.0))
        .px(Theme::SPACE_3)
        .border_b_1()
        .border_color(theme.border_subtle)
        .child(
            div()
                .text_size(Theme::TEXT_SM)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.meta)
                .child(format!("Side questions ({})", records.len())),
        )
        .child(div().flex_1())
        .child(
            Button::new("omp-btw-close", "")
                .icon(IconName::Close)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .tooltip("Hide side questions")
                .selector("omp-btw-close")
                .on_click(move |_, _, cx| on_close(cx)),
        );

    let cards = div()
        .id("omp-btw-list")
        .flex()
        .flex_col()
        .gap(Theme::SPACE_2)
        .w_full()
        .max_h(px(220.0))
        .overflow_y_scroll()
        .p(Theme::SPACE_2)
        .children(records.iter().enumerate().rev().map(|(i, rec)| {
            let copy_key = format!("btw-{i}");
            let is_copied = copied == Some(copy_key.as_str());
            let on_copy_btn = on_copy.clone();
            let answer_text = rec.answer.clone();
            let has_answer = !answer_text.trim().is_empty();
            let parsed = has_answer.then(|| markdown::parse(&rec.answer));

            div()
                .debug_selector(move || format!("omp-btw-card-{i}"))
                .flex()
                .flex_col()
                .gap(Theme::SPACE_1)
                .w_full()
                .p(Theme::SPACE_2)
                .rounded(Theme::RADIUS_MD)
                .bg(theme.surface)
                .border_1()
                .border_color(theme.border_subtle)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(Theme::SPACE_2)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(Theme::TEXT_SM)
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme.text)
                                .child(format!("? {}", rec.question)),
                        )
                        .child(Badge::new(rec.status.clone()).tone(btw_tone(&rec.status)))
                        .when(has_answer, |row| {
                            row.child(
                                Button::new(
                                    ("omp-btw-copy", i),
                                    if is_copied { "Copied" } else { "Copy" },
                                )
                                .icon(if is_copied {
                                    IconName::Check
                                } else {
                                    IconName::Copy
                                })
                                .variant(ButtonVariant::Ghost)
                                .size(ButtonSize::Compact)
                                .selector(format!("omp-btw-copy-{i}"))
                                .on_click(move |_, _, cx| {
                                    on_copy_btn(copy_key.clone(), answer_text.clone(), cx);
                                }),
                            )
                        }),
                )
                .child(match &parsed {
                    Some(blocks) => markdown::render_with_copy(
                        blocks,
                        &format!("btw-{i}"),
                        copied,
                        Some(on_copy.clone()),
                        theme,
                    ),
                    None => div()
                        .text_size(Theme::TEXT_SM)
                        .italic()
                        .text_color(theme.text_subtle)
                        .child(if rec.status == "running" {
                            "Thinking…"
                        } else {
                            "No answer."
                        })
                        .into_any_element(),
                })
        }));

    div()
        .debug_selector(|| "omp-btw-panel".to_owned())
        .flex()
        .flex_col()
        .flex_none()
        .w_full()
        .border_t_1()
        .border_color(theme.border_subtle)
        .bg(theme.raised)
        .child(header)
        .child(cards)
        .into_any_element()
}

/// Render the expandable Todo Checklist drawer above the dock.
pub(crate) fn render_todo_drawer(
    phases: &[TodoPhase],
    copied: Option<&str>,
    on_copy: CopyCallback,
    on_close: Rc<dyn Fn(&mut App)>,
    theme: &Theme,
) -> AnyElement {
    let md = todos_markdown(phases);
    let is_copied = copied == Some("todos");
    let header = div()
        .flex()
        .items_center()
        .gap(Theme::SPACE_2)
        .w_full()
        .h(px(28.0))
        .px(Theme::SPACE_3)
        .border_b_1()
        .border_color(theme.border_subtle)
        .child(
            div()
                .text_size(Theme::TEXT_SM)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child("Tasks"),
        )
        .child(div().flex_1())
        .child(
            Button::new("omp-todo-copy", if is_copied { "Copied" } else { "Copy" })
                .icon(if is_copied {
                    IconName::Check
                } else {
                    IconName::Copy
                })
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .disabled(md.is_empty())
                .tooltip("Copy tasks as Markdown checklist")
                .selector("omp-todo-copy")
                .on_click(move |_, _, cx| on_copy("todos".to_owned(), md.clone(), cx)),
        )
        .child(
            Button::new("omp-todo-close", "")
                .icon(IconName::Close)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .tooltip("Hide tasks")
                .selector("omp-todo-close")
                .on_click(move |_, _, cx| on_close(cx)),
        );

    let list = div()
        .id("omp-todo-list")
        .flex()
        .flex_col()
        .gap(Theme::SPACE_2)
        .w_full()
        .max_h(px(200.0))
        .overflow_y_scroll()
        .px(Theme::SPACE_3)
        .py(Theme::SPACE_2)
        .children(phases.iter().map(|phase| {
            div()
                .flex()
                .flex_col()
                .gap(Theme::SPACE_1)
                .when(!phase.name.trim().is_empty(), |col| {
                    col.child(
                        div()
                            .text_size(Theme::TEXT_XS)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme.text_subtle)
                            .child(phase.name.clone()),
                    )
                })
                .children(phase.tasks.iter().map(|task| {
                    let (marker, color, muted) = match task.status.as_str() {
                        "completed" => ("✓", theme.success, true),
                        "in_progress" => ("●", theme.warning, false),
                        "abandoned" => ("×", theme.text_subtle, true),
                        _ => ("○", theme.text_subtle, false),
                    };
                    div()
                        .flex()
                        .items_start()
                        .gap(Theme::SPACE_2)
                        .text_size(Theme::TEXT_SM)
                        .child(
                            div()
                                .flex_none()
                                .w(px(12.0))
                                .text_color(color)
                                .child(marker),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_color(if muted { theme.text_muted } else { theme.text })
                                .child(task.content.clone()),
                        )
                }))
        }));

    div()
        .debug_selector(|| "omp-todo-drawer".to_owned())
        .flex()
        .flex_col()
        .flex_none()
        .w_full()
        .border_t_1()
        .border_color(theme.border_subtle)
        .bg(theme.raised)
        .child(header)
        .child(list)
        .into_any_element()
}

/// Inputs for rendering the Session Info & Actions drawer.
pub(crate) struct SessionInfoProps<'a> {
    pub session: &'a SessionModel,
    pub transcript_text: String,
    pub last_reply: Option<String>,
    pub rename_input: Entity<PromptInput>,
    pub rename_value: &'a str,
    pub copied: Option<&'a str>,
    pub on_copy: CopyCallback,
    pub on_intent: IntentCallback,
    pub on_close: VoidCallback,
}

/// Render the Session Info & Actions panel (Tern's `omp.info.*` + quick actions).
pub(crate) fn render_session_info(props: SessionInfoProps<'_>, theme: &Theme) -> AnyElement {
    let session = props.session;
    let on_close = props.on_close.clone();

    let kv_row = |label: &'static str,
                  value: SharedString,
                  copy_key: Option<&'static str>,
                  copy_val: Option<String>,
                  on_copy: CopyCallback| {
        let is_copied = copy_key.is_some_and(|k| props.copied == Some(k));
        div()
            .flex()
            .items_center()
            .gap(Theme::SPACE_2)
            .w_full()
            .child(
                div()
                    .w(px(84.0))
                    .flex_none()
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text_subtle)
                    .child(label),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_family("Menlo")
                    .text_size(Theme::TEXT_XS)
                    .text_color(theme.text)
                    .child(value),
            )
            .children(copy_key.zip(copy_val).map(|(key, val)| {
                Button::new(
                    SharedString::from(format!("omp-info-copy-{key}")),
                    if is_copied { "Copied" } else { "Copy" },
                )
                .icon(if is_copied {
                    IconName::Check
                } else {
                    IconName::Copy
                })
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .selector(format!("omp-info-copy-{key}"))
                .on_click(move |_, _, cx| on_copy(key.to_owned(), val.clone(), cx))
            }))
    };

    let model_text = session
        .model
        .as_ref()
        .map(|m| format!("{} ({})", m.label(), m.provider))
        .unwrap_or_else(|| "—".to_owned());
    let stats_text = format!(
        "{} tokens · {} · {} tool calls",
        compact_tokens(session.stats.tokens),
        cost_label(session),
        session.stats.tool_calls
    );
    let context_text = session.context.map(|c| {
        format!(
            "{} / {} ({:.0}%)",
            compact_tokens(c.tokens),
            compact_tokens(c.window),
            c.percent
        )
    });

    let has_transcript = !props.transcript_text.trim().is_empty();
    let transcript_text = props.transcript_text;
    let copied_transcript = props.copied == Some("transcript");
    let on_copy_transcript = props.on_copy.clone();

    let last_reply = props.last_reply;
    let has_reply = last_reply.is_some();
    let copied_reply = props.copied == Some("last-reply");
    let on_copy_reply = props.on_copy.clone();

    let on_compact = props.on_intent.clone();
    let on_fork = props.on_intent.clone();
    let on_new = props.on_intent.clone();
    let on_export = props.on_intent.clone();
    let on_rename = props.on_intent.clone();
    let rename_text = props.rename_value.trim().to_owned();
    let can_rename = !rename_text.is_empty();

    div()
        .debug_selector(|| "omp-info-panel".to_owned())
        .flex()
        .flex_col()
        .flex_none()
        .gap(Theme::SPACE_2)
        .w_full()
        .px(Theme::SPACE_3)
        .py(Theme::SPACE_2)
        .border_b_1()
        .border_color(theme.border_subtle)
        .bg(theme.raised)
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .child(
                    div()
                        .text_size(Theme::TEXT_SM)
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme.text)
                        .child("Session info & actions"),
                )
                .child(div().flex_1())
                .child(
                    Button::new("omp-info-close", "")
                        .icon(IconName::Close)
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::Compact)
                        .tooltip("Close session info")
                        .selector("omp-info-close")
                        .on_click(move |_, _, cx| on_close(cx)),
                ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.0))
                .children(session.session.id.as_ref().map(|id| {
                    kv_row(
                        "Session ID",
                        id.clone().into(),
                        Some("id"),
                        Some(id.clone()),
                        props.on_copy.clone(),
                    )
                }))
                .children(session.session.file.as_ref().map(|file| {
                    kv_row(
                        "Session file",
                        file.clone().into(),
                        Some("file"),
                        Some(file.clone()),
                        props.on_copy.clone(),
                    )
                }))
                .child(kv_row(
                    "Model",
                    model_text.into(),
                    None,
                    None,
                    props.on_copy.clone(),
                ))
                .child(kv_row(
                    "Usage",
                    stats_text.into(),
                    None,
                    None,
                    props.on_copy.clone(),
                ))
                .children(
                    context_text.map(|ctx| {
                        kv_row("Context", ctx.into(), None, None, props.on_copy.clone())
                    }),
                ),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::SPACE_2)
                .w_full()
                .child(
                    div()
                        .w(px(84.0))
                        .flex_none()
                        .text_size(Theme::TEXT_XS)
                        .text_color(theme.text_subtle)
                        .child("Rename"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .px(Theme::SPACE_2)
                        .rounded(Theme::RADIUS_SM)
                        .bg(theme.surface)
                        .border_1()
                        .border_color(theme.border_subtle)
                        .child(props.rename_input),
                )
                .child(
                    Button::new("omp-action-rename", "Save name")
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Compact)
                        .disabled(!can_rename)
                        .selector("omp-action-rename")
                        .on_click(move |_, _, cx| {
                            if !rename_text.is_empty() {
                                on_rename(
                                    OmpIntent::RenameSession {
                                        name: rename_text.clone(),
                                    },
                                    cx,
                                );
                            }
                        }),
                ),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .gap(Theme::SPACE_1)
                .pt(Theme::SPACE_1)
                .border_t_1()
                .border_color(theme.border_subtle)
                .child(
                    Button::new(
                        "omp-info-copy-transcript",
                        if copied_transcript {
                            "Copied transcript"
                        } else {
                            "Copy transcript"
                        },
                    )
                    .icon(if copied_transcript {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .variant(ButtonVariant::Secondary)
                    .size(ButtonSize::Compact)
                    .disabled(!has_transcript)
                    .selector("omp-info-copy-transcript")
                    .on_click(move |_, _, cx| {
                        on_copy_transcript("transcript".to_owned(), transcript_text.clone(), cx);
                    }),
                )
                .child(
                    Button::new(
                        "omp-info-copy-last",
                        if copied_reply {
                            "Copied reply"
                        } else {
                            "Copy last reply"
                        },
                    )
                    .icon(if copied_reply {
                        IconName::Check
                    } else {
                        IconName::Copy
                    })
                    .variant(ButtonVariant::Secondary)
                    .size(ButtonSize::Compact)
                    .disabled(!has_reply)
                    .selector("omp-info-copy-last")
                    .on_click(move |_, _, cx| {
                        if let Some(reply) = &last_reply {
                            on_copy_reply("last-reply".to_owned(), reply.clone(), cx);
                        }
                    }),
                )
                .child(div().flex_1())
                .child(
                    Button::new("omp-action-compact", "Compact")
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Compact)
                        .tooltip("Compact conversation context to free window space")
                        .selector("omp-action-compact")
                        .on_click(move |_, _, cx| {
                            on_compact(OmpIntent::Compact { instructions: None }, cx);
                        }),
                )
                .child(
                    Button::new("omp-action-fork", "Fork")
                        .icon(IconName::Fork)
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Compact)
                        .tooltip("Fork this session into a new branch")
                        .selector("omp-action-fork")
                        .on_click(move |_, _, cx| on_fork(OmpIntent::Fork, cx)),
                )
                .child(
                    Button::new("omp-action-export", "Export HTML")
                        .icon(IconName::ExternalLink)
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Compact)
                        .disabled(session.session.file.is_none())
                        .tooltip("Export persisted session to an HTML file")
                        .selector("omp-action-export")
                        .on_click(move |_, _, cx| on_export(OmpIntent::ExportHtml, cx)),
                )
                .child(
                    Button::new("omp-action-new", "New session")
                        .icon(IconName::Plus)
                        .variant(ButtonVariant::Secondary)
                        .size(ButtonSize::Compact)
                        .tooltip("Start a fresh session in this pane")
                        .selector("omp-action-new")
                        .on_click(move |_, _, cx| on_new(OmpIntent::NewSession, cx)),
                ),
        )
        .into_any_element()
}

/// Inputs for rendering the multi-session quick strip and full Sessions switcher drawer.
pub(crate) struct SessionsDrawerProps<'a> {
    pub sessions: &'a [SessionEntry],
    pub filter_input: Entity<PromptInput>,
    pub filter_text: &'a str,
    pub on_intent: IntentCallback,
    pub on_refresh: Option<SessionsRefresher>,
    pub on_split: Option<SplitAgentHandler>,
    pub on_close: VoidCallback,
}

/// Compact horizontal strip shown below the session bar whenever multiple sessions exist,
/// letting the user see and switch sessions in one click.
pub(crate) fn render_sessions_strip(
    sessions: &[SessionEntry],
    on_intent: IntentCallback,
    on_split: Option<SplitAgentHandler>,
    on_open_drawer: VoidCallback,
    theme: &Theme,
) -> AnyElement {
    let hover = theme.hover;
    let active_bg = theme.selection;
    let on_new = on_intent.clone();
    let max_pills = 8usize;
    let extra = sessions.len().saturating_sub(max_pills);

    let pills = div()
        .id("omp-sessions-strip-scroll")
        .flex()
        .flex_1()
        .min_w_0()
        .items_center()
        .gap(Theme::SPACE_1)
        .overflow_x_scroll()
        .children(sessions.iter().take(max_pills).enumerate().map(|(i, s)| {
            let active = s.active;
            let path = s.path.to_string();
            let on_switch = on_intent.clone();
            let sel = format!("omp-session-pill-{i}");
            let count = (s.message_count > 0).then(|| format!("{}", s.message_count));
            div()
                .id(SharedString::from(format!("omp-session-pill-{i}")))
                .debug_selector(move || sel)
                .flex()
                .flex_none()
                .items_center()
                .gap(px(5.0))
                .max_w(px(180.0))
                .h(px(22.0))
                .px(Theme::SPACE_2)
                .rounded(Theme::RADIUS_SM)
                .border_1()
                .border_color(if active {
                    theme.accent
                } else {
                    theme.border_subtle
                })
                .bg(if active { active_bg } else { theme.surface })
                .cursor_pointer()
                .hover(move |style| style.bg(if active { active_bg } else { hover }))
                .on_click(move |_, _, cx| {
                    if !active {
                        on_switch(OmpIntent::SwitchSession { path: path.clone() }, cx);
                    }
                })
                .child(
                    div()
                        .flex_none()
                        .size(px(6.0))
                        .rounded_full()
                        .bg(if active { theme.accent } else { theme.border }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(Theme::TEXT_XS)
                        .font_weight(if active {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_color(if active { theme.text } else { theme.text_muted })
                        .child(s.title.clone()),
                )
                .children(count.map(|c| {
                    div()
                        .flex_none()
                        .font_family("Menlo")
                        .text_size(px(10.0))
                        .text_color(theme.text_subtle)
                        .child(c)
                }))
        }))
        .when(extra > 0, |row| {
            let on_open = on_open_drawer.clone();
            row.child(
                Button::new("omp-sessions-more", format!("+{extra} more"))
                    .variant(ButtonVariant::Ghost)
                    .size(ButtonSize::Compact)
                    .selector("omp-sessions-more")
                    .on_click(move |_, _, cx| on_open(cx)),
            )
        });

    div()
        .debug_selector(|| "omp-sessions-strip".to_owned())
        .flex()
        .flex_none()
        .items_center()
        .gap(Theme::SPACE_1)
        .w_full()
        .h(px(28.0))
        .px(Theme::SPACE_3)
        .border_b_1()
        .border_color(theme.border_subtle)
        .bg(theme.surface)
        .child(pills)
        .child(
            Button::new("omp-strip-new", "New")
                .icon(IconName::Plus)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .tooltip("Start a new session in this pane")
                .selector("omp-strip-new")
                .on_click(move |_, _, cx| on_new(OmpIntent::NewSession, cx)),
        )
        .children(on_split.map(|split| {
            Button::new("omp-strip-split", "Split")
                .icon(IconName::SplitH)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .tooltip("Open another agent session side-by-side")
                .selector("omp-strip-split")
                .on_click(move |_, window, cx| split(window, cx))
        }))
        .into_any_element()
}

/// Full searchable Sessions drawer: list all persisted sessions, switch in one click,
/// fork, start a new session, or open a second agent session side-by-side.
pub(crate) fn render_sessions_drawer(props: SessionsDrawerProps<'_>, theme: &Theme) -> AnyElement {
    let query = props.filter_text.trim().to_lowercase();
    let filtered: Vec<(usize, &SessionEntry)> = props
        .sessions
        .iter()
        .enumerate()
        .filter(|(_, s)| {
            query.is_empty()
                || s.title.to_lowercase().contains(&query)
                || s.id.to_lowercase().contains(&query)
                || s.preview
                    .as_deref()
                    .is_some_and(|p| p.to_lowercase().contains(&query))
        })
        .collect();

    let total = props.sessions.len();
    let hover = theme.hover;
    let selected_bg = theme.selection;
    let on_new = props.on_intent.clone();
    let on_fork = props.on_intent.clone();
    let on_close = props.on_close.clone();

    let header = div()
        .flex()
        .items_center()
        .gap(Theme::SPACE_1)
        .w_full()
        .px(Theme::SPACE_3)
        .py(px(6.0))
        .border_b_1()
        .border_color(theme.border_subtle)
        .child(
            div()
                .text_size(Theme::TEXT_SM)
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.text)
                .child(format!("Sessions ({total})")),
        )
        .child(div().flex_1())
        .child(
            Button::new("omp-sessions-new", "New session")
                .icon(IconName::Plus)
                .variant(ButtonVariant::Secondary)
                .size(ButtonSize::Compact)
                .tooltip("Start a fresh session in this pane")
                .selector("omp-sessions-new")
                .on_click(move |_, _, cx| on_new(OmpIntent::NewSession, cx)),
        )
        .child(
            Button::new("omp-sessions-fork", "Fork")
                .icon(IconName::Fork)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .tooltip("Fork the active session into a new branch")
                .selector("omp-sessions-fork")
                .on_click(move |_, _, cx| on_fork(OmpIntent::Fork, cx)),
        )
        .children(props.on_split.map(|split| {
            Button::new("omp-sessions-split", "Split pane")
                .icon(IconName::SplitH)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .tooltip("Open a second independent agent session side-by-side")
                .selector("omp-sessions-split")
                .on_click(move |_, window, cx| split(window, cx))
        }))
        .children(props.on_refresh.map(|refresh| {
            Button::new("omp-sessions-refresh", "")
                .icon(IconName::Refresh)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .tooltip("Refresh sessions list")
                .selector("omp-sessions-refresh")
                .on_click(move |_, _, cx| refresh(cx))
        }))
        .child(
            Button::new("omp-sessions-close", "")
                .icon(IconName::Close)
                .variant(ButtonVariant::Ghost)
                .size(ButtonSize::Compact)
                .tooltip("Close sessions drawer")
                .selector("omp-sessions-close")
                .on_click(move |_, _, cx| on_close(cx)),
        );

    let filter_bar = (total > 4).then(|| {
        div()
            .px(Theme::SPACE_3)
            .py(Theme::SPACE_1)
            .border_b_1()
            .border_color(theme.border_subtle)
            .child(props.filter_input)
    });

    let rows: AnyElement = if filtered.is_empty() {
        div()
            .px(Theme::SPACE_3)
            .py(Theme::SPACE_3)
            .text_size(Theme::TEXT_XS)
            .text_color(theme.text_muted)
            .child(if total == 0 {
                "No saved sessions yet — send a message to persist this session."
            } else {
                "No sessions match your filter."
            })
            .into_any_element()
    } else {
        div()
            .id("omp-sessions-list")
            .flex()
            .flex_col()
            .w_full()
            .max_h(px(240.0))
            .overflow_y_scroll()
            .py(Theme::SPACE_1)
            .children(filtered.into_iter().map(|(i, s)| {
                let active = s.active;
                let path = s.path.to_string();
                let on_switch = props.on_intent.clone();
                let sel = format!("omp-session-row-{i}");
                let msgs = if s.message_count == 1 {
                    "1 msg".to_owned()
                } else {
                    format!("{} msgs", s.message_count)
                };
                div()
                    .id(SharedString::from(format!("omp-session-row-{i}")))
                    .debug_selector(move || sel)
                    .flex()
                    .items_center()
                    .gap(Theme::SPACE_2)
                    .mx(Theme::SPACE_2)
                    .px(Theme::SPACE_2)
                    .py(px(5.0))
                    .rounded(Theme::RADIUS_SM)
                    .when(active, |row| row.bg(selected_bg))
                    .cursor_pointer()
                    .hover(move |style| style.bg(if active { selected_bg } else { hover }))
                    .on_click(move |_, _, cx| {
                        if !active {
                            on_switch(OmpIntent::SwitchSession { path: path.clone() }, cx);
                        }
                    })
                    .child(
                        div()
                            .flex_none()
                            .size(px(7.0))
                            .rounded_full()
                            .bg(if active { theme.accent } else { theme.border }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(6.0))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .text_size(Theme::TEXT_SM)
                                            .font_weight(if active {
                                                FontWeight::SEMIBOLD
                                            } else {
                                                FontWeight::MEDIUM
                                            })
                                            .text_color(theme.text)
                                            .child(s.title.clone()),
                                    )
                                    .when(active, |row| {
                                        row.child(Badge::new("active").tone(Tone::Accent))
                                    }),
                            )
                            .children(s.preview.as_ref().map(|p| {
                                div()
                                    .truncate()
                                    .text_size(Theme::TEXT_XS)
                                    .text_color(theme.text_muted)
                                    .child(p.clone())
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_none()
                            .items_end()
                            .child(
                                div()
                                    .font_family("Menlo")
                                    .text_size(Theme::TEXT_XS)
                                    .text_color(theme.text_subtle)
                                    .child(msgs),
                            )
                            .child(
                                div()
                                    .text_size(px(10.0))
                                    .text_color(theme.text_subtle)
                                    .child(s.updated_label.clone()),
                            ),
                    )
            }))
            .into_any_element()
    };

    div()
        .debug_selector(|| "omp-sessions-drawer".to_owned())
        .flex()
        .flex_col()
        .flex_none()
        .w_full()
        .border_b_1()
        .border_color(theme.border_subtle)
        .bg(theme.raised)
        .child(header)
        .children(filter_bar)
        .child(rows)
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use xctx_omp::Todo;

    use super::*;

    #[test]
    fn goal_summary_reads_string_or_object_title() {
        assert_eq!(
            goal_summary(&json!("Ship copy buttons\nsecond line")).as_deref(),
            Some("Ship copy buttons")
        );
        assert_eq!(
            goal_summary(&json!({"title": "Fix diffs"})).as_deref(),
            Some("Fix diffs")
        );
        assert_eq!(goal_summary(&json!({})), None);
    }

    #[test]
    fn todos_markdown_renders_phases_and_checkboxes() {
        let phases = vec![TodoPhase {
            name: "Phase 1".into(),
            tasks: vec![
                Todo {
                    content: "Add copy buttons".into(),
                    status: "completed".into(),
                },
                Todo {
                    content: "Add changes panel".into(),
                    status: "in_progress".into(),
                },
                Todo {
                    content: "Write tests".into(),
                    status: "pending".into(),
                },
            ],
        }];
        let md = todos_markdown(&phases);
        assert!(md.contains("## Phase 1\n"));
        assert!(md.contains("- [x] Add copy buttons\n"));
        assert!(md.contains("- [-] Add changes panel\n"));
        assert!(md.contains("- [ ] Write tests\n"));
    }
}
