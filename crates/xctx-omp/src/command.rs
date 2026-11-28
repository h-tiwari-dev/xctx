//! What xctx sends to omp: commands (`RpcCommand`) and answers to its UI requests (`extension_ui_response`).

use serde_json::{Map, Value, json};

/// An image attached to a prompt, as omp's `ImageContent`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// The media type, such as `image/png`.
    pub mime_type: String,
    /// The image bytes, base64 encoded.
    pub data: String,
}

impl Image {
    fn to_value(&self) -> Value {
        json!({"type": "image", "mimeType": self.mime_type, "data": self.data})
    }
}

/// How a prompt sent while the agent is running is queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Streaming {
    /// Interrupt: delivered at the next step boundary.
    Steer,
    /// Wait: delivered once the agent would otherwise stop.
    FollowUp,
}

/// Which pending-message queue a command addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Queue {
    /// Messages that interrupt the running turn.
    Steering,
    /// Messages that wait for the turn to end.
    FollowUp,
}

/// How much of its subagents' work omp forwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentLevel {
    /// Nothing.
    Off,
    /// Lifecycle and progress frames.
    Progress,
    /// Also every event of every subagent.
    Events,
}

/// One frame for omp's standard input: a command with an optional `id` the response echoes, or a UI answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Command {
    value: Value,
}

impl Command {
    /// A command of type `kind` with `fields` (a JSON object) merged in.
    pub fn raw(kind: &str, fields: Value) -> Command {
        let mut map = match fields {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        map.insert("type".into(), Value::String(kind.to_owned()));
        Command {
            value: Value::Object(map),
        }
    }

    /// The same command asking omp to echo `id` in its response.
    #[must_use]
    pub fn with_id(mut self, id: impl Into<String>) -> Command {
        if let Value::Object(map) = &mut self.value {
            map.insert("id".into(), Value::String(id.into()));
        }
        self
    }

    /// The command's `id`, if it has one.
    pub fn id(&self) -> Option<&str> {
        self.value.get("id").and_then(Value::as_str)
    }

    /// The command's `type`.
    pub fn kind(&self) -> &str {
        self.value
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    /// The JSON object that goes on the wire.
    pub fn as_value(&self) -> &Value {
        &self.value
    }

    /// Switch the transport to protocol v2 (lossless chunked frames).
    pub fn negotiate_protocol() -> Command {
        Command::raw("negotiate_protocol", json!({"protocolVersion": 2}))
    }

    /// Forward every event, and send message updates as deltas instead of whole-message snapshots.
    pub fn set_event_filter_delta() -> Command {
        Command::raw(
            "set_event_filter",
            json!({"events": null, "messageUpdates": "delta"}),
        )
    }

    /// Send a user message; `streaming` says how to queue it when the agent is already running.
    pub fn prompt(message: &str, images: &[Image], streaming: Option<Streaming>) -> Command {
        let mut fields = json!({"message": message});
        if !images.is_empty() {
            fields["images"] = Value::Array(images.iter().map(Image::to_value).collect());
        }
        match streaming {
            Some(Streaming::Steer) => fields["streamingBehavior"] = json!("steer"),
            Some(Streaming::FollowUp) => fields["streamingBehavior"] = json!("followUp"),
            None => {}
        }
        Command::raw("prompt", fields)
    }

    /// Interrupt the running turn with a message.
    pub fn steer(message: &str) -> Command {
        Command::raw("steer", json!({"message": message}))
    }

    /// Queue a message for after the running turn.
    pub fn follow_up(message: &str) -> Command {
        Command::raw("follow_up", json!({"message": message}))
    }

    /// Stop the running turn.
    pub fn abort() -> Command {
        Command::raw("abort", json!({}))
    }

    /// Take back queued user input and stop, as Esc does in omp's terminal UI.
    pub fn abort_and_restore_queue() -> Command {
        Command::raw("abort_and_restore_queue", json!({}))
    }

    /// Remove one queued message (the queue chip's text).
    pub fn remove_queued_message(message: &str, queue: Queue) -> Command {
        let queue = match queue {
            Queue::Steering => "steering",
            Queue::FollowUp => "followUp",
        };
        Command::raw(
            "remove_queued_message",
            json!({"message": message, "queue": queue}),
        )
    }

    /// Move a queued follow-up to the steering queue.
    pub fn promote_queued_message(message: &str) -> Command {
        Command::raw("promote_queued_message", json!({"message": message}))
    }

    /// The whole session state.
    pub fn get_state() -> Command {
        Command::raw("get_state", json!({}))
    }

    /// Token, cost and message counts.
    pub fn get_session_stats() -> Command {
        Command::raw("get_session_stats", json!({}))
    }

    /// The slash commands omp offers.
    pub fn get_available_commands() -> Command {
        Command::raw("get_available_commands", json!({}))
    }

    /// omp's model picker rows for `query` (blank browses).
    pub fn get_model_picker(query: &str, limit: u32) -> Command {
        Command::raw("get_model_picker", json!({"query": query, "limit": limit}))
    }

    /// Switch the session's model.
    pub fn set_model(provider: &str, model_id: &str) -> Command {
        Command::raw(
            "set_model",
            json!({"provider": provider, "modelId": model_id}),
        )
    }

    /// Set the thinking level (`off`, `low`, `medium`, `high`, ...).
    pub fn set_thinking_level(level: &str) -> Command {
        Command::raw("set_thinking_level", json!({"level": level}))
    }

    /// The thinking levels the live model supports.
    pub fn get_available_thinking_levels() -> Command {
        Command::raw("get_available_thinking_levels", json!({}))
    }

    /// One page of the transcript so far.
    pub fn get_messages_page(cursor: Option<&str>, limit: u32) -> Command {
        let mut fields = json!({"limit": limit});
        if let Some(cursor) = cursor {
            fields["cursor"] = json!(cursor);
        }
        Command::raw("get_messages_page", fields)
    }

    /// Choose which subagent frames omp forwards.
    pub fn set_subagent_subscription(level: SubagentLevel) -> Command {
        let level = match level {
            SubagentLevel::Off => "off",
            SubagentLevel::Progress => "progress",
            SubagentLevel::Events => "events",
        };
        Command::raw("set_subagent_subscription", json!({"level": level}))
    }

    /// The registry of running and finished subagents.
    pub fn get_subagents() -> Command {
        Command::raw("get_subagents", json!({}))
    }

    /// A subagent's transcript from byte `from_byte` on.
    pub fn get_subagent_messages(subagent_id: &str, from_byte: u64) -> Command {
        Command::raw(
            "get_subagent_messages",
            json!({"subagentId": subagent_id, "fromByte": from_byte}),
        )
    }

    /// Stop one running subagent.
    pub fn cancel_subagent(subagent_id: &str) -> Command {
        Command::raw("cancel_subagent", json!({"subagentId": subagent_id}))
    }

    /// Message a running subagent as its user.
    pub fn steer_subagent(subagent_id: &str, message: &str) -> Command {
        Command::raw(
            "steer_subagent",
            json!({"subagentId": subagent_id, "message": message}),
        )
    }

    /// Start a fresh session in the current process.
    pub fn new_session() -> Command {
        Command::raw("new_session", json!({}))
    }

    /// Continue the newest session in `session_dir`, or start one there.
    pub fn open_session(session_dir: &str) -> Command {
        Command::raw("open_session", json!({"sessionDir": session_dir}))
    }

    /// Switch the current process to a specific persisted session file (`sessionPath`).
    pub fn switch_session(session_path: &str) -> Command {
        Command::raw("switch_session", json!({"sessionPath": session_path}))
    }

    /// Fork the session (at `entry_id`, or whole when `None`).
    pub fn fork(entry_id: Option<&str>) -> Command {
        match entry_id {
            Some(id) => Command::raw("fork", json!({"entryId": id})),
            None => Command::raw("fork", json!({})),
        }
    }

    /// Compact the conversation.
    pub fn compact(custom_instructions: Option<&str>) -> Command {
        match custom_instructions {
            Some(text) => Command::raw("compact", json!({"customInstructions": text})),
            None => Command::raw("compact", json!({})),
        }
    }

    /// Ask a side question that is not added to the conversation.
    pub fn btw(question: &str) -> Command {
        Command::raw("btw", json!({"question": question}))
    }

    /// The session's side-question (`btw`) history.
    pub fn get_btw_history() -> Command {
        Command::raw("get_btw_history", json!({}))
    }

    /// Run a shell command the way omp's `!` prefix does; `exclude_from_context` is omp's `!!`.
    pub fn bash(command: &str, exclude_from_context: bool) -> Command {
        let mut fields = json!({"command": command});
        if exclude_from_context {
            fields["excludeFromContext"] = json!(true);
        }
        Command::raw("bash", fields)
    }

    /// Stop the shell command started with [`Command::bash`].
    pub fn abort_bash() -> Command {
        Command::raw("abort_bash", json!({}))
    }

    /// Replace the host-owned tools omp may call back into.
    pub fn set_host_tools(tools: Value) -> Command {
        Command::raw("set_host_tools", json!({"tools": tools}))
    }

    /// Answer a `select` or `input` or `editor` request with the chosen or typed text.
    pub fn ui_value(request_id: &str, value: &str) -> Command {
        ui_response(request_id, json!({"value": value}))
    }

    /// Answer a `confirm` request.
    pub fn ui_confirmed(request_id: &str, confirmed: bool) -> Command {
        ui_response(request_id, json!({"confirmed": confirmed}))
    }

    /// Dismiss a request without answering it (a `select` is then undecided, a `confirm` is no).
    pub fn ui_cancelled(request_id: &str) -> Command {
        ui_response(request_id, json!({"cancelled": true}))
    }

    /// Answer a `form` request with the submitted values by control name.
    pub fn ui_form(request_id: &str, data: Map<String, Value>) -> Command {
        ui_response(request_id, json!({"data": data}))
    }
}

fn ui_response(request_id: &str, body: Value) -> Command {
    let mut command = Command::raw("extension_ui_response", body);
    if let Value::Object(map) = &mut command.value {
        map.insert("id".into(), Value::String(request_id.to_owned()));
    }
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_carries_images_and_the_queue_mode_only_when_given() {
        let plain = Command::prompt("hi", &[], None).with_id("p1");
        assert_eq!(
            plain.as_value(),
            &json!({"id": "p1", "type": "prompt", "message": "hi"})
        );
        let image = Image {
            mime_type: "image/png".into(),
            data: "AAAA".into(),
        };
        let queued = Command::prompt("hi", &[image], Some(Streaming::FollowUp));
        assert_eq!(queued.as_value()["streamingBehavior"], "followUp");
        assert_eq!(
            queued.as_value()["images"],
            json!([{"type": "image", "mimeType": "image/png", "data": "AAAA"}])
        );
    }

    #[test]
    fn a_ui_answer_carries_the_request_id_and_no_command_id() {
        let answer = Command::ui_value("abc", "Approve");
        assert_eq!(
            answer.as_value(),
            &json!({"type": "extension_ui_response", "id": "abc", "value": "Approve"})
        );
        assert_eq!(answer.kind(), "extension_ui_response");
    }

    #[test]
    fn queues_and_levels_use_omp_spelling() {
        let remove = Command::remove_queued_message("x", Queue::FollowUp);
        assert_eq!(remove.as_value()["queue"], "followUp");
        let sub = Command::set_subagent_subscription(SubagentLevel::Events);
        assert_eq!(sub.as_value()["level"], "events");
    }
}
