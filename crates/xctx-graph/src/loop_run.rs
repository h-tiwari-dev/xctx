//! A space's structured loop: `Write -> Test -> Probe -> Review`, advanced under an autonomy level.

use serde::{Deserialize, Serialize};

/// How many stage transitions [`LoopRun::history`] keeps; the oldest beyond this are dropped.
pub const MAX_LOOP_HISTORY: usize = 256;

/// A stage of the loop. The stages run in order and `Review` wraps back to `Write`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopStage {
    /// The writers change their repos.
    #[default]
    Write,
    /// The tests helper runs the tests.
    Test,
    /// The probe helper exercises the running system.
    Probe,
    /// The reviewer reviews the change.
    Review,
}

impl LoopStage {
    /// The stage after this one; `Review` is followed by `Write`.
    pub fn next(self) -> LoopStage {
        match self {
            LoopStage::Write => LoopStage::Test,
            LoopStage::Test => LoopStage::Probe,
            LoopStage::Probe => LoopStage::Review,
            LoopStage::Review => LoopStage::Write,
        }
    }
}

/// How far the loop may go without the user.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum AutonomyLevel {
    /// Stop before every stage change until the user approves it. The safe default.
    #[default]
    AskEachStep,
    /// Run without asking until the loop starts iteration `max_iterations + 1` or has used `max_turns` turns.
    WithinBudget {
        /// How many passes through the loop may run.
        max_iterations: u32,
        /// How many agent turns may be used in total.
        max_turns: u32,
    },
    /// Run without asking until review passes or the loop is blocked.
    UntilChecksPass,
}

/// Where the loop is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LoopStatus {
    /// Not started; [`LoopRun::start`] starts it.
    #[default]
    Idle,
    /// The agents of the current stage are working.
    Running,
    /// Waiting for the user to approve moving to `next_stage` ([`LoopRun::approve_next_stage`]).
    AwaitingApproval {
        /// The stage the loop moves to once approved.
        next_stage: LoopStage,
    },
    /// The budget of [`AutonomyLevel::WithinBudget`] is used up. Raise it with [`LoopRun::set_autonomy`] and call
    /// [`LoopRun::start`] to go on.
    BudgetExhausted,
    /// Review passed; the loop is finished. [`LoopRun::reset`] starts over.
    Passed,
    /// Stopped by a failure; [`LoopRun::start`] goes on at the same stage.
    Blocked {
        /// What went wrong.
        reason: String,
    },
}

/// One stage change, for the loop's history.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct StageTransition {
    /// The stage that finished.
    pub from: LoopStage,
    /// The stage that started.
    pub to: LoopStage,
    /// The iteration the loop was in once `to` started.
    pub iteration: u32,
    /// False when `from`'s checks failed and the loop went back to `Write`.
    pub checks_passed: bool,
    /// When `to` started, in seconds since the Unix epoch; 0 in histories saved before stage changes were timed.
    #[serde(default)]
    pub at: u64,
}

/// What [`LoopRun::advance`] or [`LoopRun::approve_next_stage`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopDecision {
    /// The loop moved to this stage and is running it.
    Continue(LoopStage),
    /// The loop stays where it is until the user approves moving to this stage.
    NeedsApproval(LoopStage),
    /// Review passed; the loop is finished.
    Completed,
    /// The loop moved to its next stage but stopped there because the budget is used up.
    BudgetExhausted,
}

/// A loop operation that does not apply to the loop's status.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LoopError {
    /// [`LoopRun::advance`] needs a running loop.
    #[error("the loop is not running (it is {status:?})")]
    NotRunning {
        /// The loop's status.
        status: LoopStatus,
    },
    /// [`LoopRun::approve_next_stage`] needs a loop that is awaiting approval.
    #[error("the loop is not awaiting approval (it is {status:?})")]
    NotAwaitingApproval {
        /// The loop's status.
        status: LoopStatus,
    },
    /// [`LoopRun::start`] does not apply: the loop is awaiting approval or has passed.
    #[error("the loop cannot start while it is {status:?}")]
    CannotStart {
        /// The loop's status.
        status: LoopStatus,
    },
    /// [`LoopRun::start`] was refused because the budget is used up.
    #[error("the loop's budget is used up")]
    BudgetExhausted,
}

/// A space's loop: the stage it is in, how far it may go alone, and what it has done.
///
/// [`LoopRun::advance`] is called when the current stage finishes, saying whether its checks passed. Passing
/// review finishes the loop. Passing any other stage moves to the next stage; failing any stage goes back to
/// `Write` and starts a new iteration. Whether the move happens at once, waits for the user, or stops on the
/// budget depends on the [`AutonomyLevel`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LoopRun {
    stage: LoopStage,
    autonomy: AutonomyLevel,
    status: LoopStatus,
    iteration: u32,
    turns_used: u32,
    history: Vec<StageTransition>,
}

impl Default for LoopRun {
    fn default() -> Self {
        LoopRun::new(AutonomyLevel::default())
    }
}

impl LoopRun {
    /// A loop that has not started, at `Write`, under `autonomy`.
    pub fn new(autonomy: AutonomyLevel) -> LoopRun {
        LoopRun {
            stage: LoopStage::Write,
            autonomy,
            status: LoopStatus::Idle,
            iteration: 0,
            turns_used: 0,
            history: Vec::new(),
        }
    }

    /// The stage being run, or the one that runs next when the loop is not running.
    pub fn stage(&self) -> LoopStage {
        self.stage
    }

    /// How far the loop may go alone.
    pub fn autonomy(&self) -> AutonomyLevel {
        self.autonomy
    }

    /// Where the loop is.
    pub fn status(&self) -> &LoopStatus {
        &self.status
    }

    /// The pass through the loop, counting from 1 once started; 0 before.
    pub fn iteration(&self) -> u32 {
        self.iteration
    }

    /// The agent turns used, from [`LoopRun::record_turn`].
    pub fn turns_used(&self) -> u32 {
        self.turns_used
    }

    /// The latest stage changes, oldest first, at most [`MAX_LOOP_HISTORY`].
    pub fn history(&self) -> &[StageTransition] {
        &self.history
    }

    /// Start or go on running the current stage: from `Idle` (beginning iteration 1), `Blocked`, or
    /// `BudgetExhausted`. Starting a running loop does nothing.
    ///
    /// Fails with [`LoopError::BudgetExhausted`] when the budget has no room, and [`LoopError::CannotStart`] when
    /// the loop is awaiting approval or has passed.
    pub fn start(&mut self) -> Result<(), LoopError> {
        match self.status {
            LoopStatus::Running => Ok(()),
            LoopStatus::Idle | LoopStatus::Blocked { .. } | LoopStatus::BudgetExhausted => {
                let iteration = self.iteration.max(1);
                if self.autonomy.is_exhausted(iteration, self.turns_used) {
                    return Err(LoopError::BudgetExhausted);
                }
                self.iteration = iteration;
                self.status = LoopStatus::Running;
                Ok(())
            }
            LoopStatus::AwaitingApproval { .. } | LoopStatus::Passed => {
                Err(LoopError::CannotStart {
                    status: self.status.clone(),
                })
            }
        }
    }

    /// Change the autonomy level. A running loop whose new budget is already used up stops with
    /// `BudgetExhausted`. A loop awaiting approval keeps waiting for that one approval; a loop stopped on its budget
    /// stays stopped until [`LoopRun::start`].
    pub fn set_autonomy(&mut self, autonomy: AutonomyLevel) {
        self.autonomy = autonomy;
        if self.status == LoopStatus::Running && self.budget_exhausted() {
            self.status = LoopStatus::BudgetExhausted;
        }
    }

    /// Count `turns` more agent turns. A running loop that reaches `max_turns` stops with `BudgetExhausted`.
    /// Returns the loop's status.
    pub fn record_turn(&mut self, turns: u32) -> &LoopStatus {
        self.turns_used = self.turns_used.saturating_add(turns);
        if self.status == LoopStatus::Running && self.budget_exhausted() {
            self.status = LoopStatus::BudgetExhausted;
        }
        &self.status
    }

    /// The current stage finished; `checks_passed` says whether its checks passed. See [`LoopRun`] for where the
    /// loop goes next. Fails with [`LoopError::NotRunning`] unless the loop is running.
    pub fn advance(&mut self, checks_passed: bool) -> Result<LoopDecision, LoopError> {
        if self.status != LoopStatus::Running {
            return Err(LoopError::NotRunning {
                status: self.status.clone(),
            });
        }
        if checks_passed && self.stage == LoopStage::Review {
            self.status = LoopStatus::Passed;
            return Ok(LoopDecision::Completed);
        }
        let next = if checks_passed {
            self.stage.next()
        } else {
            LoopStage::Write
        };
        if self.autonomy == AutonomyLevel::AskEachStep {
            self.status = LoopStatus::AwaitingApproval { next_stage: next };
            return Ok(LoopDecision::NeedsApproval(next));
        }
        Ok(self.move_to(next))
    }

    /// Approve the stage change the loop is waiting for, and run the new stage. Fails with
    /// [`LoopError::NotAwaitingApproval`] unless the loop is awaiting approval.
    pub fn approve_next_stage(&mut self) -> Result<LoopDecision, LoopError> {
        match self.status {
            LoopStatus::AwaitingApproval { next_stage } => Ok(self.move_to(next_stage)),
            _ => Err(LoopError::NotAwaitingApproval {
                status: self.status.clone(),
            }),
        }
    }

    /// Stop the loop because of a failure. [`LoopRun::start`] goes on at the same stage.
    pub fn block(&mut self, reason: impl Into<String>) {
        self.status = LoopStatus::Blocked {
            reason: reason.into(),
        };
    }

    /// Start over: back to `Write`, not started, with no turns or history. The autonomy level is kept.
    pub fn reset(&mut self) {
        *self = LoopRun::new(self.autonomy);
    }

    /// Move to `next`, record it, and run it unless the budget is used up. Moving to `Write` starts a new
    /// iteration; it only happens after review or after failed checks.
    fn move_to(&mut self, next: LoopStage) -> LoopDecision {
        if next == LoopStage::Write {
            self.iteration = self.iteration.saturating_add(1);
        }
        if self.history.len() >= MAX_LOOP_HISTORY {
            let excess = self.history.len() + 1 - MAX_LOOP_HISTORY;
            self.history.drain(..excess);
        }
        self.history.push(StageTransition {
            from: self.stage,
            to: next,
            iteration: self.iteration,
            checks_passed: next != LoopStage::Write,
            at: unix_now(),
        });
        self.stage = next;
        if self.budget_exhausted() {
            self.status = LoopStatus::BudgetExhausted;
            LoopDecision::BudgetExhausted
        } else {
            self.status = LoopStatus::Running;
            LoopDecision::Continue(next)
        }
    }

    fn budget_exhausted(&self) -> bool {
        self.autonomy.is_exhausted(self.iteration, self.turns_used)
    }
}

/// Seconds since the Unix epoch; 0 when the clock is before it.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

impl AutonomyLevel {
    fn is_exhausted(self, iteration: u32, turns_used: u32) -> bool {
        match self {
            AutonomyLevel::WithinBudget {
                max_iterations,
                max_turns,
            } => iteration > max_iterations || turns_used >= max_turns,
            AutonomyLevel::AskEachStep | AutonomyLevel::UntilChecksPass => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_is_bounded() -> Result<(), LoopError> {
        let mut run = LoopRun::new(AutonomyLevel::UntilChecksPass);
        run.start()?;
        for _ in 0..MAX_LOOP_HISTORY + 10 {
            run.advance(false)?;
        }
        assert_eq!(run.history().len(), MAX_LOOP_HISTORY);
        let last = run.history().last().copied();
        assert_eq!(
            last.map(|t| t.iteration),
            Some(u32::try_from(MAX_LOOP_HISTORY).unwrap_or(u32::MAX) + 11)
        );
        Ok(())
    }
}
