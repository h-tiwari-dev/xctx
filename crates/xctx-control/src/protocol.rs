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
use serde_json::{Value, json};
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
    /// Look at spaces, layouts, the Inbox, agent graphs, channels, contracts and loops.
    Read,
    /// Rearrange a space: split, close, focus, zoom and reshape panes; pause, add and remove agent lanes; advance a
    /// loop.
    Layout,
    /// Propose work and tell the user things: draft or propose widening a space, post to the Inbox, the channel or
    /// the contract, set the status line.
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
}

impl Method {
    /// Every method, in the order `xctx-ctl help` lists them.
    pub const ALL: [Method; 26] = [
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
            | Method::LoopStatus => Capability::Read,
            Method::SpaceReshape
            | Method::PaneSplit
            | Method::PaneClose
            | Method::PaneFocus
            | Method::PaneZoom
            | Method::GraphPause
            | Method::GraphAddLane
            | Method::GraphRemoveLane
            | Method::LoopAdvance => Capability::Layout,
            Method::GraphResume | Method::LoopSetAutonomy => Capability::Steer,
            Method::SpaceDraft
            | Method::InboxPost
            | Method::StatusSet
            | Method::ChannelPost
            | Method::ContractSet
            | Method::ContractAgree
            | Method::SpaceWidenPropose => Capability::Propose,
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
    /// `plan_gate`, `missing_env_var`, `space_widen` or `notice`.
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

/// The result of `space.widen_propose`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceWidenProposeResult {
    /// The space widening item posted to the Inbox; the space widens only when the user approves it.
    pub inbox_item: String,
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
    fn calls_round_trip_through_their_params() -> Result<(), RpcError> {
        let calls = [
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
        assert_eq!(methods, Method::ALL[12..], "every M3 method is covered");
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

    #[test]
    fn approval_needs_no_capability_because_no_method_approves() {
        for m in Method::ALL {
            assert!(!m.name().contains("approve"), "{m}");
        }
    }
}
