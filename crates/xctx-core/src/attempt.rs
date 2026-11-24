//! Attempts: competing copies of one writer's work, each in its own hidden worktree.
//!
//! [`fork_attempts`] snapshots the writer's worktree (uncommitted and untracked work included, without touching it)
//! and makes one worktree per attempt on `attempt/<slug>/<n>` from that snapshot, each with the space's mise file
//! and its own `PORT`. [`adopt_attempt`] merges the winner into the space's branch, and [`discard_attempt`] removes
//! an attempt. The attempt records themselves (state, outcome, sessions) live in the `xctx-attempt` crate; these
//! functions only deal with git and files.
//!
//! The branch is `attempt/<slug>/<n>`, not under `space/<slug>/`, because git cannot have both a branch
//! `space/<slug>` and branches below it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::git;
use crate::mise;
use crate::space::{Space, SpaceRepo, SpaceStatus};

/// The most attempts one fork makes, and the most a space keeps alive at once.
pub const MAX_ATTEMPTS: u32 = 4;

/// The branch of attempt `number` of space `slug`: `attempt/<slug>/<number>`.
pub fn attempt_branch(slug: &str, number: u32) -> String {
    format!("attempt/{slug}/{number}")
}

/// The folder name of an attempt's worktree, next to the space's own: `<slug>--attempt-<number>`.
pub fn attempt_dir_name(slug: &str, number: u32) -> String {
    format!("{slug}--attempt-{number}")
}

/// One attempt's worktree, as made by [`fork_attempts`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptWorktree {
    /// The attempt's number in its space, from 1.
    pub number: u32,
    /// The name of the space's repository it is a copy of.
    pub repo: String,
    /// That repository's main checkout.
    pub repo_path: PathBuf,
    /// The attempt's worktree.
    pub worktree: PathBuf,
    /// Its branch, `attempt/<slug>/<number>`.
    pub branch: String,
    /// The snapshot of the writer's worktree it started from; its diff is measured from here.
    pub fork_commit: String,
    /// The attempt's own `PORT`.
    pub port: u16,
}

fn space_repo<'a>(space: &'a Space, repo: &str) -> Result<&'a SpaceRepo> {
    space
        .repos
        .iter()
        .find(|r| r.name == repo)
        .ok_or_else(|| Error::UnknownSpaceRepo {
            space: space.id.clone(),
            repo: repo.to_owned(),
        })
}

fn ensure_active(space: &Space) -> Result<()> {
    if space.status == SpaceStatus::Active {
        Ok(())
    } else {
        Err(Error::SpaceNotActive {
            id: space.id.clone(),
            status: space.status,
        })
    }
}

/// Fork one attempt of the writer working in `repo` per `(number, port)` in `attempts`. Every attempt starts from
/// the same snapshot of the writer's worktree, which is left exactly as it was. When any attempt cannot be made,
/// the ones made so far are removed again and the error is returned.
pub fn fork_attempts(
    space: &Space,
    repo: &str,
    attempts: &[(u32, u16)],
) -> Result<Vec<AttemptWorktree>> {
    ensure_active(space)?;
    let source = space_repo(space, repo)?;
    let slug = space.id.as_str();
    let fork_commit = git::snapshot_commit(
        &source.worktree,
        &format!("xctx: snapshot of {slug} for attempts"),
    )?;
    let main = git::main_worktree(&source.repo)?;
    for &(number, _) in attempts {
        let branch = attempt_branch(slug, number);
        let path = git::worktree_path(&main, &attempt_dir_name(slug, number));
        if git::branch_exists(&main.path, &branch)? || path.exists() {
            return Err(Error::AttemptBranchTaken {
                repo: main.path.clone(),
                branch,
            });
        }
    }
    let mut made: Vec<AttemptWorktree> = Vec::new();
    for &(number, port) in attempts {
        let branch = attempt_branch(slug, number);
        let path = git::worktree_path(&main, &attempt_dir_name(slug, number));
        let created = git::create_worktree(&main.path, &path, &branch, &fork_commit);
        let attempt = AttemptWorktree {
            number,
            repo: source.name.clone(),
            repo_path: main.path.clone(),
            worktree: path.clone(),
            branch: branch.clone(),
            fork_commit: fork_commit.clone(),
            port,
        };
        let result = created.and_then(|_| {
            mise::write_space_mise_file(
                &path,
                slug,
                &format!("{} (attempt {number})", space.title),
                port,
            )
        });
        match result {
            Ok(_) => made.push(attempt),
            Err(e) => {
                made.push(attempt);
                for attempt in made.iter().rev() {
                    let _ = discard_attempt(slug, attempt);
                }
                return Err(e);
            }
        }
    }
    Ok(made)
}

/// Remove an attempt: its worktree (even with changes), its mise file, its branch, and the worktree folder's parent
/// when that is now empty. What is already gone is skipped.
pub fn discard_attempt(slug: &str, attempt: &AttemptWorktree) -> Result<()> {
    if attempt.repo_path.exists() {
        git::remove_worktree(&attempt.repo_path, &attempt.worktree, true)?;
        mise::remove_space_mise_file(&attempt.worktree, &attempt.repo_path, slug)?;
        if git::branch_exists(&attempt.repo_path, &attempt.branch)? {
            git::delete_branch(&attempt.repo_path, &attempt.branch)?;
        }
    }
    if let Some(parent) = attempt.worktree.parent() {
        // Only succeeds while the folder is empty, so the space's own worktree stays.
        let _ = std::fs::remove_dir(parent);
    }
    Ok(())
}

/// Everything in an attempt's worktree now, uncommitted work included, as a commit: what its diff and an adoption
/// use.
pub fn attempt_tip(attempt: &AttemptWorktree) -> Result<String> {
    git::snapshot_commit(
        &attempt.worktree,
        &format!("xctx: attempt {} work in progress", attempt.number),
    )
}

/// The files an attempt changed since it was forked, uncommitted work included.
pub fn attempt_changes(attempt: &AttemptWorktree) -> Result<Vec<git::FileChange>> {
    let tip = attempt_tip(attempt)?;
    git::diff_stat(&attempt.worktree, &attempt.fork_commit, &tip)
}

/// What [`adopt_attempt`] did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdoptReport {
    /// The space branch's new head.
    pub commit: String,
    /// True when the space branch moved forward to the attempt; false when a merge commit was made.
    pub fast_forward: bool,
}

/// Merge an attempt's work, uncommitted changes included, into the space's branch in the space's worktree.
///
/// Refuses, changing nothing, with [`Error::ChangedSinceFork`] when the space's worktree no longer holds what the
/// attempts were forked from (its work would be lost), and with [`Error::AdoptConflicts`] when the merge would
/// conflict. The space's uncommitted work, which the attempt already contains, is stashed for the merge and
/// restored if the merge fails.
pub fn adopt_attempt(space: &Space, attempt: &AttemptWorktree) -> Result<AdoptReport> {
    ensure_active(space)?;
    let repo = space_repo(space, &attempt.repo)?;
    let dir = &repo.worktree;
    let tip = attempt_tip(attempt)?;
    let current = git::snapshot_commit(dir, "xctx: space before adopting")?;
    if git::tree_of(dir, &current)? != git::tree_of(dir, &attempt.fork_commit)? {
        return Err(Error::ChangedSinceFork {
            repo: repo.name.clone(),
        });
    }
    let head = git::head_commit(dir)?;
    let conflicts = git::merge_conflicts(dir, &head, &tip)?;
    if !conflicts.is_empty() {
        return Err(Error::AdoptConflicts { files: conflicts });
    }
    let dirty = current != head;
    if dirty {
        git::run_git(
            dir,
            [
                "stash",
                "push",
                "--include-untracked",
                "-m",
                &format!("xctx: before adopting attempt {}", attempt.number),
            ],
        )?;
    }
    let fast_forward = git::run_git(dir, ["merge-base", "--is-ancestor", &head, &tip]).is_ok();
    let merged = git::run_git(
        dir,
        [
            "merge",
            "--no-edit",
            "-m",
            &format!("xctx: adopt attempt {} of {}", attempt.number, space.id),
            &tip,
        ],
    );
    if let Err(e) = merged {
        let _ = git::run_git(dir, ["merge", "--abort"]);
        if dirty {
            let _ = git::run_git(dir, ["stash", "pop"]);
        }
        return Err(e);
    }
    if dirty {
        // The attempt contained this work, so the merge has it; the stash is only a safety copy.
        let _ = git::run_git(dir, ["stash", "drop"]);
    }
    Ok(AdoptReport {
        commit: git::head_commit(dir)?,
        fast_forward,
    })
}

/// The worktree folder an attempt of `repo` would get, for showing before forking.
pub fn attempt_worktree_path(repo: &Path, slug: &str, number: u32) -> Result<PathBuf> {
    let main = git::main_worktree(repo)?;
    Ok(git::worktree_path(&main, &attempt_dir_name(slug, number)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches_avoid_the_space_branch_namespace() {
        assert_eq!(attempt_branch("rate-limit", 2), "attempt/rate-limit/2");
        assert!(!attempt_branch("s", 1).starts_with("space/"));
        assert_eq!(attempt_dir_name("rate-limit", 3), "rate-limit--attempt-3");
    }
}
