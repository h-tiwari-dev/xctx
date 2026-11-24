//! A space's competing attempts as the app keeps them: each space's [`AttemptSet`], loaded when first needed and
//! saved to `attempts.json` before any change is kept, and what each attempt control method does with it.
//!
//! Everything here works without a window, so it is tested directly; the workspace applies it in
//! `attempt_jobs.rs` and `control.rs`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use xctx_attempt::{Attempt, AttemptError, AttemptSet, ForkPlan, Outcome, Repro, TestSummary};
use xctx_channel::{ChannelError, ScratchDir};
use xctx_control::cli::ATTEMPT_TOKEN_ENV;
use xctx_control::protocol::{
    AttemptDiffParams, AttemptDiffResult, AttemptFileChange, AttemptListResult,
    AttemptProposeParams, AttemptReportParams, AttemptSummary, ReproStatus,
};
use xctx_control::{ErrorCode, RpcError};
use xctx_core::space::PORT_RANGE;
use xctx_core::{AttemptWorktree, MAX_ATTEMPTS, Redactor, SpaceId};
use xctx_graph::{AgentGraph, AgentNodeId};
use xctx_inbox::{AttemptProposal, Level, Notice};

/// Seconds since the Unix epoch, for the attempt records' timestamps.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The attempts of every space loaded so far, by space.
#[derive(Debug, Default)]
pub struct SpaceAttempts {
    data_dir: Option<PathBuf>,
    sets: HashMap<SpaceId, AttemptSet>,
}

impl SpaceAttempts {
    /// Attempts kept under `data_dir`; with `None` (no data directory could be found) every load fails.
    pub fn new(data_dir: Option<PathBuf>) -> SpaceAttempts {
        SpaceAttempts {
            data_dir,
            sets: HashMap::new(),
        }
    }

    /// The loaded attempts of `id`, if they have been loaded.
    pub fn get(&self, id: &SpaceId) -> Option<&AttemptSet> {
        self.sets.get(id)
    }

    /// The attempts of `id`, loading them on first use. A space that never had any has an empty set.
    pub fn load(&mut self, id: &SpaceId) -> Result<&AttemptSet, String> {
        if !self.sets.contains_key(id) {
            let data_dir = self
                .data_dir
                .as_ref()
                .ok_or_else(|| "there is no data directory for attempts".to_owned())?;
            let set = xctx_attempt::store::load(data_dir, id)
                .map_err(|e| format!("could not read the attempts of {id}: {e}"))?;
            self.sets.insert(id.clone(), set);
        }
        self.sets
            .get(id)
            .ok_or_else(|| format!("the attempts of {id} are not loaded"))
    }

    /// Apply `change` to a copy of the attempts of `id` and keep it only once `attempts.json` is written, so
    /// memory never holds what the file does not. A change that fails leaves both as they were.
    pub fn change<R>(
        &mut self,
        id: &SpaceId,
        change: impl FnOnce(&mut AttemptSet) -> Result<R, AttemptError>,
    ) -> Result<R, String> {
        self.try_change(id, change)?
            .map_err(|e| format!("{id}: {e}"))
    }

    /// [`SpaceAttempts::change`], keeping the refusal of `change` (the inner error) apart from a failure to read
    /// or write `attempts.json` (the outer one).
    pub fn try_change<R>(
        &mut self,
        id: &SpaceId,
        change: impl FnOnce(&mut AttemptSet) -> Result<R, AttemptError>,
    ) -> Result<Result<R, AttemptError>, String> {
        let mut set = self.load(id)?.clone();
        let result = match change(&mut set) {
            Ok(result) => result,
            Err(e) => return Ok(Err(e)),
        };
        let data_dir = self
            .data_dir
            .as_ref()
            .ok_or_else(|| "there is no data directory for attempts".to_owned())?;
        xctx_attempt::store::save(data_dir, &set)
            .map_err(|e| format!("could not save the attempts of {id}: {e}"))?;
        self.sets.insert(id.clone(), set);
        Ok(Ok(result))
    }

    /// The ports the live attempts of `spaces` hold, loading each space's attempts. A space whose attempts cannot
    /// be read is skipped.
    pub fn live_ports<'a>(&mut self, spaces: impl IntoIterator<Item = &'a SpaceId>) -> Vec<u16> {
        let mut ports = Vec::new();
        for id in spaces {
            if let Ok(set) = self.load(id) {
                ports.extend(set.ports());
            }
        }
        ports
    }

    /// Drop what is held for `id`, once it is shipped or discarded. Its file stays in the data directory.
    pub fn forget(&mut self, id: &SpaceId) {
        self.sets.remove(id);
    }
}

/// `count` ports for new attempts, the lowest in [`PORT_RANGE`] that are not `taken` (by spaces and by other
/// attempts).
pub fn allocate_attempt_ports(
    count: usize,
    taken: impl IntoIterator<Item = u16>,
) -> Result<Vec<u16>, String> {
    let taken: Vec<u16> = taken.into_iter().collect();
    let ports: Vec<u16> = PORT_RANGE
        .clone()
        .filter(|p| !taken.contains(p))
        .take(count)
        .collect();
    if ports.len() < count {
        return Err(format!(
            "no free port in {}-{} for {count} attempts",
            PORT_RANGE.start(),
            PORT_RANGE.end()
        ));
    }
    Ok(ports)
}

/// One attempt as `attempt.list` reports it, text masked with `redactor`.
pub fn attempt_summary(attempt: &Attempt, redactor: &Redactor) -> AttemptSummary {
    let mask = |text: &str| redactor.redact(text).into_owned();
    let outcome = attempt.outcome.as_ref();
    let tests = outcome.and_then(|o| o.tests.as_ref());
    AttemptSummary {
        number: attempt.number(),
        round: attempt.round,
        repo: attempt.worktree.repo.clone(),
        agent: attempt.agent.clone(),
        state: attempt.state.name().to_owned(),
        instruction: attempt.instruction.as_deref().map(mask),
        branch: attempt.worktree.branch.clone(),
        worktree: attempt.worktree.worktree.to_string_lossy().into_owned(),
        port: attempt.worktree.port,
        summary: outcome.map(|o| mask(&o.summary)),
        tests_passed: tests.map(|t| t.passed),
        tests_failed: tests.map(|t| t.failed),
        repro: outcome
            .map(|o| o.repro)
            .unwrap_or_default()
            .name()
            .to_owned(),
    }
}

/// `attempt.list`: the attempts of `set` (only `round`'s when given), oldest first, masked.
pub fn attempt_list(
    set: &AttemptSet,
    round: Option<u32>,
    redactor: &Redactor,
) -> AttemptListResult {
    AttemptListResult {
        attempts: set
            .attempts
            .iter()
            .filter(|a| round.is_none_or(|r| a.round == r))
            .map(|a| attempt_summary(a, redactor))
            .collect(),
    }
}

/// The outcome `attempt.report` records.
pub fn report_outcome(params: &AttemptReportParams) -> Outcome {
    let tests = (params.tests_passed.is_some()
        || params.tests_failed.is_some()
        || params.test_command.is_some())
    .then(|| TestSummary {
        command: params
            .test_command
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_owned),
        passed: params.tests_passed.unwrap_or(0),
        failed: params.tests_failed.unwrap_or(0),
    });
    Outcome {
        summary: params.summary.trim().to_owned(),
        tests,
        repro: match params.repro {
            ReproStatus::NotChecked => Repro::NotChecked,
            ReproStatus::Fixed => Repro::Fixed,
            ReproStatus::StillFails => Repro::StillFails,
        },
    }
}

/// What recording an `attempt.report` did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reported {
    /// The reporting attempt's round.
    pub round: u32,
    /// Whether every attempt of that round has reported (or is no longer live) now.
    pub round_finished: bool,
    /// Whether it was already finished before this report, so the user was told already.
    pub was_finished: bool,
}

/// `attempt.report`: record the outcome in `set`. Refused for an empty summary.
pub fn record_report(
    set: &mut AttemptSet,
    params: &AttemptReportParams,
    now: u64,
) -> Result<Reported, AttemptError> {
    let round = set
        .get(params.attempt)
        .map(|a| a.round)
        .ok_or(AttemptError::NotFound(params.attempt))?;
    let was_finished = set.round_finished(round);
    set.report(params.attempt, report_outcome(params), params.gave_up, now)?;
    Ok(Reported {
        round,
        round_finished: set.round_finished(round),
        was_finished,
    })
}

/// The Inbox notice posted once every attempt of `round` in space `id` is done.
pub fn round_done_notice(id: &SpaceId, round: u32) -> Notice {
    Notice {
        title: format!("All attempts of round {round} in {id} are done; review them"),
        body: Some(
            "Open the space and its review sheet to compare the attempts, then adopt one and discard the rest."
                .to_owned(),
        ),
        level: Level::Success,
    }
}

/// An attempt record error of space `id` as a control error: an unknown attempt is not found, bad counts and
/// slugs are invalid, a store failure is internal, and the rest (wrong state, too many) conflicts.
pub fn attempt_rpc_error(id: &SpaceId, error: AttemptError) -> RpcError {
    match error {
        AttemptError::NotFound(_) => RpcError::not_found(format!("{id}: {error}")),
        AttemptError::BadCount { .. } | AttemptError::InvalidSlug(_) => {
            RpcError::invalid_params(format!("{id}: {error}"))
        }
        AttemptError::Store(_) | AttemptError::UnsupportedVersion { .. } => {
            RpcError::internal(format!("{id}: {error}"))
        }
        other => RpcError::conflict(format!("{id}: {other}")),
    }
}

/// `attempt.propose`: the proposal to post for space `id`, whose graph is `graph`. The agent must be one of its
/// writers, the count 1 to [`MAX_ATTEMPTS`], and there may be no more instructions than attempts. Blank
/// instructions are dropped from the end and kept empty in between, so each keeps its attempt's place.
pub fn attempt_proposal(
    id: &SpaceId,
    graph: &AgentGraph,
    params: &AttemptProposeParams,
) -> Result<AttemptProposal, RpcError> {
    if params.count == 0 || params.count > MAX_ATTEMPTS {
        return Err(RpcError::invalid_params(format!(
            "attempt.propose: count must be 1 to {MAX_ATTEMPTS}, not {}",
            params.count
        )));
    }
    let agent = params.agent.trim();
    let is_writer = graph
        .node(&AgentNodeId::new(agent))
        .is_some_and(|n| n.role.is_writer());
    if !is_writer {
        let writers: Vec<&str> = graph.writers().map(|w| w.id.as_str()).collect();
        return Err(RpcError::invalid_params(format!(
            "attempt.propose: {agent:?} is not a writer of {id}; its writers are {}",
            if writers.is_empty() {
                "none".to_owned()
            } else {
                writers.join(", ")
            }
        )));
    }
    let mut instructions: Vec<String> = params
        .instructions
        .clone()
        .unwrap_or_default()
        .into_iter()
        .map(|i| i.trim().to_owned())
        .collect();
    while instructions.last().is_some_and(String::is_empty) {
        instructions.pop();
    }
    if instructions.len() > params.count as usize {
        return Err(RpcError::invalid_params(format!(
            "attempt.propose: {} instructions for {} attempts",
            instructions.len(),
            params.count
        )));
    }
    Ok(AttemptProposal {
        space: id.clone(),
        agent: agent.to_owned(),
        count: params.count,
        instructions,
        rationale: params
            .rationale
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_owned),
    })
}

/// One instruction per attempt of a proposal for `count` attempts: those given, in order, then none.
pub fn fork_instructions(count: u32, instructions: &[String]) -> Vec<Option<String>> {
    (0..count as usize)
        .map(|i| {
            instructions
                .get(i)
                .map(|t| t.trim())
                .filter(|t| !t.is_empty())
                .map(str::to_owned)
        })
        .collect()
}

/// Why space `id` cannot be shipped or discarded (`action`) while it has live attempts in `set`, if it cannot.
pub fn live_attempts_refusal(id: &SpaceId, set: &AttemptSet, action: &str) -> Option<String> {
    let live: Vec<String> = set.live().map(|a| a.number().to_string()).collect();
    if live.is_empty() {
        return None;
    }
    let (noun, pronoun) = if live.len() == 1 {
        ("attempt", "it")
    } else {
        ("attempts", "them")
    };
    Some(format!(
        "{id} has live {noun} {}; adopt or discard {pronoun} on the review sheet before you {action} the space",
        live.join(", ")
    ))
}

/// Where report tokens come from.
const RANDOM_SOURCE: &str = "/dev/urandom";

/// How many random bytes a report token holds (printed as twice as many hex digits).
pub const TOKEN_BYTES: usize = 16;

/// A new random report token for an attempt (see [`Attempt::token`]): [`TOKEN_BYTES`] bytes of `/dev/urandom`, in
/// hex.
pub fn new_attempt_token() -> Result<String, String> {
    let file = std::fs::File::open(RANDOM_SOURCE)
        .map_err(|e| format!("could not open {RANDOM_SOURCE} for an attempt token: {e}"))?;
    token_from(file)
        .map_err(|e| format!("could not read {RANDOM_SOURCE} for an attempt token: {e}"))
}

/// A token from the first [`TOKEN_BYTES`] bytes of `source`, in lowercase hex.
pub fn token_from(mut source: impl std::io::Read) -> std::io::Result<String> {
    let mut bytes = [0u8; TOKEN_BYTES];
    source.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Refuse, with [`ErrorCode::Forbidden`], an `attempt.report` whose `token` is not `attempt`'s: only the attempt's
/// own panes have it (`XCTX_ATTEMPT_TOKEN`), so no other pane can write its outcome. An attempt without a token
/// (forked before tokens, not yet given one) cannot report.
pub fn check_report_token(
    id: &SpaceId,
    attempt: &Attempt,
    token: Option<&str>,
) -> Result<(), RpcError> {
    let number = attempt.number();
    if attempt.token.is_empty() {
        return Err(RpcError::new(
            ErrorCode::Forbidden,
            format!(
                "attempt {number} of {id} has no report token yet; reopen the space so xctx gives it one, then \
                 report from its pane"
            ),
        ));
    }
    let given = token.map(str::trim).unwrap_or_default();
    // Compared in full, whatever differs first.
    let same = given.len() == attempt.token.len()
        && given
            .bytes()
            .zip(attempt.token.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0;
    if !same {
        return Err(RpcError::new(
            ErrorCode::Forbidden,
            format!(
                "attempt.report: the token does not belong to attempt {number} of {id}; only its own panes \
                 (with {ATTEMPT_TOKEN_ENV} set) may report for it"
            ),
        ));
    }
    Ok(())
}

/// The `XCTX_AGENT_ID` of attempt `number`'s agent, forked from writer `writer`: `<writer>#attempt-<n>`. It is no
/// node of the graph, so an attempt never acts as its writer.
pub fn attempt_agent_id(writer: &str, number: u32) -> String {
    format!("{writer}{ATTEMPT_AGENT_MARK}{number}")
}

/// What an attempt agent's id has after its writer's (see [`attempt_agent_id`]).
pub const ATTEMPT_AGENT_MARK: &str = "#attempt-";

/// Refuse, with [`ErrorCode::Forbidden`], a call by which `agent` would change the space as one of its agents
/// (`method`: pausing, removing or changing the contract) when `agent` is an attempt's (see
/// [`attempt_agent_id`]). An attempt reports with `attempt.report`; its writer acts for the space.
pub fn refuse_attempt_agent(method: &str, agent: &str) -> Result<(), RpcError> {
    if agent.contains(ATTEMPT_AGENT_MARK) {
        return Err(RpcError::new(
            ErrorCode::Forbidden,
            format!(
                "{method}: {} is an attempt's agent and cannot act for the space; report with attempt.report",
                agent.trim()
            ),
        ));
    }
    Ok(())
}

/// Plan a fork of writer `agent` in `repo` (the space's repo whose worktree is `worktree`), skipping attempt
/// numbers whose branch or folder a failed fork left behind ([`xctx_core::attempt_number_taken`]), so leftovers
/// never block a later fork. Runs git.
pub fn plan_fork_skipping_leftovers(
    set: &AttemptSet,
    slug: &str,
    agent: &str,
    repo: &str,
    worktree: &Path,
    instructions: Vec<Option<String>>,
) -> Result<ForkPlan, AttemptError> {
    set.plan_fork_avoiding(agent, repo, instructions, |n| {
        xctx_core::attempt_number_taken(worktree, slug, n)
    })
}

/// Delete the refs that keep a space's work from before each adoption (`refs/xctx/pre-adopt/<slug>/*`, see
/// [`xctx_core::attempt::pre_adopt_ref`]) in the repository of `repo`, once the space is shipped or discarded.
/// Returns how many were deleted; a repository that is gone has none.
pub fn delete_pre_adopt_refs(repo: &Path, slug: &str) -> Result<usize, String> {
    if !repo.exists() {
        return Ok(0);
    }
    let prefix = format!("refs/xctx/pre-adopt/{slug}/");
    let failed = |e: xctx_core::Error| {
        format!(
            "could not remove the pre-adopt backups of {slug} in {}: {e}",
            repo.display()
        )
    };
    let listed = xctx_core::git::run_git(repo, ["for-each-ref", "--format=%(refname)", &prefix])
        .map_err(failed)?;
    let mut deleted = 0;
    for name in listed
        .lines()
        .map(str::trim)
        .filter(|n| n.starts_with(&prefix))
    {
        xctx_core::git::run_git(repo, ["update-ref", "-d", name]).map_err(failed)?;
        deleted += 1;
    }
    Ok(deleted)
}

/// What the user is told when the worktrees a fork of `id` made but could not record were not all removed:
/// each leftover with why, or `None` when every one was.
pub fn unrecorded_cleanup_message(id: &SpaceId, failures: &[String]) -> Option<String> {
    if failures.is_empty() {
        return None;
    }
    Some(format!(
        "could not remove what a failed fork of {id} made: {}; remove it by hand (later forks skip these numbers)",
        failures.join("; ")
    ))
}

/// What the user is told when attempt `number` was merged into `id` but marking it adopted could not be saved
/// (`error`): nothing was cleaned up, and adopting it again retries.
pub fn unsaved_adoption_message(id: &SpaceId, number: u32, error: &str) -> String {
    format!(
        "merged attempt {number} into {id}, but could not record it as adopted: {error}. Its worktree and agent \
         are kept; adopt it again to retry saving (it is not merged twice)"
    )
}

/// Why nothing but adopting attempt `number` again is allowed while its adoption into `id` is unsaved.
pub fn unsaved_adoption_refusal(id: &SpaceId, number: u32) -> String {
    format!(
        "attempt {number} of {id} was merged, but its adoption is not saved yet; adopt attempt {number} again to \
         retry saving it first"
    )
}

/// The scratch directory of attempt `number` inside the space's scratch directory `space`:
/// `<space scratch>/attempts/<number>`.
pub fn attempt_scratch(space: &ScratchDir, number: u32) -> Result<ScratchDir, ChannelError> {
    ScratchDir::in_base(space.root().join("attempts"), &number.to_string())
}

/// Copy every artifact of the space's scratch directory `from` (other attempts' folders excepted) into the attempt's
/// scratch directory `to`. Returns how many were copied.
pub fn copy_scratch(from: &ScratchDir, to: &ScratchDir) -> Result<usize, ChannelError> {
    let mut copied = 0;
    for rel in from.list_artifacts()? {
        if rel.starts_with("attempts") {
            continue;
        }
        let rel = rel.to_string_lossy();
        let bytes = from.read_artifact(&rel)?;
        to.write_artifact(&rel, &bytes)?;
        copied += 1;
    }
    Ok(copied)
}

/// `attempt.diff`: the files `attempt` changed since its fork, uncommitted work included, and with `path` that
/// file's diff, masked with `redactor`. Runs git, so call it off the UI thread.
pub fn attempt_diff(
    attempt: &AttemptWorktree,
    path: Option<&str>,
    redactor: &Redactor,
) -> Result<AttemptDiffResult, RpcError> {
    let failed = |e: xctx_core::Error| {
        RpcError::internal(format!(
            "could not read the changes of attempt {}: {e}",
            attempt.number
        ))
    };
    let tip = xctx_core::attempt::attempt_tip(attempt).map_err(failed)?;
    let changes =
        xctx_core::git::diff_stat(&attempt.worktree, &attempt.fork_commit, &tip).map_err(failed)?;
    let diff = match path.map(str::trim).filter(|p| !p.is_empty()) {
        Some(path) => {
            if !changes.iter().any(|c| c.path == path) {
                return Err(RpcError::not_found(format!(
                    "attempt {} did not change {path}",
                    attempt.number
                )));
            }
            let text =
                xctx_core::git::diff_file(&attempt.worktree, &attempt.fork_commit, &tip, path)
                    .map_err(failed)?;
            Some(redactor.redact(&text).into_owned())
        }
        None => None,
    };
    Ok(AttemptDiffResult {
        files: changes
            .into_iter()
            .map(|c| AttemptFileChange {
                path: c.path,
                added: c.added,
                removed: c.removed,
            })
            .collect(),
        diff,
    })
}

/// The live attempt `params.attempt` of `set`, for `attempt.diff`: a discarded or adopted one has no worktree.
pub fn diff_target(
    id: &SpaceId,
    set: &AttemptSet,
    params: &AttemptDiffParams,
) -> Result<AttemptWorktree, RpcError> {
    let attempt = set
        .get(params.attempt)
        .ok_or_else(|| RpcError::not_found(format!("{id} has no attempt {}", params.attempt)))?;
    if !attempt.state.is_live() {
        return Err(RpcError::conflict(format!(
            "attempt {} of {id} is {}; its worktree is gone",
            attempt.number(),
            attempt.state.name()
        )));
    }
    Ok(attempt.worktree.clone())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use xctx_attempt::AttemptState;
    use xctx_control::ErrorCode;
    use xctx_graph::AgentRole;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn worktree(number: u32) -> AttemptWorktree {
        AttemptWorktree {
            number,
            repo: "api".into(),
            repo_path: "/code/api".into(),
            worktree: PathBuf::from(format!("/code/api-worktrees/s--attempt-{number}")),
            branch: format!("attempt/s/{number}"),
            fork_commit: "abc".into(),
            port: 4100 + number as u16,
        }
    }

    fn forked(set: &mut AttemptSet, instructions: Vec<Option<String>>) -> TestResult {
        let plan = set.plan_fork("writer-api", "api", instructions)?;
        let trees = plan.numbers.iter().map(|&n| worktree(n)).collect();
        set.record_fork(&plan, trees, "xctx-s-writer-api", 10)?;
        Ok(())
    }

    fn report(attempt: u32, summary: &str) -> AttemptReportParams {
        AttemptReportParams {
            space: "s".into(),
            attempt,
            summary: summary.into(),
            tests_passed: None,
            tests_failed: None,
            test_command: None,
            repro: ReproStatus::NotChecked,
            gave_up: false,
            token: None,
        }
    }

    #[test]
    fn reports_need_the_attempts_own_token() -> TestResult {
        let id = SpaceId::new("s");
        let mut set = AttemptSet::new(id.clone());
        forked(&mut set, vec![None, None])?;
        let token = token_from(&[7u8; TOKEN_BYTES][..])?;
        assert_eq!(token, "07".repeat(TOKEN_BYTES));
        assert!(token_from(&[1u8; 3][..]).is_err());
        let fresh = new_attempt_token()?;
        assert_eq!(fresh.len(), TOKEN_BYTES * 2);
        assert_ne!(fresh, new_attempt_token()?);
        set.set_token(1, token.clone())?;
        let first = set.get(1).cloned().ok_or("no attempt 1")?;
        let second = set.get(2).cloned().ok_or("no attempt 2")?;
        assert!(check_report_token(&id, &first, Some(&token)).is_ok());
        let forbidden =
            |r: Result<(), RpcError>| r.err().and_then(|e| e.kind()) == Some(ErrorCode::Forbidden);
        assert!(forbidden(check_report_token(&id, &first, None)));
        assert!(forbidden(check_report_token(&id, &first, Some("07"))));
        assert!(forbidden(check_report_token(
            &id,
            &first,
            Some(&"08".repeat(TOKEN_BYTES))
        )));
        // An attempt without a token takes no report, not even one with an empty token.
        assert!(forbidden(check_report_token(&id, &second, Some(""))));
        assert!(forbidden(check_report_token(&id, &second, None)));
        Ok(())
    }

    #[test]
    fn failed_cleanups_and_unsaved_adoptions_are_explained() {
        let id = SpaceId::new("s");
        assert_eq!(unrecorded_cleanup_message(&id, &[]), None);
        let message = unrecorded_cleanup_message(
            &id,
            &[
                "attempt 3 (/w/s--attempt-3): locked".into(),
                "attempt 4 (/w): gone".into(),
            ],
        )
        .unwrap_or_default();
        assert!(
            message.contains("attempt 3 (/w/s--attempt-3): locked; attempt 4"),
            "{message}"
        );
        assert!(message.contains("later forks skip"), "{message}");
        let unsaved = unsaved_adoption_message(&id, 2, "disk full");
        assert!(unsaved.contains("disk full") && unsaved.contains("adopt it again"));
        assert!(unsaved_adoption_refusal(&id, 2).contains("adopt attempt 2 again"));
    }

    #[test]
    fn attempt_agents_have_their_own_id_and_cannot_act_for_the_space() {
        let id = attempt_agent_id("writer-api", 2);
        assert_eq!(id, "writer-api#attempt-2");
        assert_eq!(
            refuse_attempt_agent("contract.set", &id)
                .err()
                .and_then(|e| e.kind()),
            Some(ErrorCode::Forbidden)
        );
        assert!(refuse_attempt_agent("contract.set", "writer-api").is_ok());
    }

    #[test]
    fn attempt_ports_avoid_spaces_and_other_attempts() -> TestResult {
        let start = *PORT_RANGE.start();
        // The space holds the first port, another space's attempt the third.
        let ports = allocate_attempt_ports(3, [start, start + 2])?;
        assert_eq!(ports, [start + 1, start + 3, start + 4]);
        assert_eq!(allocate_attempt_ports(0, [])?, Vec::<u16>::new());
        assert!(allocate_attempt_ports(2, PORT_RANGE.clone().skip(1)).is_err());
        Ok(())
    }

    #[test]
    fn attempts_are_kept_only_once_saved() -> TestResult {
        let dir = tempfile::tempdir()?;
        let id = SpaceId::new("s");
        let mut attempts = SpaceAttempts::new(Some(dir.path().to_path_buf()));
        assert!(attempts.load(&id)?.attempts.is_empty());
        attempts.change(&id, |set| {
            forked(set, vec![None, None]).map_err(|e| AttemptError::PlanMismatch(e.to_string()))
        })?;
        let refused = attempts.change(&id, |set| set.mark_discarded(9, 1).map(|_| ()));
        assert!(refused.is_err_and(|e| e.contains("no attempt 9")));
        assert_eq!(attempts.live_ports([&id]), [4101, 4102]);
        let mut reread = SpaceAttempts::new(Some(dir.path().to_path_buf()));
        assert_eq!(reread.load(&id)?.attempts.len(), 2);
        attempts.forget(&id);
        assert!(attempts.get(&id).is_none());
        assert!(SpaceAttempts::new(None).load(&id).is_err());
        Ok(())
    }

    #[test]
    fn summaries_mask_secrets_and_say_what_was_reported() -> TestResult {
        let mut set = AttemptSet::new(SpaceId::new("s"));
        forked(&mut set, vec![Some("use tok-424242".into()), None])?;
        let mut params = report(1, "called it with tok-424242");
        params.tests_passed = Some(40);
        params.tests_failed = Some(2);
        params.repro = ReproStatus::Fixed;
        record_report(&mut set, &params, 11)?;
        let redactor = Redactor::new([("TOKEN".to_owned(), "tok-424242".to_owned())]);
        let list = attempt_list(&set, None, &redactor);
        assert_eq!(list.attempts.len(), 2);
        let first = &list.attempts[0];
        assert_eq!(first.state, "done");
        assert_eq!(first.instruction.as_deref(), Some("use [REDACTED:TOKEN]"));
        assert_eq!(
            first.summary.as_deref(),
            Some("called it with [REDACTED:TOKEN]")
        );
        assert_eq!(
            (first.tests_passed, first.tests_failed),
            (Some(40), Some(2))
        );
        assert_eq!(first.repro, "fixed");
        assert_eq!(first.branch, "attempt/s/1");
        assert_eq!(first.port, 4101);
        let second = &list.attempts[1];
        assert_eq!(
            (
                second.state.as_str(),
                second.summary.as_deref(),
                second.repro.as_str()
            ),
            ("running", None, "not_checked")
        );
        assert!(attempt_list(&set, Some(2), &redactor).attempts.is_empty());
        Ok(())
    }

    #[test]
    fn the_last_report_of_a_round_finishes_it_once() -> TestResult {
        let mut set = AttemptSet::new(SpaceId::new("s"));
        forked(&mut set, vec![None, None])?;
        let first = record_report(&mut set, &report(1, "done"), 11)?;
        assert!(!first.round_finished);
        let mut gave_up = report(2, "could not");
        gave_up.gave_up = true;
        let second = record_report(&mut set, &gave_up, 12)?;
        assert_eq!(
            second,
            Reported {
                round: 1,
                round_finished: true,
                was_finished: false
            }
        );
        assert_eq!(set.get(2).map(|a| a.state), Some(AttemptState::Failed));
        let again = record_report(&mut set, &report(1, "better"), 13)?;
        assert!(again.round_finished && again.was_finished);
        assert!(matches!(
            record_report(&mut set, &report(7, "x"), 14),
            Err(AttemptError::NotFound(7))
        ));
        let notice = round_done_notice(&SpaceId::new("s"), 1);
        assert_eq!(
            notice.title,
            "All attempts of round 1 in s are done; review them"
        );
        Ok(())
    }

    #[test]
    fn reports_record_tests_only_when_given() {
        assert_eq!(report_outcome(&report(1, "  x  ")).tests, None);
        let mut params = report(1, "x");
        params.test_command = Some("cargo test".into());
        let tests = report_outcome(&params).tests;
        assert_eq!(
            tests,
            Some(TestSummary {
                command: Some("cargo test".into()),
                passed: 0,
                failed: 0
            })
        );
        assert_eq!(report_outcome(&params).summary, "x");
    }

    #[test]
    fn proposals_need_a_writer_a_count_and_no_extra_instructions() -> TestResult {
        let id = SpaceId::new("s");
        let mut graph = AgentGraph::for_space(&["api".to_owned()]);
        let helper = graph.add_lane(
            AgentRole::Helper {
                kind: xctx_graph::HelperKind::Tests,
            },
            None,
        )?;
        let params =
            |agent: &str, count: u32, instructions: Option<Vec<&str>>| AttemptProposeParams {
                space: "s".into(),
                agent: agent.into(),
                count,
                instructions: instructions.map(|i| i.into_iter().map(str::to_owned).collect()),
                rationale: Some("  ".into()),
            };
        let proposal = attempt_proposal(
            &id,
            &graph,
            &params("writer-api", 3, Some(vec!["a", "", "c", " "])),
        )?;
        assert_eq!(proposal.count, 3);
        assert_eq!(proposal.instructions, ["a", "", "c"]);
        assert_eq!(proposal.rationale, None);
        for bad in [
            params("writer-api", 0, None),
            params("writer-api", 5, None),
            params("writer-api", 1, Some(vec!["a", "b"])),
            params(helper.as_str(), 2, None),
            params("nobody", 2, None),
        ] {
            let error = attempt_proposal(&id, &graph, &bad).err();
            assert_eq!(
                error.and_then(|e| e.kind()),
                Some(ErrorCode::InvalidParams),
                "{bad:?}"
            );
        }
        assert_eq!(
            fork_instructions(3, &["a".into(), " ".into()]),
            [Some("a".into()), None, None]
        );
        Ok(())
    }

    #[test]
    fn live_attempts_stop_ship_and_discard() -> TestResult {
        let id = SpaceId::new("s");
        let mut set = AttemptSet::new(id.clone());
        assert_eq!(live_attempts_refusal(&id, &set, "ship"), None);
        forked(&mut set, vec![None, None])?;
        assert_eq!(
            live_attempts_refusal(&id, &set, "ship").as_deref(),
            Some(
                "s has live attempts 1, 2; adopt or discard them on the review sheet before you ship the space"
            )
        );
        set.mark_discarded(1, 2)?;
        assert!(
            live_attempts_refusal(&id, &set, "discard")
                .is_some_and(|r| r.contains("live attempt 2;") && r.contains("discard it"))
        );
        set.mark_adopted(2, 3)?;
        assert_eq!(live_attempts_refusal(&id, &set, "ship"), None);
        Ok(())
    }

    #[test]
    fn errors_map_to_control_codes() {
        let id = SpaceId::new("s");
        let code = |e| attempt_rpc_error(&id, e).kind();
        assert_eq!(code(AttemptError::NotFound(1)), Some(ErrorCode::NotFound));
        assert_eq!(
            code(AttemptError::BadCount { asked: 9, max: 4 }),
            Some(ErrorCode::InvalidParams)
        );
        assert_eq!(
            code(AttemptError::AlreadyAdopted { adopted: 1 }),
            Some(ErrorCode::Conflict)
        );
    }

    #[test]
    fn scratch_is_copied_without_other_attempts() -> TestResult {
        let dir = tempfile::tempdir()?;
        let space = ScratchDir::in_base(dir.path().join("scratch"), "s")?;
        space.write_artifact("probe/out.log", b"hello")?;
        let first = attempt_scratch(&space, 1)?;
        assert_eq!(first.root(), space.root().join("attempts/1"));
        first.write_artifact("mine.txt", b"1")?;
        let second = attempt_scratch(&space, 2)?;
        assert_eq!(copy_scratch(&space, &second)?, 1);
        assert_eq!(fs::read(second.root().join("probe/out.log"))?, b"hello");
        assert!(!second.root().join("attempts").exists());
        // An empty space copies nothing.
        let empty = ScratchDir::in_base(dir.path().join("scratch"), "other")?;
        assert_eq!(copy_scratch(&empty, &attempt_scratch(&empty, 1)?)?, 0);
        Ok(())
    }

    #[test]
    fn diffs_are_only_for_live_attempts() -> TestResult {
        let id = SpaceId::new("s");
        let mut set = AttemptSet::new(id.clone());
        forked(&mut set, vec![None, None])?;
        set.mark_discarded(2, 1)?;
        let params = |attempt| AttemptDiffParams {
            space: "s".into(),
            attempt,
            path: None,
        };
        assert_eq!(diff_target(&id, &set, &params(1))?.number, 1);
        assert_eq!(
            diff_target(&id, &set, &params(2))
                .err()
                .and_then(|e| e.kind()),
            Some(ErrorCode::Conflict)
        );
        assert_eq!(
            diff_target(&id, &set, &params(3))
                .err()
                .and_then(|e| e.kind()),
            Some(ErrorCode::NotFound)
        );
        Ok(())
    }
}
