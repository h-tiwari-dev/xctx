//! The window's side of the control socket: bind it at startup, carry each request onto the UI thread, and answer
//! it from the workspace. Also the Inbox the socket posts to, and the mise checks that fill it.
//!
//! The pure parts (what a request means for the store, the index and the Inbox) are free functions, so they are
//! tested without a window; [`Workspace::handle_control`] applies them to the live workspace.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{App, Context, Task, Window};
use serde::Serialize;
use serde_json::Value;
use xctx_control::protocol::{
    Ack, AgentListResult, AgentProcess, AgentReadParams, AgentReadResult, AgentSendParams,
    AgentSendResult, AgentSpaceSummary, AgentStartResult, AgentSummary, AttemptDiffParams,
    AttemptListParams, AttemptListResult, AttemptProposeParams, AttemptProposeResult,
    AttemptReportParams, DraftRepoSummary, InboxItemSummary, InboxListParams, InboxListResult,
    InboxPostParams, InboxPostResult, LayoutInfo, PaneSplitResult, PaneZoomResult, PingResult,
    PluginCallParams, PluginCallResult, PluginEmitParams, PluginEmitResult, PluginListResult,
    PluginMountResult, PluginSummary, PluginToolsResult, PluginUnmountResult, RepoSummary,
    SpaceDraftParams, SpaceDraftResult, SpaceListResult, SpaceSummary, SpaceWidenProposeParams,
    SpaceWidenProposeResult, StatusSetParams,
};
use xctx_control::{
    Call, Incoming, Responder, RpcError, Server, ServerConfig, StatusLevel, socket_path,
};
use xctx_core::{
    DraftRepo, EnvReport, MiseRunner, Redactor, RepoIndex, Space, SpaceDraft, SpaceId,
};
use xctx_graph::{AgentNode, AgentNodeId, AgentRole, AgentState, LoopDecision, LoopStage};
use xctx_inbox::{
    Filter, Inbox, ItemBody, Level, Notice, PlanGate, Source, SpaceWiden, welcome_notice,
};
use xctx_layout::{PaneId, PaneKind};
use xctx_term::TerminalSession;
use xctx_ui::StatusBanner;

use crate::attempts::{
    self, attempt_diff, attempt_list, attempt_proposal, attempt_rpc_error, check_report_token,
    diff_target, plan_fork_skipping_leftovers, record_report, refuse_attempt_agent,
    round_done_notice,
};
use crate::lifecycle::{home_dir, repo_for_draft};
use crate::persist::restore_layout;
use crate::plugin_host::{MountSettings, event_action, plugin_summary, tool_specs};
use crate::plugin_jobs::{MountOutcome, ToolCallResult, UnmountTarget};
use crate::space_graph::{
    AddedLane, SpaceGraphState, add_lane, channel_list, channel_post, contract_agree, contract_get,
    contract_set, graph_error, loop_advance, loop_approval_notice, loop_set_autonomy, remove_lane,
    set_agent_paused,
};
use crate::space_layout::{SpaceLayout, layout_shape};
use crate::space_tab::PaneTerminal;
use crate::spawn::{ControlEnv, ctl_dir_next_to, typed_task};
use crate::workspace::{Workspace, wait_message};

/// The reason given to repos an agent named in `space.draft` or `space.widen_propose`.
pub const NAMED_BY_AGENT: &str = "named by the agent";

/// Why a widening was asked for, when the agent gave no rationale.
pub const WIDEN_DEFAULT_REASON: &str = "requested by an agent";

/// The longest status message shown, in characters; longer ones are cut with an ellipsis.
pub const MAX_STATUS_CHARS: usize = 300;

/// What every `status.set` message starts with, so an agent's message is told apart from xctx's own.
pub const AGENT_BANNER_PREFIX: &str = "agent";

/// The most characters of an `agent.send` message the window's banner shows.
pub const AGENT_SEND_PREVIEW_CHARS: usize = 80;

/// The running control socket: the server and the task that carries its requests to the workspace. Dropping
/// it stops serving and removes the socket.
pub(crate) struct Control {
    _server: Server,
    _task: Task<()>,
}

/// A `space.draft` that arrived while the repo index was being built, answered once it is.
pub(crate) struct PendingDraft {
    params: SpaceDraftParams,
    responder: Responder,
}

/// A request whose answer shows stored text to an agent (`inbox.list`, `channel.list`, `contract.get`,
/// `contract.set`, `contract.agree`, `attempt.list`, `attempt.diff`) that arrived before the startup mise check learned the secret values to
/// mask, answered once it has. They wait in one queue and are answered in the order they arrived, so a
/// `contract.agree` never applies to a contract revision set after it.
pub(crate) struct PendingMaskedRead {
    call: Call,
    responder: Responder,
}

// -------------------------------------------------------------------------------------------------------------
// Pure helpers

/// The result of `space.list` for these active spaces.
pub fn space_list(spaces: &[Space]) -> SpaceListResult {
    SpaceListResult {
        spaces: spaces
            .iter()
            .map(|space| SpaceSummary {
                id: space.id.as_str().to_owned(),
                title: space.title.clone(),
                port: space.port,
                shape: layout_shape(space.shape),
                repos: space
                    .repos
                    .iter()
                    .map(|r| RepoSummary {
                        name: r.name.clone(),
                        worktree: r.worktree.to_string_lossy().into_owned(),
                        branch: r.branch.clone(),
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// The active space called `name` whose tab is open, for a request that changes its layout: as
/// [`resolve_space`], and a conflict when the tab is not open, since opening it would start every pane (and
/// agent) of the space without the user knowing.
pub fn resolve_open_space(
    spaces: &[Space],
    in_flight: impl Fn(&SpaceId) -> bool,
    is_open: impl Fn(&SpaceId) -> bool,
    name: &str,
) -> Result<SpaceId, RpcError> {
    let id = resolve_space(spaces, in_flight, name)?;
    if !is_open(&id) {
        return Err(RpcError::conflict(format!(
            "space {id} is not open; open it first"
        )));
    }
    Ok(id)
}

/// The active space called `name`: not found (listing the active ones) when there is none, a conflict while it
/// is being shipped or discarded.
pub fn resolve_space(
    spaces: &[Space],
    in_flight: impl Fn(&SpaceId) -> bool,
    name: &str,
) -> Result<SpaceId, RpcError> {
    let id = SpaceId::new(name);
    if in_flight(&id) {
        return Err(RpcError::conflict(format!(
            "space {name} is being shipped or discarded"
        )));
    }
    if spaces.iter().any(|s| s.id == id) {
        return Ok(id);
    }
    let active: Vec<&str> = spaces.iter().map(|s| s.id.as_str()).collect();
    Err(RpcError::not_found(if active.is_empty() {
        format!("no active space named {name}; there are no active spaces")
    } else {
        format!(
            "no active space named {name}; active spaces: {}",
            active.join(", ")
        )
    }))
}

/// A space's layout as `layout.get` reports it.
pub fn layout_info(space: &Space, layout: &SpaceLayout) -> LayoutInfo {
    LayoutInfo {
        space: space.id.as_str().to_owned(),
        shape: layout_shape(space.shape),
        tree: layout.tree().clone(),
        panes: layout.panes(),
    }
}

/// The draft `space.draft` proposes: the named repos (by indexed name, else as a path) when given, else what the
/// index finds for the task.
pub fn draft_for(
    index: &RepoIndex,
    params: &SpaceDraftParams,
    home: Option<&Path>,
) -> Result<SpaceDraft, RpcError> {
    let task = params.task.trim();
    if task.is_empty() {
        return Err(RpcError::invalid_params("space.draft: task is empty"));
    }
    let named = params.repos.as_deref().unwrap_or_default();
    if named.is_empty() {
        return Ok(index.draft(task));
    }
    let mut draft = SpaceDraft::new(task);
    for name in named {
        draft.add_repo(named_repo(index, name, home)?);
    }
    Ok(draft)
}

/// The repo an agent named: an indexed repo called `name`, else the repo at the path `name`.
pub fn named_repo(
    index: &RepoIndex,
    name: &str,
    home: Option<&Path>,
) -> Result<DraftRepo, RpcError> {
    if let Some(found) = index.repos().iter().find(|r| r.name == name) {
        return Ok(DraftRepo::new(found.path.clone(), NAMED_BY_AGENT));
    }
    let mut repo = repo_for_draft(name, home).map_err(|e| {
        let known: Vec<&str> = index.repos().iter().map(|r| r.name.as_str()).collect();
        RpcError::invalid_params(format!(
            "{name} is not an indexed repo ({e}); indexed repos: {}",
            if known.is_empty() {
                "none".to_owned()
            } else {
                known.join(", ")
            }
        ))
    })?;
    repo.reason = NAMED_BY_AGENT.to_owned();
    Ok(repo)
}

/// Whether two repo paths are the same directory, comparing canonical paths when both resolve.
fn same_repo(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// The widening `space.widen_propose` asks the user for: the named repos (by indexed name, else as a path) the
/// active space does not have yet, and production when asked. Invalid when it names nothing, a conflict when the
/// space already has everything asked for. Never widens; the Inbox item does once the user approves it.
pub fn widen_proposal(
    spaces: &[Space],
    in_flight: impl Fn(&SpaceId) -> bool,
    index: &RepoIndex,
    params: &SpaceWidenProposeParams,
    home: Option<&Path>,
) -> Result<SpaceWiden, RpcError> {
    let id = resolve_space(spaces, in_flight, &params.space)?;
    let space = spaces
        .iter()
        .find(|s| s.id == id)
        .ok_or_else(|| RpcError::not_found(format!("no active space named {id}")))?;
    let names: Vec<&str> = params
        .repos
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(|n| n.trim())
        .filter(|n| !n.is_empty())
        .collect();
    if names.is_empty() && !params.production {
        return Err(RpcError::invalid_params(
            "space.widen_propose: name repos to add, or ask for production=true",
        ));
    }
    let mut add_repos: Vec<DraftRepo> = Vec::new();
    for name in names {
        let repo = named_repo(index, name, home)?;
        let known = space.repos.iter().any(|r| same_repo(&r.repo, &repo.path))
            || add_repos.iter().any(|r| same_repo(&r.path, &repo.path));
        if !known {
            add_repos.push(repo);
        }
    }
    let production = params.production && !space.production;
    if add_repos.is_empty() && !production {
        return Err(RpcError::conflict(format!(
            "space {id} already has everything asked for"
        )));
    }
    let reason = params
        .rationale
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or(WIDEN_DEFAULT_REASON)
        .to_owned();
    Ok(SpaceWiden {
        space: id,
        add_repos,
        production,
        reason,
    })
}

/// What `space.draft` answers once its Plan Gate is posted.
pub fn draft_result(inbox_item: String, draft: &SpaceDraft) -> SpaceDraftResult {
    SpaceDraftResult {
        inbox_item,
        title: draft.title.clone(),
        repos: draft
            .repos
            .iter()
            .map(|r| DraftRepoSummary {
                name: r.name.clone(),
                path: r.path.to_string_lossy().into_owned(),
                reason: r.reason.clone(),
            })
            .collect(),
    }
}

/// The result of `inbox.list`, with secrets masked.
pub fn inbox_list(inbox: &Inbox, params: &InboxListParams, redactor: &Redactor) -> InboxListResult {
    let filter = Filter {
        space: params.space.as_deref().map(SpaceId::new),
        include_settled: params.all,
    };
    InboxListResult {
        items: inbox
            .list(&filter)
            .into_iter()
            .map(|item| {
                let preview = item.preview(redactor);
                InboxItemSummary {
                    id: preview.id.as_str().to_owned(),
                    kind: preview.kind.name().to_owned(),
                    state: preview.state.name().to_owned(),
                    title: preview.title,
                    body: (!preview.detail.is_empty()).then_some(preview.detail),
                    space: preview.space.map(|s| s.as_str().to_owned()),
                }
            })
            .collect(),
    }
}

/// The notice `inbox.post` adds.
pub fn notice_for(params: &InboxPostParams) -> Result<Notice, RpcError> {
    let title = params.title.trim();
    if title.is_empty() {
        return Err(RpcError::invalid_params("inbox.post: title is empty"));
    }
    Ok(Notice {
        title: title.to_owned(),
        body: params.body.clone().filter(|b| !b.trim().is_empty()),
        level: Level::Info,
    })
}

/// The status bar message for `status.set`: warnings and errors stay until dismissed, the rest clear themselves.
/// It starts with [`AGENT_BANNER_PREFIX`] (and the space, when given), so it never reads as xctx's own message.
pub fn status_banner(params: &StatusSetParams) -> StatusBanner {
    let mut message = match &params.space {
        Some(space) => format!(
            "{AGENT_BANNER_PREFIX} in {space}: {}",
            params.message.trim()
        ),
        None => format!("{AGENT_BANNER_PREFIX}: {}", params.message.trim()),
    };
    if message.chars().count() > MAX_STATUS_CHARS {
        message = message.chars().take(MAX_STATUS_CHARS - 1).collect();
        message.push('…');
    }
    match params.level {
        StatusLevel::Warning | StatusLevel::Error => StatusBanner::error(message),
        StatusLevel::Info | StatusLevel::Success => StatusBanner::info(message),
    }
}

/// The last `count` lines of a terminal screen's `text`, without the blank lines below the last line with text
/// (an idle screen's empty bottom rows), joined with `\n`.
pub fn last_screen_lines(text: &str, count: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |i| i + 1);
    let start = end.saturating_sub(count);
    lines[start..end].join("\n")
}

/// How `agent.list` names an agent's role: `orchestrator`, `writer` or `helper:<kind>`.
pub fn agent_role_name(role: &AgentRole) -> String {
    match role {
        AgentRole::Orchestrator => "orchestrator".to_owned(),
        AgentRole::Writer { .. } => "writer".to_owned(),
        AgentRole::Helper { kind } => {
            let kind = serde_json::to_value(kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_else(|| format!("{kind:?}").to_lowercase());
            format!("helper:{kind}")
        }
    }
}

/// An agent's graph state as `graph.get` reports it: the state's name and, when blocked or failed, why.
pub fn agent_state_fields(state: &AgentState) -> (&'static str, Option<String>) {
    match state {
        AgentState::Idle => ("idle", None),
        AgentState::Running => ("running", None),
        AgentState::Paused => ("paused", None),
        AgentState::Blocked { reason } => ("blocked", Some(reason.clone())),
        AgentState::Done => ("done", None),
        AgentState::Failed { reason } => ("failed", Some(reason.clone())),
    }
}

/// One agent of `space` as `agent.list` reports it, with what its terminal says.
pub fn agent_summary(space: &Space, node: &AgentNode, terminal: &AgentTerminal) -> AgentSummary {
    let repo = node.role.repo().map(str::to_owned);
    let worktree = repo.as_deref().and_then(|name| {
        space
            .repos
            .iter()
            .find(|r| r.name == name)
            .map(|r| r.worktree.to_string_lossy().into_owned())
    });
    let (state, reason) = agent_state_fields(&node.state);
    AgentSummary {
        space: space.id.as_str().to_owned(),
        id: node.id.as_str().to_owned(),
        label: node.label.clone(),
        role: agent_role_name(&node.role),
        repo,
        worktree,
        state: state.to_owned(),
        reason,
        pane: node.pane_id,
        process: terminal.process,
        exit_code: terminal.exit_code,
    }
}

/// What an agent's terminal is doing, for `agent.list`, `agent.read` and `agent.send`.
pub struct AgentTerminal {
    /// Whether the agent's program runs.
    pub process: AgentProcess,
    /// How it exited, when known.
    pub exit_code: Option<i32>,
    /// The terminal running (or, once exited, last showing) the agent's own program. `None` without a pane, when
    /// the program could not start, or when the pane now runs the shell opened after the agent exited.
    pub session: Option<Arc<TerminalSession>>,
}

impl AgentTerminal {
    const NONE: AgentTerminal = AgentTerminal {
        process: AgentProcess::NoPane,
        exit_code: None,
        session: None,
    };

    const GONE: AgentTerminal = AgentTerminal {
        process: AgentProcess::Exited,
        exit_code: None,
        session: None,
    };
}

/// What `graph.add_lane` answers: the new lane, and the pane started for it when its space is open.
#[derive(Serialize)]
struct LaneAdded {
    #[serde(flatten)]
    lane: AddedLane,
    /// The pane the lane's agent was started in; `None` when the space is not open or it could not start.
    pane: Option<u64>,
    /// Why the pane could not start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pane_error: Option<String>,
    /// Why no pane was started although nothing failed: the space is not open.
    #[serde(skip_serializing_if = "Option::is_none")]
    pane_note: Option<String>,
}

fn to_json<T: Serialize>(result: Result<T, RpcError>) -> Result<Value, RpcError> {
    result.and_then(|v| {
        serde_json::to_value(v).map_err(|e| RpcError::internal(format!("could not encode: {e}")))
    })
}

/// One repo worktree of a space to check with mise.
#[derive(Clone, Debug)]
struct MiseCheck {
    space: SpaceId,
    /// The `MISE_ENV` the space's panes run with ([`Space::effective_mise_env`]).
    mise_env: String,
    repo: String,
    dir: PathBuf,
}

/// The worktrees of `spaces` that exist, to check with mise under the `MISE_ENV` each space's panes run with.
fn mise_checks(spaces: &[Space]) -> Vec<MiseCheck> {
    spaces
        .iter()
        .flat_map(|space| {
            space.repos.iter().map(|repo| MiseCheck {
                space: space.id.clone(),
                mise_env: space.effective_mise_env(),
                repo: repo.name.clone(),
                dir: repo.worktree.clone(),
            })
        })
        .filter(|c| c.dir.is_dir())
        .collect()
}

/// What checking a space's worktrees with mise found.
struct MiseFindings {
    checks: Vec<(MiseCheck, xctx_core::Result<EnvReport>)>,
}

// -------------------------------------------------------------------------------------------------------------
// The workspace side

impl Workspace {
    /// Open the Inbox in the data directory; a fresh one starts with the welcome notice.
    pub(crate) fn open_inbox() -> (Option<Inbox>, Option<String>) {
        match Inbox::open_default() {
            Ok(mut inbox) => {
                if inbox.is_fresh()
                    && let Err(e) =
                        inbox.post(ItemBody::Notice(welcome_notice()), None, Source::App)
                {
                    return (Some(inbox), Some(format!("could not write the Inbox: {e}")));
                }
                (Some(inbox), None)
            }
            Err(e) => (None, Some(format!("could not open the Inbox: {e}"))),
        }
    }

    /// Bind the control socket and start answering it; panes started from now on get `XCTX_CONTROL_SOCKET`
    /// and `xctx-ctl`. When the socket cannot be bound (another xctx has it, say) the app runs without one and
    /// says why.
    pub(crate) fn start_control(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = match socket_path() {
            Ok(path) => path,
            Err(e) => {
                self.info(format!("agents cannot drive this window: {e}"), cx);
                return;
            }
        };
        let (tx, mut rx) = mpsc::unbounded::<Incoming>();
        let server = Server::bind(&path, ServerConfig::default(), move |incoming| {
            if let Err(e) = tx.unbounded_send(incoming) {
                e.into_inner()
                    .respond(Err(RpcError::internal("xctx is shutting down")));
            }
        });
        let server = match server {
            Ok(server) => server,
            Err(e) => {
                self.info(format!("agents cannot drive this window: {e}"), cx);
                return;
            }
        };
        let task = cx.spawn_in(window, async move |this, cx| {
            while let Some(incoming) = rx.next().await {
                let handled = this.update_in(cx, |this, window, cx| {
                    this.handle_control(incoming, window, cx)
                });
                if handled.is_err() {
                    break;
                }
            }
        });
        let ctl_dir = std::env::current_exe()
            .ok()
            .and_then(|exe| ctl_dir_next_to(&exe));
        self.search.control = Some(ControlEnv {
            socket: path,
            ctl_dir,
        });
        self.control = Some(Control {
            _server: server,
            _task: task,
        });
    }

    /// Answer one control request.
    pub(crate) fn handle_control(
        &mut self,
        incoming: Incoming,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (call, responder) = incoming.into_parts();
        if responder.is_cancelled() {
            return;
        }
        let result = match call {
            Call::Ping => to_json(Ok(PingResult::current())),
            Call::SpaceList => to_json(Ok(space_list(&self.spaces))),
            Call::LayoutGet(p) => to_json(self.control_layout(&p.space)),
            Call::SpaceReshape(p) => {
                to_json(
                    self.in_open_space(&p.space, window, cx, |this, id, window, cx| {
                        this.reshape_in(id, p.shape, window, cx)?;
                        this.open_layout(id)
                    }),
                )
            }
            // A plugin pane needs a plugin; only plugin.mount starts one (Milestone 5, Wave 3).
            Call::PaneSplit(p) if p.kind == Some(PaneKind::Plugin) => {
                Err(RpcError::invalid_params(
                    "pane.split: a plugin pane is started with plugin.mount, not pane.split",
                ))
            }
            Call::PaneSplit(p) => {
                to_json(
                    self.in_open_space(&p.space, window, cx, |this, id, window, cx| {
                        let kind = p.kind.unwrap_or(PaneKind::Shell);
                        let pane = this.split_in(id, p.pane, p.axis, kind, p.repo, window, cx)?;
                        Ok(PaneSplitResult { pane })
                    }),
                )
            }
            Call::PaneClose(p) => to_json(self.in_open_space(
                &p.space,
                window,
                cx,
                |this, id, window, cx| this.close_pane_in(id, p.pane, window, cx).map(|()| Ack {}),
            )),
            Call::PaneFocus(p) => to_json(self.in_open_space(
                &p.space,
                window,
                cx,
                |this, id, window, cx| this.focus_pane_in(id, p.pane, window, cx).map(|()| Ack {}),
            )),
            Call::PaneZoom(p) => {
                to_json(
                    self.in_open_space(&p.space, window, cx, |this, id, window, cx| {
                        let zoomed = this.zoom_in(id, p.pane, window, cx)?;
                        Ok(PaneZoomResult { zoomed })
                    }),
                )
            }
            Call::StatusSet(p) => {
                if p.message.trim().is_empty() {
                    Err(RpcError::invalid_params("status.set: message is empty"))
                } else {
                    self.set_agent_banner(status_banner(&p), cx);
                    to_json(Ok(Ack {}))
                }
            }
            call @ (Call::InboxList(_)
            | Call::PluginCall(_)
            | Call::ChannelList(_)
            | Call::ContractGet(_)
            | Call::ContractSet(_)
            | Call::ContractAgree(_)
            | Call::AttemptList(_)
            | Call::AttemptDiff(_)
            | Call::AgentRead(_)) => {
                if !self.mise_ready {
                    self.pending_masked_reads
                        .push(PendingMaskedRead { call, responder });
                    return;
                }
                self.answer_masked_read(call, responder, cx);
                return;
            }
            Call::InboxPost(p) => to_json(self.control_inbox_post(&p, cx)),
            Call::SpaceDraft(params) => {
                if self.scanning {
                    self.pending_control_drafts
                        .push(PendingDraft { params, responder });
                    return;
                }
                to_json(self.control_draft(&params, cx))
            }
            Call::GraphGet(p) => {
                self.graph_call(&p.space, cx, |state, _, _| Ok(state.doc().clone()))
            }
            Call::GraphPause(p) => self.graph_call(&p.space, cx, |state, space, _| {
                refuse_attempt_agent("graph.pause", &p.agent)?;
                set_agent_paused(state, &space.id, &p.agent, true)
            }),
            Call::GraphResume(p) => self.graph_call(&p.space, cx, |state, space, _| {
                refuse_attempt_agent("graph.resume", &p.agent)?;
                set_agent_paused(state, &space.id, &p.agent, false)
            }),
            Call::GraphAddLane(p) => {
                let added = self.with_space_graph(&p.space, |state, space, _| {
                    add_lane(state, space, &p).map(|lane| (space.id.clone(), lane))
                });
                let result = added.map(|(id, lane)| self.start_added_lane(&id, lane, window, cx));
                cx.notify();
                to_json(result)
            }
            Call::GraphRemoveLane(p) => self.graph_call(&p.space, cx, |state, space, _| {
                refuse_attempt_agent("graph.remove_lane", &p.agent)?;
                remove_lane(state, &space.id, &p.agent).map(|_| Ack {})
            }),
            Call::ChannelPost(p) => self.graph_call(&p.space, cx, |state, space, _| {
                channel_post(state, &space.id, &p)
            }),
            Call::LoopStatus(p) => {
                self.graph_call(&p.space, cx, |state, _, _| Ok(state.loop_run().clone()))
            }
            Call::LoopAdvance(p) => {
                let advanced = self.with_space_graph(&p.space, |state, space, _| {
                    loop_advance(state, &space.id, p.checks_passed).map(|r| (space.id.clone(), r))
                });
                if let Ok((id, result)) = &advanced
                    && let LoopDecision::NeedsApproval(stage) = result.decision
                {
                    self.post_loop_approval(id, stage, cx);
                }
                cx.notify();
                to_json(advanced.map(|(_, result)| result))
            }
            Call::LoopSetAutonomy(p) => {
                self.graph_call(&p.space, cx, |state, _, _| loop_set_autonomy(state, &p))
            }
            Call::SpaceWidenPropose(p) => to_json(self.control_widen_propose(&p, cx)),
            Call::AttemptReport(p) => to_json(self.control_attempt_report(&p, cx)),
            Call::AttemptPropose(p) => to_json(self.control_attempt_propose(&p, cx)),
            Call::PluginList(p) => to_json(self.control_plugin_list(p.space.as_deref())),
            Call::PluginTools(p) => {
                to_json(self.control_plugin_tools(&p.plugin, p.space.as_deref()))
            }
            Call::PluginMount(p) => to_json(self.in_open_space(
                &p.space,
                window,
                cx,
                |this, id, window, cx| {
                    let settings = this.agent_mount_settings(id, &p.plugin, p.repo.as_deref());
                    match this.mount_plugin(id, &p.plugin, p.repo, settings, true, window, cx)? {
                        MountOutcome::Mounted(pane) => Ok(PluginMountResult {
                            pane: Some(pane),
                            inbox_item: None,
                        }),
                        MountOutcome::Gated(item) => Ok(PluginMountResult {
                            pane: None,
                            inbox_item: Some(item.as_str().to_owned()),
                        }),
                    }
                },
            )),
            Call::PluginUnmount(p) => {
                to_json(
                    self.in_open_space(&p.space, window, cx, |this, id, window, cx| {
                        let target = match (p.plugin, p.pane) {
                            (Some(plugin), None) => UnmountTarget::Plugin(plugin),
                            (None, Some(pane)) => UnmountTarget::Pane(pane),
                            _ => {
                                return Err(RpcError::invalid_params(
                                    "plugin.unmount: name exactly one of plugin= and pane=",
                                ));
                            }
                        };
                        let closed = this.unmount_plugin(id, target, window, cx)?;
                        Ok(PluginUnmountResult { closed })
                    }),
                )
            }
            Call::PluginEmit(p) => to_json(self.control_plugin_emit(&p, cx)),
            Call::AgentList(p) => to_json(self.control_agent_list(p.space.as_deref(), cx)),
            Call::AgentSend(p) => to_json(self.control_agent_send(&p, cx)),
            Call::AgentStart(p) => {
                let started = self.control_agent_start(&p.space, &p.agent, window, cx);
                cx.notify();
                to_json(started)
            }
            Call::SpaceOpen(p) => to_json(self.control_space_open(&p.space, window, cx)),
        };
        responder.respond(result);
    }

    /// Answer a request that shows masked stored text (see [`PendingMaskedRead`]). `attempt.diff` runs git, so it
    /// is answered from the background once git is done.
    fn answer_masked_read(&mut self, call: Call, responder: Responder, cx: &mut Context<Self>) {
        if let Call::AttemptDiff(p) = call {
            self.control_attempt_diff(p, responder, cx);
            return;
        }
        if let Call::PluginCall(p) = call {
            self.control_plugin_call(p, responder, cx);
            return;
        }
        let result = self.masked_read(call, cx);
        responder.respond(result);
    }

    fn masked_read(&mut self, call: Call, cx: &mut Context<Self>) -> Result<Value, RpcError> {
        match call {
            Call::AttemptList(p) => to_json(self.control_attempt_list(&p)),
            Call::InboxList(params) => to_json(self.control_inbox_list(&params)),
            Call::ChannelList(p) => self.graph_call(&p.space, cx, |state, _, redactor| {
                channel_list(state, &p, redactor)
            }),
            Call::ContractGet(p) => self.graph_call(&p.space, cx, |state, _, redactor| {
                Ok(contract_get(state, redactor))
            }),
            Call::ContractSet(p) => self.graph_call(&p.space, cx, |state, space, redactor| {
                refuse_attempt_agent("contract.set", &p.author)?;
                contract_set(state, &space.id, &p, redactor)
            }),
            Call::ContractAgree(p) => self.graph_call(&p.space, cx, |state, space, redactor| {
                refuse_attempt_agent("contract.agree", &p.agent)?;
                contract_agree(state, &space.id, &p.agent, p.version, redactor)
            }),
            Call::AgentRead(p) => to_json(self.control_agent_read(&p, cx)),
            other => Err(RpcError::internal(format!(
                "{}: not a masked read",
                other.method()
            ))),
        }
    }

    /// Run `op` on the graph state of the active space `name` (loading it on first use), redraw, and encode its
    /// result.
    fn graph_call<T: Serialize>(
        &mut self,
        name: &str,
        cx: &mut Context<Self>,
        op: impl FnOnce(&mut SpaceGraphState, &Space, &Redactor) -> Result<T, RpcError>,
    ) -> Result<Value, RpcError> {
        let result = self.with_space_graph(name, op);
        cx.notify();
        to_json(result)
    }

    /// Run `op` on the graph state of the active space `name`, loading it on first use. Not found for a space
    /// that is not active, a conflict while it is shipped or discarded.
    pub(crate) fn with_space_graph<T>(
        &mut self,
        name: &str,
        op: impl FnOnce(&mut SpaceGraphState, &Space, &Redactor) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        let id = resolve_space(&self.spaces, |id| self.in_flight_spaces.contains(id), name)?;
        let space = self
            .tabs
            .get(&id)
            .map(|tab| &tab.space)
            .or_else(|| self.spaces.iter().find(|s| s.id == id))
            .ok_or_else(|| RpcError::not_found(format!("no active space named {id}")))?;
        let state = self.graphs.load(space).map_err(RpcError::internal)?;
        op(state, space, &self.redactor)
    }

    /// `graph.add_lane`'s answer for `lane`, just added to space `id`: start its pane when the space is open. A
    /// pane that fails to start leaves the lane in the graph and says why.
    fn start_added_lane(
        &mut self,
        id: &SpaceId,
        lane: AddedLane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> LaneAdded {
        if !self.tabs.contains_key(id) {
            return LaneAdded {
                lane,
                pane: None,
                pane_error: None,
                pane_note: Some(format!(
                    "space {id} is not open, so no pane was started for the lane"
                )),
            };
        }
        let agent = AgentNodeId::new(lane.id.clone());
        match self.start_agent_pane(id, &agent, window, cx) {
            Ok(pane) => LaneAdded {
                lane,
                pane: Some(pane.0),
                pane_error: None,
                pane_note: None,
            },
            Err(e) => LaneAdded {
                lane,
                pane: None,
                pane_error: Some(e.message),
                pane_note: None,
            },
        }
    }

    /// What the terminal of the agent bound to `pane` in the open space `id` is doing.
    fn agent_terminal(&self, id: &SpaceId, pane: Option<u64>, cx: &App) -> AgentTerminal {
        let Some(terminal) = pane.and_then(|pane| {
            self.tabs
                .get(id)
                .and_then(|tab| tab.terminals.get(&PaneId(pane)))
        }) else {
            return AgentTerminal::NONE;
        };
        match terminal {
            PaneTerminal::Failed(_) => AgentTerminal::GONE,
            PaneTerminal::Running {
                after_agent: true, ..
            } => AgentTerminal::GONE,
            PaneTerminal::Running { view, exited, .. } => {
                let session = view.read(cx).session().clone();
                let (process, exit_code) = match exited {
                    Some(code) => (AgentProcess::Exited, *code),
                    None if session.is_alive() => (AgentProcess::Running, None),
                    None => (AgentProcess::Exited, None),
                };
                AgentTerminal {
                    process,
                    exit_code,
                    session: Some(session),
                }
            }
        }
    }

    /// `agent.list`: the agents of every active space (or of the active space `name`), with what their terminals
    /// do. A space that is not open lists no agents and is not opened.
    fn control_agent_list(
        &mut self,
        name: Option<&str>,
        cx: &App,
    ) -> Result<AgentListResult, RpcError> {
        let only = name
            .map(|name| resolve_space(&self.spaces, |id| self.in_flight_spaces.contains(id), name))
            .transpose()?;
        let mut spaces = Vec::new();
        for listed in &self.spaces {
            if only.as_ref().is_some_and(|only| only != &listed.id) {
                continue;
            }
            let Some(tab) = self.tabs.get(&listed.id) else {
                spaces.push(AgentSpaceSummary {
                    space: listed.id.as_str().to_owned(),
                    open: false,
                    agents: Vec::new(),
                });
                continue;
            };
            let state = self.graphs.load(&tab.space).map_err(RpcError::internal)?;
            let nodes = state.graph().nodes().to_vec();
            let agents = nodes
                .iter()
                .map(|node| {
                    let terminal = self.agent_terminal(&listed.id, node.pane_id, cx);
                    agent_summary(&tab.space, node, &terminal)
                })
                .collect();
            spaces.push(AgentSpaceSummary {
                space: listed.id.as_str().to_owned(),
                open: true,
                agents,
            });
        }
        Ok(AgentListResult { spaces })
    }

    /// The agent `agent` of the open space `space`: its space, graph node and terminal. Not found for an unknown
    /// space or agent, a conflict for a space that is not open.
    fn open_agent(
        &mut self,
        space: &str,
        agent: &str,
        cx: &App,
    ) -> Result<(SpaceId, AgentNode, AgentTerminal), RpcError> {
        let id = resolve_open_space(
            &self.spaces,
            |id| self.in_flight_spaces.contains(id),
            |id| self.tabs.contains_key(id),
            space,
        )?;
        let tab = self
            .tabs
            .get(&id)
            .ok_or_else(|| RpcError::conflict(format!("space {id} is not open; open it first")))?;
        let state = self.graphs.load(&tab.space).map_err(RpcError::internal)?;
        let node = state
            .graph()
            .node(&AgentNodeId::new(agent))
            .cloned()
            .ok_or_else(|| {
                RpcError::not_found(format!(
                    "space {id} has no agent {agent}; agent.list space={id} shows its agents"
                ))
            })?;
        let terminal = self.agent_terminal(&id, node.pane_id, cx);
        Ok((id, node, terminal))
    }

    /// `agent.read`: the last lines the agent's terminal shows, masked.
    fn control_agent_read(
        &mut self,
        p: &AgentReadParams,
        cx: &App,
    ) -> Result<AgentReadResult, RpcError> {
        let (id, node, terminal) = self.open_agent(&p.space, &p.agent, cx)?;
        let Some(session) = terminal.session else {
            return Err(no_terminal("agent.read", &id, &node, terminal.process));
        };
        let lines = usize::try_from(p.line_count()).unwrap_or(usize::MAX);
        let text = last_screen_lines(&session.snapshot().text(), lines);
        Ok(AgentReadResult {
            space: id.as_str().to_owned(),
            agent: node.id.as_str().to_owned(),
            process: terminal.process,
            exit_code: terminal.exit_code,
            text: self.redactor.redact(&text).into_owned(),
        })
    }

    /// `agent.send`: type the message into the agent's running terminal as one line, press Enter, and say in the
    /// window who got it.
    fn control_agent_send(
        &mut self,
        p: &AgentSendParams,
        cx: &mut Context<Self>,
    ) -> Result<AgentSendResult, RpcError> {
        if p.text.trim().is_empty() {
            return Err(RpcError::invalid_params("agent.send: text is empty"));
        }
        let (id, node, terminal) = self.open_agent(&p.space, &p.agent, cx)?;
        let session = match (terminal.process, terminal.session) {
            (AgentProcess::Running, Some(session)) => session,
            (process, _) => return Err(no_terminal("agent.send", &id, &node, process)),
        };
        let typed = typed_task(&p.text);
        let line = typed.strip_suffix('\r').unwrap_or(&typed);
        let write_failed = |e: xctx_term::TermError| {
            RpcError::internal(format!("agent.send: could not type to {}: {e}", node.id))
        };
        session.write(line.as_bytes()).map_err(write_failed)?;
        session.write(b"\r").map_err(write_failed)?;
        let mut preview: String = line.chars().take(AGENT_SEND_PREVIEW_CHARS).collect();
        if line.chars().count() > AGENT_SEND_PREVIEW_CHARS {
            preview.push('…');
        }
        let preview = self.redactor.redact(&preview).into_owned();
        self.info(format!("message to {} in {id}: {preview}", node.label), cx);
        Ok(AgentSendResult { sent: true })
    }

    /// `space.open`: open the active space `name` (starting its panes, as clicking it on Home does) and show it.
    fn control_space_open(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<LayoutInfo, RpcError> {
        let id = resolve_space(&self.spaces, |id| self.in_flight_spaces.contains(id), name)?;
        self.show_space(&id, window, cx);
        if !self.tabs.contains_key(&id) {
            return Err(RpcError::conflict(format!(
                "space.open: space {id} could not be opened; the window says why"
            )));
        }
        self.open_layout(&id)
    }

    /// `agent.start`: make the agent `agent` of open space `space` run. A running agent is left alone; one whose
    /// pane's agent exited restarts in that pane; one without a pane (or whose pane was closed) gets a new pane
    /// with its brief.
    fn control_agent_start(
        &mut self,
        space: &str,
        agent: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<AgentStartResult, RpcError> {
        refuse_attempt_agent("agent.start", agent)?;
        let (id, node, terminal) = self.open_agent(space, agent, cx)?;
        if let Some(pane) = node.pane_id {
            if terminal.process == AgentProcess::Running {
                return Ok(AgentStartResult {
                    pane: PaneId(pane),
                    started: false,
                });
            }
            let role_pane = self
                .tabs
                .get(&id)
                .and_then(|tab| tab.layout.panes().into_iter().find(|p| p.id.0 == pane));
            if let Some(role_pane) = role_pane {
                self.restart_agent_in(&id, &role_pane, window, cx);
                return Ok(AgentStartResult {
                    pane: role_pane.id,
                    started: true,
                });
            }
            // Its pane was closed: unbind it so it gets a new one.
            let space = self
                .tabs
                .get(&id)
                .map(|tab| tab.space.clone())
                .ok_or_else(|| RpcError::conflict(format!("space {id} is not open")))?;
            let state = self.graphs.load(&space).map_err(RpcError::internal)?;
            state.change(|doc| {
                doc.graph
                    .bind_pane(&node.id, None)
                    .map_err(|e| graph_error(&id, e))
            })?;
        }
        let pane = self.start_agent_pane(&id, &node.id, window, cx)?;
        Ok(AgentStartResult {
            pane,
            started: true,
        })
    }

    /// The loop of space `id` waits for the user to approve moving to `stage`: say so in the Inbox and the status
    /// bar.
    pub(crate) fn post_loop_approval(
        &mut self,
        id: &SpaceId,
        stage: LoopStage,
        cx: &mut Context<Self>,
    ) {
        let notice = loop_approval_notice(id, stage);
        let title = notice.title.clone();
        if let Some(inbox) = self.inbox.as_mut()
            && let Err(e) = inbox.post(ItemBody::Notice(notice), Some(id.clone()), Source::App)
        {
            self.error(format!("could not post to the Inbox: {e}"), cx);
            return;
        }
        self.info(title, cx);
    }

    /// `space.widen_propose`: post the widening to the Inbox for the user to approve. Never widens.
    fn control_widen_propose(
        &mut self,
        params: &SpaceWidenProposeParams,
        cx: &mut Context<Self>,
    ) -> Result<SpaceWidenProposeResult, RpcError> {
        if self.scanning && params.repos.as_ref().is_some_and(|r| !r.is_empty()) {
            return Err(RpcError::conflict(
                "the repo index is still being built; try again in a moment",
            ));
        }
        let widen = widen_proposal(
            &self.spaces,
            |id| self.in_flight_spaces.contains(id),
            &self.index,
            params,
            home_dir().as_deref(),
        )?;
        let id = widen.space.clone();
        let Some(inbox) = self.inbox.as_mut() else {
            return Err(self.inbox_unavailable());
        };
        let item = inbox
            .post(ItemBody::SpaceWiden(widen), Some(id.clone()), Source::Agent)
            .map_err(|e| RpcError::internal(format!("could not post to the Inbox: {e}")))?;
        self.info(
            format!("A request to widen {id} is waiting in the Inbox for your approval"),
            cx,
        );
        cx.notify();
        Ok(SpaceWidenProposeResult {
            inbox_item: item.as_str().to_owned(),
        })
    }

    /// `attempt.list`: the space's attempts, masked.
    fn control_attempt_list(
        &mut self,
        params: &AttemptListParams,
    ) -> Result<AttemptListResult, RpcError> {
        let id = resolve_space(
            &self.spaces,
            |id| self.in_flight_spaces.contains(id),
            &params.space,
        )?;
        let set = self.attempts.load(&id).map_err(RpcError::internal)?;
        Ok(attempt_list(set, params.round, &self.redactor))
    }

    /// `attempt.report`: record what an attempt's agent reports. Once every attempt of its round is done, the
    /// Inbox and the status bar say the round is ready for review.
    fn control_attempt_report(
        &mut self,
        params: &AttemptReportParams,
        cx: &mut Context<Self>,
    ) -> Result<Ack, RpcError> {
        let id = resolve_space(
            &self.spaces,
            |id| self.in_flight_spaces.contains(id),
            &params.space,
        )?;
        if params.summary.trim().is_empty() {
            return Err(RpcError::invalid_params("attempt.report: summary is empty"));
        }
        let attempt = self
            .attempts
            .load(&id)
            .map_err(RpcError::internal)?
            .get(params.attempt)
            .ok_or_else(|| {
                RpcError::not_found(format!("{id} has no attempt {}", params.attempt))
            })?;
        check_report_token(&id, attempt, params.token.as_deref())?;
        let at = attempts::now();
        let reported = self
            .attempts
            .try_change(&id, |set| record_report(set, params, at))
            .map_err(RpcError::internal)?
            .map_err(|e| attempt_rpc_error(&id, e))?;
        if reported.round_finished && !reported.was_finished {
            let notice = round_done_notice(&id, reported.round);
            let title = notice.title.clone();
            if let Some(inbox) = self.inbox.as_mut()
                && let Err(e) = inbox.post(ItemBody::Notice(notice), Some(id.clone()), Source::App)
            {
                self.error(format!("could not post to the Inbox: {e}"), cx);
            } else {
                // Offer the review sheet where the user is looking.
                self.info(format!("{title}: press ⌘⇧R in the space"), cx);
            }
        }
        cx.notify();
        Ok(Ack {})
    }

    /// `attempt.propose`: post a proposal to fork attempts of a writer to the Inbox, for the user to approve.
    /// Never forks.
    fn control_attempt_propose(
        &mut self,
        params: &AttemptProposeParams,
        cx: &mut Context<Self>,
    ) -> Result<AttemptProposeResult, RpcError> {
        let id = resolve_space(
            &self.spaces,
            |id| self.in_flight_spaces.contains(id),
            &params.space,
        )?;
        let space = self
            .active_space(&id)
            .ok_or_else(|| RpcError::not_found(format!("no active space named {id}")))?;
        let state = self.graphs.load(&space).map_err(RpcError::internal)?;
        let proposal = attempt_proposal(&id, state.graph(), params)?;
        let repo = state
            .graph()
            .node(&AgentNodeId::new(proposal.agent.clone()))
            .and_then(|n| n.role.repo().map(str::to_owned))
            .unwrap_or_default();
        let worktree = space
            .repos
            .iter()
            .find(|r| r.name == repo)
            .map(|r| r.worktree.clone())
            .ok_or_else(|| RpcError::conflict(format!("space {id} has no repo {repo}")))?;
        // Refuse now what approving would refuse, such as more live attempts than a space may have. Numbers a
        // failed fork left behind are skipped, as the fork itself skips them.
        let set = self.attempts.load(&id).map_err(RpcError::internal)?;
        plan_fork_skipping_leftovers(
            set,
            id.as_str(),
            &proposal.agent,
            &repo,
            &worktree,
            vec![None; proposal.count as usize],
        )
        .map_err(|e| attempt_rpc_error(&id, e))?;
        if let Some(why) = self.fork_refusal(&space, &AgentNodeId::new(proposal.agent.clone())) {
            return Err(RpcError::conflict(format!("{id}: {why}")));
        }
        let title = format!(
            "A request to fork {} of {} in {id} is waiting in the Inbox for your approval",
            if proposal.count == 1 {
                "1 attempt".to_owned()
            } else {
                format!("{} attempts", proposal.count)
            },
            proposal.agent
        );
        let Some(inbox) = self.inbox.as_mut() else {
            return Err(self.inbox_unavailable());
        };
        let item = inbox
            .post(
                ItemBody::AttemptProposal(proposal),
                Some(id.clone()),
                Source::Agent,
            )
            .map_err(|e| RpcError::internal(format!("could not post to the Inbox: {e}")))?;
        self.info(title, cx);
        cx.notify();
        Ok(AttemptProposeResult {
            inbox_item: item.as_str().to_owned(),
        })
    }

    /// `attempt.diff`: the attempt's changed files (and one file's diff), masked, read with git in the background
    /// and answered once it is done.
    fn control_attempt_diff(
        &mut self,
        params: AttemptDiffParams,
        responder: Responder,
        cx: &mut Context<Self>,
    ) {
        let target = resolve_space(
            &self.spaces,
            |id| self.in_flight_spaces.contains(id),
            &params.space,
        )
        .and_then(|id| {
            let set = self.attempts.load(&id).map_err(RpcError::internal)?;
            diff_target(&id, set, &params)
        });
        let worktree = match target {
            Ok(worktree) => worktree,
            Err(e) => {
                responder.respond(Err(e));
                return;
            }
        };
        let redactor = self.redactor.clone();
        let executor = cx.background_executor().clone();
        cx.spawn(async move |_, _| {
            let result = executor
                .spawn(async move {
                    to_json(attempt_diff(&worktree, params.path.as_deref(), &redactor))
                })
                .await;
            responder.respond(result);
        })
        .detach();
    }

    /// `plugin.list`: the plugins `space` can use (every active space's, merged, when `None`), where each comes
    /// from and where it is mounted.
    fn control_plugin_list(&mut self, space: Option<&str>) -> Result<PluginListResult, RpcError> {
        let spaces: Vec<Space> = match space {
            Some(name) => {
                let id =
                    resolve_space(&self.spaces, |id| self.in_flight_spaces.contains(id), name)?;
                self.active_space(&id).into_iter().collect()
            }
            None => self.spaces.clone(),
        };
        let all = self.spaces.clone();
        let mut plugins: Vec<PluginSummary> = Vec::new();
        let mut add = |summary: PluginSummary| {
            if !plugins
                .iter()
                .any(|p| p.name == summary.name && p.repo == summary.repo)
            {
                plugins.push(summary);
            }
        };
        if spaces.is_empty() {
            for plugin in self.plugins.base_registry().plugins() {
                let mounted = self.plugins.mounted_in(&all, plugin.name());
                add(plugin_summary(plugin, None, mounted));
            }
        }
        for space in &spaces {
            let registry = self.plugins.registry(space);
            for plugin in registry.plugins() {
                let mounted = match plugin.source {
                    xctx_plugin::Source::Repo { .. } => self
                        .plugins
                        .mounted_in(std::slice::from_ref(space), plugin.name()),
                    _ => self.plugins.mounted_in(&all, plugin.name()),
                };
                add(plugin_summary(plugin, Some(space), mounted));
            }
        }
        Ok(PluginListResult { plugins })
    }

    /// `plugin.tools`: `plugin`'s tools, with their parameters and effects.
    fn control_plugin_tools(
        &mut self,
        plugin: &str,
        space: Option<&str>,
    ) -> Result<PluginToolsResult, RpcError> {
        let registry = match space {
            Some(name) => {
                let id =
                    resolve_space(&self.spaces, |id| self.in_flight_spaces.contains(id), name)?;
                let space = self
                    .active_space(&id)
                    .ok_or_else(|| RpcError::not_found(format!("no active space named {id}")))?;
                self.plugins.registry(&space)
            }
            None => self.plugins.base_registry(),
        };
        let found = registry.get(plugin).ok_or_else(|| {
            RpcError::not_found(format!(
                "no plugin named {plugin}{}",
                if space.is_none() {
                    "; name the space (space=<slug>) to see its repos' plugins"
                } else {
                    ""
                }
            ))
        })?;
        Ok(tool_specs(&found.manifest))
    }

    /// `plugin.call`: run a read tool and answer with its masked output, or post a write to the Inbox and answer
    /// with its gate. Answered once the call finishes.
    fn control_plugin_call(
        &mut self,
        params: PluginCallParams,
        responder: Responder,
        cx: &mut Context<Self>,
    ) {
        let id = match resolve_space(
            &self.spaces,
            |id| self.in_flight_spaces.contains(id),
            &params.space,
        ) {
            Ok(id) => id,
            Err(e) => {
                responder.respond(Err(e));
                return;
            }
        };
        self.call_plugin_tool(
            &id,
            &params.plugin,
            &params.tool,
            params.args,
            None,
            false,
            cx,
            move |_, result, _| {
                let answer = result.map(|result| match result {
                    ToolCallResult::Output(output) => PluginCallResult {
                        output: Some(output.json),
                        inbox_item: None,
                    },
                    ToolCallResult::Gated(item) => PluginCallResult {
                        output: None,
                        inbox_item: Some(item.as_str().to_owned()),
                    },
                });
                responder.respond(to_json(answer));
            },
        );
    }

    /// `plugin.emit`: an event from a plugin's own tool. The plugin must be mounted in the space and declare the
    /// event; it then fires as the manifest maps it.
    fn control_plugin_emit(
        &mut self,
        params: &PluginEmitParams,
        cx: &mut Context<Self>,
    ) -> Result<PluginEmitResult, RpcError> {
        let id = resolve_space(
            &self.spaces,
            |id| self.in_flight_spaces.contains(id),
            &params.space,
        )?;
        // A closed tab's plugins run nothing: refused, as calls and pane actions are.
        let space = self.open_space_for_plugins(&id)?;
        let registry = self.plugins.registry(&space);
        let plugin = registry.usable(&params.plugin).ok_or_else(|| {
            RpcError::not_found(format!("no usable plugin named {}", params.plugin))
        })?;
        if plugin.manifest.event(&params.event).is_none() {
            let names: Vec<&str> = plugin
                .manifest
                .events
                .iter()
                .map(|e| e.name.as_str())
                .collect();
            return Err(RpcError::invalid_params(format!(
                "{} declares no event {}; its events are {}",
                params.plugin,
                params.event,
                if names.is_empty() {
                    "none".to_owned()
                } else {
                    names.join(", ")
                }
            )));
        }
        self.plugins.mount_of(&id, &params.plugin, None)?;
        let detail = match &params.detail {
            None => String::new(),
            Some(Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
        };
        let report = self.fire_plugin_event(&id, &params.plugin, &params.event, &detail, cx)?;
        Ok(PluginEmitResult {
            action: report.action.map(event_action),
        })
    }

    /// Answer the drafts that waited for the repo index.
    pub(crate) fn flush_control_drafts(&mut self, cx: &mut Context<Self>) {
        for pending in std::mem::take(&mut self.pending_control_drafts) {
            if pending.responder.is_cancelled() {
                continue;
            }
            let result = to_json(self.control_draft(&pending.params, cx));
            pending.responder.respond(result);
        }
    }

    /// Answer the masked reads that waited for the startup mise check.
    fn flush_masked_reads(&mut self, cx: &mut Context<Self>) {
        for pending in std::mem::take(&mut self.pending_masked_reads) {
            if pending.responder.is_cancelled() {
                continue;
            }
            self.answer_masked_read(pending.call, pending.responder, cx);
        }
    }

    /// The startup mise check has finished (or there was nothing to check): the secrets it found are masked, so
    /// masked reads may answer.
    fn startup_mise_checked(&mut self, cx: &mut Context<Self>) {
        self.mise_ready = true;
        self.flush_masked_reads(cx);
    }

    /// `inbox.list`: the Inbox items, with secrets masked.
    fn control_inbox_list(&self, params: &InboxListParams) -> Result<InboxListResult, RpcError> {
        self.with_inbox(|inbox, redactor| Ok(inbox_list(inbox, params, redactor)))
    }

    /// Run `op` on the active space `name`, whose tab must be open (see [`resolve_open_space`]).
    fn in_open_space<T>(
        &mut self,
        name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
        op: impl FnOnce(&mut Self, &SpaceId, &mut Window, &mut Context<Self>) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        let id = resolve_open_space(
            &self.spaces,
            |id| self.in_flight_spaces.contains(id),
            |id| self.tabs.contains_key(id),
            name,
        )?;
        op(self, &id, window, cx)
    }

    /// The layout of an open space.
    fn open_layout(&self, id: &SpaceId) -> Result<LayoutInfo, RpcError> {
        let tab = self
            .tabs
            .get(id)
            .ok_or_else(|| RpcError::conflict(format!("space {id} is not open")))?;
        Ok(layout_info(&tab.space, &tab.layout))
    }

    /// `layout.get`: the open tab's layout, or the saved one for a space not opened yet (without starting it).
    fn control_layout(&self, name: &str) -> Result<LayoutInfo, RpcError> {
        let id = resolve_space(&self.spaces, |id| self.in_flight_spaces.contains(id), name)?;
        if self.tabs.contains_key(&id) {
            return self.open_layout(&id);
        }
        let space = self
            .spaces
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| RpcError::not_found(format!("no active space named {id}")))?;
        let Some(store) = &self.store else {
            let busy = self.busy.as_deref().unwrap_or("using the store");
            return Err(RpcError::conflict(wait_message(busy)));
        };
        let restored = restore_layout(store.load_layout(&id), space)
            .map_err(|e| RpcError::internal(format!("could not lay out {id}: {e}")))?;
        Ok(layout_info(space, &restored.layout))
    }

    fn with_inbox<T>(
        &self,
        f: impl FnOnce(&Inbox, &Redactor) -> Result<T, RpcError>,
    ) -> Result<T, RpcError> {
        match &self.inbox {
            Some(inbox) => f(inbox, &self.redactor),
            None => Err(self.inbox_unavailable()),
        }
    }

    fn inbox_unavailable(&self) -> RpcError {
        RpcError::internal(
            self.inbox_error
                .clone()
                .unwrap_or_else(|| "the Inbox is not available".to_owned()),
        )
    }

    /// `inbox.post`: add an agent's notice and tell the user.
    fn control_inbox_post(
        &mut self,
        params: &InboxPostParams,
        cx: &mut Context<Self>,
    ) -> Result<InboxPostResult, RpcError> {
        let notice = notice_for(params)?;
        let space = match &params.space {
            Some(name) => Some(resolve_space(&self.spaces, |_| false, name)?),
            None => None,
        };
        let title = self.redactor.redact(&notice.title).into_owned();
        let Some(inbox) = self.inbox.as_mut() else {
            return Err(self.inbox_unavailable());
        };
        let id = inbox
            .post(ItemBody::Notice(notice), space, Source::Agent)
            .map_err(|e| RpcError::internal(format!("could not post to the Inbox: {e}")))?;
        self.info(format!("Inbox: {title}"), cx);
        cx.notify();
        Ok(InboxPostResult {
            id: id.as_str().to_owned(),
        })
    }

    /// What `plugin.mount` mounts `plugin` with in `repo` of open space `id` (the space's first repo when `None`):
    /// a `terminal` in a repo with a web app (see [`crate::dev_setup::web_app`]) runs its dev server on the
    /// space's `PORT`, as launch does, since the protocol has no way to name a command; anything else mounts with
    /// the defaults.
    fn agent_mount_settings(
        &self,
        id: &SpaceId,
        plugin: &str,
        repo: Option<&str>,
    ) -> MountSettings {
        if plugin != "terminal" {
            return MountSettings::default();
        }
        let worktree = self.tabs.get(id).and_then(|tab| {
            let repos = &tab.space.repos;
            match repo {
                Some(name) => repos.iter().find(|r| r.name == name),
                None => repos.first(),
            }
            .map(|r| r.worktree.clone())
        });
        MountSettings {
            command: worktree
                .and_then(|dir| crate::dev_setup::web_app(&dir))
                .map(|app| app.command),
            ..MountSettings::default()
        }
    }

    /// `space.draft`: draft a space, post it to the Inbox as a Plan Gate, and show it on Home under the Parent
    /// Agent when no other plan is shown there. Never launches.
    fn control_draft(
        &mut self,
        params: &SpaceDraftParams,
        cx: &mut Context<Self>,
    ) -> Result<SpaceDraftResult, RpcError> {
        let draft = draft_for(&self.index, params, home_dir().as_deref())?;
        let Some(inbox) = self.inbox.as_mut() else {
            return Err(self.inbox_unavailable());
        };
        let rationale = params.rationale.clone().filter(|r| !r.trim().is_empty());
        let body = ItemBody::PlanGate(PlanGate {
            draft: draft.clone(),
            rationale,
        });
        let id = inbox
            .post(body, None, Source::Agent)
            .map_err(|e| RpcError::internal(format!("could not post to the Inbox: {e}")))?;
        let shown = self.offer_plan(id.clone());
        let message = match (shown, draft.repos.is_empty()) {
            (true, true) => format!(
                "\"{}\" is drafted on Home; no repo matched, add one before launching",
                draft.title
            ),
            (true, false) => format!("\"{}\" is drafted on Home; ⌘↵ launches it", draft.title),
            (false, _) => format!(
                "\"{}\" is waiting in the Inbox; launch or set aside the plan on Home first",
                draft.title
            ),
        };
        self.info(message, cx);
        cx.notify();
        Ok(draft_result(id.as_str().to_owned(), &draft))
    }

    /// Check the worktrees of `spaces` with mise in the background, under the `MISE_ENV` each space's panes run
    /// with (`production` once it is widened to production): post the required variables that are not set to the
    /// Inbox (resolving ones that are set now), and learn the secret values to mask.
    pub(crate) fn check_mise(&mut self, spaces: Vec<Space>, cx: &mut Context<Self>) {
        self.run_mise_check(spaces, false, cx);
    }

    /// Check every active space with mise, as [`Workspace::check_mise`] does, once at startup: masked reads
    /// (see [`PendingMaskedRead`]) wait until it has finished, so none is answered before the secrets to mask are known.
    pub(crate) fn check_mise_at_startup(&mut self, cx: &mut Context<Self>) {
        let spaces = self.spaces.clone();
        self.run_mise_check(spaces, true, cx);
    }

    fn run_mise_check(&mut self, spaces: Vec<Space>, startup: bool, cx: &mut Context<Self>) {
        let Some(mise) = self.search.find("mise") else {
            if startup {
                self.startup_mise_checked(cx);
            }
            return;
        };
        let checks = mise_checks(&spaces);
        if checks.is_empty() {
            if startup {
                self.startup_mise_checked(cx);
            }
            return;
        }
        let runner = MiseRunner::new()
            .with_program(mise)
            .with_env("PATH", self.search.pane_path(None));
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this, cx| {
            let findings = executor
                .spawn(async move {
                    MiseFindings {
                        checks: checks
                            .into_iter()
                            .map(|check| {
                                let report =
                                    runner.inspect(&check.dir, Some(check.mise_env.as_str()));
                                (check, report)
                            })
                            .collect(),
                    }
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.apply_mise_findings(findings, cx);
                if startup {
                    this.startup_mise_checked(cx);
                }
            });
        })
        .detach();
    }

    fn apply_mise_findings(&mut self, findings: MiseFindings, cx: &mut Context<Self>) {
        let mut missing = 0;
        for (check, report) in findings.checks {
            let report = match report {
                Ok(report) => report,
                // An untrusted or broken config is mise's to report in the pane; it is not an Inbox item.
                Err(_) => continue,
            };
            self.redactor.extend(report.secrets.clone());
            self.plugins.learn_secrets(&check.space, &report.secrets);
            // The panes' text is masked with what mise found.
            self.plugin_panes.changed();
            missing += report.missing.len();
            if let Some(inbox) = self.inbox.as_mut()
                && let Err(e) = inbox.sync_missing(
                    &check.repo,
                    &check.dir,
                    Some(check.mise_env.as_str()),
                    Some(check.space.clone()),
                    &report,
                )
            {
                self.error(format!("could not update the Inbox: {e}"), cx);
            }
        }
        if missing > 0 {
            let vars = if missing == 1 {
                "variable"
            } else {
                "variables"
            };
            self.info(
                format!("{missing} required mise {vars} not set; see the Inbox"),
                cx,
            );
        }
        cx.notify();
    }
}

/// The conflict `method` answers for an agent whose terminal does not run its program.
fn no_terminal(method: &str, id: &SpaceId, node: &AgentNode, process: AgentProcess) -> RpcError {
    let why = match process {
        AgentProcess::NoPane => "has no pane",
        AgentProcess::Exited => "has exited",
        AgentProcess::Running => "has no terminal",
    };
    RpcError::conflict(format!(
        "{method}: agent {} of space {id} {why}, so it has no running terminal",
        node.id
    ))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use xctx_core::{SpaceRepo, SpaceShape, SpaceStatus};
    use xctx_inbox::ItemKind;
    use xctx_layout::Shape;
    use xctx_ui::BannerKind;

    use super::*;
    use xctx_control::ErrorCode;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn space(slug: &str) -> Space {
        Space {
            id: SpaceId::new(slug),
            title: slug.replace('-', " "),
            task: "a task".into(),
            shape: SpaceShape::Debug,
            status: SpaceStatus::Active,
            port: 4003,
            created_at: 0,
            repos: vec![SpaceRepo {
                name: "api".into(),
                repo: "/code/api".into(),
                worktree: format!("/code/api-worktrees/{slug}").into(),
                branch: format!("space/{slug}"),
                base_branch: "main".into(),
                base_commit: "abc".into(),
                branch_created: true,
                reason: "name matches".into(),
            }],
            production: false,
        }
    }

    #[test]
    fn space_list_reports_shapes_ports_and_worktrees() {
        let list = space_list(&[space("rate-limit")]);
        let s = &list.spaces[0];
        assert_eq!(
            (s.id.as_str(), s.port, s.shape),
            ("rate-limit", 4003, Shape::Debug)
        );
        assert_eq!(s.repos[0].branch, "space/rate-limit");
        assert_eq!(s.repos[0].worktree, "/code/api-worktrees/rate-limit");
    }

    #[test]
    fn spaces_resolve_by_slug_and_errors_list_the_active_ones() {
        let spaces = [space("a"), space("b")];
        assert_eq!(
            resolve_space(&spaces, |_| false, "b").ok(),
            Some(SpaceId::new("b"))
        );
        let missing = resolve_space(&spaces, |_| false, "c").err();
        assert!(
            matches!(&missing, Some(e) if e.kind() == Some(ErrorCode::NotFound) && e.message.ends_with("a, b"))
        );
        let busy = resolve_space(&spaces, |id| id.as_str() == "a", "a").err();
        assert!(busy.is_some_and(|e| e.kind() == Some(ErrorCode::Conflict)));
    }

    #[test]
    fn layout_requests_on_a_closed_space_are_refused_without_opening_it() {
        let spaces = [space("a"), space("b")];
        let open = |id: &SpaceId| id.as_str() == "a";
        assert_eq!(
            resolve_open_space(&spaces, |_| false, open, "a").ok(),
            Some(SpaceId::new("a"))
        );
        let closed = resolve_open_space(&spaces, |_| false, open, "b").err();
        assert!(
            matches!(&closed, Some(e) if e.kind() == Some(ErrorCode::Conflict) && e.message == "space b is not open; open it first")
        );
        let missing = resolve_open_space(&spaces, |_| false, open, "c").err();
        assert!(missing.is_some_and(|e| e.kind() == Some(ErrorCode::NotFound)));
    }

    #[test]
    fn named_repos_must_be_indexed_or_real_repos() -> TestResult {
        let root = tempfile::tempdir()?;
        let params = |repos: Option<Vec<String>>| SpaceDraftParams {
            task: "  fix the thing ".into(),
            repos,
            rationale: None,
        };
        let empty = RepoIndex::default();
        let draft = draft_for(&empty, &params(None), None)?;
        assert_eq!(draft.task, "fix the thing");
        assert!(draft.repos.is_empty());

        let err = draft_for(
            &empty,
            &params(Some(vec!["nope".into()])),
            Some(root.path()),
        )
        .err();
        assert!(
            matches!(&err, Some(e) if e.kind() == Some(ErrorCode::InvalidParams) && e.message.contains("indexed repos: none"))
        );

        let blank = SpaceDraftParams {
            task: " ".into(),
            repos: None,
            rationale: None,
        };
        assert!(draft_for(&empty, &blank, None).is_err());
        Ok(())
    }

    #[test]
    fn a_repo_path_can_be_named_directly() -> TestResult {
        let root = tempfile::tempdir()?;
        let repo = root.path().join("payments");
        fs::create_dir_all(&repo)?;
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()?;
        assert!(status.success());
        let params = SpaceDraftParams {
            task: "fix".into(),
            repos: Some(vec![repo.to_string_lossy().into_owned()]),
            rationale: None,
        };
        let draft = draft_for(&RepoIndex::default(), &params, None)?;
        assert_eq!(draft.repos.len(), 1);
        assert_eq!(draft.repos[0].name, "payments");
        assert_eq!(draft.repos[0].reason, NAMED_BY_AGENT);
        Ok(())
    }

    #[test]
    fn widen_proposals_name_new_repos_or_production_and_never_nothing() -> TestResult {
        let root = tempfile::tempdir()?;
        let web = root.path().join("web");
        fs::create_dir_all(&web)?;
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&web)
            .status()?;
        assert!(status.success());
        let spaces = [space("rate-limit")];
        let index = RepoIndex::default();
        let params = |repos: Option<Vec<String>>, production: bool| SpaceWidenProposeParams {
            space: "rate-limit".into(),
            repos,
            production,
            rationale: Some("  the web client sends the header ".into()),
        };
        let web_path = web.to_string_lossy().into_owned();

        let widen = widen_proposal(
            &spaces,
            |_| false,
            &index,
            &params(Some(vec![web_path.clone(), web_path.clone()]), false),
            None,
        )?;
        assert_eq!(widen.space, SpaceId::new("rate-limit"));
        assert_eq!(widen.add_repos.len(), 1);
        assert_eq!(widen.add_repos[0].name, "web");
        assert_eq!(widen.add_repos[0].reason, NAMED_BY_AGENT);
        assert!(!widen.production);
        assert_eq!(widen.reason, "the web client sends the header");

        let production = widen_proposal(&spaces, |_| false, &index, &params(None, true), None)?;
        assert!(production.add_repos.is_empty() && production.production);

        let nothing = widen_proposal(
            &spaces,
            |_| false,
            &index,
            &params(Some(vec![" ".into()]), false),
            None,
        );
        assert_eq!(
            nothing.err().and_then(|e| e.kind()),
            Some(ErrorCode::InvalidParams)
        );
        let unknown = widen_proposal(
            &spaces,
            |_| false,
            &index,
            &params(Some(vec!["no-such-repo".into()]), false),
            None,
        );
        assert_eq!(
            unknown.err().and_then(|e| e.kind()),
            Some(ErrorCode::InvalidParams)
        );
        let in_flight = widen_proposal(&spaces, |_| true, &index, &params(None, true), None);
        assert_eq!(
            in_flight.err().and_then(|e| e.kind()),
            Some(ErrorCode::Conflict)
        );
        let missing = widen_proposal(&[], |_| false, &index, &params(None, true), None);
        assert_eq!(
            missing.err().and_then(|e| e.kind()),
            Some(ErrorCode::NotFound)
        );
        let mut on_production = space("rate-limit");
        on_production.production = true;
        let already = widen_proposal(
            &[on_production],
            |_| false,
            &index,
            &params(None, true),
            None,
        );
        assert_eq!(
            already.err().and_then(|e| e.kind()),
            Some(ErrorCode::Conflict)
        );
        Ok(())
    }

    #[test]
    fn inbox_lists_are_redacted_and_filtered() -> TestResult {
        let dir = tempfile::tempdir()?;
        let mut inbox = Inbox::open(dir.path())?;
        let notice = notice_for(&InboxPostParams {
            title: "token is tok-98765".into(),
            body: Some("  ".into()),
            space: None,
        })?;
        assert_eq!(notice.body, None);
        inbox.post(
            ItemBody::Notice(notice),
            Some(SpaceId::new("a")),
            Source::Agent,
        )?;
        let redactor = Redactor::new([("API_TOKEN".to_owned(), "tok-98765".to_owned())]);

        let all = inbox_list(&inbox, &InboxListParams::default(), &redactor);
        assert_eq!(all.items.len(), 1);
        assert_eq!(all.items[0].title, "token is [REDACTED:API_TOKEN]");
        assert_eq!(all.items[0].kind, ItemKind::Notice.name());
        assert_eq!(all.items[0].state, "pending");
        assert_eq!(all.items[0].body, None);

        let other = InboxListParams {
            space: Some("b".into()),
            all: true,
        };
        assert!(inbox_list(&inbox, &other, &redactor).items.is_empty());
        assert!(
            notice_for(&InboxPostParams {
                title: " ".into(),
                body: None,
                space: None
            })
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn status_messages_keep_warnings_and_are_capped() {
        let banner = status_banner(&StatusSetParams {
            message: "tests failed".into(),
            level: StatusLevel::Warning,
            space: Some("rate-limit".into()),
        });
        assert_eq!(banner.kind, BannerKind::Error);
        assert_eq!(banner.message.as_ref(), "agent in rate-limit: tests failed");
        let plain = status_banner(&StatusSetParams {
            message: " done ".into(),
            level: StatusLevel::Info,
            space: None,
        });
        assert_eq!(plain.message.as_ref(), "agent: done");
        let long = status_banner(&StatusSetParams {
            message: "x".repeat(1000),
            level: StatusLevel::Success,
            space: None,
        });
        assert_eq!(long.kind, BannerKind::Info);
        assert_eq!(long.message.chars().count(), MAX_STATUS_CHARS);
    }

    #[test]
    fn mise_checks_use_the_mise_env_the_panes_run_with() -> TestResult {
        let root = tempfile::tempdir()?;
        let mut plain = space("rate-limit");
        plain.repos[0].worktree = root.path().to_path_buf();
        let mut production = plain.clone();
        production.id = SpaceId::new("prod-bug");
        production.production = true;
        let mut gone = space("gone");
        gone.repos[0].worktree = root.path().join("missing");
        let checks = mise_checks(&[plain, production, gone]);
        let envs: Vec<(&str, &str)> = checks
            .iter()
            .map(|c| (c.space.as_str(), c.mise_env.as_str()))
            .collect();
        assert_eq!(
            envs,
            [
                ("rate-limit", "rate-limit"),
                ("prod-bug", "production,prod-bug"),
            ]
        );
        Ok(())
    }

    #[test]
    fn draft_results_carry_the_plan_gate_and_repos() {
        let mut draft = SpaceDraft::new("add rate limiting");
        draft.add_repo(DraftRepo::new("/code/api", "name matches"));
        let result = draft_result("inbox-4".into(), &draft);
        assert_eq!(result.inbox_item, "inbox-4");
        assert_eq!(result.repos[0].name, "api");
    }

    #[test]
    fn the_last_screen_lines_drop_the_empty_bottom_and_honour_the_count() {
        let screen = "one\ntwo\n\nthree\n   \n\n";
        assert_eq!(last_screen_lines(screen, 40), "one\ntwo\n\nthree");
        assert_eq!(last_screen_lines(screen, 2), "\nthree");
        assert_eq!(last_screen_lines(screen, 1), "three");
        assert_eq!(last_screen_lines(screen, 0), "");
        assert_eq!(last_screen_lines("\n\n  \n", 40), "");
        assert_eq!(last_screen_lines("", 5), "");
        let long: String = (1..=300).map(|n| format!("line {n}\n")).collect();
        let last = last_screen_lines(&long, 200);
        assert_eq!(last.lines().count(), 200);
        assert!(last.starts_with("line 101\n") && last.ends_with("line 300"));
    }

    #[test]
    fn agent_roles_and_states_read_as_graph_get_names_them() {
        use xctx_graph::HelperKind;
        assert_eq!(agent_role_name(&AgentRole::Orchestrator), "orchestrator");
        assert_eq!(
            agent_role_name(&AgentRole::Writer { repo: "api".into() }),
            "writer"
        );
        assert_eq!(
            agent_role_name(&AgentRole::Helper {
                kind: HelperKind::Browser
            }),
            "helper:browser"
        );
        assert_eq!(agent_state_fields(&AgentState::Paused), ("paused", None));
        assert_eq!(
            agent_state_fields(&AgentState::Blocked {
                reason: "contract".into()
            }),
            ("blocked", Some("contract".to_owned()))
        );
    }

    #[test]
    fn an_agent_summary_names_its_folder_and_terminal() {
        let space = space("s");
        let node = AgentNode {
            id: AgentNodeId::new("writer-api"),
            role: AgentRole::Writer { repo: "api".into() },
            label: "api".into(),
            state: AgentState::Running,
            pane_id: Some(4),
            turns_used: 0,
            session: None,
            runner: None,
        };
        let terminal = AgentTerminal {
            process: AgentProcess::Exited,
            exit_code: Some(2),
            session: None,
        };
        let summary = agent_summary(&space, &node, &terminal);
        assert_eq!(summary.space, "s");
        assert_eq!(summary.role, "writer");
        assert_eq!(summary.repo.as_deref(), Some("api"));
        assert_eq!(summary.worktree.as_deref(), Some("/code/api-worktrees/s"));
        assert_eq!(summary.state, "running");
        assert_eq!(summary.pane, Some(4));
        assert_eq!(
            (summary.process, summary.exit_code),
            (AgentProcess::Exited, Some(2))
        );
    }
}
