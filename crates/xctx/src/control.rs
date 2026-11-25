//! The window's side of the control socket: bind it at startup, carry each request onto the UI thread, and answer
//! it from the workspace. Also the Inbox the socket posts to, and the mise checks that fill it.
//!
//! The pure parts (what a request means for the store, the index and the Inbox) are free functions, so they are
//! tested without a window; [`Workspace::handle_control`] applies them to the live workspace.

use std::path::{Path, PathBuf};

use futures::StreamExt;
use futures::channel::mpsc;
use gpui::{Context, Task, Window};
use serde::Serialize;
use serde_json::Value;
use xctx_control::protocol::{
    Ack, AttemptDiffParams, AttemptListParams, AttemptListResult, AttemptProposeParams,
    AttemptProposeResult, AttemptReportParams, DraftRepoSummary, InboxItemSummary, InboxListParams,
    InboxListResult, InboxPostParams, InboxPostResult, LayoutInfo, PaneSplitResult, PaneZoomResult,
    PingResult, RepoSummary, SpaceDraftParams, SpaceDraftResult, SpaceListResult, SpaceSummary,
    SpaceWidenProposeParams, SpaceWidenProposeResult, StatusSetParams,
};
use xctx_control::{
    Call, Incoming, Method, Responder, RpcError, Server, ServerConfig, StatusLevel, socket_path,
};
use xctx_core::{
    DraftRepo, EnvReport, MiseRunner, Redactor, RepoIndex, Space, SpaceDraft, SpaceId,
};
use xctx_graph::{AgentNodeId, LoopDecision, LoopStage};
use xctx_inbox::{
    Filter, Inbox, ItemBody, Level, Notice, PlanGate, Source, SpaceWiden, welcome_notice,
};
use xctx_layout::PaneKind;
use xctx_ui::StatusBanner;

use crate::attempts::{
    self, attempt_diff, attempt_list, attempt_proposal, attempt_rpc_error, check_report_token,
    diff_target, plan_fork_skipping_leftovers, record_report, refuse_attempt_agent,
    round_done_notice,
};
use crate::lifecycle::{home_dir, repo_for_draft};
use crate::persist::restore_layout;
use crate::space_graph::{
    SpaceGraphState, add_lane, channel_list, channel_post, contract_agree, contract_get,
    contract_set, loop_advance, loop_approval_notice, loop_set_autonomy, remove_lane,
    set_agent_paused,
};
use crate::space_layout::{SpaceLayout, layout_shape};
use crate::spawn::{ControlEnv, ctl_dir_next_to};
use crate::workspace::{Workspace, wait_message};

/// The reason given to repos an agent named in `space.draft` or `space.widen_propose`.
pub const NAMED_BY_AGENT: &str = "named by the agent";

/// Why a widening was asked for, when the agent gave no rationale.
pub const WIDEN_DEFAULT_REASON: &str = "requested by an agent";

/// The longest status message shown, in characters; longer ones are cut with an ellipsis.
pub const MAX_STATUS_CHARS: usize = 300;

/// What every `status.set` message starts with, so an agent's message is told apart from xctx's own.
pub const AGENT_BANNER_PREFIX: &str = "agent";

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

/// The refusal every plugin method gets until plugins are wired into the app (Milestone 5, Wave 3).
fn plugins_not_wired(method: Method) -> RpcError {
    RpcError::conflict(format!(
        "{method}: plugins are not wired into this xctx yet (Milestone 5, Wave 3)"
    ))
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
            | Call::ChannelList(_)
            | Call::ContractGet(_)
            | Call::ContractSet(_)
            | Call::ContractAgree(_)
            | Call::AttemptList(_)
            | Call::AttemptDiff(_)) => {
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
                self.graph_call(&p.space, cx, |state, space, _| add_lane(state, space, &p))
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
            // The plugin registry, tool runner and plugin panes come in Milestone 5, Wave 3. Until then every
            // plugin method is refused, so an agent learns plugins are not available rather than guessing.
            call @ (Call::PluginList(_)
            | Call::PluginTools(_)
            | Call::PluginMount(_)
            | Call::PluginUnmount(_)
            | Call::PluginCall(_)
            | Call::PluginEmit(_)) => Err(plugins_not_wired(call.method())),
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

    /// `space.draft`: draft a space and post it to the Inbox as a Plan Gate. Never launches.
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
        let message = if draft.repos.is_empty() {
            format!(
                "\"{}\" is waiting in the Inbox; no repo matched, add one before approving",
                draft.title
            )
        } else {
            format!(
                "\"{}\" is waiting in the Inbox for your approval",
                draft.title
            )
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

    #[test]
    fn plugin_methods_are_refused_until_wave_3() {
        for method in Method::ALL
            .into_iter()
            .filter(|m| m.name().starts_with("plugin."))
        {
            let error = plugins_not_wired(method);
            assert_eq!(error.kind(), Some(ErrorCode::Conflict), "{method}");
            assert!(error.message.starts_with(method.name()), "{method}");
            assert!(error.message.contains("not wired"), "{method}");
        }
    }

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
}
