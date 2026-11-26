//! A space's agent graph and loop (`graph.json`) and its shared space (`shared.json`: contract, channel, findings
//! and notes), loaded when first needed and saved under the data directory; what each M3 control method does to
//! them; and which agent each pane runs.
//!
//! Everything here works without a window, so it is tested directly; the workspace applies it in
//! `flow.rs` and `control.rs`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::Serialize;
use xctx_channel::{ChannelError, ChannelMessage, Contract, ContractDraft, MessageKind};
use xctx_channel::{ScratchDir, SharedSpaceStore};
use xctx_control::RpcError;
use xctx_control::protocol::{
    Ack, ChannelListParams, ChannelPostParams, ChannelPostResult, ContractSetParams,
    GraphAddLaneParams, LaneRole, LoopAutonomy, LoopSetAutonomyParams,
};
use xctx_core::store::DATA_DIR_ENV;
use xctx_core::{Redactor, Space, SpaceId, SpaceRepo};
use xctx_graph::{
    AgentGraph, AgentNode, AgentNodeId, AgentRole, AutonomyLevel, GraphError, HelperKind,
    LoopDecision, LoopError, LoopRun, LoopStage, LoopStatus, SpaceGraphDocument,
};
use xctx_inbox::{Filter, Inbox, ItemBody, ItemId, Level, Notice, Source};
use xctx_layout::{PaneId, PaneKind, RolePane};
use xctx_ui::{DEFAULT_MAX_ITERATIONS, DEFAULT_MAX_TURNS, loop_stage_label};

use crate::space_layout::repo_names;
use crate::spawn::PaneAgent;

/// The message kinds `channel.post` and `channel.list` accept, as the channel names them.
pub const MESSAGE_KIND_NAMES: &str = "proposal, question, finding, status, contract_update";

/// A space's agent graph, loop and shared space, as loaded from (and saved to) its data directory.
#[derive(Debug)]
pub struct SpaceGraphState {
    graph_path: PathBuf,
    doc: SpaceGraphDocument,
    shared: SharedSpaceStore,
}

impl SpaceGraphState {
    /// Load the graph and shared space of `space` from `data_dir`. A space without a `graph.json` starts with
    /// the orchestrator, one writer per repo and a loop that asks before each step; nothing is written until the
    /// first change. A damaged or newer file is an error, so it is never replaced.
    pub fn open(data_dir: &Path, space: &Space) -> Result<SpaceGraphState, String> {
        let slug = space.id.as_str();
        let graph_path = xctx_graph::graph_path(data_dir, slug)
            .map_err(|e| format!("could not open the agent graph of {slug}: {e}"))?;
        let exists = graph_path.try_exists().map_err(|e| {
            format!(
                "could not open the agent graph of {slug}: {}: {e}",
                graph_path.display()
            )
        })?;
        let doc = if exists {
            xctx_graph::load(&graph_path)
                .map_err(|e| format!("could not open the agent graph of {slug}: {e}"))?
        } else {
            SpaceGraphDocument::new(
                AgentGraph::for_space(&repo_names(space)),
                LoopRun::new(AutonomyLevel::AskEachStep),
            )
        };
        let shared = SharedSpaceStore::open_for_space(data_dir, slug)
            .map_err(|e| format!("could not open the shared space of {slug}: {e}"))?;
        Ok(SpaceGraphState {
            graph_path,
            doc,
            shared,
        })
    }

    /// The graph and loop.
    pub fn doc(&self) -> &SpaceGraphDocument {
        &self.doc
    }

    /// The agent graph.
    pub fn graph(&self) -> &AgentGraph {
        &self.doc.graph
    }

    /// The loop.
    pub fn loop_run(&self) -> &LoopRun {
        &self.doc.loop_run
    }

    /// The contract, channel, findings and notes. Read them through a [`Redactor`].
    pub fn shared(&self) -> &SharedSpaceStore {
        &self.shared
    }

    /// The shared space, to change; every change is saved before its method returns.
    pub fn shared_mut(&mut self) -> &mut SharedSpaceStore {
        &mut self.shared
    }

    /// Apply `change` to a copy of the graph and loop and keep it only once `graph.json` is written, so memory
    /// never holds what the file does not. A change that fails leaves both as they were.
    pub fn change<R>(
        &mut self,
        change: impl FnOnce(&mut SpaceGraphDocument) -> Result<R, RpcError>,
    ) -> Result<R, RpcError> {
        let mut doc = self.doc.clone();
        let result = change(&mut doc)?;
        xctx_graph::save(&self.graph_path, &doc).map_err(|e| {
            RpcError::internal(format!("could not save {}: {e}", self.graph_path.display()))
        })?;
        self.doc = doc;
        Ok(result)
    }
}

/// The graph state of every space loaded so far, by space.
#[derive(Debug, Default)]
pub struct SpaceGraphs {
    data_dir: Option<PathBuf>,
    states: HashMap<SpaceId, SpaceGraphState>,
    errors: HashMap<SpaceId, String>,
}

impl SpaceGraphs {
    /// Graphs kept under `data_dir`; with `None` (no data directory could be found) every load fails.
    pub fn new(data_dir: Option<PathBuf>) -> SpaceGraphs {
        SpaceGraphs {
            data_dir,
            states: HashMap::new(),
            errors: HashMap::new(),
        }
    }

    /// The loaded state of `id`, if it has been loaded.
    pub fn get(&self, id: &SpaceId) -> Option<&SpaceGraphState> {
        self.states.get(id)
    }

    /// Why the state of `id` could not be loaded the last time it was tried, if it could not.
    pub fn error(&self, id: &SpaceId) -> Option<&str> {
        self.errors.get(id).map(String::as_str)
    }

    /// The state of `space`, loading it on first use. A failed load is remembered for [`SpaceGraphs::error`] and
    /// tried again next time.
    pub fn load(&mut self, space: &Space) -> Result<&mut SpaceGraphState, String> {
        let id = space.id.clone();
        if !self.states.contains_key(&id) {
            let Some(data_dir) = &self.data_dir else {
                let message = "there is no data directory for agent graphs".to_owned();
                self.errors.insert(id, message.clone());
                return Err(message);
            };
            match SpaceGraphState::open(data_dir, space) {
                Ok(state) => {
                    self.errors.remove(&id);
                    self.states.insert(id.clone(), state);
                }
                Err(e) => {
                    self.errors.insert(id, e.clone());
                    return Err(e);
                }
            }
        }
        self.states
            .get_mut(&id)
            .ok_or_else(|| format!("the agent graph of {id} is not loaded"))
    }

    /// Drop what is held for `id`, once it is shipped or discarded. Its files stay in the data directory.
    pub fn forget(&mut self, id: &SpaceId) {
        self.states.remove(id);
        self.errors.remove(id);
    }
}

/// The scratch directory of the space `slug`: `<XCTX_DATA_DIR>/scratch/<slug>` when `XCTX_DATA_DIR` is set (a
/// development or test instance), so it never touches the real instance's artifacts, else
/// `/tmp/xctx-scratch/<slug>`. A slug that could escape the base is refused.
pub fn scratch_dir_for(slug: &str) -> Result<ScratchDir, ChannelError> {
    scratch_dir_in(std::env::var_os(DATA_DIR_ENV), slug)
}

/// [`scratch_dir_for`] from an explicit value of `XCTX_DATA_DIR`, so it can be decided without reading the
/// process environment. An empty value counts as unset.
pub fn scratch_dir_in(data_dir: Option<OsString>, slug: &str) -> Result<ScratchDir, ChannelError> {
    match data_dir.filter(|d| !d.is_empty()) {
        Some(data_dir) => ScratchDir::in_base(PathBuf::from(data_dir).join("scratch"), slug),
        None => ScratchDir::for_space(slug),
    }
}

/// Remove the scratch directory of the space `slug` (see [`scratch_dir_for`]) once it is shipped or discarded.
/// Returns whether there was anything to remove.
pub fn clean_scratch(slug: &str) -> Result<bool, String> {
    scratch_dir_for(slug)
        .and_then(|scratch| scratch.clean())
        .map_err(|e| format!("could not remove the scratch directory of {slug}: {e}"))
}

// -------------------------------------------------------------------------------------------------------------
// Errors

/// A graph error of space `id` as a control error: unknown agents are not found, bad input is invalid, a damaged
/// or unwritable file is internal, and everything else (a second writer for a repo, removing the orchestrator, a
/// state change that does not apply) conflicts.
pub fn graph_error(id: &SpaceId, error: GraphError) -> RpcError {
    match error {
        GraphError::NotFound(agent) => {
            RpcError::not_found(format!("space {id} has no agent {agent}"))
        }
        GraphError::EmptyRepo | GraphError::InvalidSlug(_) => {
            RpcError::invalid_params(format!("{id}: {error}"))
        }
        GraphError::Io { .. } | GraphError::Json { .. } | GraphError::UnsupportedVersion { .. } => {
            RpcError::internal(format!("{id}: {error}"))
        }
        other => RpcError::conflict(format!("{id}: {other}")),
    }
}

/// A shared-space error of space `id` as a control error: blank fields and bad paths are invalid, a missing
/// contract conflicts, an unknown finding is not found, and store failures are internal.
pub fn channel_error(id: &SpaceId, error: ChannelError) -> RpcError {
    match error {
        ChannelError::EmptyField(_)
        | ChannelError::InvalidSlug(_)
        | ChannelError::InvalidArtifactPath { .. } => {
            RpcError::invalid_params(format!("{id}: {error}"))
        }
        ChannelError::NoContract | ChannelError::StaleContract { .. } => {
            RpcError::conflict(format!("{id}: {error}"))
        }
        ChannelError::FindingNotFound(_) => RpcError::not_found(format!("{id}: {error}")),
        other => RpcError::internal(format!("{id}: {other}")),
    }
}

/// A loop operation that does not apply to the loop's status, as a conflict.
pub fn loop_error(id: &SpaceId, error: LoopError) -> RpcError {
    RpcError::conflict(format!("{id}: {error}"))
}

// -------------------------------------------------------------------------------------------------------------
// Control methods

/// What `graph.add_lane` answers: the new agent's id and the agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AddedLane {
    /// The new agent's id.
    pub id: String,
    /// The new agent.
    pub node: AgentNode,
}

/// What `channel.list` answers: the messages, oldest first, secrets masked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChannelListResult {
    /// The messages.
    pub messages: Vec<ChannelMessage>,
}

/// What `contract.get`, `contract.set` and `contract.agree` answer: the contract, secrets masked, or `None`
/// before anyone set one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ContractResult {
    /// The contract.
    pub contract: Option<Contract>,
}

/// What `loop.advance` answers: what the loop did and where it is now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct LoopAdvanceResult {
    /// What the loop did.
    pub decision: LoopDecision,
    /// The loop afterwards.
    pub loop_run: LoopRun,
}

/// The graph's own helper kind for the protocol's.
pub fn helper_kind(kind: xctx_control::protocol::HelperKind) -> HelperKind {
    match kind {
        xctx_control::protocol::HelperKind::Tests => HelperKind::Tests,
        xctx_control::protocol::HelperKind::Reviewer => HelperKind::Reviewer,
        xctx_control::protocol::HelperKind::Probe => HelperKind::Probe,
        xctx_control::protocol::HelperKind::Browser => HelperKind::Browser,
    }
}

/// The role `graph.add_lane` asks for in `space`: a writer needs `repo`, one of the space's repos, and no
/// `kind`; a helper needs `kind`, and a `repo` it names must be one of the space's (helpers read every repo, so
/// it is not kept).
pub fn lane_role(space: &Space, params: &GraphAddLaneParams) -> Result<AgentRole, RpcError> {
    let check_repo = |repo: &str| -> Result<(), RpcError> {
        if space.repos.iter().any(|r| r.name == repo) {
            return Ok(());
        }
        let names: Vec<&str> = space.repos.iter().map(|r| r.name.as_str()).collect();
        Err(RpcError::invalid_params(format!(
            "space {} has no repo {repo}; its repos are {}",
            space.id,
            names.join(", ")
        )))
    };
    match params.role {
        LaneRole::Writer => {
            if params.kind.is_some() {
                return Err(RpcError::invalid_params(
                    "graph.add_lane: kind is only for helpers",
                ));
            }
            let repo = params
                .repo
                .as_deref()
                .map(str::trim)
                .filter(|r| !r.is_empty())
                .ok_or_else(|| {
                    RpcError::invalid_params("graph.add_lane: a writer needs repo=<name>")
                })?;
            check_repo(repo)?;
            Ok(AgentRole::Writer {
                repo: repo.to_owned(),
            })
        }
        LaneRole::Helper => {
            let kind = params.kind.ok_or_else(|| {
                RpcError::invalid_params(
                    "graph.add_lane: a helper needs kind=tests, reviewer, probe or browser",
                )
            })?;
            if let Some(repo) = params.repo.as_deref().map(str::trim)
                && !repo.is_empty()
            {
                check_repo(repo)?;
            }
            Ok(AgentRole::Helper {
                kind: helper_kind(kind),
            })
        }
    }
}

/// `graph.pause` and `graph.resume`: pause or resume `agent` and save the graph.
pub fn set_agent_paused(
    state: &mut SpaceGraphState,
    id: &SpaceId,
    agent: &str,
    paused: bool,
) -> Result<Ack, RpcError> {
    let agent = AgentNodeId::new(agent.trim());
    state.change(|doc| {
        if paused {
            doc.graph.pause(&agent)
        } else {
            doc.graph.resume(&agent)
        }
        .map_err(|e| graph_error(id, e))
    })?;
    Ok(Ack {})
}

/// Who the channel says paused or resumed an agent from the graph panel.
pub const PANEL_STATUS_AUTHOR: &str = "user";

/// The graph panel's Pause or Resume: pause or resume `agent` as [`set_agent_paused`] does, then tell it on the
/// space's channel with a [`MessageKind::Status`] message addressed to it from [`PANEL_STATUS_AUTHOR`], so an
/// agent polling the channel knows. Returns the message's id.
pub fn set_agent_paused_and_tell(
    state: &mut SpaceGraphState,
    id: &SpaceId,
    agent: &str,
    paused: bool,
) -> Result<u64, RpcError> {
    set_agent_paused(state, id, agent, paused)?;
    let body = if paused {
        "You were paused from the agent graph. Stop work and wait until you are resumed."
    } else {
        "You were resumed from the agent graph. Carry on where you left off."
    };
    state
        .shared_mut()
        .post_message(
            PANEL_STATUS_AUTHOR,
            Some(agent.trim()),
            MessageKind::Status,
            body,
        )
        .map_err(|e| channel_error(id, e))
}

/// `graph.add_lane`: add a writer (at most one per repo) or a helper to the graph of `space` and save it.
pub fn add_lane(
    state: &mut SpaceGraphState,
    space: &Space,
    params: &GraphAddLaneParams,
) -> Result<AddedLane, RpcError> {
    let role = lane_role(space, params)?;
    let label = params
        .label
        .as_deref()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned);
    add_role(state, &space.id, role, label)
}

/// Add an agent in `role` to the graph and save it. Returns the new agent.
pub fn add_role(
    state: &mut SpaceGraphState,
    id: &SpaceId,
    role: AgentRole,
    label: Option<String>,
) -> Result<AddedLane, RpcError> {
    state.change(|doc| {
        let agent = doc
            .graph
            .add_lane(role, label)
            .map_err(|e| graph_error(id, e))?;
        let node =
            doc.graph.node(&agent).cloned().ok_or_else(|| {
                RpcError::internal(format!("{id}: the new agent {agent} is missing"))
            })?;
        Ok(AddedLane {
            id: agent.as_str().to_owned(),
            node,
        })
    })
}

/// `graph.remove_lane`: remove a writer or helper (never the orchestrator) and save the graph. Its pane, if it
/// has one, keeps running.
pub fn remove_lane(
    state: &mut SpaceGraphState,
    id: &SpaceId,
    agent: &str,
) -> Result<AgentNode, RpcError> {
    let agent = AgentNodeId::new(agent.trim());
    state.change(|doc| {
        doc.graph
            .remove_lane(&agent)
            .map_err(|e| graph_error(id, e))
    })
}

/// The channel's message kind called `name` (`proposal`, `question`, `finding`, `status` or
/// `contract_update`).
pub fn message_kind(name: &str) -> Result<MessageKind, RpcError> {
    serde_json::from_value(serde_json::Value::String(name.trim().to_owned())).map_err(|_| {
        RpcError::invalid_params(format!(
            "{name:?} is not a message kind; kinds are {MESSAGE_KIND_NAMES}"
        ))
    })
}

/// A trimmed agent id, `None` when blank.
fn agent_name(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// `channel.post`: add a message to the space's channel.
pub fn channel_post(
    state: &mut SpaceGraphState,
    id: &SpaceId,
    params: &ChannelPostParams,
) -> Result<ChannelPostResult, RpcError> {
    let kind = message_kind(&params.kind)?;
    let from = agent_name(Some(&params.from))
        .ok_or_else(|| RpcError::invalid_params("channel.post: from is empty"))?;
    let to = agent_name(params.to.as_deref());
    let message = state
        .shared_mut()
        .post_message(from, to, kind, &params.body)
        .map_err(|e| channel_error(id, e))?;
    Ok(ChannelPostResult { id: message })
}

/// `channel.list`: the space's messages that pass the filters, secrets masked by `redactor`.
pub fn channel_list(
    state: &SpaceGraphState,
    params: &ChannelListParams,
    redactor: &Redactor,
) -> Result<ChannelListResult, RpcError> {
    let kind = params.kind.as_deref().map(message_kind).transpose()?;
    let limit = params.limit.map(|l| l as usize);
    Ok(ChannelListResult {
        messages: state.shared().list_messages(
            agent_name(params.to.as_deref()),
            kind,
            params.since_id,
            limit,
            redactor,
        ),
    })
}

/// `contract.get`: the space's contract, secrets masked.
pub fn contract_get(state: &SpaceGraphState, redactor: &Redactor) -> ContractResult {
    ContractResult {
        contract: state.shared().contract(redactor),
    }
}

/// `contract.set`: write a new revision of the space's contract by `author`. Returns it, secrets masked.
pub fn contract_set(
    state: &mut SpaceGraphState,
    id: &SpaceId,
    params: &ContractSetParams,
    redactor: &Redactor,
) -> Result<ContractResult, RpcError> {
    let author = agent_name(Some(&params.author))
        .ok_or_else(|| RpcError::invalid_params("contract.set: author is empty"))?;
    let draft = ContractDraft {
        title: params.title.trim().to_owned(),
        summary: params.summary.trim().to_owned(),
        spec: params.spec.clone(),
    };
    state
        .shared_mut()
        .set_contract(author, draft)
        .map_err(|e| channel_error(id, e))?;
    Ok(contract_get(state, redactor))
}

/// `contract.agree`: record that `agent` agrees to the current contract (or to `expected_version`, refusing
/// stale revisions with a conflict). Returns it, secrets masked.
pub fn contract_agree(
    state: &mut SpaceGraphState,
    id: &SpaceId,
    agent: &str,
    expected_version: Option<u32>,
    redactor: &Redactor,
) -> Result<ContractResult, RpcError> {
    let agent = agent_name(Some(agent))
        .ok_or_else(|| RpcError::invalid_params("contract.agree: agent is empty"))?;
    state
        .shared_mut()
        .agree_contract_version(agent, expected_version)
        .map_err(|e| channel_error(id, e))?;
    Ok(contract_get(state, redactor))
}

/// `loop.advance`: the current stage finished, its checks passed unless `checks_passed` says otherwise. A loop
/// not started yet is started first; any other loop must be running. Saves the loop.
pub fn loop_advance(
    state: &mut SpaceGraphState,
    id: &SpaceId,
    checks_passed: Option<bool>,
) -> Result<LoopAdvanceResult, RpcError> {
    state.change(|doc| {
        if *doc.loop_run.status() == LoopStatus::Idle {
            doc.loop_run.start().map_err(|e| loop_error(id, e))?;
        }
        let decision = doc
            .loop_run
            .advance(checks_passed.unwrap_or(true))
            .map_err(|e| loop_error(id, e))?;
        Ok(LoopAdvanceResult {
            decision,
            loop_run: doc.loop_run.clone(),
        })
    })
}

/// The autonomy level `loop.set_autonomy` asks for. Within budget takes `max_iterations` and `max_turns` (each
/// at least 1; [`DEFAULT_MAX_ITERATIONS`] and [`DEFAULT_MAX_TURNS`] when absent); the other levels take neither.
pub fn autonomy_level(params: &LoopSetAutonomyParams) -> Result<AutonomyLevel, RpcError> {
    match params.autonomy {
        LoopAutonomy::WithinBudget => {
            let max_iterations = params.max_iterations.unwrap_or(DEFAULT_MAX_ITERATIONS);
            let max_turns = params.max_turns.unwrap_or(DEFAULT_MAX_TURNS);
            if max_iterations == 0 || max_turns == 0 {
                return Err(RpcError::invalid_params(
                    "loop.set_autonomy: max_iterations and max_turns must be at least 1",
                ));
            }
            Ok(AutonomyLevel::WithinBudget {
                max_iterations,
                max_turns,
            })
        }
        other => {
            if params.max_iterations.is_some() || params.max_turns.is_some() {
                return Err(RpcError::invalid_params(
                    "loop.set_autonomy: max_iterations and max_turns are only for within_budget",
                ));
            }
            Ok(match other {
                LoopAutonomy::AskEachStep => AutonomyLevel::AskEachStep,
                _ => AutonomyLevel::UntilChecksPass,
            })
        }
    }
}

/// `loop.set_autonomy`: change how far the loop runs without asking and save it. Returns the loop.
pub fn loop_set_autonomy(
    state: &mut SpaceGraphState,
    params: &LoopSetAutonomyParams,
) -> Result<LoopRun, RpcError> {
    let autonomy = autonomy_level(params)?;
    state.change(|doc| {
        doc.loop_run.set_autonomy(autonomy);
        Ok(doc.loop_run.clone())
    })
}

/// What the title of every loop approval notice (see [`loop_approval_notice`]) starts with.
pub const LOOP_APPROVAL_TITLE_PREFIX: &str = "Loop ready for ";

/// The Inbox notice posted when the loop of space `id` waits for the user to approve moving to `stage`.
pub fn loop_approval_notice(id: &SpaceId, stage: LoopStage) -> Notice {
    Notice {
        title: format!(
            "{LOOP_APPROVAL_TITLE_PREFIX}{} in {id}",
            loop_stage_label(stage)
        ),
        body: Some(
            "Open the space's agent graph (⌘G) and choose Approve next to run the stage."
                .to_owned(),
        ),
        level: Level::Warning,
    }
}

/// The pending loop approval notices xctx posted for space `id` (see [`loop_approval_notice`]), to dismiss once
/// the user approves the next stage.
pub fn pending_loop_approvals(inbox: &Inbox, id: &SpaceId) -> Vec<ItemId> {
    let filter = Filter {
        space: Some(id.clone()),
        include_settled: false,
    };
    inbox
        .list(&filter)
        .into_iter()
        .filter(|item| item.source == Source::App)
        .filter(|item| {
            matches!(&item.body, ItemBody::Notice(n) if n.title.starts_with(LOOP_APPROVAL_TITLE_PREFIX))
        })
        .map(|item| item.id.clone())
        .collect()
}

// -------------------------------------------------------------------------------------------------------------
// Widening and panes

/// Add a writer for each repo in `added` that has none yet. Returns the new writers' ids.
pub fn add_writer_lanes(
    state: &mut SpaceGraphState,
    id: &SpaceId,
    added: &[SpaceRepo],
) -> Result<Vec<AgentNodeId>, RpcError> {
    state.change(|doc| {
        let mut ids = Vec::new();
        for repo in added {
            if doc
                .graph
                .writers()
                .any(|w| w.role.repo() == Some(&repo.name))
            {
                continue;
            }
            let agent = doc
                .graph
                .add_lane(
                    AgentRole::Writer {
                        repo: repo.name.clone(),
                    },
                    None,
                )
                .map_err(|e| graph_error(id, e))?;
            ids.push(agent);
        }
        Ok(ids)
    })
}

/// The `XCTX_AGENT_ROLE` of an agent: `orchestrator`, `writer`, or `helper:<kind>` (`tests`, `reviewer`,
/// `probe`).
pub fn agent_role_name(role: &AgentRole) -> String {
    match role {
        AgentRole::Orchestrator => "orchestrator".to_owned(),
        AgentRole::Writer { .. } => "writer".to_owned(),
        AgentRole::Helper { kind } => format!("helper:{}", xctx_ui::helper_kind_name(*kind)),
    }
}

/// The agent `pane` runs as: the agent bound to it, else (for an agent pane) the writer of its repo. Shells and
/// git panes run no agent.
pub fn pane_agent(graph: &AgentGraph, pane: &RolePane) -> Option<PaneAgent> {
    let node = graph
        .nodes()
        .iter()
        .find(|n| n.pane_id == Some(pane.id.0))
        .or_else(|| {
            (pane.role.kind == PaneKind::Agent)
                .then(|| {
                    graph
                        .writers()
                        .find(|w| w.role.repo() == Some(pane.role.repo.as_str()))
                })
                .flatten()
        })?;
    Some(PaneAgent {
        id: node.id.as_str().to_owned(),
        role: agent_role_name(&node.role),
    })
}

/// The agent panes of `panes` bound to the writer of their own repo, which resume the worktree's session when the
/// space is reopened (see [`crate::spawn::restore_agent_starts`]).
pub fn writer_panes(graph: &AgentGraph, panes: &[RolePane]) -> Vec<PaneId> {
    panes
        .iter()
        .filter(|pane| pane.role.kind == PaneKind::Agent)
        .filter(|pane| {
            graph.writers().any(|w| {
                w.pane_id == Some(pane.id.0) && w.role.repo() == Some(pane.role.repo.as_str())
            })
        })
        .map(|pane| pane.id)
        .collect()
}

/// Bring the graph's pane bindings in step with the panes of the space: an agent bound to a pane that is gone
/// (or is no longer an agent pane) is unbound, and each writer without a pane is bound to the first agent pane in
/// its repo that no other agent has. Returns whether anything changed.
pub fn sync_pane_bindings(graph: &mut AgentGraph, panes: &[RolePane]) -> bool {
    let agent_panes: Vec<&RolePane> = panes
        .iter()
        .filter(|p| p.role.kind == PaneKind::Agent)
        .collect();
    let mut changed = false;
    let stale: Vec<AgentNodeId> = graph
        .nodes()
        .iter()
        .filter(|n| {
            n.pane_id
                .is_some_and(|bound| !agent_panes.iter().any(|p| p.id.0 == bound))
        })
        .map(|n| n.id.clone())
        .collect();
    for agent in stale {
        changed |= graph.bind_pane(&agent, None).is_ok();
    }
    let unbound: Vec<(AgentNodeId, String)> = graph
        .writers()
        .filter(|w| w.pane_id.is_none())
        .filter_map(|w| w.role.repo().map(|repo| (w.id.clone(), repo.to_owned())))
        .collect();
    for (agent, repo) in unbound {
        let free = agent_panes.iter().find(|p| {
            p.role.repo == repo && !graph.nodes().iter().any(|n| n.pane_id == Some(p.id.0))
        });
        if let Some(pane) = free {
            changed |= graph.bind_pane(&agent, Some(pane.id.0)).is_ok();
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use xctx_channel::FindingSeverity;
    use xctx_control::ErrorCode;
    use xctx_control::protocol::HelperKind as LaneHelperKind;
    use xctx_core::{SpaceShape, SpaceStatus};
    use xctx_layout::{PaneId, PaneRole};

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn repo(name: &str) -> SpaceRepo {
        SpaceRepo {
            name: name.to_owned(),
            repo: format!("/code/{name}").into(),
            worktree: format!("/code/{name}-worktrees/rate-limit").into(),
            branch: "space/rate-limit".into(),
            base_branch: "main".into(),
            base_commit: "abc".into(),
            branch_created: true,
            reason: "name matches".into(),
        }
    }

    fn space() -> Space {
        Space {
            id: SpaceId::new("rate-limit"),
            title: "rate limit".into(),
            task: "add rate limiting".into(),
            shape: SpaceShape::Dev,
            status: SpaceStatus::Active,
            port: 4003,
            created_at: 0,
            repos: vec![repo("api"), repo("web")],
            production: false,
        }
    }

    fn lane(
        role: LaneRole,
        repo: Option<&str>,
        kind: Option<LaneHelperKind>,
    ) -> GraphAddLaneParams {
        GraphAddLaneParams {
            space: "rate-limit".into(),
            role,
            repo: repo.map(str::to_owned),
            kind,
            label: None,
        }
    }

    fn role_pane(id: u64, kind: PaneKind, repo: &str) -> RolePane {
        RolePane {
            id: PaneId(id),
            role: PaneRole::new(kind, repo),
        }
    }

    fn code(result: Result<impl std::fmt::Debug, RpcError>) -> Option<ErrorCode> {
        result.err().and_then(|e| e.kind())
    }

    #[test]
    fn a_new_space_starts_with_an_orchestrator_and_a_writer_per_repo() -> TestResult {
        let data = tempfile::tempdir()?;
        let state = SpaceGraphState::open(data.path(), &space())?;
        let writers: Vec<Option<&str>> = state.graph().writers().map(|w| w.role.repo()).collect();
        assert_eq!(writers, [Some("api"), Some("web")]);
        assert!(state.graph().orchestrator().is_some());
        assert_eq!(state.loop_run().autonomy(), AutonomyLevel::AskEachStep);
        // Nothing is written until the first change.
        assert!(!data.path().join("spaces/rate-limit/graph.json").exists());
        Ok(())
    }

    #[test]
    fn changes_are_saved_and_reloaded() -> TestResult {
        let data = tempfile::tempdir()?;
        let space = space();
        let mut state = SpaceGraphState::open(data.path(), &space)?;
        let writer = state
            .graph()
            .writers()
            .next()
            .map(|w| w.id.as_str().to_owned())
            .ok_or("no writer")?;
        set_agent_paused(&mut state, &space.id, &writer, true)?;
        let reloaded = SpaceGraphState::open(data.path(), &space)?;
        assert_eq!(
            reloaded
                .graph()
                .node(&AgentNodeId::new(&writer))
                .map(|n| n.state.clone()),
            Some(xctx_graph::AgentState::Paused)
        );
        set_agent_paused(&mut state, &space.id, &writer, false)?;
        assert_eq!(
            code(set_agent_paused(&mut state, &space.id, "nobody", true)),
            Some(ErrorCode::NotFound)
        );
        Ok(())
    }

    #[test]
    fn a_damaged_graph_is_an_error_and_is_kept() -> TestResult {
        let data = tempfile::tempdir()?;
        let path = data.path().join("spaces/rate-limit/graph.json");
        std::fs::create_dir_all(path.parent().ok_or("no parent")?)?;
        std::fs::write(&path, "not json")?;
        let mut graphs = SpaceGraphs::new(Some(data.path().to_path_buf()));
        assert!(graphs.load(&space()).is_err());
        assert!(graphs.error(&space().id).is_some());
        assert_eq!(std::fs::read_to_string(&path)?, "not json");
        assert!(SpaceGraphs::new(None).load(&space()).is_err());
        Ok(())
    }

    #[test]
    fn lanes_are_added_with_one_writer_per_repo() -> TestResult {
        let data = tempfile::tempdir()?;
        let space = space();
        let mut state = SpaceGraphState::open(data.path(), &space)?;
        let duplicate = add_lane(
            &mut state,
            &space,
            &lane(LaneRole::Writer, Some("api"), None),
        );
        assert_eq!(code(duplicate), Some(ErrorCode::Conflict));
        let unknown = add_lane(
            &mut state,
            &space,
            &lane(LaneRole::Writer, Some("db"), None),
        );
        assert_eq!(code(unknown), Some(ErrorCode::InvalidParams));
        let no_repo = add_lane(&mut state, &space, &lane(LaneRole::Writer, None, None));
        assert_eq!(code(no_repo), Some(ErrorCode::InvalidParams));
        let no_kind = add_lane(&mut state, &space, &lane(LaneRole::Helper, None, None));
        assert_eq!(code(no_kind), Some(ErrorCode::InvalidParams));

        let mut params = lane(LaneRole::Helper, None, Some(LaneHelperKind::Reviewer));
        params.label = Some("  Second opinion ".into());
        let added = add_lane(&mut state, &space, &params)?;
        assert_eq!(added.node.label, "Second opinion");
        assert_eq!(
            added.node.role,
            AgentRole::Helper {
                kind: HelperKind::Reviewer
            }
        );
        let json = serde_json::to_value(&added)?;
        assert_eq!(json["id"], added.id.as_str());

        let removed = remove_lane(&mut state, &space.id, &added.id)?;
        assert_eq!(removed.id.as_str(), added.id);
        let orchestrator = remove_lane(&mut state, &space.id, xctx_graph::ORCHESTRATOR_ID);
        assert_eq!(code(orchestrator), Some(ErrorCode::Conflict));
        Ok(())
    }

    #[test]
    fn the_channel_validates_kinds_and_masks_secrets() -> TestResult {
        let data = tempfile::tempdir()?;
        let space = space();
        let mut state = SpaceGraphState::open(data.path(), &space)?;
        let post = |kind: &str, body: &str, to: Option<&str>| ChannelPostParams {
            space: "rate-limit".into(),
            from: "writer-api".into(),
            to: to.map(str::to_owned),
            kind: kind.into(),
            body: body.into(),
        };
        let bad = channel_post(&mut state, &space.id, &post("gossip", "hi", None));
        assert_eq!(code(bad), Some(ErrorCode::InvalidParams));
        let first = channel_post(
            &mut state,
            &space.id,
            &post("status", "token tok-1234 works", None),
        )?;
        let second = channel_post(
            &mut state,
            &space.id,
            &post("question", "header name?", Some("writer-web")),
        )?;
        assert!(second.id > first.id);

        let redactor = Redactor::new([("API_TOKEN".to_owned(), "tok-1234".to_owned())]);
        let all = ChannelListParams {
            space: "rate-limit".into(),
            to: None,
            kind: None,
            since_id: None,
            limit: None,
        };
        let listed = channel_list(&state, &all, &redactor)?;
        assert_eq!(listed.messages.len(), 2);
        assert_eq!(listed.messages[0].body, "token [REDACTED:API_TOKEN] works");
        let questions = ChannelListParams {
            kind: Some("question".into()),
            ..all.clone()
        };
        assert_eq!(
            channel_list(&state, &questions, &redactor)?.messages.len(),
            1
        );
        let since = ChannelListParams {
            since_id: Some(second.id),
            ..all.clone()
        };
        assert!(channel_list(&state, &since, &redactor)?.messages.is_empty());
        let bad_kind = ChannelListParams {
            kind: Some("chat".into()),
            ..all
        };
        assert_eq!(
            code(channel_list(&state, &bad_kind, &redactor)),
            Some(ErrorCode::InvalidParams)
        );
        Ok(())
    }

    #[test]
    fn contracts_are_set_agreed_and_masked() -> TestResult {
        let data = tempfile::tempdir()?;
        let space = space();
        let mut state = SpaceGraphState::open(data.path(), &space)?;
        let redactor = Redactor::new([("API_TOKEN".to_owned(), "tok-1234".to_owned())]);
        assert_eq!(contract_get(&state, &redactor).contract, None);
        let agree_first = contract_agree(&mut state, &space.id, "writer-web", None, &redactor);
        assert_eq!(code(agree_first), Some(ErrorCode::Conflict));

        let set = ContractSetParams {
            space: "rate-limit".into(),
            author: "writer-api".into(),
            title: "rate-limit headers".into(),
            summary: "what the api sends".into(),
            spec: "X-RateLimit-Key: tok-1234".into(),
        };
        let result = contract_set(&mut state, &space.id, &set, &redactor)?;
        let contract = result.contract.ok_or("no contract")?;
        assert_eq!(contract.version, 1);
        assert_eq!(contract.spec, "X-RateLimit-Key: [REDACTED:API_TOKEN]");
        let stale = contract_agree(&mut state, &space.id, "writer-web", Some(99), &redactor);
        assert_eq!(code(stale), Some(ErrorCode::Conflict));
        let agreed = contract_agree(&mut state, &space.id, "writer-web", Some(1), &redactor)?;
        assert_eq!(
            agreed.contract.map(|c| c.agreed_by),
            Some(vec!["writer-api".to_owned(), "writer-web".to_owned()])
        );
        let blank = ContractSetParams {
            spec: " ".into(),
            ..set
        };
        assert_eq!(
            code(contract_set(&mut state, &space.id, &blank, &redactor)),
            Some(ErrorCode::InvalidParams)
        );
        Ok(())
    }

    #[test]
    fn the_loop_asks_before_each_step_until_autonomy_changes() -> TestResult {
        let data = tempfile::tempdir()?;
        let space = space();
        let mut state = SpaceGraphState::open(data.path(), &space)?;
        let advanced = loop_advance(&mut state, &space.id, None)?;
        assert_eq!(
            advanced.decision,
            LoopDecision::NeedsApproval(LoopStage::Test)
        );
        assert_eq!(
            *advanced.loop_run.status(),
            LoopStatus::AwaitingApproval {
                next_stage: LoopStage::Test
            }
        );
        // Waiting for approval: advancing again conflicts.
        assert_eq!(
            code(loop_advance(&mut state, &space.id, Some(true))),
            Some(ErrorCode::Conflict)
        );
        let notice = loop_approval_notice(&space.id, LoopStage::Test);
        assert_eq!(notice.title, "Loop ready for Test in rate-limit");

        let budget = LoopSetAutonomyParams {
            space: "rate-limit".into(),
            autonomy: LoopAutonomy::WithinBudget,
            max_iterations: None,
            max_turns: Some(10),
        };
        let run = loop_set_autonomy(&mut state, &budget)?;
        assert_eq!(
            run.autonomy(),
            AutonomyLevel::WithinBudget {
                max_iterations: DEFAULT_MAX_ITERATIONS,
                max_turns: 10
            }
        );
        let zero = LoopSetAutonomyParams {
            max_iterations: Some(0),
            ..budget.clone()
        };
        assert_eq!(
            code(loop_set_autonomy(&mut state, &zero)),
            Some(ErrorCode::InvalidParams)
        );
        let stray = LoopSetAutonomyParams {
            autonomy: LoopAutonomy::AskEachStep,
            ..budget
        };
        assert_eq!(code(autonomy_level(&stray)), Some(ErrorCode::InvalidParams));
        let reloaded = SpaceGraphState::open(data.path(), &space)?;
        assert_eq!(reloaded.loop_run(), state.loop_run());
        Ok(())
    }

    #[test]
    fn writers_are_added_for_new_repos_only() -> TestResult {
        let data = tempfile::tempdir()?;
        let space = space();
        let mut state = SpaceGraphState::open(data.path(), &space)?;
        let added = add_writer_lanes(&mut state, &space.id, &[repo("api"), repo("db")])?;
        assert_eq!(added.len(), 1);
        let repos: Vec<Option<&str>> = state.graph().writers().map(|w| w.role.repo()).collect();
        assert_eq!(repos, [Some("api"), Some("web"), Some("db")]);
        Ok(())
    }

    #[test]
    fn panes_take_their_bound_agent_or_their_repos_writer() {
        let mut graph = AgentGraph::for_space(&["api".to_owned(), "web".to_owned()]);
        let helper = graph
            .add_lane(
                AgentRole::Helper {
                    kind: HelperKind::Tests,
                },
                None,
            )
            .ok();
        let panes = [
            role_pane(1, PaneKind::Agent, "api"),
            role_pane(2, PaneKind::Shell, "api"),
            role_pane(3, PaneKind::Agent, "web"),
            role_pane(4, PaneKind::Agent, "api"),
        ];
        if let Some(helper) = &helper {
            assert!(graph.bind_pane(helper, Some(4)).is_ok());
            // A stale binding from an earlier layout.
            assert!(
                graph
                    .bind_pane(&AgentNodeId::new("orchestrator"), Some(9))
                    .is_ok()
            );
        }
        assert!(sync_pane_bindings(&mut graph, &panes));
        assert!(!sync_pane_bindings(&mut graph, &panes));
        let bound: Vec<Option<u64>> = graph.nodes().iter().map(|n| n.pane_id).collect();
        assert_eq!(bound, [None, Some(1), Some(3), Some(4)]);

        let agent = pane_agent(&graph, &panes[0]);
        assert_eq!(agent.as_ref().map(|a| a.role.as_str()), Some("writer"));
        assert_eq!(
            pane_agent(&graph, &panes[3]).map(|a| a.role),
            Some("helper:tests".to_owned())
        );
        assert_eq!(pane_agent(&graph, &panes[1]), None);
        assert_eq!(
            agent_role_name(&AgentRole::Orchestrator),
            "orchestrator".to_owned()
        );

        // The writer's pane closes: it is unbound, and a new agent pane in its repo picks it up.
        let after = [
            panes[1].clone(),
            panes[2].clone(),
            panes[3].clone(),
            role_pane(5, PaneKind::Agent, "api"),
        ];
        assert!(sync_pane_bindings(&mut graph, &after));
        assert_eq!(
            graph.writers().map(|w| w.pane_id).collect::<Vec<_>>(),
            [Some(5), Some(3)]
        );
    }

    #[test]
    fn writer_panes_are_the_agent_panes_bound_to_their_repos_writer() {
        let mut graph = AgentGraph::for_space(&["api".to_owned(), "web".to_owned()]);
        let helper = graph
            .add_lane(
                AgentRole::Helper {
                    kind: HelperKind::Tests,
                },
                None,
            )
            .ok();
        let panes = [
            role_pane(1, PaneKind::Agent, "api"),
            role_pane(2, PaneKind::Agent, "api"),
            role_pane(3, PaneKind::Agent, "web"),
        ];
        if let Some(helper) = &helper {
            assert!(graph.bind_pane(helper, Some(1)).is_ok());
        }
        assert!(sync_pane_bindings(&mut graph, &panes));
        // The helper has the first api pane, so api's writer is bound to the second.
        assert_eq!(writer_panes(&graph, &panes), [PaneId(2), PaneId(3)]);
    }

    #[test]
    fn scratch_moves_under_the_data_dir_when_it_is_set() -> TestResult {
        let data = tempfile::tempdir()?;
        let isolated = scratch_dir_in(Some(data.path().into()), "rate-limit")?;
        assert_eq!(isolated.root(), data.path().join("scratch/rate-limit"));
        let real = scratch_dir_in(None, "rate-limit")?;
        assert_eq!(real.root(), Path::new("/tmp/xctx-scratch/rate-limit"));
        let empty = scratch_dir_in(Some(OsString::new()), "rate-limit")?;
        assert_eq!(empty.root(), real.root());
        assert!(scratch_dir_in(Some(data.path().into()), "../escape").is_err());
        Ok(())
    }

    #[test]
    fn pausing_from_the_panel_tells_the_agent_on_the_channel() -> TestResult {
        let data = tempfile::tempdir()?;
        let space = space();
        let mut state = SpaceGraphState::open(data.path(), &space)?;
        set_agent_paused_and_tell(&mut state, &space.id, "writer-api", true)?;
        set_agent_paused_and_tell(&mut state, &space.id, "writer-api", false)?;
        let told = state.shared().list_messages(
            Some("writer-api"),
            Some(MessageKind::Status),
            None,
            None,
            &Redactor::default(),
        );
        assert_eq!(told.len(), 2);
        assert!(told.iter().all(|m| m.from == PANEL_STATUS_AUTHOR));
        assert!(told[0].body.contains("paused") && told[1].body.contains("resumed"));
        // An unknown agent is not paused, and nothing is posted.
        assert_eq!(
            code(set_agent_paused_and_tell(
                &mut state, &space.id, "nobody", true
            )),
            Some(ErrorCode::NotFound)
        );
        assert_eq!(
            state
                .shared()
                .list_messages(None, None, None, None, &Redactor::default())
                .len(),
            2
        );
        Ok(())
    }

    #[test]
    fn loop_approvals_of_a_space_are_found_to_dismiss() -> TestResult {
        let dir = tempfile::tempdir()?;
        let mut inbox = Inbox::open(dir.path())?;
        let id = space().id;
        let other = SpaceId::new("other");
        let notice =
            |space: &SpaceId| ItemBody::Notice(loop_approval_notice(space, LoopStage::Test));
        let first = inbox.post(notice(&id), Some(id.clone()), Source::App)?;
        let second = inbox.post(notice(&id), Some(id.clone()), Source::App)?;
        inbox.post(notice(&other), Some(other.clone()), Source::App)?;
        // An agent's notice with the same title is not xctx's to dismiss.
        inbox.post(notice(&id), Some(id.clone()), Source::Agent)?;
        inbox.post(
            ItemBody::Notice(Notice {
                title: "tests are flaky".into(),
                body: None,
                level: Level::Info,
            }),
            Some(id.clone()),
            Source::App,
        )?;
        let mut found = pending_loop_approvals(&inbox, &id);
        found.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        assert_eq!(found, [first.clone(), second]);
        inbox.dismiss(&first)?;
        assert_eq!(pending_loop_approvals(&inbox, &id).len(), 1);
        Ok(())
    }

    #[test]
    fn findings_resolve_through_the_shared_store() -> TestResult {
        let data = tempfile::tempdir()?;
        let mut state = SpaceGraphState::open(data.path(), &space())?;
        let id = state.shared_mut().add_finding(
            "helper-tests-1",
            FindingSeverity::Warning,
            Some("api"),
            None,
            "flaky retry test",
        )?;
        state.shared_mut().resolve_finding(id)?;
        assert!(
            state
                .shared()
                .findings(true, &Redactor::default())
                .is_empty()
        );
        Ok(())
    }
}
