//! A space's attempts and the rules for changing them.

use serde::{Deserialize, Serialize};
use xctx_core::pi_session::attempt_session_id;
use xctx_core::{AttemptWorktree, MAX_ATTEMPTS, SpaceId};

use crate::error::{AttemptError, Result};

/// Where an attempt is in its life.
///
/// `Running` becomes `Done` or `Failed` when its agent reports (and may report again). Any live attempt
/// (`Running`, `Done`, `Failed`) can be `Discarded` or, once per round, `Adopted`. Those two are final.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    /// Its agent is working.
    Running,
    /// Its agent reported its work finished.
    Done,
    /// Its agent reported it could not finish.
    Failed,
    /// The user threw it away; its worktree and branch are gone.
    Discarded,
    /// The user merged it into the space.
    Adopted,
}

impl AttemptState {
    /// Whether it still has a worktree and can be adopted or discarded.
    pub fn is_live(self) -> bool {
        matches!(self, Self::Running | Self::Done | Self::Failed)
    }

    /// The name shown and used on the control socket.
    pub fn name(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Discarded => "discarded",
            Self::Adopted => "adopted",
        }
    }
}

/// Tests an attempt's agent ran.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestSummary {
    /// The command, e.g. `cargo test`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// How many passed.
    pub passed: u32,
    /// How many failed.
    pub failed: u32,
}

/// Whether the problem the attempts are about still reproduces.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Repro {
    /// Not checked.
    #[default]
    NotChecked,
    /// The repro passes now: fixed.
    Fixed,
    /// It still fails.
    StillFails,
}

impl Repro {
    /// The name shown and used on the control socket.
    pub fn name(self) -> &'static str {
        match self {
            Self::NotChecked => "not_checked",
            Self::Fixed => "fixed",
            Self::StillFails => "still_fails",
        }
    }
}

/// What an attempt's agent says about its work.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// What it did, in its own words.
    pub summary: String,
    /// Tests it ran, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<TestSummary>,
    /// Whether the problem still reproduces.
    #[serde(default)]
    pub repro: Repro,
}

/// One attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    /// Its worktree, branch, fork point and port.
    pub worktree: AttemptWorktree,
    /// The fork it belongs to, from 1. Each round is one fork of one writer.
    pub round: u32,
    /// The graph node of the writer it was forked from.
    pub agent: String,
    /// The `pi` session it was forked from.
    pub source_session: String,
    /// Its own `pi` session.
    pub session: String,
    /// What it was told to try differently, if anything.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    /// Where it is in its life.
    pub state: AttemptState,
    /// What its agent last reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    /// When it was forked, in seconds since the Unix epoch.
    pub created_at: u64,
    /// When it last changed.
    pub updated_at: u64,
}

impl Attempt {
    /// Its number in the space.
    pub fn number(&self) -> u32 {
        self.worktree.number
    }
}

/// What [`AttemptSet::plan_fork`] reserved: the round and the numbers the new attempts get.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForkPlan {
    /// The new round.
    pub round: u32,
    /// The attempt numbers, in order.
    pub numbers: Vec<u32>,
    /// The writer being forked.
    pub agent: String,
    /// The writer's repo in the space.
    pub repo: String,
    /// One instruction per attempt (empty ones are none).
    pub instructions: Vec<Option<String>>,
}

/// A space's attempts, oldest first.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptSet {
    /// The space.
    pub space: SpaceId,
    /// The next attempt number; numbers are never reused.
    pub next_number: u32,
    /// The next round.
    pub next_round: u32,
    /// Every attempt, discarded and adopted ones included.
    pub attempts: Vec<Attempt>,
}

impl AttemptSet {
    /// A space with no attempts yet.
    pub fn new(space: SpaceId) -> AttemptSet {
        AttemptSet {
            space,
            next_number: 1,
            next_round: 1,
            attempts: Vec::new(),
        }
    }

    /// The attempt with this number.
    pub fn get(&self, number: u32) -> Option<&Attempt> {
        self.attempts.iter().find(|a| a.number() == number)
    }

    fn get_mut(&mut self, number: u32) -> Result<&mut Attempt> {
        self.attempts
            .iter_mut()
            .find(|a| a.number() == number)
            .ok_or(AttemptError::NotFound(number))
    }

    /// The attempts that still have a worktree.
    pub fn live(&self) -> impl Iterator<Item = &Attempt> {
        self.attempts.iter().filter(|a| a.state.is_live())
    }

    /// The attempts of one round, in order.
    pub fn round(&self, round: u32) -> impl Iterator<Item = &Attempt> {
        self.attempts.iter().filter(move |a| a.round == round)
    }

    /// The newest round that has attempts.
    pub fn latest_round(&self) -> Option<u32> {
        self.attempts.iter().map(|a| a.round).max()
    }

    /// The ports live attempts hold, so a new attempt or space does not take one.
    pub fn ports(&self) -> Vec<u16> {
        self.live().map(|a| a.worktree.port).collect()
    }

    /// The attempt whose agent runs as session `session`, for `attempt.report` from that agent.
    pub fn by_session(&self, session: &str) -> Option<&Attempt> {
        self.attempts.iter().find(|a| a.session == session)
    }

    /// Reserve numbers for `instructions.len()` attempts of the writer `agent` in `repo`. Nothing changes until
    /// [`AttemptSet::record_fork`]. Fails when the count is 0 or above [`MAX_ATTEMPTS`], or when the space would
    /// have more than [`MAX_ATTEMPTS`] live attempts.
    pub fn plan_fork(
        &self,
        agent: &str,
        repo: &str,
        instructions: Vec<Option<String>>,
    ) -> Result<ForkPlan> {
        let count = u32::try_from(instructions.len()).unwrap_or(u32::MAX);
        if count == 0 || count > MAX_ATTEMPTS {
            return Err(AttemptError::BadCount {
                asked: count,
                max: MAX_ATTEMPTS,
            });
        }
        let live = u32::try_from(self.live().count()).unwrap_or(u32::MAX);
        if live.saturating_add(count) > MAX_ATTEMPTS {
            return Err(AttemptError::TooMany {
                live,
                max: MAX_ATTEMPTS,
            });
        }
        Ok(ForkPlan {
            round: self.next_round,
            numbers: (self.next_number..self.next_number + count).collect(),
            agent: agent.to_owned(),
            repo: repo.to_owned(),
            instructions: instructions
                .into_iter()
                .map(|i| i.filter(|t| !t.trim().is_empty()))
                .collect(),
        })
    }

    /// Record the worktrees made for `plan` as running attempts, forked from session `source_session`. Each gets
    /// the session id `xctx-<slug>-<agent>-attempt-<n>`. Returns the new attempts.
    pub fn record_fork(
        &mut self,
        plan: &ForkPlan,
        worktrees: Vec<AttemptWorktree>,
        source_session: &str,
        now: u64,
    ) -> Result<Vec<Attempt>> {
        if plan.round != self.next_round || plan.numbers.first() != Some(&self.next_number) {
            return Err(AttemptError::PlanMismatch(
                "the set changed since the plan was made".into(),
            ));
        }
        let numbers: Vec<u32> = worktrees.iter().map(|w| w.number).collect();
        if numbers != plan.numbers || worktrees.iter().any(|w| w.repo != plan.repo) {
            return Err(AttemptError::PlanMismatch(format!(
                "planned attempts {:?} of {}, got {numbers:?}",
                plan.numbers, plan.repo
            )));
        }
        let slug = self.space.as_str().to_owned();
        let made: Vec<Attempt> = worktrees
            .into_iter()
            .zip(plan.instructions.iter().cloned())
            .map(|(worktree, instruction)| Attempt {
                session: attempt_session_id(&slug, &plan.agent, worktree.number),
                worktree,
                round: plan.round,
                agent: plan.agent.clone(),
                source_session: source_session.to_owned(),
                instruction,
                state: AttemptState::Running,
                outcome: None,
                created_at: now,
                updated_at: now,
            })
            .collect();
        self.next_number += u32::try_from(made.len()).unwrap_or(0);
        self.next_round += 1;
        self.attempts.extend(made.iter().cloned());
        Ok(made)
    }

    /// Record what attempt `number`'s agent reported: `Failed` when `failed`, else `Done`. A live attempt may
    /// report again; a discarded or adopted one may not.
    pub fn report(
        &mut self,
        number: u32,
        outcome: Outcome,
        failed: bool,
        now: u64,
    ) -> Result<&Attempt> {
        let attempt = self.get_mut(number)?;
        if !attempt.state.is_live() {
            return Err(AttemptError::WrongState {
                number,
                state: attempt.state,
                action: "reported on",
            });
        }
        attempt.state = if failed {
            AttemptState::Failed
        } else {
            AttemptState::Done
        };
        attempt.outcome = Some(outcome);
        attempt.updated_at = now;
        Ok(attempt)
    }

    /// Check that attempt `number` can be adopted: it is live and no other attempt of its round was adopted.
    pub fn check_adopt(&self, number: u32) -> Result<&Attempt> {
        let attempt = self.get(number).ok_or(AttemptError::NotFound(number))?;
        if !attempt.state.is_live() {
            return Err(AttemptError::WrongState {
                number,
                state: attempt.state,
                action: "adopted",
            });
        }
        if let Some(adopted) = self
            .round(attempt.round)
            .find(|a| a.state == AttemptState::Adopted)
        {
            return Err(AttemptError::AlreadyAdopted {
                adopted: adopted.number(),
            });
        }
        Ok(attempt)
    }

    /// Mark attempt `number` adopted, after its work was merged into the space.
    pub fn mark_adopted(&mut self, number: u32, now: u64) -> Result<&Attempt> {
        self.check_adopt(number)?;
        let attempt = self.get_mut(number)?;
        attempt.state = AttemptState::Adopted;
        attempt.updated_at = now;
        Ok(attempt)
    }

    /// Mark attempt `number` discarded, after its worktree and branch were removed.
    pub fn mark_discarded(&mut self, number: u32, now: u64) -> Result<&Attempt> {
        let attempt = self.get_mut(number)?;
        if !attempt.state.is_live() {
            return Err(AttemptError::WrongState {
                number,
                state: attempt.state,
                action: "discarded",
            });
        }
        attempt.state = AttemptState::Discarded;
        attempt.updated_at = now;
        Ok(attempt)
    }

    /// Whether every attempt of `round` has reported (or is no longer live), so the review sheet can open.
    pub fn round_finished(&self, round: u32) -> bool {
        let mut any = false;
        for attempt in self.round(round) {
            any = true;
            if attempt.state == AttemptState::Running {
                return false;
            }
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

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

    fn forked(set: &mut AttemptSet, count: usize) -> Result<Vec<Attempt>> {
        let plan = set.plan_fork("writer-api", "api", vec![None; count])?;
        let trees = plan.numbers.iter().map(|&n| worktree(n)).collect();
        set.record_fork(&plan, trees, "xctx-s-writer-api", 10)
    }

    #[test]
    fn a_fork_gets_new_numbers_a_round_and_forked_session_ids() -> Result<()> {
        let mut set = AttemptSet::new(SpaceId::new("s"));
        let first = forked(&mut set, 2)?;
        assert_eq!(
            first.iter().map(Attempt::number).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(first[1].session, "xctx-s-writer-api-attempt-2");
        assert_eq!(first[0].round, 1);
        assert_eq!(set.ports(), [4101, 4102]);
        set.mark_discarded(1, 11)?;
        let second = forked(&mut set, 1)?;
        assert_eq!(
            (second[0].number(), second[0].round),
            (3, 2),
            "numbers are never reused"
        );
        assert_eq!(
            set.by_session("xctx-s-writer-api-attempt-3")
                .map(Attempt::number),
            Some(3)
        );
        Ok(())
    }

    #[test]
    fn forks_are_limited_and_plans_must_match_what_was_made() -> Result<()> {
        let mut set = AttemptSet::new(SpaceId::new("s"));
        assert!(matches!(
            set.plan_fork("w", "api", vec![]),
            Err(AttemptError::BadCount { asked: 0, .. })
        ));
        assert!(matches!(
            set.plan_fork("w", "api", vec![None; 5]),
            Err(AttemptError::BadCount { asked: 5, .. })
        ));
        forked(&mut set, 3)?;
        assert!(matches!(
            set.plan_fork("w", "api", vec![None; 2]),
            Err(AttemptError::TooMany { live: 3, .. })
        ));
        let plan = set.plan_fork("w", "api", vec![Some("  ".into())])?;
        assert_eq!(plan.instructions, [None], "blank instructions are none");
        assert!(matches!(
            set.record_fork(&plan, vec![worktree(9)], "src", 1),
            Err(AttemptError::PlanMismatch(_))
        ));
        Ok(())
    }

    #[test]
    fn reports_move_running_attempts_and_finish_the_round() -> Result<()> {
        let mut set = AttemptSet::new(SpaceId::new("s"));
        forked(&mut set, 2)?;
        assert!(!set.round_finished(1));
        set.report(1, Outcome::default(), false, 12)?;
        set.report(2, Outcome::default(), true, 12)?;
        assert_eq!(set.get(2).map(|a| a.state), Some(AttemptState::Failed));
        assert!(set.round_finished(1));
        assert!(!set.round_finished(7));
        // A done attempt may report again.
        assert_eq!(
            set.report(1, Outcome::default(), true, 13)?.state,
            AttemptState::Failed
        );
        Ok(())
    }

    #[test]
    fn one_adoption_per_round_and_final_states_stay_final() -> Result<()> {
        let mut set = AttemptSet::new(SpaceId::new("s"));
        forked(&mut set, 3)?;
        set.mark_adopted(2, 20)?;
        assert!(matches!(
            set.check_adopt(1),
            Err(AttemptError::AlreadyAdopted { adopted: 2 })
        ));
        set.mark_discarded(1, 21)?;
        for number in [1, 2] {
            assert!(matches!(
                set.mark_discarded(number, 22),
                Err(AttemptError::WrongState { .. })
            ));
            assert!(matches!(
                set.report(number, Outcome::default(), false, 22),
                Err(AttemptError::WrongState { .. })
            ));
        }
        assert!(matches!(set.check_adopt(9), Err(AttemptError::NotFound(9))));
        // A later round can adopt again.
        forked(&mut set, 1)?;
        assert!(set.check_adopt(4).is_ok());
        Ok(())
    }
}
