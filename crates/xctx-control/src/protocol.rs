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
    /// Look at spaces, layouts and the Inbox.
    Read,
    /// Rearrange a space's panes: split, close, focus, zoom, reshape.
    Layout,
    /// Propose work and tell the user things: draft a space, post to the Inbox, set the status line.
    Propose,
}

impl Capability {
    /// Every capability.
    pub const ALL: [Capability; 3] = [Capability::Read, Capability::Layout, Capability::Propose];
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
}

impl Method {
    /// Every method, in the order `xctx-ctl help` lists them.
    pub const ALL: [Method; 12] = [
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
        }
    }

    /// The method with this wire name.
    pub fn from_name(name: &str) -> Option<Method> {
        Method::ALL.into_iter().find(|m| m.name() == name)
    }

    /// What a connection must be granted to call it.
    pub fn capability(self) -> Capability {
        match self {
            Method::Ping | Method::SpaceList | Method::LayoutGet | Method::InboxList => {
                Capability::Read
            }
            Method::SpaceReshape
            | Method::PaneSplit
            | Method::PaneClose
            | Method::PaneFocus
            | Method::PaneZoom => Capability::Layout,
            Method::SpaceDraft | Method::InboxPost | Method::StatusSet => Capability::Propose,
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
        }
    }

    /// Its parameters, as `xctx-ctl` takes them. `[x]` is optional.
    pub fn usage(self) -> &'static str {
        match self {
            Method::Ping | Method::SpaceList => "",
            Method::SpaceDraft => "task=<text> [repos=<name>,<name>...]",
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

/// Parameters that name only a space: `layout.get`.
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
