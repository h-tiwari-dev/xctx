//! An agent xctx runs over omp's RPC protocol (a runner with `interface = "rpc"`): its child process, the
//! session model its frames feed, and what the rest of xctx reads from it.
//!
//! An [`OmpAgent`] turns frames into [`AgentUpdate`]s: its state for the graph (`agent_start` is Running, a
//! yield is Idle, `session_settled` is Done, any request waiting on the user is Blocked, an unexpected exit is
//! Failed), the omp session it ran once that session is on disk, the requests that wait on the user (each also
//! an Inbox item, see [`OmpAgent::apply_updates`]), and a retry badge for the Roster. Nothing here answers a
//! request: approvals stay the user's. There is no window in sight, so the workspace and the tests (against
//! `fake-omp`) drive it the same way.
//!
//! Every agent has its own session folder, `<data>/omp-sessions/<space>/<agent>` ([`agent_session_dir`]): omp
//! chooses its session ids, so resuming is `open_session` on that folder, which continues its newest session.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
#[cfg(test)]
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use xctx_control::RpcError;
use xctx_control::protocol::{
    AgentDelivery, AgentProcess, AgentReadResult, AgentSendResult, WhileRunning,
};
use xctx_core::{Redactor, SpaceId};
use xctx_graph::{AgentNodeId, AgentState};
use xctx_inbox::{Inbox, ItemBody, ItemId, Level, Notice, OmpGate, Source};
use xctx_omp::dialog::permission_of;
use xctx_omp::{
    Block, Command, Entry, Frame, Incoming, LaunchSpec, OmpClient, OmpError, SessionModel, Started,
    UiMethod, UiRequest,
};

use crate::space_graph::{SpaceGraphState, graph_error};
use crate::spawn::RpcLaunch;

/// The folder under the data directory that holds every RPC agent's session folder.
pub const SESSIONS_DIR: &str = "omp-sessions";

/// How many times an agent whose child exited on its own is started again before it stays failed. A turn that
/// settles resets the count.
pub const MAX_RESTARTS: u32 = 3;

/// How long a child may take for each handshake step.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);

/// How long the session lease check waits for its answer.
pub const LEASE_TIMEOUT: Duration = Duration::from_secs(15);

/// The session folder of agent `agent` of space `space`: `<data_dir>/omp-sessions/<space>/<agent>`, with every
/// character of the agent id other than letters, digits, `-`, `_` and `#` written as `_`, so it is always one
/// folder of its own.
pub fn agent_session_dir(data_dir: &Path, space: &SpaceId, agent: &str) -> PathBuf {
    let folder: String = agent
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '#') {
                c
            } else {
                '_'
            }
        })
        .collect();
    data_dir
        .join(SESSIONS_DIR)
        .join(space.as_str())
        .join(folder)
}

/// The client spec that starts `launch` with `args`.
pub fn launch_spec(launch: &RpcLaunch, args: Vec<String>) -> LaunchSpec {
    let mut spec = LaunchSpec::new(&launch.program, &launch.cwd);
    spec.args = args.into_iter().map(Into::into).collect();
    spec.env = launch
        .env
        .iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
    spec.handshake_timeout = HANDSHAKE_TIMEOUT;
    spec
}

/// What an RPC agent is doing, as its frames say.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentStatus {
    /// Its child has not answered yet.
    Starting,
    /// A turn runs (`agent_start`).
    Running,
    /// It yielded and waits for a message.
    Idle,
    /// Its session settled: nothing can wake it but a new message.
    Done,
    /// A request waits on the user.
    Blocked {
        /// What it waits on.
        reason: String,
    },
    /// Its child did not start, or exited without being asked to.
    Failed {
        /// What happened.
        reason: String,
    },
    /// Its child was stopped by xctx.
    Stopped,
}

impl AgentStatus {
    /// The graph state it means; `None` while starting and once stopped, which leave the graph as it was.
    pub fn graph_state(&self) -> Option<AgentState> {
        match self {
            AgentStatus::Starting | AgentStatus::Stopped => None,
            AgentStatus::Running => Some(AgentState::Running),
            AgentStatus::Idle => Some(AgentState::Idle),
            AgentStatus::Done => Some(AgentState::Done),
            AgentStatus::Blocked { reason } => Some(AgentState::Blocked {
                reason: reason.clone(),
            }),
            AgentStatus::Failed { reason } => Some(AgentState::Failed {
                reason: reason.clone(),
            }),
        }
    }

    /// Whether it works: a turn runs, or waits on the user mid-turn.
    pub fn is_working(&self) -> bool {
        matches!(self, AgentStatus::Running | AgentStatus::Blocked { .. })
    }
}

/// How a child starts its conversation once it is ready.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opening {
    /// A new conversation, given this task as its first prompt.
    Task(String),
    /// Continue the newest session in the agent's session folder (`open_session`), or start one there.
    Resume,
    /// A new conversation with no first message.
    Fresh,
}

/// A request that waits on the user, as the Inbox shows it (not yet masked).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateNotice {
    /// omp's request id; answering it clears the gate.
    pub request_id: String,
    /// One line.
    pub title: String,
    /// The detail: for a tool approval, the exact tool and arguments.
    pub body: String,
    /// The Inbox form of a tool approval that has an approve and a refuse option; `None` for other gates, which
    /// the Inbox shows as a notice. The agent's name is filled in when it is posted.
    pub inbox: Option<OmpGate>,
}

/// Something about an RPC agent the rest of xctx keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AgentUpdate {
    /// Its status changed.
    Status(AgentStatus),
    /// omp persisted the session it runs, so it can be resumed by id.
    SessionRecorded {
        /// omp's session id.
        id: String,
        /// The session file, which exists.
        file: PathBuf,
    },
    /// A request now waits on the user.
    GateRaised(GateNotice),
    /// Request `0` no longer waits: answered, cancelled, timed out, or its child exited.
    GateCleared(String),
    /// The retry badge: why omp is retrying after a retryable error, or `None` once it stopped.
    RetryBadge(Option<String>),
    /// The child exited; `unexpected` unless xctx stopped it.
    Exited {
        /// Its exit code, `None` when a signal ended it.
        code: Option<i32>,
        /// Whether it exited on its own.
        unexpected: bool,
    },
}

/// An agent run over RPC: its child, when one runs, and the session model its frames build.
#[derive(Debug)]
pub struct OmpAgent {
    launch: RpcLaunch,
    space: SpaceId,
    agent: Option<AgentNodeId>,
    label: String,
    client: Option<OmpClient>,
    frames: Option<Receiver<Incoming>>,
    model: SessionModel,
    status: AgentStatus,
    raised: Vec<String>,
    inbox_items: HashMap<String, ItemId>,
    settled: bool,
    prompt_in_flight: bool,
    retry: Option<String>,
    recorded: Option<(String, PathBuf)>,
    exit: Option<Option<i32>>,
    failure: Option<String>,
    stopping: bool,
    start_failed: bool,
    restarts: u32,
    next_id: u64,
}

impl OmpAgent {
    /// An agent of space `space` that will run `launch`, as graph agent `agent` (`None` for a pane bound to no
    /// agent) named `label` in Inbox items. `recorded` is the omp session id its graph node already has. Nothing
    /// runs until [`OmpAgent::attach`].
    pub fn new(
        launch: RpcLaunch,
        space: SpaceId,
        agent: Option<AgentNodeId>,
        label: impl Into<String>,
        recorded: Option<String>,
    ) -> OmpAgent {
        OmpAgent {
            launch,
            space,
            agent,
            label: label.into(),
            client: None,
            frames: None,
            model: SessionModel::new(),
            status: AgentStatus::Starting,
            raised: Vec::new(),
            inbox_items: HashMap::new(),
            settled: false,
            prompt_in_flight: false,
            retry: None,
            recorded: recorded.map(|id| (id, PathBuf::new())),
            exit: None,
            failure: None,
            stopping: false,
            start_failed: false,
            restarts: 0,
            next_id: 0,
        }
    }

    /// The client spec of its child: the launch's arguments with its session folder.
    pub fn spec(&self) -> LaunchSpec {
        launch_spec(&self.launch, self.launch.args())
    }

    /// How it was launched.
    pub fn launch(&self) -> &RpcLaunch {
        &self.launch
    }

    /// The graph agent it runs as.
    pub fn agent(&self) -> Option<&AgentNodeId> {
        self.agent.as_ref()
    }

    /// What it is doing.
    pub fn status(&self) -> &AgentStatus {
        &self.status
    }

    /// The session model, as a pane draws it.
    pub fn model(&self) -> &SessionModel {
        &self.model
    }

    /// The Roster's retry badge, while omp retries after a retryable error.
    pub fn retry_badge(&self) -> Option<&str> {
        self.retry.as_deref()
    }

    /// The omp session id it runs, once persisted (or the one its graph node had).
    pub fn session_id(&self) -> Option<&str> {
        self.recorded.as_ref().map(|(id, _)| id.as_str())
    }

    /// Whether its child runs.
    pub fn is_live(&self) -> bool {
        self.client.is_some()
    }

    /// How its child exited, once it did: `Some(code)`.
    pub fn exit(&self) -> Option<Option<i32>> {
        self.exit
    }

    /// Whether its child exited on its own and may be started again ([`MAX_RESTARTS`] times until a turn
    /// settles). A child that could not start at all is not retried.
    pub fn may_restart(&self) -> bool {
        self.exit.is_some() && !self.stopping && !self.start_failed && self.restarts < MAX_RESTARTS
    }

    /// How to start its child again after it exited on its own: resume its session, counted as one of the
    /// [`MAX_RESTARTS`]; `None` when it may not (see [`OmpAgent::may_restart`]) and stays failed.
    pub fn restart_opening(&mut self) -> Option<Opening> {
        if !self.may_restart() {
            return None;
        }
        self.restarts += 1;
        Some(Opening::Resume)
    }

    /// Take the channel its child's frames arrive on, to read them elsewhere (the workspace forwards them to
    /// the UI loop) and hand each to [`OmpAgent::apply`].
    pub fn take_frames(&mut self) -> Option<Receiver<Incoming>> {
        self.frames.take()
    }

    /// Start its child and wait for the handshake (up to [`HANDSHAKE_TIMEOUT`] per step), then [`OmpAgent::attach`].
    /// Blocks, so only tests call it; the workspace starts the child off the UI thread.
    #[cfg(test)]
    pub fn start_blocking(&mut self, opening: Opening) -> Vec<AgentUpdate> {
        let started = OmpClient::start(&self.spec());
        self.attach(started, opening)
    }

    /// Take over a started child (or its failure to start): apply the handshake's frames to a fresh model, then
    /// open the conversation as `opening` says.
    pub fn attach(
        &mut self,
        started: xctx_omp::Result<Started>,
        opening: Opening,
    ) -> Vec<AgentUpdate> {
        let mut updates = Vec::new();
        self.model = SessionModel::new();
        self.exit = None;
        self.failure = None;
        self.stopping = false;
        self.settled = false;
        self.prompt_in_flight = false;
        match started {
            Ok(started) => {
                self.start_failed = false;
                self.client = Some(started.client);
                self.frames = Some(started.frames);
                for frame in started.startup {
                    self.on_frame(frame, &mut updates);
                }
                let opened = match opening {
                    Opening::Task(task) => self.send_prompt(&task),
                    Opening::Resume => {
                        let dir = self.launch.session_dir.to_string_lossy().into_owned();
                        let id = self.command_id();
                        self.send(&Command::open_session(&dir).with_id(id))
                    }
                    Opening::Fresh => Ok(()),
                };
                if let Err(e) = opened {
                    self.failure = Some(format!("omp stopped taking commands: {e}"));
                }
            }
            Err(e) => {
                self.start_failed = true;
                self.exit = Some(None);
                self.failure = Some(start_failure(&self.launch, &e));
            }
        }
        self.refresh(&mut updates);
        updates
    }

    /// Wait up to `timeout` for the next thing from its child and apply it; `None` when nothing came (or the
    /// frames were taken, see [`OmpAgent::take_frames`]). Blocks, so only tests call it.
    #[cfg(test)]
    pub fn recv(&mut self, timeout: Duration) -> Option<Vec<AgentUpdate>> {
        let incoming = match self.frames.as_ref()?.recv_timeout(timeout) {
            Ok(incoming) => incoming,
            Err(RecvTimeoutError::Timeout) => return None,
            Err(RecvTimeoutError::Disconnected) => Incoming::Exited {
                code: None,
                stderr: String::new(),
            },
        };
        Some(self.apply(incoming))
    }

    /// Apply one thing from its child.
    pub fn apply(&mut self, incoming: Incoming) -> Vec<AgentUpdate> {
        let mut updates = Vec::new();
        match incoming {
            Incoming::Frame(frame) => self.on_frame(frame, &mut updates),
            Incoming::Malformed(_) => {}
            Incoming::Exited { code, stderr } => {
                self.client = None;
                self.frames = None;
                self.exit = Some(code);
                self.prompt_in_flight = false;
                let unexpected = !self.stopping;
                if unexpected {
                    self.failure = Some(exit_reason(code, &stderr));
                }
                // Its requests died with it.
                self.model.pending_ui.clear();
                updates.push(AgentUpdate::Exited { code, unexpected });
            }
        }
        self.refresh(&mut updates);
        updates
    }

    /// Send `text` the way `agent.send` does: a new prompt when no turn runs, else a steer or a follow-up as
    /// `while_running` says. Fails when its child does not run.
    pub fn send_message(
        &mut self,
        text: &str,
        while_running: WhileRunning,
    ) -> Result<AgentDelivery, String> {
        if self.client.is_none() {
            return Err("its omp is not running".to_owned());
        }
        if !(self.model.is_running() || self.prompt_in_flight) {
            self.send_prompt(text).map_err(|e| e.to_string())?;
            return Ok(AgentDelivery::Prompt);
        }
        let (command, delivery) = match while_running {
            WhileRunning::Steer => (Command::steer(text), AgentDelivery::Steer),
            WhileRunning::FollowUp => (Command::follow_up(text), AgentDelivery::FollowUp),
        };
        let id = self.command_id();
        self.send(&command.with_id(id)).map_err(|e| e.to_string())?;
        Ok(delivery)
    }

    /// Send `command`, which the user gave in the agent's pane, under an id of its own. A `prompt` without a
    /// streaming behaviour starts a turn. Fails when its child does not run.
    pub fn send_command(&mut self, command: Command) -> Result<(), String> {
        if self.client.is_none() {
            return Err("its omp is not running".to_owned());
        }
        let starts_turn =
            command.kind() == "prompt" && command.as_value().get("streamingBehavior").is_none();
        let id = self.command_id();
        self.send(&command.with_id(id)).map_err(|e| e.to_string())?;
        if starts_turn {
            self.prompt_in_flight = true;
        }
        Ok(())
    }

    /// Answer a pending request with `answer` (an `extension_ui_response` the user chose), and return what
    /// changed. Refused for a request that does not wait.
    pub fn respond_ui(&mut self, answer: &Command) -> Result<Vec<AgentUpdate>, String> {
        let id = answer
            .as_value()
            .get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .filter(|_| answer.kind() == "extension_ui_response")
            .ok_or_else(|| "not an answer to a request".to_owned())?;
        if !self.model.pending_ui.iter().any(|r| r.id == id) {
            return Err(format!("request {id} does not wait for an answer"));
        }
        self.send(answer).map_err(|e| e.to_string())?;
        self.model.resolve_ui(&id);
        let mut updates = Vec::new();
        self.refresh(&mut updates);
        Ok(updates)
    }

    /// Stop its child as omp expects: close its input; it exits once it has finished writing (killed after a
    /// grace period). The exit arrives as [`AgentUpdate::Exited`] with `unexpected: false`.
    pub fn close(&mut self) {
        self.stopping = true;
        if let Some(client) = &mut self.client {
            client.close_input();
        }
    }
    /// The last `lines` lines of its transcript as text (see [`transcript_text`]), not yet masked.
    pub fn transcript_tail(&self, lines: usize) -> String {
        crate::control::last_screen_lines(&transcript_text(&self.model), lines)
    }

    /// Carry `updates` into xctx's state: the agent's graph state and recorded session into `graph`, and each
    /// request that waits on the user into `inbox` as a notice (dismissed again once the request clears), all
    /// masked with `redactor`. A Paused agent stays paused unless it failed. Returns what could not be saved.
    pub fn apply_updates(
        &mut self,
        updates: &[AgentUpdate],
        mut graph: Option<&mut SpaceGraphState>,
        mut inbox: Option<&mut Inbox>,
        redactor: &Redactor,
    ) -> Vec<String> {
        let mut problems = Vec::new();
        let masked = |text: &str| redactor.redact(text).into_owned();
        for update in updates {
            match update {
                AgentUpdate::Status(status) => {
                    let (Some(agent), Some(graph), Some(state)) =
                        (&self.agent, graph.as_deref_mut(), status.graph_state())
                    else {
                        continue;
                    };
                    let state = match state {
                        AgentState::Blocked { reason } => AgentState::Blocked {
                            reason: masked(&reason),
                        },
                        AgentState::Failed { reason } => AgentState::Failed {
                            reason: masked(&reason),
                        },
                        other => other,
                    };
                    let current = graph.graph().node(agent).map(|n| n.state.clone());
                    let paused = current == Some(AgentState::Paused)
                        && !matches!(state, AgentState::Failed { .. });
                    if paused || current.as_ref() == Some(&state) {
                        continue;
                    }
                    let space = self.space.clone();
                    if let Err(e) = graph.change(|doc| {
                        doc.graph
                            .set_state(agent, state)
                            .map_err(|e| graph_error(&space, e))
                    }) {
                        problems.push(e.message);
                    }
                }
                AgentUpdate::SessionRecorded { id, .. } => {
                    let (Some(agent), Some(graph)) = (&self.agent, graph.as_deref_mut()) else {
                        continue;
                    };
                    if graph.graph().node(agent).and_then(|n| n.session.as_deref())
                        == Some(id.as_str())
                    {
                        continue;
                    }
                    let space = self.space.clone();
                    if let Err(e) = graph.change(|doc| {
                        doc.graph
                            .set_session(agent, Some(id.clone()))
                            .map_err(|e| graph_error(&space, e))
                    }) {
                        problems.push(e.message);
                    }
                }
                AgentUpdate::GateRaised(gate) => {
                    let Some(inbox) = inbox.as_deref_mut() else {
                        continue;
                    };
                    let body = match &gate.inbox {
                        Some(omp) => ItemBody::OmpGate(OmpGate {
                            agent: self.label.clone(),
                            detail: masked(&omp.detail),
                            ..omp.clone()
                        }),
                        None => ItemBody::Notice(Notice {
                            title: masked(&format!("{} asks: {}", self.label, gate.title)),
                            body: Some(masked(&gate.body)),
                            level: Level::Warning,
                        }),
                    };
                    match inbox.post(body, Some(self.space.clone()), Source::App) {
                        Ok(item) => {
                            self.inbox_items.insert(gate.request_id.clone(), item);
                        }
                        Err(e) => problems.push(format!("could not post to the Inbox: {e}")),
                    }
                }
                AgentUpdate::GateCleared(request) => {
                    let (Some(inbox), Some(item)) =
                        (inbox.as_deref_mut(), self.inbox_items.remove(request))
                    else {
                        continue;
                    };
                    // The user may have dismissed it already.
                    if inbox.get(&item).is_some_and(|i| i.is_pending())
                        && let Err(e) = inbox.dismiss(&item)
                    {
                        problems.push(format!("could not update the Inbox: {e}"));
                    }
                }
                AgentUpdate::RetryBadge(_) | AgentUpdate::Exited { .. } => {}
            }
        }
        problems
    }

    /// Whether omp waits on request `request_id` for an answer.
    pub fn waits_for(&self, request_id: &str) -> bool {
        self.model
            .pending_ui
            .iter()
            .any(|r| r.id == request_id && r.blocks())
    }

    /// The Inbox item of request `request_id`, while it waits.
    #[cfg(test)]
    pub fn inbox_item(&self, request_id: &str) -> Option<&ItemId> {
        self.inbox_items.get(request_id)
    }

    fn command_id(&mut self) -> String {
        self.next_id += 1;
        format!("xctx-{}", self.next_id)
    }

    fn send(&self, command: &Command) -> xctx_omp::Result<()> {
        self.client.as_ref().ok_or(OmpError::Closed)?.send(command)
    }

    fn send_prompt(&mut self, text: &str) -> xctx_omp::Result<()> {
        let id = self.command_id();
        self.send(&Command::prompt(text, &[], None).with_id(id))?;
        self.prompt_in_flight = true;
        Ok(())
    }

    fn on_frame(&mut self, frame: Frame, updates: &mut Vec<AgentUpdate>) {
        let state_answer =
            matches!(&frame, Frame::Response(r) if r.command == "get_state" && r.success);
        match &frame {
            Frame::Event { kind, value } => match kind.as_str() {
                "agent_start" => {
                    self.settled = false;
                    self.prompt_in_flight = false;
                    self.set_retry(None, updates);
                }
                "auto_retry_start" => {
                    let error = value
                        .get("errorMessage")
                        .and_then(Value::as_str)
                        .unwrap_or("a retryable error");
                    let attempt = match (
                        value.get("attempt").and_then(Value::as_u64),
                        value.get("maxAttempts").and_then(Value::as_u64),
                    ) {
                        (Some(n), Some(max)) => format!(" ({n}/{max})"),
                        _ => String::new(),
                    };
                    self.set_retry(Some(format!("retrying{attempt}: {error}")), updates);
                }
                "auto_retry_end" => self.set_retry(None, updates),
                _ => {}
            },
            Frame::PromptResult(result) => {
                self.prompt_in_flight = false;
                let retryable = result
                    .error
                    .as_ref()
                    .and_then(|e| e.get("retryable"))
                    .and_then(Value::as_bool)
                    == Some(true);
                if result.status == "error" && retryable {
                    let message = result
                        .error
                        .as_ref()
                        .and_then(|e| e.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("the turn failed");
                    self.set_retry(Some(format!("retryable error: {message}")), updates);
                }
            }
            Frame::Response(r) if r.command == "prompt" && !r.success => {
                self.prompt_in_flight = false;
            }
            Frame::SessionSettled => {
                self.settled = true;
                self.restarts = 0;
            }
            _ => {}
        }
        for effect in self.model.apply(frame) {
            if let xctx_omp::Effect::Send(command) = effect {
                // A child that stopped reading is reported by its exit.
                let _ = self.send(&command);
            }
        }
        if state_answer {
            self.check_persisted(updates);
        }
    }

    fn set_retry(&mut self, badge: Option<String>, updates: &mut Vec<AgentUpdate>) {
        if self.retry != badge {
            self.retry.clone_from(&badge);
            updates.push(AgentUpdate::RetryBadge(badge));
        }
    }

    /// Record the session omp reports once its file exists: omp writes it with the first persisted turn.
    fn check_persisted(&mut self, updates: &mut Vec<AgentUpdate>) {
        let (Some(id), Some(file)) = (&self.model.session.id, &self.model.session.file) else {
            return;
        };
        let file = PathBuf::from(file);
        if self
            .recorded
            .as_ref()
            .is_some_and(|(r, f)| r == id && *f == file)
            || !file.is_file()
        {
            return;
        }
        self.recorded = Some((id.clone(), file.clone()));
        updates.push(AgentUpdate::SessionRecorded {
            id: id.clone(),
            file,
        });
    }

    /// Bring the gates and status up to date with the model, pushing what changed.
    fn refresh(&mut self, updates: &mut Vec<AgentUpdate>) {
        let waiting: Vec<String> = self
            .model
            .pending_ui
            .iter()
            .filter(|r| r.blocks())
            .map(|r| r.id.clone())
            .collect();
        let gone: Vec<String> = self
            .raised
            .iter()
            .filter(|id| !waiting.contains(id))
            .cloned()
            .collect();
        for id in gone {
            self.raised.retain(|r| *r != id);
            updates.push(AgentUpdate::GateCleared(id));
        }
        for id in waiting {
            if self.raised.contains(&id) {
                continue;
            }
            if let Some(request) = self.model.pending_ui.iter().find(|r| r.id == id) {
                updates.push(AgentUpdate::GateRaised(gate_notice(
                    &self.model,
                    request,
                    &self.launch.cwd,
                )));
            }
            self.raised.push(id);
        }
        let status = self.compute_status();
        if status != self.status {
            self.status = status.clone();
            updates.push(AgentUpdate::Status(status));
        }
    }

    fn compute_status(&self) -> AgentStatus {
        if let Some(reason) = &self.failure {
            return AgentStatus::Failed {
                reason: reason.clone(),
            };
        }
        if self.exit.is_some() {
            return AgentStatus::Stopped;
        }
        if self.client.is_none() {
            return AgentStatus::Starting;
        }
        if let Some(request) = self.model.next_gate() {
            return AgentStatus::Blocked {
                reason: blocked_reason(&self.model, request, &self.launch.cwd),
            };
        }
        if self.model.is_running() {
            AgentStatus::Running
        } else if self.settled {
            AgentStatus::Done
        } else {
            AgentStatus::Idle
        }
    }
}

fn start_failure(launch: &RpcLaunch, error: &OmpError) -> String {
    format!(
        "omp did not start in {}: {error}",
        launch.cwd.to_string_lossy()
    )
}

fn exit_reason(code: Option<i32>, stderr: &str) -> String {
    let how = code.map_or_else(|| "was killed".to_owned(), |c| format!("exited ({c})"));
    match stderr.trim().lines().last() {
        Some(line) if !line.trim().is_empty() => format!("omp {how}: {}", line.trim()),
        _ => format!("omp {how}"),
    }
}

/// The arguments of the tool call waiting for approval `tool`: the running or pending call of that tool.
fn approval_args(model: &SessionModel, tool: &str) -> Option<Value> {
    use xctx_omp::ToolState;
    let mut runs: Vec<_> = model
        .tools
        .values()
        .filter(|run| {
            run.name == tool && matches!(run.state, ToolState::Running | ToolState::Pending)
        })
        .collect();
    runs.sort_by_key(|run| run.state != ToolState::Running);
    runs.first().map(|run| run.args.clone())
}

/// One line on what `request` waits for, for the graph's Blocked reason.
fn blocked_reason(model: &SessionModel, request: &UiRequest, worktree: &Path) -> String {
    let notice = gate_notice(model, request, worktree);
    match request.approval() {
        Some(approval) if !approval.detail.is_empty() => {
            format!("waiting on you: {} ({})", notice.title, approval.detail)
        }
        _ => format!("waiting on you: {}", notice.title),
    }
}

/// How `request` reads in the Inbox: for a tool approval, the exact tool, what it does and its arguments.
fn gate_notice(model: &SessionModel, request: &UiRequest, worktree: &Path) -> GateNotice {
    let mut body: Vec<String> = Vec::new();
    let permission = permission_of(request, worktree);
    let title = match (&request.method, request.approval()) {
        (_, Some(approval)) => {
            body.push(format!("Tool: {}", approval.tool));
            if !approval.detail.is_empty() {
                body.push(approval.detail.clone());
            }
            if let Some(args) = approval_args(model, &approval.tool).filter(|a| !a.is_null()) {
                body.push(format!("Arguments: {args}"));
            }
            if !approval.options.is_empty() {
                body.push(format!("Options: {}", approval.options.join(" / ")));
            }
            format!("Allow {}?", approval.tool)
        }
        (UiMethod::Select { title, options }, None) => {
            body.push(format!("Options: {}", options.join(" / ")));
            title.clone()
        }
        (UiMethod::Confirm { title, message }, None) => {
            body.push(message.clone());
            title.clone()
        }
        (
            UiMethod::Input {
                title, placeholder, ..
            },
            None,
        ) => {
            body.push(placeholder.clone());
            title.clone()
        }
        (UiMethod::Editor { title, .. }, None) => title.clone(),
        (UiMethod::Form { .. }, None) => "a question (ask form)".to_owned(),
        (UiMethod::Ask { .. }, None) => "questions".to_owned(),
        (_, None) => "an answer".to_owned(),
    };
    body.retain(|line| !line.trim().is_empty());
    let inbox = permission.and_then(|p| {
        Some(OmpGate {
            request_id: request.id.clone(),
            agent: String::new(),
            tool: p.tool.clone(),
            detail: body.join("\n"),
            approve: p.approve_once()?.to_owned(),
            deny: p.deny()?.to_owned(),
            danger: !p.one_click_allowed(),
        })
    });
    body.push("omp waits for your answer; xctx never answers for you.".to_owned());
    GateNotice {
        request_id: request.id.clone(),
        title,
        body: body.join("\n"),
        inbox,
    }
}

/// The most characters of a tool call's arguments a transcript line shows.
const TOOL_ARGS_CHARS: usize = 120;

/// A session's transcript as plain text, oldest first: `> ` before what the user said, the model's text as it
/// wrote it (its thinking left out), `• tool: arguments` for each tool call with how it ended, and `! ` before
/// notices. Entries are separated by a blank line.
pub fn transcript_text(model: &SessionModel) -> String {
    let mut out: Vec<String> = Vec::new();
    for entry in &model.entries {
        match entry {
            Entry::User {
                text, from_agent, ..
            } => {
                let who = if *from_agent { "> (agent) " } else { "> " };
                out.push(format!("{who}{text}"));
            }
            Entry::Assistant(message) => {
                let mut parts: Vec<String> = Vec::new();
                for block in &message.blocks {
                    match block {
                        Block::Text(text) if !text.trim().is_empty() => parts.push(text.clone()),
                        Block::ToolCall(id) => {
                            if let Some(run) = model.tools.get(id) {
                                parts.push(tool_line(run));
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(error) = &message.error {
                    parts.push(format!("! {error}"));
                }
                if !parts.is_empty() {
                    out.push(parts.join("\n"));
                }
            }
            Entry::Notice { text, .. } => out.push(format!("! {text}")),
        }
    }
    out.join("\n\n")
}

fn tool_line(run: &xctx_omp::ToolRun) -> String {
    use xctx_omp::ToolState;
    let args = match run.args.as_object() {
        Some(map) => ["command", "path", "pattern", "url", "query"]
            .iter()
            .find_map(|key| map.get(*key).and_then(Value::as_str))
            .map_or_else(|| run.args.to_string(), str::to_owned),
        None if run.args.is_null() => String::new(),
        None => run.args.to_string(),
    };
    let mut shown: String = args.chars().take(TOOL_ARGS_CHARS).collect();
    if args.chars().count() > TOOL_ARGS_CHARS {
        shown.push('…');
    }
    let args = shown;
    let state = match run.state {
        ToolState::Pending => " (waiting)",
        ToolState::Running => " (running)",
        ToolState::Done => "",
        ToolState::Failed => " (failed)",
    };
    format!("• {}: {args}{state}", run.name)
}

/// `agent.read` for an RPC agent: the last `lines` lines of its transcript, masked with `redactor` as a
/// terminal's screen is.
pub fn rpc_agent_read(
    agent: &OmpAgent,
    space: &SpaceId,
    node: &AgentNodeId,
    lines: usize,
    redactor: &Redactor,
) -> AgentReadResult {
    let (process, exit_code) = match agent.exit() {
        Some(code) => (AgentProcess::Exited, code),
        None => (AgentProcess::Running, None),
    };
    AgentReadResult {
        space: space.as_str().to_owned(),
        agent: node.as_str().to_owned(),
        process,
        exit_code,
        text: redactor.redact(&agent.transcript_tail(lines)).into_owned(),
    }
}

/// `agent.send` for an RPC agent: deliver `text` (see [`OmpAgent::send_message`]). A conflict when its child
/// does not run.
pub fn rpc_agent_send(
    agent: &mut OmpAgent,
    node: &AgentNodeId,
    text: &str,
    while_running: Option<WhileRunning>,
) -> Result<AgentSendResult, RpcError> {
    let delivered = agent
        .send_message(text, while_running.unwrap_or_default())
        .map_err(|e| RpcError::conflict(format!("agent.send: {node}: {e}; start it again")))?;
    Ok(AgentSendResult {
        sent: true,
        delivered: Some(delivered),
    })
}

/// Whether a running omp other than the stopped child holds omp session `session_id`: a short-lived child of
/// `launch` that holds no session itself ([`RpcLaunch::probe_args`]) is asked `get_session_lease`. Blocks for up
/// to the handshake and [`LEASE_TIMEOUT`]: call it off the UI thread. Fails when it cannot tell.
pub fn session_lease_held(launch: &RpcLaunch, session_id: &str) -> Result<bool, String> {
    let started = OmpClient::start(&launch_spec(launch, launch.probe_args()))
        .map_err(|e| format!("could not check who holds session {session_id}: {e}"))?;
    let lease = Command::raw("get_session_lease", json!({"sessionIds": [session_id]}))
        .with_id("xctx-lease");
    started
        .client
        .send(&lease)
        .map_err(|e| format!("could not check who holds session {session_id}: {e}"))?;
    let deadline = Instant::now() + LEASE_TIMEOUT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match started.frames.recv_timeout(left) {
            Ok(Incoming::Frame(Frame::Response(r))) if r.id.as_deref() == Some("xctx-lease") => {
                if !r.success {
                    return Err(format!(
                        "omp could not say who holds session {session_id}: {}",
                        r.error.unwrap_or_default()
                    ));
                }
                let held = r.data.get("held").and_then(Value::as_array);
                return Ok(held.is_some_and(|held| {
                    held.iter().any(|h| {
                        h.as_str() == Some(session_id)
                            || ["sessionId", "id"]
                                .iter()
                                .any(|k| h.get(*k).and_then(Value::as_str) == Some(session_id))
                    })
                }));
            }
            Ok(Incoming::Exited { .. }) | Err(_) => {
                return Err(format!(
                    "omp did not say who holds session {session_id} in time"
                ));
            }
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod gate_tests {
    use std::path::Path;

    use serde_json::json;
    use xctx_omp::{Frame, SessionModel};

    use super::gate_notice;

    fn request(value: serde_json::Value) -> xctx_omp::UiRequest {
        match Frame::parse(value) {
            Frame::Ui(request) => request,
            other => panic!("not a UI request: {other:?}"),
        }
    }

    #[test]
    fn a_tool_approval_is_an_inbox_gate_with_its_approve_and_deny_options() {
        let approval = request(json!({
            "type": "extension_ui_request",
            "id": "r1",
            "method": "select",
            "title": "Allow tool: bash\nCommand: rm -rf build",
            "options": ["Allow once", "Allow for session", "Deny"],
        }));
        let gate = gate_notice(&SessionModel::new(), &approval, Path::new("/nonexistent"))
            .inbox
            .expect("a tool approval is offered in the Inbox");
        assert_eq!(gate.request_id, "r1");
        assert_eq!(gate.tool, "bash");
        assert_eq!(gate.approve, "Allow once");
        assert_eq!(gate.deny, "Deny");
        assert!(gate.detail.contains("rm -rf build"));
        assert!(
            gate.danger,
            "a destructive command is never approved with one click"
        );
    }

    #[test]
    fn other_questions_stay_notices_in_the_inbox() {
        let select = request(json!({
            "type": "extension_ui_request",
            "id": "r2",
            "method": "select",
            "title": "Pick a colour",
            "options": ["red", "blue"],
        }));
        assert!(
            gate_notice(&SessionModel::new(), &select, Path::new("/nonexistent"))
                .inbox
                .is_none()
        );
    }
}
