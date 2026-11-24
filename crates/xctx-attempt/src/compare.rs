//! What the review sheet shows for one round: a column per attempt, secrets masked.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use xctx_core::Redactor;
use xctx_core::git::FileChange;

use crate::set::{AttemptSet, AttemptState, Repro, TestSummary};

/// One attempt's column.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Column {
    /// The attempt's number.
    pub number: u32,
    /// What it was told to try, masked.
    pub instruction: Option<String>,
    /// Where it is.
    pub state: AttemptState,
    /// Its agent's summary, masked; `None` before it reports.
    pub summary: Option<String>,
    /// Tests it ran.
    pub tests: Option<TestSummary>,
    /// Whether the problem still reproduces.
    pub repro: Repro,
    /// The files it changed since the fork; `None` when they could not be read (its worktree is gone).
    pub files: Option<Vec<FileChange>>,
    /// Lines added across `files` (binary files count 0).
    pub added: u32,
    /// Lines removed across `files`.
    pub removed: u32,
}

/// The review sheet's content for one round.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comparison {
    /// The round.
    pub round: u32,
    /// One column per attempt of the round, in number order, discarded ones included.
    pub columns: Vec<Column>,
    /// The adopted attempt, if one was.
    pub adopted: Option<u32>,
}

/// The comparison of `round` in `set`, with each attempt's changed files from `changes` (computed by the caller
/// from the worktrees) and every text masked with `redactor`.
pub fn comparison(
    set: &AttemptSet,
    round: u32,
    changes: &BTreeMap<u32, Vec<FileChange>>,
    redactor: &Redactor,
) -> Comparison {
    let mask = |text: &str| redactor.redact(text).into_owned();
    let columns: Vec<Column> = set
        .round(round)
        .map(|attempt| {
            let files = changes.get(&attempt.number()).cloned();
            let (added, removed) = files.iter().flatten().fold((0u32, 0u32), |(a, r), f| {
                (
                    a.saturating_add(f.added.unwrap_or(0)),
                    r.saturating_add(f.removed.unwrap_or(0)),
                )
            });
            let outcome = attempt.outcome.as_ref();
            Column {
                number: attempt.number(),
                instruction: attempt.instruction.as_deref().map(mask),
                state: attempt.state,
                summary: outcome.map(|o| mask(&o.summary)),
                tests: outcome.and_then(|o| o.tests.clone()).map(|mut t| {
                    t.command = t.command.as_deref().map(mask);
                    t
                }),
                repro: outcome.map(|o| o.repro).unwrap_or_default(),
                files,
                added,
                removed,
            }
        })
        .collect();
    Comparison {
        round,
        adopted: columns
            .iter()
            .find(|c| c.state == AttemptState::Adopted)
            .map(|c| c.number),
        columns,
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use xctx_core::{AttemptWorktree, SpaceId};

    use super::*;
    use crate::set::Outcome;

    #[test]
    fn columns_total_lines_mask_secrets_and_note_the_winner() -> crate::Result<()> {
        let mut set = AttemptSet::new(SpaceId::new("s"));
        let plan = set.plan_fork("w", "api", vec![Some("use tok-777777".into()), None])?;
        let trees = plan
            .numbers
            .iter()
            .map(|&number| AttemptWorktree {
                number,
                repo: "api".into(),
                repo_path: "/code/api".into(),
                worktree: PathBuf::from(format!("/w/{number}")),
                branch: format!("attempt/s/{number}"),
                fork_commit: "abc".into(),
                port: 4100,
            })
            .collect();
        set.record_fork(&plan, trees, "src", 0)?;
        set.report(
            1,
            Outcome {
                summary: "called the api with tok-777777".into(),
                tests: Some(TestSummary {
                    command: Some("cargo test".into()),
                    passed: 41,
                    failed: 1,
                }),
                repro: Repro::Fixed,
            },
            false,
            1,
        )?;
        set.mark_adopted(1, 2)?;
        let changes = BTreeMap::from([(
            1,
            vec![
                FileChange {
                    path: "a.rs".into(),
                    added: Some(3),
                    removed: Some(1),
                },
                FileChange {
                    path: "b.png".into(),
                    added: None,
                    removed: None,
                },
            ],
        )]);
        let redactor = Redactor::new([("T".to_owned(), "tok-777777".to_owned())]);
        let view = comparison(&set, 1, &changes, &redactor);
        assert_eq!(view.columns.len(), 2);
        assert_eq!(view.adopted, Some(1));
        let first = &view.columns[0];
        assert_eq!((first.added, first.removed), (3, 1));
        assert_eq!(
            first.summary.as_deref(),
            Some("called the api with [REDACTED:T]")
        );
        assert_eq!(first.instruction.as_deref(), Some("use [REDACTED:T]"));
        assert_eq!(first.repro, Repro::Fixed);
        let second = &view.columns[1];
        assert_eq!(second.summary, None);
        assert_eq!(second.files, None);
        assert_eq!(second.state, AttemptState::Running);
        Ok(())
    }
}
