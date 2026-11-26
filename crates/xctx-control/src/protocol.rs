//! The control protocol: JSON-RPC 2.0 requests and responses, one JSON object per line.
//!
//! A request is `{"jsonrpc":"2.0","id":1,"method":"pane.split","params":{...}}`. A request without an `id` is a
//! notification: it runs and gets no response. The reply is `{"jsonrpc":"2.0","id":1,"result":...}` or
//! `{"jsonrpc":"2.0","id":1,"error":{"code":...,"message":...}}`. Batches are not supported.
//!
//! [`Call`] is a parsed request: a [`Method`] with typed parameters. Parameter objects reject unknown fields, so
//! a typo from an agent is an error instead of being ignored.

use std::fmt;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value, json};
use xctx_layout::{PaneId, PaneKind, PaneTree, RolePane, Shape, SplitAxis};

/// The protocol version [`Method::Ping`] reports. It goes up when a change would break existing clients.
pub const PROTOCOL_VERSION: u32 = 1;

/// The `jsonrpc` field every message carries.
pub const JSONRPC_VERSION: &str = "2.0";

/// The longest request line the server reads, in bytes. Longer lines are refused and the connection is closed.
pub const MAX_LINE_BYTES: usize = 1 << 20;

// ---------------------------------------------------------------------------------------------------------------
// Errors

/// The error codes a response can carry: the JSON-RPC 2.0 ones and xctx's own in the server-error range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    /// The line was not JSON.
    ParseError,
    /// The JSON was not a request object.
    InvalidRequest,
    /// No method has this name.
    MethodNotFound,
    /// The parameters do not fit the method.
    InvalidParams,
    /// The server failed for a reason of its own.
    InternalError,
    /// The space, pane or inbox item named does not exist.
    NotFound,
    /// The connection is not allowed to call this method.
    Forbidden,
    /// The request is well formed but cannot be done now (for example, closing a space's last agent).
    Conflict,
    /// The window did not answer in time.
    Timeout,
}

impl ErrorCode {
    /// The number on the wire.
    pub fn code(self) -> i64 {
        match self {
            ErrorCode::ParseError => -32700,
            ErrorCode::InvalidRequest => -32600,
            ErrorCode::MethodNotFound => -32601,
            ErrorCode::InvalidParams => -32602,
            ErrorCode::InternalError => -32603,
            ErrorCode::NotFound => -32001,
            ErrorCode::Forbidden => -32002,
            ErrorCode::Conflict => -32003,
            ErrorCode::Timeout => -32004,
        }
    }

    /// The code for a number on the wire, or `None` for one this version does not know.
    pub fn from_code(code: i64) -> Option<ErrorCode> {
        [
            ErrorCode::ParseError,
            ErrorCode::InvalidRequest,
            ErrorCode::MethodNotFound,
            ErrorCode::InvalidParams,
            ErrorCode::InternalError,
            ErrorCode::NotFound,
            ErrorCode::Forbidden,
            ErrorCode::Conflict,
            ErrorCode::Timeout,
        ]
        .into_iter()
        .find(|c| c.code() == code)
    }
}

/// The `error` of a failed response.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    /// See [`ErrorCode`]; unknown codes are kept as they are.
    pub code: i64,
    /// A sentence a person or agent can act on.
    pub message: String,
    /// Extra structured detail, when there is any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    /// An error with `code` and `message` and no data.
    pub fn new(code: ErrorCode, message: impl Into<String>) -> RpcError {
        RpcError {
            code: code.code(),
            message: message.into(),
            data: None,
        }
    }

    /// This error with `data` attached.
    pub fn with_data(mut self, data: Value) -> RpcError {
        self.data = Some(data);
        self
    }

    /// The code as an [`ErrorCode`], or `None` when this version does not know it.
    pub fn kind(&self) -> Option<ErrorCode> {
        ErrorCode::from_code(self.code)
    }

    /// [`ErrorCode::NotFound`].
    pub fn not_found(message: impl Into<String>) -> RpcError {
        RpcError::new(ErrorCode::NotFound, message)
    }

    /// [`ErrorCode::InvalidParams`].
    pub fn invalid_params(message: impl Into<String>) -> RpcError {
        RpcError::new(ErrorCode::InvalidParams, message)
    }

    /// [`ErrorCode::Conflict`].
    pub fn conflict(message: impl Into<String>) -> RpcError {
        RpcError::new(ErrorCode::Conflict, message)
    }

    /// [`ErrorCode::InternalError`].
    pub fn internal(message: impl Into<String>) -> RpcError {
        RpcError::new(ErrorCode::InternalError, message)
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} (code {})", self.message, self.code)
    }
}

impl std::error::Error for RpcError {}

// ---------------------------------------------------------------------------------------------------------------
// Envelopes

/// A request as it is on the wire, before its method and parameters are checked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestEnvelope {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// The id the response echoes. `None` (the field is absent) makes the request a notification; an explicit
    /// `null` is kept as `Some(Value::Null)`.
    #[serde(
        default,
        deserialize_with = "present",
        skip_serializing_if = "Option::is_none"
    )]
    pub id: Option<Value>,
    /// The method name, for example `pane.split`.
    pub method: String,
    /// The parameters: an object, or absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

/// Deserialize a field that is present, even as `null`, to `Some`.
fn present<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

impl RequestEnvelope {
    /// A request for `call` with `id`.
    pub fn new(id: Value, call: &Call) -> RequestEnvelope {
        RequestEnvelope {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id: Some(id),
            method: call.method().name().to_owned(),
            params: Some(call.params()),
        }
    }

    /// Parse one request line. On failure returns the error response to send back, addressed to the request's
    /// id when it could be read and is a valid id, else to `null`.
    pub fn parse_line(line: &str) -> Result<RequestEnvelope, Box<Response>> {
        let reject = |id: Value, code: ErrorCode, message: String| {
            Box::new(Response::new(id, Err(RpcError::new(code, message))))
        };
        let value: Value = serde_json::from_str(line)
            .map_err(|e| reject(Value::Null, ErrorCode::ParseError, format!("not JSON: {e}")))?;
        if value.is_array() {
            return Err(reject(
                Value::Null,
                ErrorCode::InvalidRequest,
                "batches are not supported".into(),
            ));
        }
        let id = value
            .get("id")
            .filter(|id| id.is_string() || id.is_number())
            .cloned()
            .unwrap_or(Value::Null);
        if let Some(raw) = value.get("id")
            && !(raw.is_null() || raw.is_string() || raw.is_number())
        {
            return Err(reject(
                Value::Null,
                ErrorCode::InvalidRequest,
                "id must be a string, number or null".into(),
            ));
        }
        let envelope: RequestEnvelope = serde_json::from_value(value).map_err(|e| {
            reject(
                id.clone(),
                ErrorCode::InvalidRequest,
                format!("not a request: {e}"),
            )
        })?;
        if envelope.jsonrpc != JSONRPC_VERSION {
            return Err(reject(
                id,
                ErrorCode::InvalidRequest,
                format!("jsonrpc must be \"{JSONRPC_VERSION}\""),
            ));
        }
        Ok(envelope)
    }

    /// The typed call this request makes.
    pub fn call(&self) -> Result<Call, RpcError> {
        Call::parse(&self.method, self.params.clone())
    }
}

/// A response as it is on the wire.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Response {
    /// Always `"2.0"`.
    pub jsonrpc: String,
    /// The request's id, or `null` when it could not be read.
    pub id: Value,
    /// The result or the error.
    #[serde(flatten)]
    pub outcome: Outcome,
}

/// Whether a request succeeded.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// It did; the method's result.
    Result(Value),
    /// It did not.
    Error(RpcError),
}

impl Response {
    /// A response to `id` with `outcome`.
    pub fn new(id: Value, outcome: Result<Value, RpcError>) -> Response {
        Response {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id,
            outcome: match outcome {
                Ok(v) => Outcome::Result(v),
                Err(e) => Outcome::Error(e),
            },
        }
    }

    /// The response as one line of JSON, without the newline.
    pub fn to_line(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| {
            // Serializing a Value cannot fail in practice; answer with an error rather than nothing.
            json!({
                "jsonrpc": JSONRPC_VERSION,
                "id": self.id,
                "error": { "code": ErrorCode::InternalError.code(), "message": e.to_string() },
            })
            .to_string()
        })
    }

    /// The result, or the error as an `Err`.
    pub fn into_result(self) -> Result<Value, RpcError> {
        match self.outcome {
            Outcome::Result(v) => Ok(v),
            Outcome::Error(e) => Err(e),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Capabilities and methods

/// What a method may do. A server grants a set of these (see [`crate::ServerConfig`]) and refuses calls outside
/// it with [`ErrorCode::Forbidden`]. Approving Inbox items is not a capability: no method does it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Look at spaces, layouts, the Inbox, agent graphs, channels, contracts, loops, attempts, plugins, and what
    /// a space's agents show in their terminals (`agent.list`, `agent.read`).
    Read,
    /// Rearrange a space: split, close, focus, zoom and reshape panes; pause, add and remove agent lanes; advance a
    /// loop; mount and unmount plugins; report plugin events (`plugin.emit`, whose actions can pass or fail the loop).
    Layout,
    /// Propose work and tell the user and agents things: draft or propose widening a space, post to the Inbox, the
    /// channel or the contract, set the status line, type a message to an agent (`agent.send`), and call plugin
    /// tools (writes become Inbox gates).
    Propose,
    /// Steer on the user's behalf: resume a paused agent and set how far a loop runs without asking. These undo a
    /// user's pause or loosen a user's gate, so [`crate::ServerConfig::default`] (what pane agents get) leaves it
    /// out.
    Steer,
}

impl Capability {
    /// Every capability.
    pub const ALL: [Capability; 4] = [
        Capability::Read,
        Capability::Layout,
        Capability::Propose,
        Capability::Steer,
    ];
}

/// A method of the protocol, without its parameters.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Method {
    /// `ping`: the protocol version, app version and pid. Answered by the server itself.
    Ping,
    /// `space.list`: the active spaces.
    SpaceList,
    /// `space.draft`: propose a space for a task; posts a Plan Gate to the Inbox and never launches.
    SpaceDraft,
    /// `space.reshape`: lay a space out as dev, debug or review, keeping its agents.
    SpaceReshape,
    /// `layout.get`: a space's pane tree and what each pane is for.
    LayoutGet,
    /// `pane.split`: split a pane and start a new one beside it.
    PaneSplit,
    /// `pane.close`: close a pane.
    PaneClose,
    /// `pane.focus`: focus a pane.
    PaneFocus,
    /// `pane.zoom`: zoom a pane to fill the space, or unzoom.
    PaneZoom,
    /// `inbox.list`: Inbox items.
    InboxList,
    /// `inbox.post`: post a notice to the Inbox.
    InboxPost,
    /// `status.set`: show a message in the status line.
    StatusSet,
    /// `graph.get`: a space's agent graph.
    GraphGet,
    /// `graph.pause`: pause an agent.
    GraphPause,
    /// `graph.resume`: resume a paused agent.
    GraphResume,
    /// `graph.add_lane`: add a writer or helper lane to a space's agent graph.
    GraphAddLane,
    /// `graph.remove_lane`: remove an agent's lane.
    GraphRemoveLane,
    /// `channel.post`: post a message to a space's channel.
    ChannelPost,
    /// `channel.list`: a space's channel messages.
    ChannelList,
    /// `contract.get`: a space's shared contract.
    ContractGet,
    /// `contract.set`: propose a new version of a space's shared contract.
    ContractSet,
    /// `contract.agree`: record an agent's agreement to the current contract.
    ContractAgree,
    /// `loop.status`: a space's loop stage and autonomy.
    LoopStatus,
    /// `loop.advance`: move a space's loop to its next stage.
    LoopAdvance,
    /// `loop.set_autonomy`: set how far a space's loop runs without asking.
    LoopSetAutonomy,
    /// `space.widen_propose`: propose adding repos to a space or switching it to production; posts a space
    /// widening to the Inbox and never widens.
    SpaceWidenPropose,
    /// `attempt.list`: a space's attempts.
    AttemptList,
    /// `attempt.report`: an attempt's agent reports its outcome.
    AttemptReport,
    /// `attempt.propose`: propose forking attempts of a writer; posts a gate to the Inbox and never forks.
    AttemptPropose,
    /// `attempt.diff`: the files an attempt changed since its fork, or one file's diff.
    AttemptDiff,
    /// `plugin.list`: the plugins this xctx knows, where they come from and where they are mounted.
    PluginList,
    /// `plugin.tools`: a plugin's tools with their parameters and effects, so an agent knows how to call them.
    PluginTools,
    /// `plugin.mount`: open a tool's pane in a space, beside the agents; an agent asking for a tool that is open
    /// gets its pane. A connector with write tools or an untrusted repo plugin posts a gate to the Inbox instead.
    PluginMount,
    /// `plugin.unmount`: close a tool's panes (or one pane) in a space, as ⌘W does; the tool stays available.
    PluginUnmount,
    /// `plugin.call`: call a tool. Its pane opens first when it is closed and the space is open. Read tools run;
    /// write tools post a gate to the Inbox.
    PluginCall,
    /// `plugin.emit`: report an event from a plugin's own tool, which the space routes to its loop. Needs
    /// [`Capability::Layout`]: a manifest action can pass or fail the loop's stage, moving the loop as
    /// `loop.advance` does.
    PluginEmit,
    /// `agent.list`: every open space's agents with their graph state and whether their terminal runs.
    AgentList,
    /// `agent.read`: the last lines an agent's terminal shows, secrets masked.
    AgentRead,
    /// `agent.send`: type a message into an agent's terminal and submit it, or send it to an RPC agent as a
    /// prompt (a steer or follow-up while it works).
    AgentSend,
    /// `agent.start`: start an agent's pane when its terminal does not run it.
    AgentStart,
    /// `space.open`: open an active space's tab, starting its panes, and show it.
    SpaceOpen,
}

impl Method {
    /// Every method, in the order `xctx-ctl help` lists them.
    pub const ALL: [Method; 41] = [
        Method::Ping,
        Method::SpaceList,
        Method::SpaceDraft,
        Method::SpaceReshape,
        Method::LayoutGet,
        Method::PaneSplit,
        Method::PaneClose,
        Method::PaneFocus,
        Method::PaneZoom,
        Method::InboxList,
        Method::InboxPost,
        Method::StatusSet,
        Method::GraphGet,
        Method::GraphPause,
        Method::GraphResume,
        Method::GraphAddLane,
        Method::GraphRemoveLane,
        Method::ChannelPost,
        Method::ChannelList,
        Method::ContractGet,
        Method::ContractSet,
        Method::ContractAgree,
        Method::LoopStatus,
        Method::LoopAdvance,
        Method::LoopSetAutonomy,
        Method::SpaceWidenPropose,
        Method::AttemptList,
        Method::AttemptReport,
        Method::AttemptPropose,
        Method::AttemptDiff,
        Method::PluginList,
        Method::PluginTools,
        Method::PluginMount,
        Method::PluginUnmount,
        Method::PluginCall,
        Method::PluginEmit,
        Method::AgentList,
        Method::AgentRead,
        Method::AgentSend,
        Method::AgentStart,
        Method::SpaceOpen,
    ];

    /// The name on the wire.
    pub fn name(self) -> &'static str {
        match self {
            Method::Ping => "ping",
            Method::SpaceList => "space.list",
            Method::SpaceDraft => "space.draft",
            Method::SpaceReshape => "space.reshape",
            Method::LayoutGet => "layout.get",
            Method::PaneSplit => "pane.split",
            Method::PaneClose => "pane.close",
            Method::PaneFocus => "pane.focus",
            Method::PaneZoom => "pane.zoom",
            Method::InboxList => "inbox.list",
            Method::InboxPost => "inbox.post",
            Method::StatusSet => "status.set",
            Method::GraphGet => "graph.get",
            Method::GraphPause => "graph.pause",
            Method::GraphResume => "graph.resume",
            Method::GraphAddLane => "graph.add_lane",
            Method::GraphRemoveLane => "graph.remove_lane",
            Method::ChannelPost => "channel.post",
            Method::ChannelList => "channel.list",
            Method::ContractGet => "contract.get",
            Method::ContractSet => "contract.set",
            Method::ContractAgree => "contract.agree",
            Method::LoopStatus => "loop.status",
            Method::LoopAdvance => "loop.advance",
            Method::LoopSetAutonomy => "loop.set_autonomy",
            Method::SpaceWidenPropose => "space.widen_propose",
            Method::AttemptList => "attempt.list",
            Method::AttemptReport => "attempt.report",
            Method::AttemptPropose => "attempt.propose",
            Method::AttemptDiff => "attempt.diff",
            Method::PluginList => "plugin.list",
            Method::PluginTools => "plugin.tools",
            Method::PluginMount => "plugin.mount",
            Method::PluginUnmount => "plugin.unmount",
            Method::PluginCall => "plugin.call",
            Method::PluginEmit => "plugin.emit",
            Method::AgentList => "agent.list",
            Method::AgentRead => "agent.read",
            Method::AgentSend => "agent.send",
            Method::AgentStart => "agent.start",
            Method::SpaceOpen => "space.open",
        }
    }

    /// The method with this wire name.
    pub fn from_name(name: &str) -> Option<Method> {
        Method::ALL.into_iter().find(|m| m.name() == name)
    }

    /// What a connection must be granted to call it.
    pub fn capability(self) -> Capability {
        match self {
            Method::Ping
            | Method::SpaceList
            | Method::LayoutGet
            | Method::InboxList
            | Method::GraphGet
            | Method::ChannelList
            | Method::ContractGet
            | Method::LoopStatus
            | Method::AgentList
            | Method::AgentRead => Capability::Read,
            Method::SpaceReshape
            | Method::PaneSplit
            | Method::PaneClose
            | Method::PaneFocus
            | Method::PaneZoom
            | Method::GraphPause
            | Method::GraphAddLane
            | Method::GraphRemoveLane
            | Method::LoopAdvance
            | Method::PluginMount
            | Method::PluginUnmount
            | Method::PluginEmit
            | Method::AgentStart
            | Method::SpaceOpen => Capability::Layout,
            Method::GraphResume | Method::LoopSetAutonomy => Capability::Steer,
            Method::SpaceDraft
            | Method::InboxPost
            | Method::StatusSet
            | Method::ChannelPost
            | Method::ContractSet
            | Method::ContractAgree
            | Method::SpaceWidenPropose
            | Method::AttemptReport
            | Method::AttemptPropose
            | Method::PluginCall
            | Method::AgentSend => Capability::Propose,
            Method::AttemptList
            | Method::AttemptDiff
            | Method::PluginList
            | Method::PluginTools => Capability::Read,
        }
    }

    /// One line on what it does.
    pub fn summary(self) -> &'static str {
        match self {
            Method::Ping => "protocol version, app version and pid",
            Method::SpaceList => "the active spaces with their ports, shapes and repos",
            Method::SpaceDraft => {
                "propose a space for a task; posts a Plan Gate to the Inbox, never launches"
            }
            Method::SpaceReshape => "lay a space out as dev, debug or review, keeping its agents",
            Method::LayoutGet => "a space's pane tree and what each pane is for",
            Method::PaneSplit => "split a pane (the focused one by default) and start a new pane",
            Method::PaneClose => "close a pane",
            Method::PaneFocus => "focus a pane",
            Method::PaneZoom => "zoom a pane to fill the space; without a pane, toggle zoom",
            Method::InboxList => "Inbox items, pending ones only unless all=true",
            Method::InboxPost => "post a notice to the Inbox",
            Method::StatusSet => "show a message in the status line",
            Method::GraphGet => {
                "a space's agent graph: orchestrator, writers, helpers and their states"
            }
            Method::GraphPause => "pause an agent",
            Method::GraphResume => "resume a paused agent",
            Method::GraphAddLane => {
                "add a writer (at most one per repo) or helper lane to a space's agent graph"
            }
            Method::GraphRemoveLane => "remove an agent's lane from a space's agent graph",
            Method::ChannelPost => "post a message to a space's channel, to everyone or one agent",
            Method::ChannelList => "a space's channel messages, oldest first, secrets redacted",
            Method::ContractGet => "a space's shared contract and who has agreed to it",
            Method::ContractSet => "propose a new version of a space's shared contract",
            Method::ContractAgree => "record an agent's agreement to the current contract",
            Method::LoopStatus => "a space's loop stage, autonomy and budget",
            Method::LoopAdvance => "move a space's loop to its next stage",
            Method::LoopSetAutonomy => "set how far a space's loop runs without asking",
            Method::SpaceWidenPropose => {
                "propose adding repos or production to a space; posts to the Inbox, never widens"
            }
            Method::AttemptList => "a space's competing attempts with their state and outcome",
            Method::AttemptReport => "report this attempt's outcome: summary, tests, repro",
            Method::AttemptPropose => {
                "propose forking attempts of a writer; posts to the Inbox, never forks"
            }
            Method::AttemptDiff => {
                "the files an attempt changed since its fork, or one file's diff"
            }
            Method::PluginList => {
                "the plugins available (to a space), their tools and where they are mounted"
            }
            Method::PluginTools => {
                "a plugin's tools with their parameters and whether each reads or writes"
            }
            Method::PluginMount => {
                "open a tool's pane beside the agents (an open one is returned); connectors with writes and untrusted repo plugins ask in the Inbox"
            }
            Method::PluginUnmount => {
                "close a tool's panes (or one pane) in a space; the tool stays available"
            }
            Method::PluginCall => {
                "call a tool (its pane opens if closed); read tools run, write tools post a gate to the Inbox"
            }
            Method::PluginEmit => {
                "report a plugin event (from the plugin's own tool) to the space's loop"
            }
            Method::AgentList => {
                "every open space's agents (or one space's): role, folder, graph state, pane and whether it runs"
            }
            Method::AgentRead => {
                "the last lines an agent's terminal (or an RPC agent's transcript) shows (default 40, at most \
                 200), secrets masked"
            }
            Method::AgentSend => {
                "send an agent a message: typed into its terminal, or for an RPC agent a prompt (steering or \
                 queued while it works)"
            }
            Method::AgentStart => {
                "start an agent that is not running in a pane of its own, with its role brief"
            }
            Method::SpaceOpen => "open an active space's tab (starting its panes) and show it",
        }
    }

    /// Its parameters, as `xctx-ctl` takes them. `[x]` is optional.
    pub fn usage(self) -> &'static str {
        match self {
            Method::Ping | Method::SpaceList => "",
            Method::SpaceDraft => "task=<text> [repos=<name>,<name>...] [rationale=<text>]",
            Method::SpaceReshape => "space=<slug> shape=dev|debug|review",
            Method::LayoutGet => "space=<slug>",
            Method::PaneSplit => {
                "space=<slug> axis=horizontal|vertical [pane=<id>] [kind=agent|shell|git] [repo=<name>]"
            }
            Method::PaneClose | Method::PaneFocus => "space=<slug> pane=<id>",
            Method::PaneZoom => "space=<slug> [pane=<id>]",
            Method::InboxList => "[space=<slug>] [all=true]",
            Method::InboxPost => "title=<text> [body=<text>] [space=<slug>]",
            Method::StatusSet => "message=<text> [level=info|success|warning|error] [space=<slug>]",
            Method::GraphGet | Method::ContractGet | Method::LoopStatus => "space=<slug>",
            Method::GraphPause | Method::GraphResume | Method::GraphRemoveLane => {
                "space=<slug> agent=<id>"
            }
            Method::ContractAgree => "space=<slug> agent=<id> [version=<n>]",
            Method::GraphAddLane => {
                "space=<slug> role=writer|helper [repo=<name>] [kind=tests|reviewer|probe] [label=<text>]"
            }
            Method::ChannelPost => "space=<slug> from=<agent> kind=<kind> body=<text> [to=<agent>]",
            Method::ChannelList => {
                "space=<slug> [to=<agent>] [kind=<kind>] [since_id=<n>] [limit=<n>]"
            }
            Method::ContractSet => {
                "space=<slug> author=<agent> title=<text> summary=<text> spec=<text>"
            }
            Method::LoopAdvance => "space=<slug> [checks_passed=true|false]",
            Method::LoopSetAutonomy => {
                "space=<slug> autonomy=ask_each_step|within_budget|until_checks_pass [max_iterations=<n>] [max_turns=<n>]"
            }
            Method::SpaceWidenPropose => {
                "space=<slug> [repos=<name>,<name>...] [production=true] [rationale=<text>]"
            }
            Method::AttemptList => "space=<slug> [round=<n>]",
            Method::AttemptReport => {
                "space=<slug> attempt=<n> summary=<text> [tests_passed=<n>] [tests_failed=<n>] \
                 [test_command=<cmd>] [repro=fixed|still_fails|not_checked] [gave_up=true] [token=<t>]"
            }
            Method::AttemptPropose => {
                "space=<slug> agent=<writer id> count=<1-4> [instructions:='[\"...\",\"...\"]'] [rationale=<text>]"
            }
            Method::AttemptDiff => "space=<slug> attempt=<n> [path=<file>]",
            Method::PluginList => "[space=<slug>]",
            Method::PluginTools => "plugin=<name> [space=<slug>]",
            Method::PluginMount => "space=<slug> plugin=<name> [repo=<name>]",
            Method::PluginUnmount => "space=<slug> plugin=<name> | pane=<id>",
            Method::PluginCall => {
                "space=<slug> plugin=<name> tool=<name> [args:='{\"key\":\"value\"}']"
            }
            Method::PluginEmit => "space=<slug> plugin=<name> event=<name> [detail=<text>]",
            Method::AgentList => "[space=<slug>]",
            Method::AgentRead => "space=<slug> agent=<id> [lines=<1-200>]",
            Method::AgentSend => {
                "space=<slug> agent=<id> text=<message> [while_running=steer|follow_up]"
            }
            Method::AgentStart => "space=<slug> agent=<id>",
            Method::SpaceOpen => "space=<slug>",
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Parameters

/// Parameters of a method that takes none. Accepts an absent, `null` or empty object.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

/// `space.draft`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceDraftParams {
    /// The task in the user's words.
    pub task: String,
    /// Repos to include, by indexed name or path. When absent the window picks them from its repo index.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repos: Option<Vec<String>>,
    /// Why the space should look like this, shown with the Plan Gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// `space.reshape`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceReshapeParams {
    /// The space's slug.
    pub space: String,
    /// The shape to lay it out in.
    pub shape: Shape,
}

/// Parameters that name only a space: `layout.get`, `graph.get`, `contract.get`, `loop.status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceParams {
    /// The space's slug.
    pub space: String,
}

/// `pane.split`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneSplitParams {
    /// The space's slug.
    pub space: String,
    /// The pane to split; the focused pane when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PaneId>,
    /// `horizontal` puts the new pane to the right, `vertical` below.
    pub axis: SplitAxis,
    /// What the new pane runs; a shell when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<PaneKind>,
    /// The repo the new pane works in; the split pane's repo when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// Parameters that name one pane: `pane.close`, `pane.focus`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneParams {
    /// The space's slug.
    pub space: String,
    /// The pane.
    pub pane: PaneId,
}

/// `pane.zoom`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PaneZoomParams {
    /// The space's slug.
    pub space: String,
    /// The pane to zoom. When absent, zoom toggles on the focused pane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PaneId>,
}

/// `inbox.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxListParams {
    /// Only items about this space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
    /// Include approved, rejected and dismissed items too.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub all: bool,
}

/// `inbox.post`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxPostParams {
    /// One line the Inbox shows.
    pub title: String,
    /// More detail, shown when the item is opened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The space it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
}

/// How a status message is shown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusLevel {
    /// Neutral.
    #[default]
    Info,
    /// Something finished well.
    Success,
    /// Something needs a look.
    Warning,
    /// Something failed.
    Error,
}

/// `status.set`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatusSetParams {
    /// The message.
    pub message: String,
    /// How it is shown.
    #[serde(default)]
    pub level: StatusLevel,
    /// The space whose status line shows it; Home's when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
}

/// Parameters that name one agent of a space: `graph.pause`, `graph.resume`, `graph.remove_lane`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentParams {
    /// The space's slug.
    pub space: String,
    /// The agent's id in the space's graph.
    pub agent: String,
}

/// `contract.agree`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractAgreeParams {
    /// The space's slug.
    pub space: String,
    /// The agreeing agent's id in the space's graph.
    pub agent: String,
    /// The contract version the agent read and agrees to (as `contract.get` reports it). Optional; clients that do
    /// not know the field leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u32>,
}

/// The kind of lane `graph.add_lane` adds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaneRole {
    /// An agent that edits one repo. A repo has at most one writer.
    Writer,
    /// A read-only agent that tests, reviews or probes.
    Helper,
}

/// What a helper lane does.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HelperKind {
    /// Runs the tests.
    Tests,
    /// Reviews the diff.
    Reviewer,
    /// Exercises the running app.
    Probe,
    /// Drives the space's in-app browser (the `browser` plugin): opens the app, clicks, screenshots, reads the
    /// console.
    Browser,
}

/// `graph.add_lane`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GraphAddLaneParams {
    /// The space's slug.
    pub space: String,
    /// A writer or a helper.
    pub role: LaneRole,
    /// The repo the lane works in, by its name in the space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// What a helper lane does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<HelperKind>,
    /// A name for the lane shown in the graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `channel.post`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelPostParams {
    /// The space's slug.
    pub space: String,
    /// The posting agent's id.
    pub from: String,
    /// The agent it is for; everyone in the space when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// The message's kind, as the space's channel names it.
    pub kind: String,
    /// The message.
    pub body: String,
}

/// `channel.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelListParams {
    /// The space's slug.
    pub space: String,
    /// Only messages for this agent (and those for everyone).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
    /// Only messages of this kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Only messages with a larger id than this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_id: Option<u64>,
    /// At most this many messages, the newest ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

/// `contract.set`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContractSetParams {
    /// The space's slug.
    pub space: String,
    /// The proposing agent's id.
    pub author: String,
    /// A short name for the contract.
    pub title: String,
    /// One or two sentences on what it fixes.
    pub summary: String,
    /// The contract itself: an API shape, event payload or schema.
    pub spec: String,
}

/// `loop.advance`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopAdvanceParams {
    /// The space's slug.
    pub space: String,
    /// Whether the stage being left passed its checks; unknown when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks_passed: Option<bool>,
}

/// How far a space's loop runs without asking the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopAutonomy {
    /// Pause at every stage change and post an Inbox gate.
    AskEachStep,
    /// Run up to the iteration and turn budget, then pause.
    WithinBudget,
    /// Run until the tests and review pass or a failure blocks.
    UntilChecksPass,
}

/// `loop.set_autonomy`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoopSetAutonomyParams {
    /// The space's slug.
    pub space: String,
    /// The autonomy level.
    pub autonomy: LoopAutonomy,
    /// The most write-test-probe-review iterations to run before pausing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_iterations: Option<u32>,
    /// The most agent turns to run before pausing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
}

/// `attempt.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptListParams {
    /// The space's slug.
    pub space: String,
    /// Only this round (one fork); every round when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
}

/// Whether the problem an attempt works on still reproduces, as `attempt.report` says it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReproStatus {
    /// Not checked.
    #[default]
    NotChecked,
    /// The repro passes now.
    Fixed,
    /// It still fails.
    StillFails,
}

/// `attempt.report`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptReportParams {
    /// The space's slug.
    pub space: String,
    /// The attempt's number; `xctx-ctl` takes it from `$XCTX_ATTEMPT_ID`.
    pub attempt: u32,
    /// What the attempt did, in the agent's words.
    pub summary: String,
    /// Tests that passed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<u32>,
    /// Tests that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_failed: Option<u32>,
    /// The test command run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_command: Option<String>,
    /// Whether the problem still reproduces.
    #[serde(default)]
    pub repro: ReproStatus,
    /// The agent could not finish; the attempt is marked failed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub gave_up: bool,
    /// The attempt's report token, which only its own panes know; `xctx-ctl` takes it from
    /// `$XCTX_ATTEMPT_TOKEN`. A report without the right token is refused with [`ErrorCode::Forbidden`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// `attempt.propose`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptProposeParams {
    /// The space's slug.
    pub space: String,
    /// The graph node of the writer to fork.
    pub agent: String,
    /// How many attempts, 1 to 4.
    pub count: u32,
    /// What each attempt should try differently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<Vec<String>>,
    /// Why, shown with the gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// `attempt.diff`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AttemptDiffParams {
    /// The space's slug.
    pub space: String,
    /// The attempt's number.
    pub attempt: u32,
    /// One file to show the diff of; the changed-file list when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// `space.widen_propose`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpaceWidenProposeParams {
    /// The space's slug.
    pub space: String,
    /// Repos to add, by indexed name or path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repos: Option<Vec<String>>,
    /// Switch the space's terminals to `MISE_ENV=production`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub production: bool,
    /// Why, shown with the Inbox item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// `plugin.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginListParams {
    /// Only the plugins this space can mount (built-in, user, and its own repos' plugins); every plugin this
    /// xctx knows when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
}

/// `plugin.tools`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginToolsParams {
    /// The plugin's manifest name.
    pub plugin: String,
    /// The space asking, so a repo plugin of that space's repos can be found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
}

/// `plugin.mount`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginMountParams {
    /// The space's slug.
    pub space: String,
    /// The plugin's manifest name.
    pub plugin: String,
    /// The space repo the plugin's pane and tools work in; the space's first repo when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
}

/// `plugin.unmount`. Name exactly one of `plugin` (every pane of it in the space) or `pane`; [`Call::parse`]
/// refuses both or neither.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginUnmountParams {
    /// The space's slug.
    pub space: String,
    /// The plugin to unmount, by its manifest name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    /// One plugin pane to close.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PaneId>,
}

/// `plugin.call`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCallParams {
    /// The space's slug.
    pub space: String,
    /// The mounted plugin's manifest name.
    pub plugin: String,
    /// The tool's name, as `plugin.tools` lists it.
    pub tool: String,
    /// The tool's parameters by name. `xctx-ctl` takes them as `args:='{"key":"value"}'` (or `args={...}`).
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub args: Map<String, Value>,
}

/// `plugin.emit`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginEmitParams {
    /// The space's slug.
    pub space: String,
    /// The plugin whose manifest declares the event.
    pub plugin: String,
    /// The event's name, as the manifest declares it.
    pub event: String,
    /// What happened: a line of text or the JSON the tool printed. Masked before it is stored or shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
}

/// `agent.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentListParams {
    /// Only this space, which must be active; every active space when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
}

/// How many lines `agent.read` returns when `lines` is absent.
pub const AGENT_READ_DEFAULT_LINES: u32 = 40;

/// The most lines `agent.read` returns; [`Call::parse`] refuses a larger `lines`.
pub const AGENT_READ_MAX_LINES: u32 = 200;

/// `agent.read`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentReadParams {
    /// The space's slug.
    pub space: String,
    /// The agent's id in the space's graph.
    pub agent: String,
    /// How many of the screen's last lines to return, 1 to [`AGENT_READ_MAX_LINES`];
    /// [`AGENT_READ_DEFAULT_LINES`] when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
}

impl AgentReadParams {
    /// The number of lines asked for, with the default filled in.
    pub fn line_count(&self) -> u32 {
        self.lines.unwrap_or(AGENT_READ_DEFAULT_LINES)
    }
}

/// `agent.send`. [`Call::parse`] refuses an empty or whitespace-only `text`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSendParams {
    /// The space's slug.
    pub space: String,
    /// The agent's id in the space's graph.
    pub agent: String,
    /// The message. Control characters (newlines, escapes) are typed as spaces, so it is one line that cannot
    /// press keys of its own; Enter is pressed after it. An agent xctx runs over RPC gets it unchanged, as a
    /// message rather than keys.
    pub text: String,
    /// For an agent xctx runs over RPC that is working: whether the message interrupts at the next step
    /// (`steer`, the default) or waits until the turn ends (`follow_up`). Ignored for a terminal agent, which gets
    /// the message typed in either way, and for an idle one, which gets it as a new prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub while_running: Option<WhileRunning>,
}

/// How `agent.send` delivers a message to an RPC agent that is working.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WhileRunning {
    /// Interrupt the running turn at its next step.
    #[default]
    Steer,
    /// Queue it for when the turn ends.
    FollowUp,
}

/// How `agent.send` delivered its message.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDelivery {
    /// Typed into the agent's terminal, then Enter.
    Typed,
    /// Sent to an idle RPC agent as a new prompt.
    Prompt,
    /// Sent to a working RPC agent to steer its running turn.
    Steer,
    /// Queued on a working RPC agent for when its turn ends.
    FollowUp,
}

/// A parsed request: a method and its typed parameters.
#[derive(Clone, Debug, PartialEq)]
pub enum Call {
    /// See [`Method::Ping`].
    Ping,
    /// See [`Method::SpaceList`].
    SpaceList,
    /// See [`Method::SpaceDraft`].
    SpaceDraft(SpaceDraftParams),
    /// See [`Method::SpaceReshape`].
    SpaceReshape(SpaceReshapeParams),
    /// See [`Method::LayoutGet`].
    LayoutGet(SpaceParams),
    /// See [`Method::PaneSplit`].
    PaneSplit(PaneSplitParams),
    /// See [`Method::PaneClose`].
    PaneClose(PaneParams),
    /// See [`Method::PaneFocus`].
    PaneFocus(PaneParams),
    /// See [`Method::PaneZoom`].
    PaneZoom(PaneZoomParams),
    /// See [`Method::InboxList`].
    InboxList(InboxListParams),
    /// See [`Method::InboxPost`].
    InboxPost(InboxPostParams),
    /// See [`Method::StatusSet`].
    StatusSet(StatusSetParams),
    /// See [`Method::GraphGet`].
    GraphGet(SpaceParams),
    /// See [`Method::GraphPause`].
    GraphPause(AgentParams),
    /// See [`Method::GraphResume`].
    GraphResume(AgentParams),
    /// See [`Method::GraphAddLane`].
    GraphAddLane(GraphAddLaneParams),
    /// See [`Method::GraphRemoveLane`].
    GraphRemoveLane(AgentParams),
    /// See [`Method::ChannelPost`].
    ChannelPost(ChannelPostParams),
    /// See [`Method::ChannelList`].
    ChannelList(ChannelListParams),
    /// See [`Method::ContractGet`].
    ContractGet(SpaceParams),
    /// See [`Method::ContractSet`].
    ContractSet(ContractSetParams),
    /// See [`Method::ContractAgree`].
    ContractAgree(ContractAgreeParams),
    /// See [`Method::LoopStatus`].
    LoopStatus(SpaceParams),
    /// See [`Method::LoopAdvance`].
    LoopAdvance(LoopAdvanceParams),
    /// See [`Method::LoopSetAutonomy`].
    LoopSetAutonomy(LoopSetAutonomyParams),
    /// See [`Method::SpaceWidenPropose`].
    SpaceWidenPropose(SpaceWidenProposeParams),
    /// See [`Method::AttemptList`].
    AttemptList(AttemptListParams),
    /// See [`Method::AttemptReport`].
    AttemptReport(AttemptReportParams),
    /// See [`Method::AttemptPropose`].
    AttemptPropose(AttemptProposeParams),
    /// See [`Method::AttemptDiff`].
    AttemptDiff(AttemptDiffParams),
    /// See [`Method::PluginList`].
    PluginList(PluginListParams),
    /// See [`Method::PluginTools`].
    PluginTools(PluginToolsParams),
    /// See [`Method::PluginMount`].
    PluginMount(PluginMountParams),
    /// See [`Method::PluginUnmount`].
    PluginUnmount(PluginUnmountParams),
    /// See [`Method::PluginCall`].
    PluginCall(PluginCallParams),
    /// See [`Method::PluginEmit`].
    PluginEmit(PluginEmitParams),
    /// See [`Method::AgentList`].
    AgentList(AgentListParams),
    /// See [`Method::AgentRead`].
    AgentRead(AgentReadParams),
    /// See [`Method::AgentSend`].
    AgentSend(AgentSendParams),
    /// See [`Method::AgentStart`].
    AgentStart(AgentParams),
    /// See [`Method::SpaceOpen`].
    SpaceOpen(SpaceParams),
}

fn params<P: DeserializeOwned>(method: Method, params: Option<Value>) -> Result<P, RpcError> {
    let value = match params {
        None | Some(Value::Null) => Value::Object(Default::default()),
        Some(v @ Value::Object(_)) => v,
        Some(_) => {
            return Err(RpcError::invalid_params(format!(
                "{method}: params must be an object"
            )));
        }
    };
    serde_json::from_value(value).map_err(|e| {
        let usage = method.usage();
        let hint = if usage.is_empty() {
            "it takes no parameters".to_owned()
        } else {
            format!("usage: {method} {usage}")
        };
        RpcError::invalid_params(format!("{method}: {e}; {hint}"))
    })
}

fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

impl Call {
    /// Check `method` and `params` and build the call. Fails with [`ErrorCode::MethodNotFound`] for an unknown
    /// method and [`ErrorCode::InvalidParams`] (with the method's usage) for parameters that do not fit.
    pub fn parse(method: &str, raw: Option<Value>) -> Result<Call, RpcError> {
        let Some(m) = Method::from_name(method) else {
            return Err(RpcError::new(
                ErrorCode::MethodNotFound,
                format!("no method named {method:?}"),
            ));
        };
        Ok(match m {
            Method::Ping => {
                params::<NoParams>(m, raw)?;
                Call::Ping
            }
            Method::SpaceList => {
                params::<NoParams>(m, raw)?;
                Call::SpaceList
            }
            Method::SpaceDraft => Call::SpaceDraft(params(m, raw)?),
            Method::SpaceReshape => Call::SpaceReshape(params(m, raw)?),
            Method::LayoutGet => Call::LayoutGet(params(m, raw)?),
            Method::PaneSplit => Call::PaneSplit(params(m, raw)?),
            Method::PaneClose => Call::PaneClose(params(m, raw)?),
            Method::PaneFocus => Call::PaneFocus(params(m, raw)?),
            Method::PaneZoom => Call::PaneZoom(params(m, raw)?),
            Method::InboxList => Call::InboxList(params(m, raw)?),
            Method::InboxPost => Call::InboxPost(params(m, raw)?),
            Method::StatusSet => Call::StatusSet(params(m, raw)?),
            Method::GraphGet => Call::GraphGet(params(m, raw)?),
            Method::GraphPause => Call::GraphPause(params(m, raw)?),
            Method::GraphResume => Call::GraphResume(params(m, raw)?),
            Method::GraphAddLane => Call::GraphAddLane(params(m, raw)?),
            Method::GraphRemoveLane => Call::GraphRemoveLane(params(m, raw)?),
            Method::ChannelPost => Call::ChannelPost(params(m, raw)?),
            Method::ChannelList => Call::ChannelList(params(m, raw)?),
            Method::ContractGet => Call::ContractGet(params(m, raw)?),
            Method::ContractSet => Call::ContractSet(params(m, raw)?),
            Method::ContractAgree => Call::ContractAgree(params(m, raw)?),
            Method::LoopStatus => Call::LoopStatus(params(m, raw)?),
            Method::LoopAdvance => Call::LoopAdvance(params(m, raw)?),
            Method::LoopSetAutonomy => Call::LoopSetAutonomy(params(m, raw)?),
            Method::SpaceWidenPropose => Call::SpaceWidenPropose(params(m, raw)?),
            Method::AttemptList => Call::AttemptList(params(m, raw)?),
            Method::AttemptReport => Call::AttemptReport(params(m, raw)?),
            Method::AttemptPropose => Call::AttemptPropose(params(m, raw)?),
            Method::AttemptDiff => Call::AttemptDiff(params(m, raw)?),
            Method::PluginList => Call::PluginList(params(m, raw)?),
            Method::PluginTools => Call::PluginTools(params(m, raw)?),
            Method::PluginMount => Call::PluginMount(params(m, raw)?),
            Method::PluginUnmount => {
                let p: PluginUnmountParams = params(m, raw)?;
                if p.plugin.is_some() == p.pane.is_some() {
                    return Err(RpcError::invalid_params(format!(
                        "{m}: name a plugin or a pane, not both or neither; usage: {m} {}",
                        m.usage()
                    )));
                }
                Call::PluginUnmount(p)
            }
            Method::PluginCall => Call::PluginCall(params(m, raw)?),
            Method::PluginEmit => Call::PluginEmit(params(m, raw)?),
            Method::AgentList => Call::AgentList(params(m, raw)?),
            Method::AgentRead => {
                let p: AgentReadParams = params(m, raw)?;
                if !(1..=AGENT_READ_MAX_LINES).contains(&p.line_count()) {
                    return Err(RpcError::invalid_params(format!(
                        "{m}: lines must be 1 to {AGENT_READ_MAX_LINES}; usage: {m} {}",
                        m.usage()
                    )));
                }
                Call::AgentRead(p)
            }
            Method::AgentSend => {
                let p: AgentSendParams = params(m, raw)?;
                if p.text.trim().is_empty() {
                    return Err(RpcError::invalid_params(format!(
                        "{m}: text is empty; usage: {m} {}",
                        m.usage()
                    )));
                }
                Call::AgentSend(p)
            }
            Method::AgentStart => Call::AgentStart(params(m, raw)?),
            Method::SpaceOpen => Call::SpaceOpen(params(m, raw)?),
        })
    }

    /// The method.
    pub fn method(&self) -> Method {
        match self {
            Call::Ping => Method::Ping,
            Call::SpaceList => Method::SpaceList,
            Call::SpaceDraft(_) => Method::SpaceDraft,
            Call::SpaceReshape(_) => Method::SpaceReshape,
            Call::LayoutGet(_) => Method::LayoutGet,
            Call::PaneSplit(_) => Method::PaneSplit,
            Call::PaneClose(_) => Method::PaneClose,
            Call::PaneFocus(_) => Method::PaneFocus,
            Call::PaneZoom(_) => Method::PaneZoom,
            Call::InboxList(_) => Method::InboxList,
            Call::InboxPost(_) => Method::InboxPost,
            Call::StatusSet(_) => Method::StatusSet,
            Call::GraphGet(_) => Method::GraphGet,
            Call::GraphPause(_) => Method::GraphPause,
            Call::GraphResume(_) => Method::GraphResume,
            Call::GraphAddLane(_) => Method::GraphAddLane,
            Call::GraphRemoveLane(_) => Method::GraphRemoveLane,
            Call::ChannelPost(_) => Method::ChannelPost,
            Call::ChannelList(_) => Method::ChannelList,
            Call::ContractGet(_) => Method::ContractGet,
            Call::ContractSet(_) => Method::ContractSet,
            Call::ContractAgree(_) => Method::ContractAgree,
            Call::LoopStatus(_) => Method::LoopStatus,
            Call::LoopAdvance(_) => Method::LoopAdvance,
            Call::LoopSetAutonomy(_) => Method::LoopSetAutonomy,
            Call::SpaceWidenPropose(_) => Method::SpaceWidenPropose,
            Call::AttemptList(_) => Method::AttemptList,
            Call::AttemptReport(_) => Method::AttemptReport,
            Call::AttemptPropose(_) => Method::AttemptPropose,
            Call::AttemptDiff(_) => Method::AttemptDiff,
            Call::PluginList(_) => Method::PluginList,
            Call::PluginTools(_) => Method::PluginTools,
            Call::PluginMount(_) => Method::PluginMount,
            Call::PluginUnmount(_) => Method::PluginUnmount,
            Call::PluginCall(_) => Method::PluginCall,
            Call::PluginEmit(_) => Method::PluginEmit,
            Call::AgentList(_) => Method::AgentList,
            Call::AgentRead(_) => Method::AgentRead,
            Call::AgentSend(_) => Method::AgentSend,
            Call::AgentStart(_) => Method::AgentStart,
            Call::SpaceOpen(_) => Method::SpaceOpen,
        }
    }

    /// The parameters as a JSON object.
    pub fn params(&self) -> Value {
        match self {
            Call::Ping | Call::SpaceList => json!({}),
            Call::SpaceDraft(p) => to_value(p),
            Call::SpaceReshape(p) => to_value(p),
            Call::LayoutGet(p) => to_value(p),
            Call::PaneSplit(p) => to_value(p),
            Call::PaneClose(p) | Call::PaneFocus(p) => to_value(p),
            Call::PaneZoom(p) => to_value(p),
            Call::InboxList(p) => to_value(p),
            Call::InboxPost(p) => to_value(p),
            Call::StatusSet(p) => to_value(p),
            Call::GraphGet(p) | Call::ContractGet(p) | Call::LoopStatus(p) => to_value(p),
            Call::GraphPause(p) | Call::GraphResume(p) | Call::GraphRemoveLane(p) => to_value(p),
            Call::ContractAgree(p) => to_value(p),
            Call::GraphAddLane(p) => to_value(p),
            Call::ChannelPost(p) => to_value(p),
            Call::ChannelList(p) => to_value(p),
            Call::ContractSet(p) => to_value(p),
            Call::LoopAdvance(p) => to_value(p),
            Call::LoopSetAutonomy(p) => to_value(p),
            Call::SpaceWidenPropose(p) => to_value(p),
            Call::AttemptList(p) => to_value(p),
            Call::AttemptReport(p) => to_value(p),
            Call::AttemptPropose(p) => to_value(p),
            Call::AttemptDiff(p) => to_value(p),
            Call::PluginList(p) => to_value(p),
            Call::PluginTools(p) => to_value(p),
            Call::PluginMount(p) => to_value(p),
            Call::PluginUnmount(p) => to_value(p),
            Call::PluginCall(p) => to_value(p),
            Call::PluginEmit(p) => to_value(p),
            Call::AgentList(p) => to_value(p),
            Call::AgentRead(p) => to_value(p),
            Call::AgentSend(p) => to_value(p),
            Call::AgentStart(p) => to_value(p),
            Call::SpaceOpen(p) => to_value(p),
        }
    }

    /// The space the call is about, if it names one.
    pub fn space(&self) -> Option<&str> {
        match self {
            Call::Ping | Call::SpaceList | Call::SpaceDraft(_) => None,
            Call::SpaceReshape(p) => Some(&p.space),
            Call::LayoutGet(p) => Some(&p.space),
            Call::PaneSplit(p) => Some(&p.space),
            Call::PaneClose(p) | Call::PaneFocus(p) => Some(&p.space),
            Call::PaneZoom(p) => Some(&p.space),
            Call::InboxList(p) => p.space.as_deref(),
            Call::InboxPost(p) => p.space.as_deref(),
            Call::StatusSet(p) => p.space.as_deref(),
            Call::GraphGet(p) | Call::ContractGet(p) | Call::LoopStatus(p) => Some(&p.space),
            Call::GraphPause(p) | Call::GraphResume(p) | Call::GraphRemoveLane(p) => Some(&p.space),
            Call::ContractAgree(p) => Some(&p.space),
            Call::GraphAddLane(p) => Some(&p.space),
            Call::ChannelPost(p) => Some(&p.space),
            Call::ChannelList(p) => Some(&p.space),
            Call::ContractSet(p) => Some(&p.space),
            Call::LoopAdvance(p) => Some(&p.space),
            Call::LoopSetAutonomy(p) => Some(&p.space),
            Call::SpaceWidenPropose(p) => Some(&p.space),
            Call::AttemptList(p) => Some(&p.space),
            Call::AttemptReport(p) => Some(&p.space),
            Call::AttemptPropose(p) => Some(&p.space),
            Call::AttemptDiff(p) => Some(&p.space),
            Call::PluginList(p) => p.space.as_deref(),
            Call::PluginTools(p) => p.space.as_deref(),
            Call::PluginMount(p) => Some(&p.space),
            Call::PluginUnmount(p) => Some(&p.space),
            Call::PluginCall(p) => Some(&p.space),
            Call::PluginEmit(p) => Some(&p.space),
            Call::AgentList(p) => p.space.as_deref(),
            Call::AgentRead(p) => Some(&p.space),
            Call::AgentSend(p) => Some(&p.space),
            Call::AgentStart(p) => Some(&p.space),
            Call::SpaceOpen(p) => Some(&p.space),
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Results

/// The result of `ping`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PingResult {
    /// [`PROTOCOL_VERSION`] of the server.
    pub protocol: u32,
    /// The xctx version.
    pub version: String,
    /// The window's process id.
    pub pid: u32,
}

impl PingResult {
    /// This process's answer.
    pub fn current() -> PingResult {
        PingResult {
            protocol: PROTOCOL_VERSION,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            pid: std::process::id(),
        }
    }
}

/// A repo of a space in `space.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoSummary {
    /// The repo's name in the space.
    pub name: String,
    /// The space's worktree of it.
    pub worktree: String,
    /// The worktree's branch, `space/<slug>`.
    pub branch: String,
}

/// A space in `space.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceSummary {
    /// The slug: the space's id, `MISE_ENV` and branch tail.
    pub id: String,
    /// The title Home shows.
    pub title: String,
    /// The space's `PORT`.
    pub port: u16,
    /// How it is laid out now.
    pub shape: Shape,
    /// Its repos in display order.
    pub repos: Vec<RepoSummary>,
}

/// The result of `space.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceListResult {
    /// The active spaces, in the order Home shows them.
    pub spaces: Vec<SpaceSummary>,
}

/// A repo proposed in `space.draft`'s result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftRepoSummary {
    /// The repo's name.
    pub name: String,
    /// Its checkout.
    pub path: String,
    /// Why it was picked.
    pub reason: String,
}

/// The result of `space.draft`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceDraftResult {
    /// The Plan Gate item posted to the Inbox; the space launches only when the user approves it.
    pub inbox_item: String,
    /// The proposed title.
    pub title: String,
    /// The proposed repos.
    pub repos: Vec<DraftRepoSummary>,
}

/// The result of `layout.get` and `space.reshape`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutInfo {
    /// The space's slug.
    pub space: String,
    /// How it is laid out.
    pub shape: Shape,
    /// The pane tree, with the focused and zoomed pane.
    pub tree: PaneTree,
    /// Every pane with what it is for, in leaf order.
    pub panes: Vec<RolePane>,
}

/// The result of `pane.split`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneSplitResult {
    /// The new pane.
    pub pane: PaneId,
}

/// The result of `pane.zoom`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneZoomResult {
    /// The zoomed pane, or `None` when the space is now unzoomed.
    pub zoomed: Option<PaneId>,
}

/// An Inbox item in `inbox.list`. Secret values are already redacted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxItemSummary {
    /// The item's id.
    pub id: String,
    /// `plan_gate`, `space_widen`, `attempt_proposal`, `plugin_gate`, `missing_env_var` or `notice`.
    pub kind: String,
    /// `pending`, `approved`, `rejected` or `dismissed`.
    pub state: String,
    /// One line.
    pub title: String,
    /// More detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// The space it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<String>,
}

/// The result of `inbox.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxListResult {
    /// The items, newest first.
    pub items: Vec<InboxItemSummary>,
}

/// The result of `inbox.post`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxPostResult {
    /// The new item's id.
    pub id: String,
}

/// The result of `channel.post`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelPostResult {
    /// The new message's id; pass it as `since_id` to `channel.list` to read only what came after.
    pub id: u64,
}

/// One attempt in `attempt.list`. Text is already masked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptSummary {
    /// Its number in the space.
    pub number: u32,
    /// Its round (fork).
    pub round: u32,
    /// The space repo it is a copy of.
    pub repo: String,
    /// The writer it was forked from.
    pub agent: String,
    /// `running`, `done`, `failed`, `discarded` or `adopted`.
    pub state: String,
    /// What it was told to try.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// Its branch, `attempt/<slug>/<n>`.
    pub branch: String,
    /// Its worktree.
    pub worktree: String,
    /// Its `PORT`.
    pub port: u16,
    /// What its agent last reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Tests that passed, as reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_passed: Option<u32>,
    /// Tests that failed, as reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests_failed: Option<u32>,
    /// `fixed`, `still_fails` or `not_checked`.
    pub repro: String,
}

/// The result of `attempt.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptListResult {
    /// The attempts, oldest first.
    pub attempts: Vec<AttemptSummary>,
}

/// The result of `attempt.propose`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptProposeResult {
    /// The gate posted to the Inbox; the attempts are forked only when the user approves it.
    pub inbox_item: String,
}

/// One changed file in `attempt.diff`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptFileChange {
    /// The path in the repository.
    pub path: String,
    /// Lines added; `None` for a binary file.
    pub added: Option<u32>,
    /// Lines removed; `None` for a binary file.
    pub removed: Option<u32>,
}

/// The result of `attempt.diff`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptDiffResult {
    /// The files changed since the fork, uncommitted work included.
    pub files: Vec<AttemptFileChange>,
    /// The unified diff of the file asked for, masked; absent without `path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
}

/// The result of `space.widen_propose`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceWidenProposeResult {
    /// The space widening item posted to the Inbox; the space widens only when the user approves it.
    pub inbox_item: String,
}

/// Where a plugin comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginSource {
    /// Shipped with xctx: terminal, API client, logs, database, browser, the connectors.
    Builtin,
    /// The user's own, in `~/.config/xctx/plugins/<name>/`.
    User,
    /// A repo's own, in its `.xctx/plugins/`; it needs approval in the Inbox the first time.
    Repo,
}

/// Whether a plugin tool only reads or also changes something.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolEffect {
    /// Reads only; `plugin.call` runs it.
    Read,
    /// Changes something; `plugin.call` posts a gate to the Inbox and it runs only when the user approves.
    Write,
}

/// A tool in `plugin.list`: enough to know it exists; `plugin.tools` has its parameters.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginToolSummary {
    /// The tool's name, for example `http.send`.
    pub name: String,
    /// Whether it reads or writes.
    pub effect: ToolEffect,
}

/// A plugin in `plugin.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginSummary {
    /// The manifest name, which the other plugin methods take as `plugin=`.
    pub name: String,
    /// One line on what it is for.
    pub description: String,
    /// Where it comes from.
    pub source: PluginSource,
    /// For a repo plugin, the repo it lives in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// For a repo plugin, whether the user has trusted it yet; mounting an untrusted one posts a gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trusted: Option<bool>,
    /// The slugs of the spaces it is mounted in.
    #[serde(default)]
    pub mounted_in: Vec<String>,
    /// Its tools.
    #[serde(default)]
    pub tools: Vec<PluginToolSummary>,
}

/// The result of `plugin.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginListResult {
    /// The plugins, built-in ones first, then by name.
    pub plugins: Vec<PluginSummary>,
}

/// One parameter of a plugin tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginToolParam {
    /// The name `plugin.call` takes it by in `args`.
    pub name: String,
    /// Its JSON type as the manifest declares it, for example `string`, `integer`, `number` or `boolean`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Whether a call must give it.
    #[serde(default)]
    pub required: bool,
    /// What it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A plugin tool in `plugin.tools`: everything an agent needs to call it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginToolSpec {
    /// The tool's name, which `plugin.call` takes as `tool=`.
    pub name: String,
    /// What it does.
    pub description: String,
    /// Whether it reads or writes; a write is a gate in the Inbox.
    pub effect: ToolEffect,
    /// Its parameters.
    #[serde(default)]
    pub params: Vec<PluginToolParam>,
}

/// The result of `plugin.tools`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginToolsResult {
    /// The plugin's manifest name.
    pub plugin: String,
    /// Its tools, in manifest order.
    pub tools: Vec<PluginToolSpec>,
}

/// The result of `plugin.mount`: exactly one of `pane` and `inbox_item` is set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginMountResult {
    /// The plugin's new pane, when it was mounted now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane: Option<PaneId>,
    /// The gate posted to the Inbox instead, when mounting needs the user's approval (a connector with write
    /// tools, or a repo plugin not trusted yet). It mounts only when the user approves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_item: Option<String>,
}

/// The result of `plugin.unmount`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginUnmountResult {
    /// The panes closed.
    pub closed: Vec<PaneId>,
}

/// The result of `plugin.call`: exactly one of `output` and `inbox_item` is set.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginCallResult {
    /// What a read tool returned, masked: the JSON it printed, or its text as a string.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<Value>,
    /// The gate posted to the Inbox for a write tool; it runs only when the user approves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inbox_item: Option<String>,
}

/// What a space's loop does with a plugin event, as the plugin's manifest maps it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginEventAction {
    /// Stop the loop at its stage.
    Fail,
    /// The stage's check passed.
    Pass,
    /// Post to the Inbox.
    Notice,
}

/// The result of `plugin.emit`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginEmitResult {
    /// What the loop did with the event; absent when the manifest maps it to nothing and it was only recorded
    /// as a finding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<PluginEventAction>,
}

/// Whether an agent's program runs, in `agent.list` and `agent.read`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentProcess {
    /// Its terminal runs it.
    Running,
    /// It exited, or could not start; `exit_code` says how when it is known.
    Exited,
    /// It has no terminal: it is bound to no pane, or the space has no terminal for its pane.
    NoPane,
}

/// One agent in `agent.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSummary {
    /// The space's slug.
    pub space: String,
    /// Its id in the space's graph, which `agent.read` and `agent.send` take as `agent=`.
    pub id: String,
    /// The name the graph shows.
    pub label: String,
    /// `orchestrator`, `writer` or `helper:<kind>` (`helper:tests`, `helper:browser`, ...).
    pub role: String,
    /// The repo a writer writes to, by its name in the space.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<String>,
    /// The folder a writer works in: the space's worktree of its repo.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<String>,
    /// Its graph state as `graph.get` reports it: `idle`, `running`, `paused`, `blocked`, `done` or `failed`.
    pub state: String,
    /// Why it is blocked or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The pane it is bound to.
    pub pane: Option<u64>,
    /// Whether its program runs.
    pub process: AgentProcess,
    /// How it exited, when it did and the code is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

/// One active space in `agent.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSpaceSummary {
    /// The space's slug.
    pub space: String,
    /// Whether its tab is open. A space that is not open runs no agents and lists none; `agent.list` never
    /// opens it.
    pub open: bool,
    /// Its agents in graph order; empty when it is not open.
    pub agents: Vec<AgentSummary>,
}

/// The result of `agent.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentListResult {
    /// The active spaces in the order Home shows them (or the one asked for).
    pub spaces: Vec<AgentSpaceSummary>,
}

/// The result of `agent.read`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentReadResult {
    /// The space's slug.
    pub space: String,
    /// The agent's id.
    pub agent: String,
    /// Whether its program runs.
    pub process: AgentProcess,
    /// How it exited, when it did and the code is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The screen's last lines joined with `\n`, blank lines at the bottom dropped, secrets masked.
    pub text: String,
}

/// The result of `agent.send`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSendResult {
    /// Always `true`: the message was typed and submitted.
    pub sent: bool,
    /// How it was delivered; absent from xctx versions before RPC agents.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered: Option<AgentDelivery>,
}

/// The result of `agent.start`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStartResult {
    /// The pane the agent runs in.
    pub pane: PaneId,
    /// `false` when it was already running there and nothing was started.
    pub started: bool,
}

/// The result of methods that only report success: `pane.close`, `pane.focus`, `status.set`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ack {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_method_name_round_trips() {
        for m in Method::ALL {
            assert_eq!(Method::from_name(m.name()), Some(m));
        }
        assert_eq!(Method::from_name("pane.explode"), None);
    }

    #[test]
    fn nothing_on_the_socket_forks_adopts_or_discards_attempts() {
        for m in Method::ALL {
            for word in ["fork", "adopt", "discard"] {
                assert!(!m.name().contains(word), "{m}");
            }
        }
        assert_eq!(Method::AttemptList.capability(), Capability::Read);
        assert_eq!(Method::AttemptDiff.capability(), Capability::Read);
        assert_eq!(Method::AttemptReport.capability(), Capability::Propose);
        assert_eq!(Method::AttemptPropose.capability(), Capability::Propose);
    }

    #[test]
    fn calls_round_trip_through_their_params() -> Result<(), RpcError> {
        let calls = [
            Call::AttemptList(AttemptListParams {
                space: "s".into(),
                round: Some(2),
            }),
            Call::AttemptReport(AttemptReportParams {
                space: "s".into(),
                attempt: 3,
                summary: "token bucket".into(),
                tests_passed: Some(41),
                tests_failed: Some(0),
                test_command: Some("cargo test".into()),
                repro: ReproStatus::Fixed,
                gave_up: false,
                token: Some("0f0f".into()),
            }),
            Call::AttemptPropose(AttemptProposeParams {
                space: "s".into(),
                agent: "writer-api".into(),
                count: 3,
                instructions: Some(vec!["a, with a comma".into()]),
                rationale: None,
            }),
            Call::AttemptDiff(AttemptDiffParams {
                space: "s".into(),
                attempt: 1,
                path: Some("src/limit.rs".into()),
            }),
            Call::Ping,
            Call::SpaceList,
            Call::SpaceDraft(SpaceDraftParams {
                task: "rate limit".into(),
                repos: Some(vec!["api".into()]),
                rationale: Some("the limiter lives in the api".into()),
            }),
            Call::SpaceReshape(SpaceReshapeParams {
                space: "s".into(),
                shape: Shape::Review,
            }),
            Call::PaneSplit(PaneSplitParams {
                space: "s".into(),
                pane: Some(PaneId(3)),
                axis: SplitAxis::Vertical,
                kind: Some(PaneKind::Git),
                repo: None,
            }),
            Call::PaneZoom(PaneZoomParams {
                space: "s".into(),
                pane: None,
            }),
            Call::InboxList(InboxListParams::default()),
            Call::StatusSet(StatusSetParams {
                message: "hi".into(),
                level: StatusLevel::Warning,
                space: None,
            }),
        ];
        for call in calls {
            let parsed = Call::parse(call.method().name(), Some(call.params()))?;
            assert_eq!(parsed, call);
        }
        Ok(())
    }

    fn m3_calls() -> Vec<Call> {
        let space = || SpaceParams { space: "s".into() };
        let agent = || AgentParams {
            space: "s".into(),
            agent: "writer-api".into(),
        };
        vec![
            Call::GraphGet(space()),
            Call::GraphPause(agent()),
            Call::GraphResume(agent()),
            Call::GraphAddLane(GraphAddLaneParams {
                space: "s".into(),
                role: LaneRole::Helper,
                repo: Some("api".into()),
                kind: Some(HelperKind::Reviewer),
                label: Some("second reviewer".into()),
            }),
            Call::GraphAddLane(GraphAddLaneParams {
                space: "s".into(),
                role: LaneRole::Writer,
                repo: Some("web".into()),
                kind: None,
                label: None,
            }),
            Call::GraphRemoveLane(agent()),
            Call::ChannelPost(ChannelPostParams {
                space: "s".into(),
                from: "writer-api".into(),
                to: Some("writer-web".into()),
                kind: "question".into(),
                body: "is amount in cents?".into(),
            }),
            Call::ChannelList(ChannelListParams {
                space: "s".into(),
                to: None,
                kind: Some("finding".into()),
                since_id: Some(41),
                limit: Some(20),
            }),
            Call::ContractGet(space()),
            Call::ContractSet(ContractSetParams {
                space: "s".into(),
                author: "orchestrator".into(),
                title: "charge event".into(),
                summary: "the payload both repos agree on".into(),
                spec: "{\"amount_cents\": \"u64\"}".into(),
            }),
            Call::ContractAgree(ContractAgreeParams {
                space: "s".into(),
                agent: "writer-api".into(),
                version: Some(2),
            }),
            Call::ContractAgree(ContractAgreeParams {
                space: "s".into(),
                agent: "writer-web".into(),
                version: None,
            }),
            Call::LoopStatus(space()),
            Call::LoopAdvance(LoopAdvanceParams {
                space: "s".into(),
                checks_passed: Some(false),
            }),
            Call::LoopSetAutonomy(LoopSetAutonomyParams {
                space: "s".into(),
                autonomy: LoopAutonomy::WithinBudget,
                max_iterations: Some(3),
                max_turns: None,
            }),
            Call::SpaceWidenPropose(SpaceWidenProposeParams {
                space: "s".into(),
                repos: Some(vec!["web".into()]),
                production: true,
                rationale: Some("the event is consumed in web".into()),
            }),
        ]
    }

    #[test]
    fn m3_calls_round_trip_through_their_params_and_name_their_space() -> Result<(), RpcError> {
        let calls = m3_calls();
        let mut methods: Vec<Method> = calls.iter().map(Call::method).collect();
        methods.dedup();
        assert_eq!(methods, Method::ALL[12..26], "every M3 method is covered");
        for call in calls {
            let parsed = Call::parse(call.method().name(), Some(call.params()))?;
            assert_eq!(parsed, call);
            assert_eq!(call.space(), Some("s"), "{}", call.method());
        }
        Ok(())
    }

    #[test]
    fn m3_params_use_snake_case_names_and_reject_unknown_values() -> Result<(), RpcError> {
        let call = Call::parse(
            "loop.set_autonomy",
            Some(json!({"space": "s", "autonomy": "until_checks_pass"})),
        )?;
        assert_eq!(
            call.params(),
            json!({"space": "s", "autonomy": "until_checks_pass"})
        );
        let call = Call::parse(
            "graph.add_lane",
            Some(json!({"space": "s", "role": "helper", "kind": "probe"})),
        )?;
        assert_eq!(
            call.params(),
            json!({"space": "s", "role": "helper", "kind": "probe"})
        );
        let widen = Call::parse("space.widen_propose", Some(json!({"space": "s"})))?;
        assert_eq!(
            widen,
            Call::SpaceWidenPropose(SpaceWidenProposeParams {
                space: "s".into(),
                repos: None,
                production: false,
                rationale: None,
            })
        );
        assert_eq!(widen.params(), json!({"space": "s"}));
        for (method, params) in [
            (
                "loop.set_autonomy",
                json!({"space": "s", "autonomy": "yolo"}),
            ),
            (
                "graph.add_lane",
                json!({"space": "s", "role": "orchestrator"}),
            ),
            (
                "graph.add_lane",
                json!({"space": "s", "role": "helper", "kind": "lint"}),
            ),
            ("graph.pause", json!({"space": "s"})),
            (
                "channel.post",
                json!({"space": "s", "from": "a", "body": "hi"}),
            ),
            ("channel.list", json!({"space": "s", "since_id": "x"})),
            (
                "contract.set",
                json!({"space": "s", "author": "a", "title": "t", "summary": "s"}),
            ),
            ("loop.advance", json!({"space": "s", "checks": true})),
            (
                "space.widen_propose",
                json!({"space": "s", "production": "yes"}),
            ),
        ] {
            let err = Call::parse(method, Some(params)).err();
            let err = err.map(|e| (e.kind(), e.message));
            assert!(
                matches!(&err, Some((Some(ErrorCode::InvalidParams), m)) if m.contains(&format!("usage: {method}"))),
                "{method}: {err:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn contract_agree_takes_an_optional_version() -> Result<(), RpcError> {
        let without = Call::parse(
            "contract.agree",
            Some(json!({"space": "s", "agent": "writer-api"})),
        )?;
        assert_eq!(
            without,
            Call::ContractAgree(ContractAgreeParams {
                space: "s".into(),
                agent: "writer-api".into(),
                version: None,
            })
        );
        assert_eq!(
            without.params(),
            json!({"space": "s", "agent": "writer-api"}),
            "an absent version stays off the wire"
        );
        let with = Call::parse(
            "contract.agree",
            Some(json!({"space": "s", "agent": "writer-api", "version": 3})),
        )?;
        assert_eq!(
            with.params(),
            json!({"space": "s", "agent": "writer-api", "version": 3})
        );
        let negative = Call::parse(
            "contract.agree",
            Some(json!({"space": "s", "agent": "writer-api", "version": -1})),
        );
        assert_eq!(
            negative.err().and_then(|e| e.kind()),
            Some(ErrorCode::InvalidParams)
        );
        Ok(())
    }

    #[test]
    fn m3_methods_need_the_capabilities_their_effects_call_for() {
        use Capability::{Layout, Propose, Read, Steer};
        let expected = [
            (Method::GraphGet, Read),
            (Method::GraphPause, Layout),
            (Method::GraphResume, Steer),
            (Method::GraphAddLane, Layout),
            (Method::GraphRemoveLane, Layout),
            (Method::ChannelPost, Propose),
            (Method::ChannelList, Read),
            (Method::ContractGet, Read),
            (Method::ContractSet, Propose),
            (Method::ContractAgree, Propose),
            (Method::LoopStatus, Read),
            (Method::LoopAdvance, Layout),
            (Method::LoopSetAutonomy, Steer),
            (Method::SpaceWidenPropose, Propose),
        ];
        for (method, capability) in expected {
            assert_eq!(method.capability(), capability, "{method}");
            assert!(!method.usage().is_empty(), "{method}");
            assert!(!method.summary().is_empty(), "{method}");
        }
    }

    #[test]
    fn m3_results_have_stable_field_names() -> Result<(), serde_json::Error> {
        assert_eq!(
            serde_json::to_value(ChannelPostResult { id: 7 })?,
            json!({"id": 7})
        );
        let widen = SpaceWidenProposeResult {
            inbox_item: "item-3".into(),
        };
        assert_eq!(
            serde_json::to_value(&widen)?,
            json!({"inbox_item": "item-3"})
        );
        assert_eq!(
            serde_json::from_value::<SpaceWidenProposeResult>(json!({"inbox_item": "item-3"}))?,
            widen
        );
        Ok(())
    }

    #[test]
    fn unknown_fields_and_methods_are_errors_with_usage() {
        let err = Call::parse("pane.close", Some(json!({"space": "s", "pain": 1})));
        let err = err.err().map(|e| (e.kind(), e.message));
        assert!(
            matches!(&err, Some((Some(ErrorCode::InvalidParams), m)) if m.contains("usage: pane.close")),
            "{err:?}"
        );
        let err = Call::parse("nope", None).err().and_then(|e| e.kind());
        assert_eq!(err, Some(ErrorCode::MethodNotFound));
        let err = Call::parse("ping", Some(json!([1])))
            .err()
            .and_then(|e| e.kind());
        assert_eq!(err, Some(ErrorCode::InvalidParams));
    }

    #[test]
    fn no_param_methods_accept_absent_null_and_empty() -> Result<(), RpcError> {
        assert_eq!(Call::parse("ping", None)?, Call::Ping);
        assert_eq!(Call::parse("ping", Some(Value::Null))?, Call::Ping);
        assert_eq!(Call::parse("space.list", Some(json!({})))?, Call::SpaceList);
        Ok(())
    }

    #[test]
    fn envelopes_tell_notifications_from_null_ids() {
        let n = RequestEnvelope::parse_line(r#"{"jsonrpc":"2.0","method":"ping"}"#);
        assert!(matches!(n, Ok(RequestEnvelope { id: None, .. })));
        let r = RequestEnvelope::parse_line(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#);
        assert!(matches!(
            r,
            Ok(RequestEnvelope {
                id: Some(Value::Null),
                ..
            })
        ));
    }

    #[test]
    fn bad_lines_get_the_right_codes_and_keep_the_id() {
        let code = |line: &str| {
            RequestEnvelope::parse_line(line)
                .err()
                .map(|r| (r.id.clone(), r.into_result().err().and_then(|e| e.kind())))
        };
        assert_eq!(
            code("{nope"),
            Some((Value::Null, Some(ErrorCode::ParseError)))
        );
        assert_eq!(
            code("[]"),
            Some((Value::Null, Some(ErrorCode::InvalidRequest)))
        );
        assert_eq!(
            code(r#"{"jsonrpc":"1.0","id":7,"method":"ping"}"#),
            Some((json!(7), Some(ErrorCode::InvalidRequest)))
        );
        assert_eq!(
            code(r#"{"jsonrpc":"2.0","id":{"a":1},"method":"ping"}"#),
            Some((Value::Null, Some(ErrorCode::InvalidRequest)))
        );
    }

    #[test]
    fn responses_have_result_or_error_fields() -> Result<(), serde_json::Error> {
        let ok = Response::new(json!(1), Ok(json!({"a": 1})));
        assert_eq!(
            serde_json::from_str::<Value>(&ok.to_line())?,
            json!({"jsonrpc": "2.0", "id": 1, "result": {"a": 1}})
        );
        let err = Response::new(json!("x"), Err(RpcError::not_found("no space")));
        assert_eq!(
            serde_json::from_str::<Value>(&err.to_line())?,
            json!({"jsonrpc": "2.0", "id": "x", "error": {"code": -32001, "message": "no space"}})
        );
        let back: Response = serde_json::from_str(&err.to_line())?;
        assert_eq!(back, err);
        Ok(())
    }

    fn m5_calls() -> Vec<Call> {
        let mut args = Map::new();
        args.insert("path".into(), json!("/health"));
        args.insert("headers".into(), json!({"accept": "application/json"}));
        vec![
            Call::PluginList(PluginListParams {
                space: Some("s".into()),
            }),
            Call::PluginTools(PluginToolsParams {
                plugin: "http".into(),
                space: Some("s".into()),
            }),
            Call::PluginMount(PluginMountParams {
                space: "s".into(),
                plugin: "http".into(),
                repo: Some("api".into()),
            }),
            Call::PluginUnmount(PluginUnmountParams {
                space: "s".into(),
                plugin: Some("http".into()),
                pane: None,
            }),
            Call::PluginUnmount(PluginUnmountParams {
                space: "s".into(),
                plugin: None,
                pane: Some(PaneId(7)),
            }),
            Call::PluginCall(PluginCallParams {
                space: "s".into(),
                plugin: "http".into(),
                tool: "http.send".into(),
                args,
            }),
            Call::PluginEmit(PluginEmitParams {
                space: "s".into(),
                plugin: "http".into(),
                event: "response.status".into(),
                detail: Some(json!({"status": 503})),
            }),
        ]
    }

    #[test]
    fn m5_calls_round_trip_through_their_params_and_name_their_space() -> Result<(), RpcError> {
        let calls = m5_calls();
        let mut methods: Vec<Method> = calls.iter().map(Call::method).collect();
        methods.dedup();
        assert_eq!(methods, Method::ALL[30..36], "every M5 method is covered");
        for call in calls {
            let parsed = Call::parse(call.method().name(), Some(call.params()))?;
            assert_eq!(parsed, call);
            assert_eq!(call.space(), Some("s"), "{}", call.method());
        }
        // Optional fields stay off the wire.
        let list = Call::parse("plugin.list", None)?;
        assert_eq!(list, Call::PluginList(PluginListParams::default()));
        assert_eq!(list.space(), None);
        let call = Call::parse(
            "plugin.call",
            Some(json!({"space": "s", "plugin": "logs", "tool": "logs.query"})),
        )?;
        assert_eq!(
            call.params(),
            json!({"space": "s", "plugin": "logs", "tool": "logs.query"})
        );
        Ok(())
    }

    #[test]
    fn m5_params_reject_mistakes_with_usage() {
        for (method, params) in [
            ("plugin.tools", json!({})),
            ("plugin.mount", json!({"space": "s"})),
            (
                "plugin.mount",
                json!({"space": "s", "plugin": "http", "pane": 1}),
            ),
            ("plugin.unmount", json!({"space": "s"})),
            (
                "plugin.unmount",
                json!({"space": "s", "plugin": "http", "pane": 2}),
            ),
            (
                "plugin.call",
                json!({"space": "s", "plugin": "http", "tool": "t", "args": [1]}),
            ),
            (
                "plugin.call",
                json!({"space": "s", "plugin": "http", "tool": "t", "args": "x=1"}),
            ),
            ("plugin.call", json!({"space": "s", "plugin": "http"})),
            ("plugin.emit", json!({"space": "s", "plugin": "http"})),
        ] {
            let err = Call::parse(method, Some(params)).err();
            let err = err.map(|e| (e.kind(), e.message));
            assert!(
                matches!(&err, Some((Some(ErrorCode::InvalidParams), m)) if m.contains(&format!("usage: {method}"))),
                "{method}: {err:?}"
            );
        }
    }

    #[test]
    fn m5_methods_need_the_capabilities_the_plan_gives_them() {
        use Capability::{Layout, Propose, Read};
        let expected = [
            (Method::PluginList, Read),
            (Method::PluginTools, Read),
            (Method::PluginMount, Layout),
            (Method::PluginUnmount, Layout),
            (Method::PluginCall, Propose),
            (Method::PluginEmit, Layout),
        ];
        for (method, capability) in expected {
            assert_eq!(method.capability(), capability, "{method}");
            assert!(!method.usage().is_empty(), "{method}");
            assert!(!method.summary().is_empty(), "{method}");
            assert_ne!(method.capability(), Capability::Steer);
        }
    }

    #[test]
    fn m5_results_have_stable_field_names() -> Result<(), serde_json::Error> {
        let list = PluginListResult {
            plugins: vec![PluginSummary {
                name: "http".into(),
                description: "API client".into(),
                source: PluginSource::Builtin,
                repo: None,
                trusted: None,
                mounted_in: vec!["s".into()],
                tools: vec![PluginToolSummary {
                    name: "http.send".into(),
                    effect: ToolEffect::Read,
                }],
            }],
        };
        let value = serde_json::to_value(&list)?;
        assert_eq!(
            value,
            json!({"plugins": [{
                "name": "http", "description": "API client", "source": "builtin", "mounted_in": ["s"],
                "tools": [{"name": "http.send", "effect": "read"}],
            }]})
        );
        assert_eq!(serde_json::from_value::<PluginListResult>(value)?, list);

        let tools = PluginToolsResult {
            plugin: "sql".into(),
            tools: vec![PluginToolSpec {
                name: "sql.write".into(),
                description: "run a write".into(),
                effect: ToolEffect::Write,
                params: vec![PluginToolParam {
                    name: "query".into(),
                    kind: "string".into(),
                    required: true,
                    description: None,
                }],
            }],
        };
        let value = serde_json::to_value(&tools)?;
        assert_eq!(
            value["tools"][0]["params"][0],
            json!({"name": "query", "type": "string", "required": true})
        );
        assert_eq!(value["tools"][0]["effect"], "write");
        assert_eq!(serde_json::from_value::<PluginToolsResult>(value)?, tools);

        let gated = PluginMountResult {
            pane: None,
            inbox_item: Some("inbox-4".into()),
        };
        assert_eq!(
            serde_json::to_value(&gated)?,
            json!({"inbox_item": "inbox-4"})
        );
        let mounted = PluginMountResult {
            pane: Some(PaneId(9)),
            inbox_item: None,
        };
        assert_eq!(serde_json::to_value(&mounted)?, json!({"pane": 9}));
        assert_eq!(
            serde_json::to_value(PluginCallResult {
                output: Some(json!({"status": 200})),
                inbox_item: None,
            })?,
            json!({"output": {"status": 200}})
        );
        assert_eq!(
            serde_json::to_value(PluginUnmountResult {
                closed: vec![PaneId(3)]
            })?,
            json!({"closed": [3]})
        );
        assert_eq!(
            serde_json::to_value(PluginEmitResult {
                action: Some(PluginEventAction::Fail)
            })?,
            json!({"action": "fail"})
        );
        Ok(())
    }

    #[test]
    fn approval_needs_no_capability_because_no_method_approves() {
        for m in Method::ALL {
            assert!(!m.name().contains("approve"), "{m}");
        }
    }

    fn agent_calls() -> Vec<Call> {
        vec![
            Call::AgentList(AgentListParams {
                space: Some("s".into()),
            }),
            Call::AgentRead(AgentReadParams {
                space: "s".into(),
                agent: "writer-api".into(),
                lines: Some(200),
            }),
            Call::AgentSend(AgentSendParams {
                space: "s".into(),
                agent: "helper-browser-1".into(),
                text: "Parent Agent here: check /login".into(),
                while_running: Some(WhileRunning::FollowUp),
            }),
        ]
    }

    #[test]
    fn agent_calls_round_trip_through_their_params_and_name_their_space() -> Result<(), RpcError> {
        let calls = agent_calls();
        let methods: Vec<Method> = calls.iter().map(Call::method).collect();
        assert_eq!(
            methods,
            Method::ALL[36..39],
            "every agent method is covered"
        );
        for call in calls {
            let parsed = Call::parse(call.method().name(), Some(call.params()))?;
            assert_eq!(parsed, call);
            assert_eq!(call.space(), Some("s"), "{}", call.method());
        }
        let all = Call::parse("agent.list", None)?;
        assert_eq!(all, Call::AgentList(AgentListParams::default()));
        assert_eq!(all.space(), None);
        let read = Call::parse(
            "agent.read",
            Some(json!({"space": "s", "agent": "orchestrator"})),
        )?;
        let Call::AgentRead(p) = &read else {
            return Err(RpcError::internal(format!("not a read: {read:?}")));
        };
        assert_eq!(p.line_count(), AGENT_READ_DEFAULT_LINES);
        assert_eq!(
            read.params(),
            json!({"space": "s", "agent": "orchestrator"})
        );
        Ok(())
    }

    #[test]
    fn agent_params_refuse_too_many_lines_and_empty_text() {
        for (method, params) in [
            (
                "agent.read",
                json!({"space": "s", "agent": "a", "lines": 201}),
            ),
            (
                "agent.read",
                json!({"space": "s", "agent": "a", "lines": 0}),
            ),
            (
                "agent.read",
                json!({"space": "s", "agent": "a", "lines": -1}),
            ),
            ("agent.read", json!({"space": "s"})),
            (
                "agent.send",
                json!({"space": "s", "agent": "a", "text": ""}),
            ),
            (
                "agent.send",
                json!({"space": "s", "agent": "a", "text": " \n\t "}),
            ),
            ("agent.send", json!({"space": "s", "agent": "a"})),
            ("agent.list", json!({"space": "s", "all": true})),
        ] {
            let err = Call::parse(method, Some(params.clone())).err();
            let err = err.map(|e| (e.kind(), e.message));
            assert!(
                matches!(&err, Some((Some(ErrorCode::InvalidParams), m)) if m.contains(&format!("usage: {method}"))),
                "{method} {params}: {err:?}"
            );
        }
    }

    #[test]
    fn agent_methods_read_or_propose_but_never_steer() {
        assert_eq!(Method::AgentList.capability(), Capability::Read);
        assert_eq!(Method::AgentRead.capability(), Capability::Read);
        assert_eq!(Method::AgentSend.capability(), Capability::Propose);
        for m in [Method::AgentList, Method::AgentRead, Method::AgentSend] {
            assert!(!m.summary().is_empty(), "{m}");
        }
    }

    #[test]
    fn starting_agents_and_opening_spaces_parse_and_need_layout() -> Result<(), RpcError> {
        let start = Call::AgentStart(AgentParams {
            space: "s".into(),
            agent: "orchestrator".into(),
        });
        let open = Call::SpaceOpen(SpaceParams { space: "s".into() });
        let methods: Vec<Method> = [&start, &open].iter().map(|c| c.method()).collect();
        assert_eq!(methods, Method::ALL[39..41]);
        for call in [start, open] {
            assert_eq!(
                Call::parse(call.method().name(), Some(call.params()))?,
                call
            );
            assert_eq!(call.space(), Some("s"));
            assert_eq!(call.method().capability(), Capability::Layout);
        }
        for (method, params) in [
            ("agent.start", json!({"space": "s"})),
            ("space.open", json!({})),
        ] {
            let err = Call::parse(method, Some(params))
                .err()
                .and_then(|e| e.kind());
            assert_eq!(err, Some(ErrorCode::InvalidParams), "{method}");
        }
        assert_eq!(
            serde_json::to_value(AgentStartResult {
                pane: PaneId(5),
                started: true
            })
            .map_err(|e| RpcError::internal(e.to_string()))?,
            json!({"pane": 5, "started": true})
        );
        Ok(())
    }

    #[test]
    fn agent_results_have_stable_field_names() -> Result<(), serde_json::Error> {
        let list = AgentListResult {
            spaces: vec![
                AgentSpaceSummary {
                    space: "s".into(),
                    open: true,
                    agents: vec![AgentSummary {
                        space: "s".into(),
                        id: "writer-api".into(),
                        label: "api".into(),
                        role: "writer".into(),
                        repo: Some("api".into()),
                        worktree: Some("/w/api".into()),
                        state: "running".into(),
                        reason: None,
                        pane: Some(3),
                        process: AgentProcess::Exited,
                        exit_code: Some(1),
                    }],
                },
                AgentSpaceSummary {
                    space: "t".into(),
                    open: false,
                    agents: Vec::new(),
                },
            ],
        };
        let value = serde_json::to_value(&list)?;
        assert_eq!(
            value,
            json!({"spaces": [
                {"space": "s", "open": true, "agents": [{
                    "space": "s", "id": "writer-api", "label": "api", "role": "writer", "repo": "api",
                    "worktree": "/w/api", "state": "running", "pane": 3, "process": "exited", "exit_code": 1,
                }]},
                {"space": "t", "open": false, "agents": []},
            ]})
        );
        assert_eq!(serde_json::from_value::<AgentListResult>(value)?, list);
        let unbound = AgentSummary {
            space: "s".into(),
            id: "helper-browser-1".into(),
            label: "Browser".into(),
            role: "helper:browser".into(),
            repo: None,
            worktree: None,
            state: "blocked".into(),
            reason: Some("no app".into()),
            pane: None,
            process: AgentProcess::NoPane,
            exit_code: None,
        };
        let value = serde_json::to_value(&unbound)?;
        assert_eq!(
            value.get("pane"),
            Some(&Value::Null),
            "an unbound agent says so"
        );
        assert_eq!(value["process"], "no_pane");
        assert_eq!(value["reason"], "no app");
        assert_eq!(
            serde_json::to_value(AgentReadResult {
                space: "s".into(),
                agent: "orchestrator".into(),
                process: AgentProcess::Running,
                exit_code: None,
                text: "done".into(),
            })?,
            json!({"space": "s", "agent": "orchestrator", "process": "running", "text": "done"})
        );
        assert_eq!(
            serde_json::to_value(AgentSendResult {
                sent: true,
                delivered: None
            })?,
            json!({"sent": true})
        );
        assert_eq!(
            serde_json::to_value(AgentSendResult {
                sent: true,
                delivered: Some(AgentDelivery::FollowUp)
            })?,
            json!({"sent": true, "delivered": "follow_up"})
        );
        Ok(())
    }
}
