//! What omp writes, classified. Decoding never fails: a frame this version does not know is [`Frame::Unknown`]
//! and a known one with missing fields gets defaults, so a newer omp cannot break a pane.

use serde_json::Value;

/// The `ready` frame: what the transport supports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ready {
    /// The protocol version in use right now (always 1 until negotiated).
    pub protocol_version: u32,
    /// The versions omp can speak.
    pub supported: Vec<u32>,
    /// The largest line omp writes.
    pub max_frame_bytes: usize,
    /// The largest object a chunked sequence may reassemble to.
    pub max_reassembled_bytes: usize,
}

/// A command's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    /// The `id` the command carried, if it had one.
    pub id: Option<String>,
    /// The command it answers.
    pub command: String,
    /// Whether it succeeded.
    pub success: bool,
    /// The payload (`null` when there is none).
    pub data: Value,
    /// The failure text when it did not succeed.
    pub error: Option<String>,
    /// A machine-readable failure code, such as `session_busy`.
    pub code: Option<String>,
}

/// How a `prompt` ended.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptResult {
    /// The `id` of the prompt.
    pub id: Option<String>,
    /// Whether the agent ran for it.
    pub agent_invoked: bool,
    /// `completed`, `aborted` or `error`.
    pub status: String,
    /// The provider error when `status` is `error`: `{message, retryable, ...}`.
    pub error: Option<Value>,
    /// Whether nothing can wake the session again.
    pub session_settled: bool,
}

/// What an `extension_ui_request` asks.
#[derive(Debug, Clone, PartialEq)]
pub enum UiMethod {
    /// Pick one of `options`. Tool approvals arrive as a `select` whose title starts `Allow tool:`.
    Select {
        /// The question.
        title: String,
        /// The labels to choose from.
        options: Vec<String>,
    },
    /// Yes or no.
    Confirm {
        /// The question.
        title: String,
        /// More detail.
        message: String,
    },
    /// One line of text.
    Input {
        /// The question.
        title: String,
        /// Greyed hint text.
        placeholder: String,
        /// Whether to mask what is typed.
        secret: bool,
    },
    /// Several lines of text.
    Editor {
        /// The question.
        title: String,
        /// The text to start from.
        prefill: String,
    },
    /// An HTML form (the `ask` tool).
    Form {
        /// The `<form>` markup.
        html: String,
    },
    /// Several questions with options (an extension's `ui.askDialog`).
    Ask {
        /// The questions, as omp sent them.
        questions: Value,
    },
    /// Close the dialog of request `target_id`: it timed out or was aborted.
    Cancel {
        /// The request being cancelled.
        target_id: String,
    },
    /// A message to show briefly.
    Notify {
        /// The text.
        message: String,
        /// `info`, `warning` or `error`.
        level: String,
    },
    /// A status line segment (`None` text clears it).
    SetStatus {
        /// The segment's key.
        key: String,
        /// Its text.
        text: Option<String>,
    },
    /// Text to put in the composer.
    SetEditorText {
        /// The text.
        text: String,
    },
    /// A login flow wants a page opened.
    OpenUrl {
        /// The page.
        url: String,
        /// What to tell the user to do with it.
        instructions: Option<String>,
    },
    /// A presentation request xctx does not show (`setWidget`, `setTitle`, ...), or one it does not know.
    Ignored {
        /// The request's `method`.
        method: String,
    },
}

/// An `extension_ui_request`.
#[derive(Debug, Clone, PartialEq)]
pub struct UiRequest {
    /// The id the answer must carry.
    pub id: String,
    /// What it asks.
    pub method: UiMethod,
    /// The timeout omp applies, in milliseconds, if any.
    pub timeout_ms: Option<u64>,
}

impl UiRequest {
    /// Whether the agent waits for an answer, so the user has to be asked.
    pub fn blocks(&self) -> bool {
        matches!(
            self.method,
            UiMethod::Select { .. }
                | UiMethod::Confirm { .. }
                | UiMethod::Input { .. }
                | UiMethod::Editor { .. }
                | UiMethod::Form { .. }
                | UiMethod::Ask { .. }
        )
    }

    /// The `<form>` markup of an `ask` request.
    pub fn html(&self) -> Option<&str> {
        match &self.method {
            UiMethod::Form { html } => Some(html),
            _ => None,
        }
    }

    /// Whether this is a tool approval: a `select` titled `Allow tool: <name>` (see [`UiRequest::approval`]).
    pub fn is_approval(&self) -> bool {
        self.approval().is_some()
    }

    /// The tool and call detail of a tool approval request.
    pub fn approval(&self) -> Option<Approval> {
        let UiMethod::Select { title, options } = &self.method else {
            return None;
        };
        let rest = title.strip_prefix("Allow tool:")?;
        let (tool, detail) = rest.split_once('\n').unwrap_or((rest, ""));
        Some(Approval {
            tool: tool.trim().to_owned(),
            detail: detail.trim().to_owned(),
            options: options.clone(),
        })
    }
}

/// The parts of a tool approval request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Approval {
    /// The tool asking, such as `bash`.
    pub tool: String,
    /// What it will do, such as `Command: rm -rf build`.
    pub detail: String,
    /// The offered answers: approve once, approve for the session, deny.
    pub options: Vec<String>,
}

/// One frame from omp.
#[derive(Debug, Clone, PartialEq)]
pub enum Frame {
    /// The first frame.
    Ready(Ready),
    /// A command's answer.
    Response(Response),
    /// A prompt's outcome.
    PromptResult(PromptResult),
    /// Nothing can wake the session again.
    SessionSettled,
    /// A dialog or presentation request.
    Ui(UiRequest),
    /// A session or agent event (`agent_start`, `message_update`, `queue_update`, ...); `kind` is its `type`.
    Event {
        /// The event's `type`.
        kind: String,
        /// The whole frame.
        value: Value,
    },
    /// A frame without a usable `type`.
    Unknown(Value),
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn opt_text(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn number(value: &Value, key: &str) -> Option<u64> {
    value.get(key).and_then(Value::as_u64)
}

impl Frame {
    /// Classify one logical frame.
    pub fn parse(value: Value) -> Frame {
        let Some(kind) = value.get("type").and_then(Value::as_str).map(str::to_owned) else {
            return Frame::Unknown(value);
        };
        match kind.as_str() {
            "ready" => Frame::Ready(Ready {
                protocol_version: number(&value, "protocolVersion").unwrap_or(1) as u32,
                supported: value
                    .get("supportedProtocolVersions")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_u64)
                            .map(|v| v as u32)
                            .collect()
                    })
                    .unwrap_or_else(|| vec![1]),
                max_frame_bytes: number(&value, "maxFrameBytes").unwrap_or(1 << 20) as usize,
                max_reassembled_bytes: number(&value, "maxReassembledFrameBytes")
                    .unwrap_or(64 << 20) as usize,
            }),
            "response" => Frame::Response(Response {
                id: opt_text(&value, "id"),
                command: text(&value, "command"),
                success: value
                    .get("success")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                data: value.get("data").cloned().unwrap_or(Value::Null),
                error: opt_text(&value, "error"),
                code: opt_text(&value, "code"),
            }),
            "prompt_result" => Frame::PromptResult(PromptResult {
                id: opt_text(&value, "id"),
                agent_invoked: value
                    .get("agentInvoked")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                status: text(&value, "status"),
                error: value.get("error").filter(|e| !e.is_null()).cloned(),
                session_settled: value
                    .get("sessionSettled")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            }),
            "session_settled" => Frame::SessionSettled,
            "extension_ui_request" => Frame::Ui(parse_ui(&value)),
            _ => Frame::Event { kind, value },
        }
    }
}

fn parse_ui(value: &Value) -> UiRequest {
    let method = text(value, "method");
    let parsed = match method.as_str() {
        "select" => UiMethod::Select {
            title: text(value, "title"),
            options: value
                .get("options")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default(),
        },
        "confirm" => UiMethod::Confirm {
            title: text(value, "title"),
            message: text(value, "message"),
        },
        "input" => UiMethod::Input {
            title: text(value, "title"),
            placeholder: text(value, "placeholder"),
            secret: value
                .get("secret")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        "editor" => UiMethod::Editor {
            title: text(value, "title"),
            prefill: text(value, "prefill"),
        },
        "form" => UiMethod::Form {
            html: text(value, "html"),
        },
        "ask" => UiMethod::Ask {
            questions: value.get("questions").cloned().unwrap_or(Value::Null),
        },
        "cancel" => UiMethod::Cancel {
            target_id: text(value, "targetId"),
        },
        "notify" => UiMethod::Notify {
            message: text(value, "message"),
            level: opt_text(value, "notifyType").unwrap_or_else(|| "info".into()),
        },
        "setStatus" => UiMethod::SetStatus {
            key: text(value, "statusKey"),
            text: opt_text(value, "statusText"),
        },
        "set_editor_text" => UiMethod::SetEditorText {
            text: text(value, "text"),
        },
        "open_url" => UiMethod::OpenUrl {
            url: text(value, "url"),
            instructions: opt_text(value, "instructions"),
        },
        _ => UiMethod::Ignored { method },
    };
    UiRequest {
        id: text(value, "id"),
        method: parsed,
        timeout_ms: number(value, "timeout"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_frame_without_a_type_is_unknown_and_an_unrecognised_type_is_an_event() {
        assert!(matches!(Frame::parse(json!({"a": 1})), Frame::Unknown(_)));
        let f = Frame::parse(json!({"type": "brand_new_event", "x": 1}));
        assert!(matches!(f, Frame::Event { ref kind, .. } if kind == "brand_new_event"));
    }

    #[test]
    fn a_tool_approval_is_a_select_titled_allow_tool() {
        let f = Frame::parse(json!({
            "type": "extension_ui_request", "id": "1", "method": "select",
            "title": "Allow tool: bash\nCommand: echo hi",
            "options": ["Approve", "Approve bash for this session", "Deny"],
        }));
        let Frame::Ui(req) = f else {
            panic!("not a ui request")
        };
        assert!(req.blocks());
        let approval = req.approval().expect("an approval");
        assert_eq!(approval.tool, "bash");
        assert_eq!(approval.detail, "Command: echo hi");
        assert_eq!(approval.options.len(), 3);
    }

    #[test]
    fn a_plain_select_is_not_an_approval_and_presentation_requests_do_not_block() {
        let select = Frame::parse(json!({
            "type": "extension_ui_request", "id": "1", "method": "select",
            "title": "Pick a branch", "options": ["a", "b"],
        }));
        assert!(matches!(&select, Frame::Ui(r) if r.blocks() && !r.is_approval()));
        let widget = Frame::parse(json!({
            "type": "extension_ui_request", "id": "2", "method": "setWidget", "widgetKey": "autoresearch",
        }));
        assert!(matches!(&widget, Frame::Ui(r) if !r.blocks()));
    }

    #[test]
    fn a_ready_frame_missing_fields_gets_the_documented_defaults() {
        let Frame::Ready(ready) = Frame::parse(json!({"type": "ready"})) else {
            panic!("not ready")
        };
        assert_eq!(ready.supported, vec![1]);
        assert_eq!(ready.max_frame_bytes, 1 << 20);
        assert_eq!(ready.max_reassembled_bytes, 64 << 20);
    }
}
