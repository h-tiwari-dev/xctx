//! The session as a pane shows it: a pure reducer from omp's frames to state.
//!
//! [`SessionModel::apply`] takes one [`Frame`] and returns the [`Effect`]s the host has to carry out (commands to
//! send, gates to raise, phase changes). It never reads a clock or a file, so every test replays a recorded
//! conversation and compares snapshots. What it does not know, it ignores.

use std::collections::{BTreeMap, HashMap};

use serde_json::Value;

use crate::command::{Command, SubagentLevel};
use crate::frame::{Frame, PromptResult, Ready, Response, UiMethod, UiRequest};

/// How loud a [`Entry::Notice`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Something happened.
    Info,
    /// Something is off but the agent goes on.
    Warning,
    /// Something failed.
    Error,
}

/// A piece of an assistant message.
#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    /// What the model said.
    Text(String),
    /// What the model reasoned.
    Thinking(String),
    /// A tool call; the run is in [`SessionModel::tools`] under this id.
    ToolCall(String),
    /// An image the model produced.
    Image,
}

/// An assistant message, possibly still streaming.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AssistantEntry {
    /// omp's id for the message.
    pub message_id: String,
    /// Its content so far.
    pub blocks: Vec<Block>,
    /// Whether more is coming.
    pub streaming: bool,
    /// Why it stopped (`stop`, `toolUse`, `error`, `aborted`, ...), once it has.
    pub stop_reason: Option<String>,
    /// The provider's error text when it stopped on one.
    pub error: Option<String>,
    /// The model that wrote it.
    pub model: Option<String>,
}

/// One line of the transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// What the user (or, with `from_agent`, another agent) said.
    User {
        /// The text.
        text: String,
        /// How many images came with it.
        images: usize,
        /// Whether an agent wrote it (a parent's steer, a handoff) rather than the user.
        from_agent: bool,
    },
    /// What the model said.
    Assistant(AssistantEntry),
    /// A line that is not part of the conversation: a failure, a retry, a compaction, a notification.
    Notice {
        /// How loud.
        level: Level,
        /// What happened.
        text: String,
    },
}

/// How far a tool call has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolState {
    /// The model asked for it; it has not started.
    Pending,
    /// It runs.
    Running,
    /// It finished.
    Done,
    /// It finished with an error (including a denied approval).
    Failed,
}

/// A tool call and its result.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolRun {
    /// The call id.
    pub id: String,
    /// The tool, such as `bash`, `read`, `edit`, `task`.
    pub name: String,
    /// Its arguments as the model gave them.
    pub args: Value,
    /// How far it got.
    pub state: ToolState,
    /// The text it returned so far (a partial result while it runs).
    pub output: String,
    /// Its structured `details` (diffs, line counts, ...).
    pub details: Value,
}

/// Where the agent is, as the rest of xctx cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Nothing is running.
    Idle,
    /// The agent is working.
    Running,
    /// Something waits for the user: an approval, a form, a question.
    NeedsYou,
}

/// What the user must answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateKind {
    /// A tool wants permission.
    Approval {
        /// The tool.
        tool: String,
        /// What it will do.
        detail: String,
    },
    /// An HTML form from the `ask` tool.
    Form,
    /// A question with options.
    Select,
    /// Yes or no.
    Confirm,
    /// One line of text.
    Input,
    /// Several lines of text.
    Editor,
    /// Several questions.
    Ask,
}

/// A blocking request, summarised for the Inbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gate {
    /// The request id; answering it clears the gate.
    pub request_id: String,
    /// What kind.
    pub kind: GateKind,
    /// A one-line title.
    pub title: String,
}

/// What the host has to do after a frame.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    /// Send this to omp.
    Send(Command),
    /// The phase changed.
    PhaseChanged(Phase),
    /// The agent now waits for the user.
    GateRaised(Gate),
    /// A request is no longer pending (answered elsewhere, timed out or cancelled).
    GateCleared(String),
    /// omp reported which session the process runs.
    SessionBound(SessionInfo),
    /// A login flow wants this page opened.
    OpenUrl(String),
}

/// The session the process runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionInfo {
    /// omp's session id.
    pub id: Option<String>,
    /// The session's file; omp writes it with the first persisted entry, so it may not exist yet.
    pub file: Option<String>,
    /// The session's name.
    pub name: Option<String>,
}

/// The model the session runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ModelInfo {
    /// The provider.
    pub provider: String,
    /// The model id.
    pub id: String,
    /// A display name when omp gives one.
    pub name: Option<String>,
}

impl ModelInfo {
    /// What to show: the display name, else the id.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

/// How full the model's context window is.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ContextUsage {
    /// Tokens in context.
    pub tokens: u64,
    /// The window size.
    pub window: u64,
    /// Percent used, 0 to 100.
    pub percent: f64,
}

/// Totals from `get_session_stats`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stats {
    /// Tokens used.
    pub tokens: u64,
    /// Cost so far, in dollars.
    pub cost: f64,
    /// Tool calls made.
    pub tool_calls: u64,
}

/// Messages waiting for the agent, as chip text.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueueState {
    /// Messages that interrupt the running turn.
    pub steering: Vec<String>,
    /// Messages that wait for the turn to end.
    pub follow_up: Vec<String>,
}

/// A todo item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Todo {
    /// What to do.
    pub content: String,
    /// `pending`, `in_progress`, `completed` or `abandoned`.
    pub status: String,
}

/// A phase of the todo list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoPhase {
    /// The phase's name.
    pub name: String,
    /// Its items.
    pub tasks: Vec<Todo>,
}

/// A slash command omp offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlashCommand {
    /// The name without the slash.
    pub name: String,
    /// What it does.
    pub description: String,
    /// The argument hint, if it takes one.
    pub hint: Option<String>,
}

/// A side question and its answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BtwRecord {
    /// The record's id.
    pub id: String,
    /// What was asked.
    pub question: String,
    /// The answer so far.
    pub answer: String,
    /// `running`, `done`, `cancelled` or `error`.
    pub status: String,
}

/// A subagent the agent started.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Subagent {
    /// Its id (`ReadAB`, `ReadAB-2` for a retry).
    pub id: String,
    /// The agent type that runs it (`scout`).
    pub agent: String,
    /// The assignment it was given.
    pub task: String,
    /// The tool call that started it.
    pub parent_tool_call_id: String,
    /// `started`, `running`, `completed`, `failed` or `aborted`.
    pub status: String,
    /// Its own session file.
    pub session_file: Option<String>,
    /// Tools it has run.
    pub tool_count: u64,
    /// Tokens it has used.
    pub tokens: u64,
    /// How long it has run, in milliseconds.
    pub duration_ms: u64,
    /// The tool it ran last.
    pub current_tool: String,
    /// The last lines of what it said.
    pub recent_output: Vec<String>,
}

/// Messages requested per `get_messages_page` call when hydrating a resumed session.
const HISTORY_PAGE_LIMIT: u32 = 100;

/// Safety cap on pages fetched while hydrating one session's transcript.
const MAX_HISTORY_PAGES: u32 = 200;

/// The state of one omp session as a pane shows it.
#[derive(Debug, Clone, Default)]
pub struct SessionModel {
    /// The `ready` frame.
    pub ready: Option<Ready>,
    /// The transcript.
    pub entries: Vec<Entry>,
    /// Every tool call by id.
    pub tools: HashMap<String, ToolRun>,
    /// The session the process runs.
    pub session: SessionInfo,
    /// The model.
    pub model: Option<ModelInfo>,
    /// The thinking level in effect.
    pub thinking: Option<String>,
    /// The thinking levels the model supports.
    pub thinking_levels: Vec<String>,
    /// Context window use.
    pub context: Option<ContextUsage>,
    /// Token and cost totals.
    pub stats: Stats,
    /// The queued messages.
    pub queue: QueueState,
    /// The todo list.
    pub todos: Vec<TodoPhase>,
    /// The goal, as omp reports it.
    pub goal: Option<Value>,
    /// The slash commands.
    pub commands: Vec<SlashCommand>,
    /// Requests that wait for the user, oldest first.
    pub pending_ui: Vec<UiRequest>,
    /// Status segments omp's extensions set.
    pub status_segments: BTreeMap<String, String>,
    /// Text omp wants in the composer (`set_editor_text`), until the host takes it.
    pub composer_seed: Option<String>,
    /// Side questions.
    pub btw: Vec<BtwRecord>,
    /// Subagents by id.
    pub subagents: BTreeMap<String, Subagent>,
    /// Whether a compaction runs.
    pub compacting: bool,
    /// Whether omp reports the session settled (nothing can wake it).
    pub settled: bool,
    /// The generation of the last failure the host should show above the composer, if any.
    pub banner: Option<String>,
    running: bool,
    message_index: HashMap<String, usize>,
    history_gen: u64,
    history_page: u32,
    history_insert_at: usize,
    history_loading: bool,
    history_last_cursor: Option<String>,
}

fn s<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// The text parts of a message's `content` joined with newlines.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter(|p| s(p, "type") == Some("text"))
            .filter_map(|p| s(p, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn image_count(content: &Value) -> usize {
    content
        .as_array()
        .map(|a| a.iter().filter(|p| s(p, "type") == Some("image")).count())
        .unwrap_or(0)
}

impl SessionModel {
    /// An empty model, before omp has said anything.
    pub fn new() -> Self {
        SessionModel::default()
    }

    /// The commands to send once the handshake is done, so the pane has its first picture: the state, the slash
    /// commands, the session stats, and subagent progress updates.
    pub fn startup_commands() -> Vec<Command> {
        vec![
            Command::get_state().with_id("startup-state"),
            Command::get_available_commands().with_id("startup-commands"),
            Command::get_session_stats().with_id("startup-stats"),
            Command::set_subagent_subscription(SubagentLevel::Progress)
                .with_id("startup-subagents"),
        ]
    }

    /// Where the agent is.
    pub fn phase(&self) -> Phase {
        if self.pending_ui.iter().any(UiRequest::blocks) {
            Phase::NeedsYou
        } else if self.running {
            Phase::Running
        } else {
            Phase::Idle
        }
    }

    /// Whether a turn is running (a message sent now is queued, not started).
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// The request the user should answer first, if any.
    pub fn next_gate(&self) -> Option<&UiRequest> {
        self.pending_ui.iter().find(|r| r.blocks())
    }

    /// Record that request `id` was answered, and return what changed: its gate clearing and, when that was the
    /// last thing waiting on the user, the phase. Empty when it is not pending. The host sends the answer itself.
    pub fn resolve_ui(&mut self, id: &str) -> Vec<Effect> {
        let before = self.phase();
        let Some(at) = self.pending_ui.iter().position(|r| r.id == id) else {
            return Vec::new();
        };
        self.pending_ui.remove(at);
        let mut effects = vec![Effect::GateCleared(id.to_owned())];
        let after = self.phase();
        if after != before {
            effects.push(Effect::PhaseChanged(after));
        }
        effects
    }

    /// Apply one frame; return what the host has to do.
    pub fn apply(&mut self, frame: Frame) -> Vec<Effect> {
        let before = self.phase();
        let mut effects = Vec::new();
        match frame {
            Frame::Ready(ready) => self.ready = Some(ready),
            Frame::Response(response) => self.on_response(&response, &mut effects),
            Frame::PromptResult(result) => self.on_prompt_result(&result),
            Frame::SessionSettled => {
                self.settled = true;
                self.running = false;
                effects.push(Effect::Send(Command::get_state().with_id("settled-state")));
                effects.push(Effect::Send(
                    Command::get_session_stats().with_id("settled-stats"),
                ));
            }
            Frame::Ui(request) => self.on_ui(request, &mut effects),
            Frame::Event { kind, value } => self.on_event(&kind, &value, &mut effects),
            Frame::Unknown(_) => {}
        }
        let after = self.phase();
        if after != before {
            effects.push(Effect::PhaseChanged(after));
        }
        effects
    }

    /// Fold a subagent frame's payload in: a lifecycle payload carries `id`/`status`, a progress payload carries
    /// the same fields plus a `progress` object with counters.
    fn merge_subagent(&mut self, payload: &Value) {
        let progress = payload.get("progress").unwrap_or(&Value::Null);
        let Some(id) = s(payload, "id").or_else(|| s(progress, "id")) else {
            return;
        };
        let sub = self
            .subagents
            .entry(id.to_owned())
            .or_insert_with(|| Subagent {
                id: id.to_owned(),
                status: "started".to_owned(),
                ..Subagent::default()
            });
        if let Some(v) = s(payload, "agent") {
            v.clone_into(&mut sub.agent);
        }
        if let Some(v) = s(payload, "task") {
            v.clone_into(&mut sub.task);
        }
        if let Some(v) = s(payload, "parentToolCallId") {
            v.clone_into(&mut sub.parent_tool_call_id);
        }
        if let Some(v) = s(payload, "sessionFile") {
            sub.session_file = Some(v.to_owned());
        }
        if let Some(v) = s(payload, "status").or_else(|| s(progress, "status")) {
            v.clone_into(&mut sub.status);
        }
        if let Some(v) = progress.get("toolCount").and_then(Value::as_u64) {
            sub.tool_count = v;
        }
        if let Some(v) = progress.get("tokens").and_then(Value::as_u64) {
            sub.tokens = v;
        }
        if let Some(v) = progress.get("durationMs").and_then(Value::as_u64) {
            sub.duration_ms = v;
        }
        if let Some(tool) = progress
            .get("recentTools")
            .and_then(Value::as_array)
            .and_then(|a| a.last())
            .and_then(|t| s(t, "tool"))
        {
            tool.clone_into(&mut sub.current_tool);
        }
        if let Some(lines) = progress.get("recentOutput").and_then(Value::as_array) {
            sub.recent_output = lines
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
        }
    }

    fn notice(&mut self, level: Level, text: impl Into<String>) {
        self.entries.push(Entry::Notice {
            level,
            text: strip_ansi(&text.into()),
        });
    }

    fn on_response(&mut self, response: &Response, effects: &mut Vec<Effect>) {
        if !response.success {
            let text = response
                .error
                .clone()
                .unwrap_or_else(|| format!("{} failed", response.command));
            // A refused fork, a busy session and the like are worth a line; they never end the session.
            self.banner = Some(format!("{}: {text}", response.command));
            return;
        }
        let data = &response.data;
        match response.command.as_str() {
            "get_state" => self.apply_state(data, effects),
            "get_available_commands" => self.apply_commands(data),
            "get_session_stats" => {
                self.stats = Stats {
                    tokens: data
                        .pointer("/tokens/total")
                        .and_then(Value::as_u64)
                        .unwrap_or(0),
                    cost: data.get("cost").and_then(Value::as_f64).unwrap_or(0.0),
                    tool_calls: data.get("toolCalls").and_then(Value::as_u64).unwrap_or(0),
                };
            }
            "get_btw_history" => {
                self.btw = data
                    .get("records")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(btw_record).collect())
                    .unwrap_or_default();
            }
            "get_subagents" => {
                if let Some(list) = data.get("subagents").and_then(Value::as_array) {
                    for item in list {
                        self.merge_subagent(item);
                    }
                }
            }
            "new_session" if data.get("cancelled").and_then(Value::as_bool) != Some(true) => {
                self.reset_session_view();
                self.history_gen = self.history_gen.wrapping_add(1);
                self.history_loading = false;
                self.history_last_cursor = None;
                effects.push(Effect::Send(Command::get_state().with_id("session-state")));
                effects.push(Effect::Send(
                    Command::get_session_stats().with_id("session-stats"),
                ));
            }
            "open_session" | "fork" | "switch_session"
                if data.get("cancelled").and_then(Value::as_bool) != Some(true) =>
            {
                self.reset_session_view();
                effects.push(Effect::Send(Command::get_state().with_id("session-state")));
                self.start_history_load(effects);
            }
            "get_messages_page" => self.apply_messages_page(response.id.as_deref(), data, effects),
            _ => {}
        }
    }

    fn reset_session_view(&mut self) {
        self.entries.clear();
        self.tools.clear();
        self.message_index.clear();
        self.queue = QueueState::default();
        self.pending_ui.clear();
        self.btw.clear();
        self.subagents.clear();
        self.todos.clear();
        self.goal = None;
        self.context = None;
        self.stats = Stats::default();
        self.banner = None;
        self.history_insert_at = 0;
    }

    fn history_page_id(&self) -> String {
        format!(
            "session-messages-{}-{}",
            self.history_gen, self.history_page
        )
    }

    fn start_history_load(&mut self, effects: &mut Vec<Effect>) {
        self.history_gen = self.history_gen.wrapping_add(1);
        self.history_page = 1;
        self.history_insert_at = 0;
        self.history_loading = true;
        self.history_last_cursor = None;
        effects.push(Effect::Send(
            Command::get_session_stats().with_id("session-stats"),
        ));
        effects.push(Effect::Send(
            Command::get_messages_page(None, HISTORY_PAGE_LIMIT).with_id(self.history_page_id()),
        ));
        effects.push(Effect::Send(
            Command::get_btw_history().with_id("session-btw"),
        ));
        effects.push(Effect::Send(
            Command::get_subagents().with_id("session-subagents"),
        ));
    }

    fn insert_history_entry(&mut self, entry: Entry) {
        let at = self.history_insert_at.min(self.entries.len());
        self.entries.insert(at, entry);
        for idx in self.message_index.values_mut() {
            if *idx >= at {
                *idx += 1;
            }
        }
        self.history_insert_at = at + 1;
    }

    fn apply_messages_page(&mut self, id: Option<&str>, data: &Value, effects: &mut Vec<Effect>) {
        if let Some(id) = id
            && id.starts_with("session-messages-")
        {
            let expected_prefix = format!("session-messages-{}-", self.history_gen);
            if !id.starts_with(&expected_prefix) {
                return;
            }
        }
        let messages = data
            .get("messages")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        for msg in messages {
            self.apply_history_message(msg);
        }
        let next = s(data, "nextCursor").filter(|c| !c.is_empty());
        if let Some(cursor) = next
            && !messages.is_empty()
            && self.history_page < MAX_HISTORY_PAGES
            && self.history_last_cursor.as_deref() != Some(cursor)
        {
            self.history_last_cursor = Some(cursor.to_owned());
            self.history_page += 1;
            effects.push(Effect::Send(
                Command::get_messages_page(Some(cursor), HISTORY_PAGE_LIMIT)
                    .with_id(self.history_page_id()),
            ));
        } else {
            self.history_loading = false;
            self.history_last_cursor = None;
        }
    }

    fn apply_history_message(&mut self, message: &Value) {
        match s(message, "role") {
            Some("user") => {
                let content = &message["content"];
                self.insert_history_entry(Entry::User {
                    text: content_text(content),
                    images: image_count(content),
                    from_agent: s(message, "attribution") == Some("agent"),
                });
            }
            Some("assistant") => {
                let blocks = self.blocks_from(&message["content"]);
                self.insert_history_entry(Entry::Assistant(AssistantEntry {
                    message_id: s(message, "id").unwrap_or_default().to_owned(),
                    blocks,
                    streaming: false,
                    stop_reason: s(message, "stopReason").map(str::to_owned),
                    error: s(message, "errorMessage").map(str::to_owned),
                    model: s(message, "model").map(str::to_owned),
                }));
            }
            Some("toolResult") => {
                let id = s(message, "toolCallId").unwrap_or_default().to_owned();
                if id.is_empty() {
                    return;
                }
                self.ensure_tool(&id, s(message, "toolName"), None);
                let failed = message
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if let Some(run) = self.tools.get_mut(&id) {
                    run.state = if failed {
                        ToolState::Failed
                    } else {
                        ToolState::Done
                    };
                    run.output = content_text(&message["content"]);
                    run.details = message.get("details").cloned().unwrap_or(Value::Null);
                }
            }
            Some("bashExecution") => {
                let cmd = s(message, "command").unwrap_or_default();
                let out = s(message, "output").unwrap_or_default();
                let text = match (cmd.is_empty(), out.is_empty()) {
                    (false, false) => format!("$ {cmd}\n{out}"),
                    (false, true) => format!("$ {cmd}"),
                    (true, false) => out.to_owned(),
                    (true, true) => return,
                };
                self.insert_history_entry(Entry::Notice {
                    level: Level::Info,
                    text: strip_ansi(&text),
                });
            }
            Some("compactionSummary" | "branchSummary") => {
                if let Some(summary) = s(message, "summary").filter(|t| !t.trim().is_empty()) {
                    self.insert_history_entry(Entry::Notice {
                        level: Level::Info,
                        text: strip_ansi(summary),
                    });
                }
            }
            Some(_) if message.get("display").and_then(Value::as_bool) != Some(false) => {
                let text = content_text(&message["content"]);
                if !text.is_empty() {
                    self.insert_history_entry(Entry::Notice {
                        level: Level::Info,
                        text: strip_ansi(&text),
                    });
                }
            }
            Some(_) | None => {}
        }
    }

    fn apply_state(&mut self, data: &Value, effects: &mut Vec<Effect>) {
        if let Some(model) = data.get("model").filter(|m| m.is_object()) {
            self.model = Some(ModelInfo {
                provider: s(model, "provider").unwrap_or_default().to_owned(),
                id: s(model, "id").unwrap_or_default().to_owned(),
                name: s(model, "name").map(str::to_owned),
            });
        }
        if let Some(level) = s(data, "thinkingLevel") {
            self.thinking = Some(level.to_owned());
        }
        if data.get("thinkingLevels").is_some() {
            self.thinking_levels = strings(&data["thinkingLevels"]);
        }
        if let Some(streaming) = data.get("isStreaming").and_then(Value::as_bool) {
            self.running = streaming;
        }
        if let Some(compacting) = data.get("isCompacting").and_then(Value::as_bool) {
            self.compacting = compacting;
        }
        if let Some(settled) = data.get("isSettled").and_then(Value::as_bool) {
            self.settled = settled;
        }
        if let Some(usage) = data.get("contextUsage").filter(|u| u.is_object()) {
            self.context = Some(ContextUsage {
                tokens: usage.get("tokens").and_then(Value::as_u64).unwrap_or(0),
                window: usage
                    .get("contextWindow")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
                percent: usage.get("percent").and_then(Value::as_f64).unwrap_or(0.0),
            });
        }
        if let Some(phases) = data.get("todoPhases").and_then(Value::as_array) {
            self.todos = phases.iter().map(todo_phase).collect();
        }
        if let Some(q) = data.get("queuedMessages").filter(|q| q.is_object()) {
            self.queue = QueueState {
                steering: strings(&q["steering"]),
                follow_up: strings(&q["followUp"]),
            };
        }
        self.goal = data.get("goal").filter(|g| !g.is_null()).cloned();
        let session = SessionInfo {
            id: s(data, "sessionId").map(str::to_owned),
            file: s(data, "sessionFile").map(str::to_owned),
            name: s(data, "sessionName").map(str::to_owned),
        };
        if session != self.session {
            self.session = session.clone();
            effects.push(Effect::SessionBound(session));
        }
        // If omp started directly on a resumed session (e.g. via CLI flags rather than `open_session`)
        // and reports prior messages we have not hydrated yet, fetch its transcript now.
        if data
            .get("messageCount")
            .and_then(Value::as_u64)
            .unwrap_or(0)
            > 0
            && self.entries.is_empty()
            && !self.history_loading
            && !self.running
        {
            self.start_history_load(effects);
        }
    }

    fn apply_commands(&mut self, data: &Value) {
        self.commands = data
            .get("commands")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(slash_command).collect())
            .unwrap_or_default();
    }

    fn on_prompt_result(&mut self, result: &PromptResult) {
        if result.status != "error" {
            return;
        }
        let message = result
            .error
            .as_ref()
            .and_then(|e| s(e, "message"))
            .unwrap_or("The turn failed")
            .to_owned();
        self.notice(Level::Error, message);
    }

    fn on_ui(&mut self, request: UiRequest, effects: &mut Vec<Effect>) {
        match &request.method {
            UiMethod::Cancel { target_id } => {
                let target = target_id.clone();
                if let Some(at) = self.pending_ui.iter().position(|r| r.id == target) {
                    self.pending_ui.remove(at);
                    effects.push(Effect::GateCleared(target));
                }
            }
            UiMethod::Notify { message, level } => {
                let level = match level.as_str() {
                    "error" => Level::Error,
                    "warning" => Level::Warning,
                    _ => Level::Info,
                };
                let message = message.clone();
                self.notice(level, message);
            }
            UiMethod::SetStatus { key, text } => match text {
                Some(text) => {
                    self.status_segments.insert(key.clone(), text.clone());
                }
                None => {
                    self.status_segments.remove(key);
                }
            },
            UiMethod::SetEditorText { text } => self.composer_seed = Some(text.clone()),
            UiMethod::OpenUrl { url, .. } => effects.push(Effect::OpenUrl(url.clone())),
            UiMethod::Ignored { .. } => {}
            _ => {
                if let Some(gate) = gate_of(&request) {
                    effects.push(Effect::GateRaised(gate));
                }
                self.pending_ui.push(request);
            }
        }
    }

    fn on_event(&mut self, kind: &str, v: &Value, effects: &mut Vec<Effect>) {
        match kind {
            "agent_start" => {
                self.running = true;
                self.settled = false;
            }
            "agent_end" => {
                let terminal = v.get("isTerminal").and_then(Value::as_bool).unwrap_or(true);
                let yielded = v.get("yielded").and_then(Value::as_bool).unwrap_or(true);
                if terminal && yielded {
                    self.running = false;
                    for entry in &mut self.entries {
                        if let Entry::Assistant(a) = entry {
                            a.streaming = false;
                        }
                    }
                }
            }
            "message_start" => self.on_message_start(v),
            "message_update" => self.on_message_update(v),
            "message_end" => self.on_message_end(v),
            "tool_execution_start" => self.on_tool_start(v),
            "tool_execution_update" | "tool_stream_update" => self.on_tool_update(v),
            "tool_execution_end" => self.on_tool_end(v),
            "queue_update" => {
                self.queue = QueueState {
                    steering: strings(&v["steering"]),
                    follow_up: strings(&v["followUp"]),
                };
            }
            "model_changed" | "thinking_level_changed" => {
                effects.push(Effect::Send(Command::get_state().with_id("model-state")));
            }
            "manual_compaction_start" | "auto_compaction_start" => {
                self.compacting = true;
                self.notice(Level::Info, "Compacting the conversation");
            }
            "manual_compaction_end" | "auto_compaction_end" => {
                self.compacting = false;
                let aborted = v.get("aborted").and_then(Value::as_bool).unwrap_or(false);
                let text = match (aborted, s(v, "errorMessage")) {
                    (_, Some(error)) => format!("Compaction: {error}"),
                    (true, None) => "Compaction cancelled".to_owned(),
                    (false, None) => "Conversation compacted".to_owned(),
                };
                self.notice(Level::Info, text);
                effects.push(Effect::Send(
                    Command::get_state().with_id("compaction-state"),
                ));
            }
            "auto_retry_start" => {
                let text = s(v, "errorMessage")
                    .map_or_else(|| "Retrying".to_owned(), |e| format!("Retrying after: {e}"));
                self.notice(Level::Warning, text);
            }
            "retry_fallback_applied" => {
                let to = s(v, "to").unwrap_or("another model");
                self.notice(Level::Warning, format!("Switched to {to} after a failure"));
            }
            "goal_updated" => self.goal = v.get("goal").filter(|g| !g.is_null()).cloned(),
            "notice" | "command_output" => {
                if let Some(text) = s(v, "message").or_else(|| s(v, "text")) {
                    self.notice(Level::Info, text);
                }
            }
            "extension_error" => {
                let text = format!(
                    "Extension error: {}",
                    s(v, "error").unwrap_or("unknown error")
                );
                self.notice(Level::Warning, text);
            }
            "rpc_frame_error" => {
                self.banner = Some("omp sent a frame that did not fit the transport".into());
            }
            "available_commands_update" => self.apply_commands(v),
            "btw_record" => {
                if let Some(record) = v.get("record").and_then(btw_record) {
                    match self.btw.iter_mut().find(|r| r.id == record.id) {
                        Some(existing) => *existing = record,
                        None => self.btw.push(record),
                    }
                }
            }
            "btw_delta" => {
                if let (Some(id), Some(delta)) = (s(v, "recordId"), s(v, "delta"))
                    && let Some(record) = self.btw.iter_mut().find(|r| r.id == id)
                {
                    record.answer.push_str(delta);
                }
            }
            "subagent_lifecycle" | "subagent_progress" => {
                if let Some(payload) = v.get("payload") {
                    self.merge_subagent(payload);
                }
            }
            _ => {}
        }
    }

    fn assistant_at(&mut self, message_id: &str) -> Option<&mut AssistantEntry> {
        let at = *self.message_index.get(message_id)?;
        match self.entries.get_mut(at) {
            Some(Entry::Assistant(a)) => Some(a),
            _ => None,
        }
    }

    fn blocks_from(&mut self, content: &Value) -> Vec<Block> {
        let Some(parts) = content.as_array() else {
            return Vec::new();
        };
        let mut blocks = Vec::with_capacity(parts.len());
        for part in parts {
            match s(part, "type") {
                Some("text") => blocks.push(Block::Text(s(part, "text").unwrap_or("").to_owned())),
                Some("thinking") => {
                    blocks.push(Block::Thinking(
                        s(part, "thinking").unwrap_or("").to_owned(),
                    ));
                }
                Some("redacted_thinking") => blocks.push(Block::Thinking(String::new())),
                Some("toolCall") => {
                    let id = s(part, "id").unwrap_or_default().to_owned();
                    self.ensure_tool(&id, s(part, "name"), part.get("arguments"));
                    blocks.push(Block::ToolCall(id));
                }
                Some("image") => blocks.push(Block::Image),
                _ => {}
            }
        }
        blocks
    }

    fn ensure_tool(&mut self, id: &str, name: Option<&str>, args: Option<&Value>) {
        let run = self.tools.entry(id.to_owned()).or_insert_with(|| ToolRun {
            id: id.to_owned(),
            name: String::new(),
            args: Value::Null,
            state: ToolState::Pending,
            output: String::new(),
            details: Value::Null,
        });
        if let Some(name) = name
            && !name.is_empty()
        {
            run.name = name.to_owned();
        }
        if let Some(args) = args
            && !args.is_null()
            && args.as_object().is_none_or(|o| !o.is_empty())
        {
            run.args = args.clone();
        }
    }

    fn on_message_start(&mut self, v: &Value) {
        let Some(message) = v.get("message") else {
            return;
        };
        match s(message, "role") {
            Some("user") => {
                let content = &message["content"];
                self.entries.push(Entry::User {
                    text: content_text(content),
                    images: image_count(content),
                    from_agent: s(message, "attribution") == Some("agent"),
                });
            }
            Some("assistant") => {
                let blocks = self.blocks_from(&message["content"]);
                let id = s(v, "messageId").unwrap_or_default().to_owned();
                self.message_index.insert(id.clone(), self.entries.len());
                self.entries.push(Entry::Assistant(AssistantEntry {
                    message_id: id,
                    blocks,
                    streaming: true,
                    stop_reason: None,
                    error: None,
                    model: s(message, "model").map(str::to_owned),
                }));
            }
            // Results are shown on their tool card; other roles carry custom messages.
            Some("toolResult") | None => {}
            Some(_) => {
                if message.get("display").and_then(Value::as_bool) != Some(false) {
                    let text = content_text(&message["content"]);
                    if !text.is_empty() {
                        self.notice(Level::Info, text);
                    }
                }
            }
        }
    }

    fn on_message_update(&mut self, v: &Value) {
        let Some(id) = s(v, "messageId").map(str::to_owned) else {
            return;
        };
        let Some(event) = v.get("assistantMessageEvent") else {
            return;
        };
        // Whole-message snapshots (the default stream) replace the blocks; deltas patch them.
        let snapshot = event
            .pointer("/partial/content")
            .or_else(|| event.pointer("/message/content"))
            .cloned();
        if let Some(content) = snapshot {
            let blocks = self.blocks_from(&content);
            if let Some(entry) = self.assistant_at(&id) {
                entry.blocks = blocks;
            }
            return;
        }
        let index = event
            .get("contentIndex")
            .and_then(Value::as_u64)
            .unwrap_or(0) as usize;
        let kind = s(event, "type").unwrap_or_default();
        match kind {
            "text_start" | "thinking_start" | "toolcall_start" | "image_end" => {
                let shell = event.get("block").cloned().unwrap_or(Value::Null);
                let block = match kind {
                    "text_start" => Block::Text(String::new()),
                    "thinking_start" => Block::Thinking(String::new()),
                    "image_end" => Block::Image,
                    _ => {
                        let call = s(&shell, "id").unwrap_or_default().to_owned();
                        self.ensure_tool(&call, s(&shell, "name"), shell.get("arguments"));
                        Block::ToolCall(call)
                    }
                };
                if let Some(entry) = self.assistant_at(&id) {
                    set_block(&mut entry.blocks, index, block);
                }
            }
            "text_delta" | "thinking_delta" => {
                let delta = s(event, "delta").unwrap_or_default().to_owned();
                let thinking = kind == "thinking_delta";
                if let Some(entry) = self.assistant_at(&id) {
                    append_delta(&mut entry.blocks, index, &delta, thinking);
                }
            }
            "text_end" | "thinking_end" => {
                let content = s(event, "content").unwrap_or_default().to_owned();
                let block = if kind == "text_end" {
                    Block::Text(content)
                } else {
                    Block::Thinking(content)
                };
                if let Some(entry) = self.assistant_at(&id) {
                    set_block(&mut entry.blocks, index, block);
                }
            }
            "toolcall_end" => {
                if let Some(call) = event.get("toolCall") {
                    let call_id = s(call, "id").unwrap_or_default().to_owned();
                    self.ensure_tool(&call_id, s(call, "name"), call.get("arguments"));
                    if let Some(entry) = self.assistant_at(&id) {
                        set_block(&mut entry.blocks, index, Block::ToolCall(call_id));
                    }
                }
            }
            "start" => {
                if let Some(content) = event.pointer("/message/content").cloned() {
                    let blocks = self.blocks_from(&content);
                    if let Some(entry) = self.assistant_at(&id) {
                        entry.blocks = blocks;
                    }
                }
            }
            _ => {}
        }
    }

    fn on_message_end(&mut self, v: &Value) {
        let Some(message) = v.get("message") else {
            return;
        };
        if s(message, "role") != Some("assistant") {
            return;
        }
        let id = s(v, "messageId").unwrap_or_default().to_owned();
        let blocks = self.blocks_from(&message["content"]);
        if !self.message_index.contains_key(&id) {
            self.message_index.insert(id.clone(), self.entries.len());
            self.entries.push(Entry::Assistant(AssistantEntry {
                message_id: id.clone(),
                ..AssistantEntry::default()
            }));
        }
        if let Some(entry) = self.assistant_at(&id) {
            entry.blocks = blocks;
            entry.streaming = false;
            entry.stop_reason = s(message, "stopReason").map(str::to_owned);
            entry.error = s(message, "errorMessage").map(str::to_owned);
            if entry.model.is_none() {
                entry.model = s(message, "model").map(str::to_owned);
            }
        }
    }

    fn on_tool_start(&mut self, v: &Value) {
        let id = s(v, "toolCallId").unwrap_or_default().to_owned();
        self.ensure_tool(&id, s(v, "toolName"), v.get("args"));
        if let Some(run) = self.tools.get_mut(&id) {
            run.state = ToolState::Running;
        }
    }

    fn on_tool_update(&mut self, v: &Value) {
        let id = s(v, "toolCallId").unwrap_or_default().to_owned();
        let result = v.get("partialResult").or_else(|| v.get("result"));
        if let (Some(run), Some(result)) = (self.tools.get_mut(&id), result) {
            let text = content_text(&result["content"]);
            if !text.is_empty() {
                run.output = text;
            }
        }
    }

    fn on_tool_end(&mut self, v: &Value) {
        let id = s(v, "toolCallId").unwrap_or_default().to_owned();
        self.ensure_tool(&id, s(v, "toolName"), None);
        let failed = v.get("isError").and_then(Value::as_bool).unwrap_or(false);
        if let Some(run) = self.tools.get_mut(&id) {
            run.state = if failed {
                ToolState::Failed
            } else {
                ToolState::Done
            };
            if let Some(result) = v.get("result") {
                run.output = content_text(&result["content"]);
                run.details = result.get("details").cloned().unwrap_or(Value::Null);
            }
        }
    }
}

fn set_block(blocks: &mut Vec<Block>, index: usize, block: Block) {
    if index < blocks.len() {
        blocks[index] = block;
    } else {
        blocks.push(block);
    }
}

fn append_delta(blocks: &mut Vec<Block>, index: usize, delta: &str, thinking: bool) {
    match blocks.get_mut(index) {
        Some(Block::Text(text)) if !thinking => text.push_str(delta),
        Some(Block::Thinking(text)) if thinking => text.push_str(delta),
        _ => {
            let text = delta.to_owned();
            set_block(
                blocks,
                index,
                if thinking {
                    Block::Thinking(text)
                } else {
                    Block::Text(text)
                },
            );
        }
    }
}

fn gate_of(request: &UiRequest) -> Option<Gate> {
    let (kind, title) = match &request.method {
        UiMethod::Select { title, .. } => match request.approval() {
            Some(a) => (
                GateKind::Approval {
                    tool: a.tool.clone(),
                    detail: a.detail,
                },
                format!("Allow {}?", a.tool),
            ),
            None => (GateKind::Select, title.clone()),
        },
        UiMethod::Confirm { title, .. } => (GateKind::Confirm, title.clone()),
        UiMethod::Input { title, .. } => (GateKind::Input, title.clone()),
        UiMethod::Editor { title, .. } => (GateKind::Editor, title.clone()),
        UiMethod::Form { .. } => (GateKind::Form, "The agent asks a question".to_owned()),
        UiMethod::Ask { .. } => (GateKind::Ask, "The agent asks questions".to_owned()),
        _ => return None,
    };
    Some(Gate {
        request_id: request.id.clone(),
        kind,
        title,
    })
}

fn todo_phase(v: &Value) -> TodoPhase {
    TodoPhase {
        name: s(v, "name").unwrap_or_default().to_owned(),
        tasks: v
            .get("tasks")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|t| Todo {
                        content: s(t, "content").unwrap_or_default().to_owned(),
                        status: s(t, "status").unwrap_or("pending").to_owned(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn slash_command(v: &Value) -> Option<SlashCommand> {
    Some(SlashCommand {
        name: s(v, "name")?.to_owned(),
        description: s(v, "description").unwrap_or_default().to_owned(),
        hint: v
            .pointer("/input/hint")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn btw_record(v: &Value) -> Option<BtwRecord> {
    Some(BtwRecord {
        id: s(v, "id")?.to_owned(),
        question: s(v, "question").unwrap_or_default().to_owned(),
        answer: s(v, "answer").unwrap_or_default().to_owned(),
        status: s(v, "status").unwrap_or_default().to_owned(),
    })
}

/// `text` without terminal colour and cursor codes: slash-command output arrives dressed for a terminal.
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for n in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(n) = chars.next() {
                    if n == '\u{7}' || (n == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod ansi_tests {
    use serde_json::json;

    use super::{Block, Effect, Entry, Frame, SessionModel, ToolState, strip_ansi};

    #[test]
    fn colour_and_link_codes_go_and_text_stays() {
        assert_eq!(
            strip_ansi("\u{1b}[1m\u{1b}[38;2;0;180;255m░░\u{1b}[22m\u{1b}[39m 1%"),
            "░░ 1%"
        );
        assert_eq!(
            strip_ansi("\u{1b}]8;;http://x\u{7}link\u{1b}]8;;\u{7}"),
            "link"
        );
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn open_session_hydrates_paginated_history_and_cross_page_tool_results() {
        let mut model = SessionModel::new();
        let effects = model.apply(Frame::parse(json!({
            "id": "o1",
            "type": "response",
            "command": "open_session",
            "success": true,
            "data": {"cancelled": false, "resumed": true, "sessionId": "s1"}
        })));
        let cmds: Vec<&str> = effects
            .iter()
            .filter_map(|e| match e {
                Effect::Send(c) => Some(c.kind()),
                _ => None,
            })
            .collect();
        assert!(cmds.contains(&"get_state"), "{cmds:?}");
        assert!(cmds.contains(&"get_session_stats"), "{cmds:?}");
        assert!(cmds.contains(&"get_messages_page"), "{cmds:?}");
        assert!(cmds.contains(&"get_btw_history"), "{cmds:?}");
        assert!(cmds.contains(&"get_subagents"), "{cmds:?}");

        let page1_id = effects
            .iter()
            .find_map(|e| match e {
                Effect::Send(c) if c.kind() == "get_messages_page" => c.id().map(str::to_owned),
                _ => None,
            })
            .expect("page 1 command");

        // Page 1 has a user prompt and an assistant message calling tool `t1` (whose result is on page 2).
        let next_effects = model.apply(Frame::parse(json!({
            "id": page1_id,
            "type": "response",
            "command": "get_messages_page",
            "success": true,
            "data": {
                "totalMessages": 4,
                "nextCursor": "cur-2",
                "messages": [
                    {
                        "role": "user",
                        "content": [{"type": "text", "text": "Fix checkout retries"}],
                        "attribution": "user"
                    },
                    {
                        "role": "assistant",
                        "model": "claude-sonnet",
                        "stopReason": "toolUse",
                        "content": [
                            {"type": "thinking", "thinking": "Checking files"},
                            {"type": "toolCall", "id": "t1", "name": "read", "arguments": {"path": "src/pay.rs"}}
                        ]
                    }
                ]
            }
        })));
        let page2_cmd = next_effects
            .iter()
            .find_map(|e| match e {
                Effect::Send(c) if c.kind() == "get_messages_page" => Some(c),
                _ => None,
            })
            .expect("requests page 2 with cursor");
        assert_eq!(page2_cmd.as_value()["cursor"], "cur-2");
        assert_eq!(model.tools["t1"].state, ToolState::Pending);

        // Page 2 completes the tool result and adds the final assistant reply.
        let done_effects = model.apply(Frame::parse(json!({
            "id": page2_cmd.id().unwrap(),
            "type": "response",
            "command": "get_messages_page",
            "success": true,
            "data": {
                "totalMessages": 4,
                "messages": [
                    {
                        "role": "toolResult",
                        "toolCallId": "t1",
                        "toolName": "read",
                        "isError": false,
                        "content": [{"type": "text", "text": "fn retry() {}"}]
                    },
                    {
                        "role": "assistant",
                        "model": "claude-sonnet",
                        "stopReason": "stop",
                        "content": [{"type": "text", "text": "Retries now reuse one idempotency key."}]
                    }
                ]
            }
        })));
        assert!(
            !done_effects
                .iter()
                .any(|e| matches!(e, Effect::Send(c) if c.kind() == "get_messages_page")),
            "pagination stops when nextCursor is absent"
        );
        assert_eq!(model.entries.len(), 3);
        assert!(matches!(
            &model.entries[0],
            Entry::User { text, from_agent: false, .. } if text == "Fix checkout retries"
        ));
        assert_eq!(model.tools["t1"].state, ToolState::Done);
        assert_eq!(model.tools["t1"].output, "fn retry() {}");
        assert!(matches!(
            &model.entries[2],
            Entry::Assistant(a) if !a.streaming && a.blocks == vec![Block::Text("Retries now reuse one idempotency key.".into())]
        ));
    }

    #[test]
    fn stale_history_pages_are_ignored_and_live_messages_stay_after_history() {
        let mut model = SessionModel::new();
        let effects = model.apply(Frame::parse(json!({
            "id": "o1",
            "type": "response",
            "command": "open_session",
            "success": true,
            "data": {"cancelled": false, "resumed": true}
        })));
        let page1_id = effects
            .iter()
            .find_map(|e| match e {
                Effect::Send(c) if c.kind() == "get_messages_page" => c.id().map(str::to_owned),
                _ => None,
            })
            .unwrap();

        // A live user prompt arrives before page 1 returns.
        model.apply(Frame::parse(json!({
            "type": "message_start",
            "message": {"role": "user", "content": "live follow-up"}
        })));
        // Page 1 from the current generation inserts before the live message.
        model.apply(Frame::parse(json!({
            "id": page1_id,
            "type": "response",
            "command": "get_messages_page",
            "success": true,
            "data": {
                "totalMessages": 1,
                "messages": [{"role": "user", "content": "earlier prompt"}]
            }
        })));
        assert_eq!(model.entries.len(), 2);
        assert!(matches!(&model.entries[0], Entry::User { text, .. } if text == "earlier prompt"));
        assert!(matches!(&model.entries[1], Entry::User { text, .. } if text == "live follow-up"));

        // A stale page from an older generation is ignored after new_session.
        model.apply(Frame::parse(json!({
            "id": "n1",
            "type": "response",
            "command": "new_session",
            "success": true,
            "data": {"cancelled": false}
        })));
        model.apply(Frame::parse(json!({
            "id": page1_id,
            "type": "response",
            "command": "get_messages_page",
            "success": true,
            "data": {
                "totalMessages": 1,
                "messages": [{"role": "user", "content": "stale"}]
            }
        })));
        assert!(model.entries.is_empty());
    }

    #[test]
    fn get_state_with_unloaded_messages_triggers_history_load() {
        let mut model = SessionModel::new();
        let effects = model.apply(Frame::parse(json!({
            "id": "startup-state",
            "type": "response",
            "command": "get_state",
            "success": true,
            "data": {
                "sessionId": "s1",
                "isStreaming": false,
                "messageCount": 6
            }
        })));
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Send(c) if c.kind() == "get_messages_page"))
        );
    }
}
