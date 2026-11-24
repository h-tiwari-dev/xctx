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
/// the same snapshot of the writer's worktree, which is left exactly as it was. Fails with
/// [`Error::AttemptBranchTaken`], before making anything, when a number's branch or worktree folder already exists
/// (see [`attempt_number_taken`] for choosing numbers that are free). When any attempt cannot be made, the ones made
/// so far are removed again and the error is returned; when that removal fails too, the error is
/// [`Error::ForkCleanupFailed`] and names what was left behind.
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
                let leftovers: Vec<String> = made
                    .iter()
                    .rev()
                    .filter_map(|attempt| {
                        discard_attempt(slug, attempt).err().map(|why| {
                            format!("attempt {} ({}): {why}", attempt.number, attempt.branch)
                        })
                    })
                    .collect();
                if leftovers.is_empty() {
                    return Err(e);
                }
                return Err(Error::ForkCleanupFailed {
                    error: Box::new(e),
                    leftovers,
                });
            }
        }
    }
    Ok(made)
}

/// Whether attempt `number` of space `slug` cannot be forked in the repository containing `repo`, because its
/// branch or its worktree folder already exists (left, for example, by a fork whose cleanup failed). Callers
/// choosing attempt numbers skip the taken ones.
pub fn attempt_number_taken(repo: &Path, slug: &str, number: u32) -> Result<bool> {
    let main = git::main_worktree(repo)?;
    let path = git::worktree_path(&main, &attempt_dir_name(slug, number));
    Ok(path.exists() || git::branch_exists(&main.path, &attempt_branch(slug, number))?)
}

/// Remove an attempt: its worktree (even with changes), its mise file, its branch, and the worktree folder's parent
/// when that is now empty. What is already gone is skipped. The repository's exclude line for the space's mise
/// file stays, since the space and its other attempts still use it.
pub fn discard_attempt(slug: &str, attempt: &AttemptWorktree) -> Result<()> {
    if attempt.repo_path.exists() {
        git::remove_worktree(&attempt.repo_path, &attempt.worktree, true)?;
        mise::delete_space_mise_file(&attempt.worktree, slug)?;
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

/// The ref that keeps the space's uncommitted work as it was before attempt `number` of `slug` was adopted.
pub fn pre_adopt_ref(slug: &str, number: u32) -> String {
    format!("refs/xctx/pre-adopt/{slug}/{number}")
}

/// Refuse with [`Error::ChangedSinceFork`] unless the worktree at `dir` holds exactly `fork_tree` on top of `head`.
/// Returns the snapshot of the worktree.
fn ensure_unchanged(dir: &Path, repo: &str, fork_tree: &str, head: &str) -> Result<String> {
    let current = git::snapshot_commit(dir, "xctx: space before adopting")?;
    if git::tree_of(dir, &current)? != fork_tree || git::head_commit(dir)? != head {
        return Err(Error::ChangedSinceFork {
            repo: repo.to_owned(),
        });
    }
    Ok(current)
}

/// The commit `refs/stash` points at, or `None` when there is no stash.
fn stash_top(dir: &Path) -> Result<Option<String>> {
    match git::run_git(dir, ["rev-parse", "-q", "--verify", "refs/stash"]) {
        Ok(out) => Ok(Some(out.trim().to_owned()).filter(|s| !s.is_empty())),
        Err(Error::Command { .. }) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The `stash@{n}` entry whose commit is `sha`, so a stash is only ever touched by its identity.
fn stash_entry(dir: &Path, sha: &str) -> Result<Option<String>> {
    let list = git::run_git(dir, ["stash", "list", "--format=%H"])?;
    Ok(list
        .lines()
        .position(|line| line.trim() == sha)
        .map(|n| format!("stash@{{{n}}}")))
}

/// Stash the uncommitted work in `dir` and return the new stash's commit, or `None` when git stashed nothing.
/// A stash the user already had is never mistaken for it.
fn stash_push(dir: &Path, message: &str) -> Result<Option<String>> {
    let before = stash_top(dir)?;
    let pushed = git::run_git(dir, ["stash", "push", "--include-untracked", "-m", message]);
    let after = stash_top(dir)?;
    let made = after.filter(|after| Some(after) != before.as_ref());
    if let Err(e) = pushed {
        if let Some(sha) = &made {
            let _ = restore_stash(dir, sha);
        }
        return Err(e);
    }
    Ok(made)
}

/// Put the stash with commit `sha` back into the worktree and remove it from the stash list.
fn restore_stash(dir: &Path, sha: &str) -> Result<()> {
    let Some(entry) = stash_entry(dir, sha)? else {
        return Ok(());
    };
    if git::run_git(dir, ["stash", "pop", "-q", "--index", &entry]).is_err() {
        git::run_git(dir, ["stash", "pop", "-q", &entry])?;
    }
    Ok(())
}

/// Merge an attempt's work, uncommitted changes included, into the space's branch in the space's worktree.
///
/// Refuses, changing nothing, with [`Error::ChangedSinceFork`] when the space's worktree no longer holds what the
/// attempts were forked from (its work would be lost; this is checked again right before anything changes), and
/// with [`Error::AdoptConflicts`] when the merge would conflict. The space's uncommitted work, which the attempt
/// already contains, is stashed for the merge and restored if the merge fails; after a merge it is kept under
/// [`pre_adopt_ref`] instead of in the stash list. Stashes the user made are never touched.
///
/// The merge is a fast-forward whenever it can be, whatever the user's `merge.ff` setting.
pub fn adopt_attempt(space: &Space, attempt: &AttemptWorktree) -> Result<AdoptReport> {
    ensure_active(space)?;
    let repo = space_repo(space, &attempt.repo)?;
    let dir = &repo.worktree;
    let tip = attempt_tip(attempt)?;
    let fork_tree = git::tree_of(dir, &attempt.fork_commit)?;
    let head = git::head_commit(dir)?;
    ensure_unchanged(dir, &repo.name, &fork_tree, &head)?;
    let conflicts = git::merge_conflicts(dir, &head, &tip)?;
    if !conflicts.is_empty() {
        return Err(Error::AdoptConflicts { files: conflicts });
    }
    // Checked again just before stashing, so work written meanwhile is refused instead of stashed and lost.
    let current = ensure_unchanged(dir, &repo.name, &fork_tree, &head)?;
    let stash = if current != head {
        stash_push(
            dir,
            &format!("xctx: before adopting attempt {}", attempt.number),
        )?
    } else {
        None
    };
    let merged = git::run_git(
        dir,
        [
            "-c",
            "merge.ff=true",
            "merge",
            "--ff",
            "--no-autostash",
            "--no-edit",
            "-m",
            &format!("xctx: adopt attempt {} of {}", attempt.number, space.id),
            &tip,
        ],
    );
    if let Err(e) = merged {
        let _ = git::run_git(dir, ["merge", "--abort"]);
        if let Some(sha) = &stash {
            let _ = restore_stash(dir, sha);
        }
        return Err(e);
    }
    if let Some(sha) = &stash {
        // The attempt contained this work, so the merge has it. The stash is a safety copy: keep it as a ref, in
        // case anything was written between the last check and the stash, and take it off the stash list.
        // When the ref cannot be written, the stash stays on the list.
        let kept = git::run_git(
            dir,
            [
                "update-ref",
                &pre_adopt_ref(space.id.as_str(), attempt.number),
                sha,
            ],
        );
        if let (Ok(_), Ok(Some(entry))) = (kept, stash_entry(dir, sha)) {
            let _ = git::run_git(dir, ["stash", "drop", "-q", &entry]);
        }
    }
    let commit = git::head_commit(dir)?;
    Ok(AdoptReport {
        fast_forward: commit == tip,
        commit,
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
