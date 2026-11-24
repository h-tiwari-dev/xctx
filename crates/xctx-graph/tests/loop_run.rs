//! The structured loop under each autonomy level.

use xctx_graph::{AutonomyLevel, LoopDecision, LoopError, LoopRun, LoopStage, LoopStatus};

#[test]
fn stages_cycle_in_order() {
    assert_eq!(LoopStage::Write.next(), LoopStage::Test);
    assert_eq!(LoopStage::Test.next(), LoopStage::Probe);
    assert_eq!(LoopStage::Probe.next(), LoopStage::Review);
    assert_eq!(LoopStage::Review.next(), LoopStage::Write);
}

#[test]
fn a_new_loop_is_idle_at_write_and_asks_each_step_by_default() {
    let run = LoopRun::default();
    assert_eq!(run.autonomy(), AutonomyLevel::AskEachStep);
    assert_eq!(run.stage(), LoopStage::Write);
    assert_eq!(run.status(), &LoopStatus::Idle);
    assert_eq!(run.iteration(), 0);
    assert_eq!(run.turns_used(), 0);
    assert!(run.history().is_empty());
}

#[test]
fn advance_needs_a_running_loop() {
    let mut run = LoopRun::new(AutonomyLevel::UntilChecksPass);
    assert_eq!(
        run.advance(true),
        Err(LoopError::NotRunning {
            status: LoopStatus::Idle
        })
    );
}

#[test]
fn ask_each_step_waits_for_approval_before_every_stage() -> Result<(), LoopError> {
    let mut run = LoopRun::new(AutonomyLevel::AskEachStep);
    run.start()?;
    assert_eq!(run.iteration(), 1);
    for (from, to) in [
        (LoopStage::Write, LoopStage::Test),
        (LoopStage::Test, LoopStage::Probe),
        (LoopStage::Probe, LoopStage::Review),
    ] {
        assert_eq!(run.advance(true)?, LoopDecision::NeedsApproval(to));
        assert_eq!(run.stage(), from);
        assert_eq!(
            run.status(),
            &LoopStatus::AwaitingApproval { next_stage: to }
        );
        assert!(matches!(
            run.advance(true),
            Err(LoopError::NotRunning { .. })
        ));
        assert!(matches!(run.start(), Err(LoopError::CannotStart { .. })));
        assert_eq!(run.approve_next_stage()?, LoopDecision::Continue(to));
        assert_eq!(run.stage(), to);
        assert_eq!(run.status(), &LoopStatus::Running);
    }
    assert_eq!(run.advance(true)?, LoopDecision::Completed);
    assert_eq!(run.status(), &LoopStatus::Passed);
    assert_eq!(run.history().len(), 3);
    assert!(
        run.history()
            .iter()
            .all(|t| t.checks_passed && t.iteration == 1)
    );
    Ok(())
}

#[test]
fn approval_is_refused_when_nothing_awaits_it() -> Result<(), LoopError> {
    let mut run = LoopRun::new(AutonomyLevel::AskEachStep);
    assert_eq!(
        run.approve_next_stage(),
        Err(LoopError::NotAwaitingApproval {
            status: LoopStatus::Idle
        })
    );
    run.start()?;
    assert!(matches!(
        run.approve_next_stage(),
        Err(LoopError::NotAwaitingApproval { .. })
    ));
    Ok(())
}

#[test]
fn failed_checks_go_back_to_write_in_a_new_iteration() -> Result<(), LoopError> {
    let mut run = LoopRun::new(AutonomyLevel::AskEachStep);
    run.start()?;
    run.advance(true)?;
    run.approve_next_stage()?;
    assert_eq!(run.stage(), LoopStage::Test);
    assert_eq!(
        run.advance(false)?,
        LoopDecision::NeedsApproval(LoopStage::Write)
    );
    assert_eq!(run.iteration(), 1);
    assert_eq!(
        run.approve_next_stage()?,
        LoopDecision::Continue(LoopStage::Write)
    );
    assert_eq!(run.iteration(), 2);
    let last = run.history().last().copied();
    assert_eq!(
        last.map(|t| (t.from, t.to)),
        Some((LoopStage::Test, LoopStage::Write))
    );
    assert_eq!(
        last.map(|t| (t.iteration, t.checks_passed)),
        Some((2, false))
    );
    Ok(())
}

#[test]
fn until_checks_pass_runs_alone_until_review_passes() -> Result<(), LoopError> {
    let mut run = LoopRun::new(AutonomyLevel::UntilChecksPass);
    run.start()?;
    assert_eq!(run.advance(true)?, LoopDecision::Continue(LoopStage::Test));
    assert_eq!(
        run.advance(false)?,
        LoopDecision::Continue(LoopStage::Write)
    );
    assert_eq!(run.iteration(), 2);
    assert_eq!(run.advance(true)?, LoopDecision::Continue(LoopStage::Test));
    assert_eq!(run.advance(true)?, LoopDecision::Continue(LoopStage::Probe));
    assert_eq!(
        run.advance(true)?,
        LoopDecision::Continue(LoopStage::Review)
    );
    assert_eq!(
        run.advance(false)?,
        LoopDecision::Continue(LoopStage::Write)
    );
    assert_eq!(run.iteration(), 3);
    run.record_turn(1_000);
    assert_eq!(run.status(), &LoopStatus::Running);
    for _ in 0..3 {
        run.advance(true)?;
    }
    assert_eq!(run.stage(), LoopStage::Review);
    assert_eq!(run.advance(true)?, LoopDecision::Completed);
    assert_eq!(run.status(), &LoopStatus::Passed);
    assert!(matches!(run.start(), Err(LoopError::CannotStart { .. })));
    Ok(())
}

#[test]
fn a_blocked_loop_stops_and_start_goes_on_at_the_same_stage() -> Result<(), LoopError> {
    let mut run = LoopRun::new(AutonomyLevel::UntilChecksPass);
    run.start()?;
    run.advance(true)?;
    run.block("tests cannot reach the database");
    assert_eq!(
        run.status(),
        &LoopStatus::Blocked {
            reason: "tests cannot reach the database".into()
        }
    );
    assert!(matches!(
        run.advance(true),
        Err(LoopError::NotRunning { .. })
    ));
    run.start()?;
    assert_eq!(run.status(), &LoopStatus::Running);
    assert_eq!(run.stage(), LoopStage::Test);
    assert_eq!(run.iteration(), 1);
    Ok(())
}

#[test]
fn within_budget_stops_when_iterations_run_out() -> Result<(), LoopError> {
    let mut run = LoopRun::new(AutonomyLevel::WithinBudget {
        max_iterations: 1,
        max_turns: 100,
    });
    run.start()?;
    assert_eq!(run.advance(true)?, LoopDecision::Continue(LoopStage::Test));
    assert_eq!(run.advance(true)?, LoopDecision::Continue(LoopStage::Probe));
    assert_eq!(
        run.advance(true)?,
        LoopDecision::Continue(LoopStage::Review)
    );
    assert_eq!(run.advance(false)?, LoopDecision::BudgetExhausted);
    assert_eq!(run.status(), &LoopStatus::BudgetExhausted);
    assert_eq!(run.stage(), LoopStage::Write);
    assert_eq!(run.iteration(), 2);
    assert_eq!(run.start(), Err(LoopError::BudgetExhausted));
    assert_eq!(run.status(), &LoopStatus::BudgetExhausted);

    run.set_autonomy(AutonomyLevel::WithinBudget {
        max_iterations: 2,
        max_turns: 100,
    });
    assert_eq!(run.status(), &LoopStatus::BudgetExhausted);
    run.start()?;
    assert_eq!(run.status(), &LoopStatus::Running);
    assert_eq!(run.stage(), LoopStage::Write);
    Ok(())
}

#[test]
fn within_budget_stops_when_turns_run_out() -> Result<(), LoopError> {
    let mut run = LoopRun::new(AutonomyLevel::WithinBudget {
        max_iterations: 10,
        max_turns: 5,
    });
    run.start()?;
    assert_eq!(run.record_turn(3), &LoopStatus::Running);
    assert_eq!(run.advance(true)?, LoopDecision::Continue(LoopStage::Test));
    assert_eq!(run.record_turn(2), &LoopStatus::BudgetExhausted);
    assert_eq!(run.turns_used(), 5);
    assert!(matches!(
        run.advance(true),
        Err(LoopError::NotRunning { .. })
    ));
    assert_eq!(run.start(), Err(LoopError::BudgetExhausted));

    run.set_autonomy(AutonomyLevel::UntilChecksPass);
    run.start()?;
    assert_eq!(run.stage(), LoopStage::Test);
    assert_eq!(run.advance(true)?, LoopDecision::Continue(LoopStage::Probe));
    Ok(())
}

#[test]
fn a_zero_budget_cannot_start() {
    let mut run = LoopRun::new(AutonomyLevel::WithinBudget {
        max_iterations: 0,
        max_turns: 10,
    });
    assert_eq!(run.start(), Err(LoopError::BudgetExhausted));
    assert_eq!(run.status(), &LoopStatus::Idle);
    assert_eq!(run.iteration(), 0);
}

#[test]
fn lowering_the_budget_of_a_running_loop_stops_it() -> Result<(), LoopError> {
    let mut run = LoopRun::new(AutonomyLevel::UntilChecksPass);
    run.start()?;
    run.record_turn(4);
    run.set_autonomy(AutonomyLevel::WithinBudget {
        max_iterations: 5,
        max_turns: 4,
    });
    assert_eq!(run.status(), &LoopStatus::BudgetExhausted);
    Ok(())
}

#[test]
fn switching_away_from_ask_each_step_still_needs_the_pending_approval() -> Result<(), LoopError> {
    let mut run = LoopRun::new(AutonomyLevel::AskEachStep);
    run.start()?;
    run.advance(true)?;
    run.set_autonomy(AutonomyLevel::UntilChecksPass);
    assert_eq!(
        run.status(),
        &LoopStatus::AwaitingApproval {
            next_stage: LoopStage::Test
        }
    );
    assert_eq!(
        run.approve_next_stage()?,
        LoopDecision::Continue(LoopStage::Test)
    );
    assert_eq!(run.advance(true)?, LoopDecision::Continue(LoopStage::Probe));
    Ok(())
}

#[test]
fn approving_into_an_exhausted_budget_stops_the_loop() -> Result<(), LoopError> {
    let mut run = LoopRun::new(AutonomyLevel::AskEachStep);
    run.start()?;
    run.advance(false)?;
    run.set_autonomy(AutonomyLevel::WithinBudget {
        max_iterations: 1,
        max_turns: 10,
    });
    assert_eq!(run.approve_next_stage()?, LoopDecision::BudgetExhausted);
    assert_eq!(run.status(), &LoopStatus::BudgetExhausted);
    Ok(())
}

#[test]
fn reset_starts_over_and_keeps_the_autonomy() -> Result<(), LoopError> {
    let level = AutonomyLevel::WithinBudget {
        max_iterations: 3,
        max_turns: 30,
    };
    let mut run = LoopRun::new(level);
    run.start()?;
    run.record_turn(7);
    run.advance(true)?;
    run.reset();
    assert_eq!(run, LoopRun::new(level));
    Ok(())
}

#[test]
fn starting_a_running_loop_does_nothing() -> Result<(), LoopError> {
    let mut run = LoopRun::new(AutonomyLevel::UntilChecksPass);
    run.start()?;
    run.advance(true)?;
    let before = run.clone();
    run.start()?;
    assert_eq!(run, before);
    Ok(())
}
