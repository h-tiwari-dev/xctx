//! Forking, adopting and discarding attempts on temporary repositories.

mod common;

use std::fs;
use std::path::Path;

use common::{TestResult, git, make_repos, temp_root};
use xctx_core::attempt::{attempt_changes, attempt_tip};
use xctx_core::git as xgit;
use xctx_core::{
    DraftRepo, Error, Space, SpaceDraft, Store, adopt_attempt, discard_attempt, fork_attempts,
    launch,
};

/// A launched space `rate-limit` on the `payments` repo, and its worktree.
fn space(root: &Path) -> TestResult<(Space, std::path::PathBuf)> {
    let repos = make_repos(root)?;
    let mut store = Store::open(root.join("data"))?;
    let mut draft = SpaceDraft::new("rate limit payments");
    draft.add_repo(DraftRepo::new(&repos.payments, "test"));
    let space = launch(&mut store, draft)?;
    let worktree = space.repos[0].worktree.clone();
    Ok((space, worktree))
}

fn status(dir: &Path) -> TestResult<String> {
    git(dir, &["status", "--porcelain", "--untracked-files=all"])
}

#[test]
fn forking_copies_uncommitted_and_untracked_work_without_touching_the_writer() -> TestResult {
    let root = temp_root()?;
    let (space, writer) = space(&root.path)?;
    fs::write(writer.join("src/limit.ts"), "export const limit = 10;\n")?;
    fs::write(writer.join("NOTES.md"), "try a token bucket\n")?;
    git(&writer, &["add", "src/limit.ts"])?;
    let before = status(&writer)?;
    let head = xgit::head_commit(&writer)?;

    let attempts = fork_attempts(&space, "payments", &[(1, 4101), (2, 4102)])?;
    assert_eq!(attempts.len(), 2);
    assert_eq!(
        status(&writer)?,
        before,
        "the writer's worktree and index are as they were"
    );
    assert_eq!(xgit::head_commit(&writer)?, head);

    for (attempt, port) in attempts.iter().zip([4101, 4102]) {
        assert_eq!(
            attempt.branch,
            format!("attempt/{}/{}", space.id, attempt.number)
        );
        assert_eq!(
            attempt.fork_commit, attempts[0].fork_commit,
            "one snapshot for all"
        );
        assert_eq!(
            fs::read_to_string(attempt.worktree.join("src/limit.ts"))?,
            "export const limit = 10;\n"
        );
        assert_eq!(
            fs::read_to_string(attempt.worktree.join("NOTES.md"))?,
            "try a token bucket\n"
        );
        let mise = fs::read_to_string(attempt.worktree.join(format!("mise.{}.toml", space.id)))?;
        assert!(mise.contains(&format!("PORT = \"{port}\"")), "{mise}");
        assert_eq!(
            status(&attempt.worktree)?,
            "",
            "the mise file is ignored, the rest committed"
        );
    }
    // The snapshot never contains the space's mise file.
    let files = git(
        &writer,
        &["ls-tree", "-r", "--name-only", &attempts[0].fork_commit],
    )?;
    assert!(!files.contains("mise."), "{files}");
    Ok(())
}

#[test]
fn a_clean_writer_forks_from_its_head() -> TestResult {
    let root = temp_root()?;
    let (space, writer) = space(&root.path)?;
    let attempts = fork_attempts(&space, "payments", &[(1, 4101)])?;
    assert_eq!(attempts[0].fork_commit, xgit::head_commit(&writer)?);
    Ok(())
}

#[test]
fn a_taken_attempt_branch_stops_the_fork_before_anything_is_made() -> TestResult {
    let root = temp_root()?;
    let (space, writer) = space(&root.path)?;
    let repo = &space.repos[0].repo;
    git(repo, &["branch", &format!("attempt/{}/2", space.id)])?;
    let result = fork_attempts(&space, "payments", &[(1, 4101), (2, 4102)]);
    assert!(
        matches!(result, Err(Error::AttemptBranchTaken { .. })),
        "{result:?}"
    );
    assert!(!xgit::branch_exists(
        repo,
        &format!("attempt/{}/1", space.id)
    )?);
    let worktrees = xgit::list_worktrees(&writer)?;
    assert_eq!(
        worktrees.len(),
        2,
        "only the main checkout and the space's worktree"
    );
    assert!(matches!(
        fork_attempts(&space, "web", &[(3, 4103)]),
        Err(Error::UnknownSpaceRepo { .. })
    ));
    Ok(())
}

#[test]
fn changes_include_uncommitted_work_in_the_attempt() -> TestResult {
    let root = temp_root()?;
    let (space, _) = space(&root.path)?;
    let attempts = fork_attempts(&space, "payments", &[(1, 4101)])?;
    let attempt = &attempts[0];
    fs::write(attempt.worktree.join("src/limit.ts"), "a\nb\n")?;
    fs::write(attempt.worktree.join("src/bucket.ts"), "bucket\n")?;
    let changes = attempt_changes(attempt)?;
    let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, ["src/bucket.ts", "src/limit.ts"]);
    assert_eq!(changes[1].added, Some(2));
    Ok(())
}

#[test]
fn adopting_from_a_clean_space_fast_forwards_and_discarding_cleans_up() -> TestResult {
    let root = temp_root()?;
    let (space, writer) = space(&root.path)?;
    let attempts = fork_attempts(&space, "payments", &[(1, 4101), (2, 4102)])?;
    fs::write(
        attempts[0].worktree.join("src/limit.ts"),
        "sliding window\n",
    )?;
    fs::write(attempts[1].worktree.join("src/limit.ts"), "token bucket\n")?;

    let report = adopt_attempt(&space, &attempts[1])?;
    assert!(report.fast_forward);
    assert_eq!(xgit::head_commit(&writer)?, report.commit);
    assert_eq!(
        fs::read_to_string(writer.join("src/limit.ts"))?,
        "token bucket\n"
    );
    assert_eq!(status(&writer)?, "");

    for attempt in &attempts {
        discard_attempt(space.id.as_str(), attempt)?;
        assert!(!attempt.worktree.exists());
        assert!(!xgit::branch_exists(&attempt.repo_path, &attempt.branch)?);
    }
    assert_eq!(xgit::list_worktrees(&writer)?.len(), 2);
    assert!(
        writer.join(format!("mise.{}.toml", space.id)).exists(),
        "the space keeps its own"
    );
    // Discarding twice is fine.
    discard_attempt(space.id.as_str(), &attempts[0])?;
    Ok(())
}

#[test]
fn adopting_into_a_space_with_the_forked_uncommitted_work_keeps_it_and_leaves_no_stash()
-> TestResult {
    let root = temp_root()?;
    let (space, writer) = space(&root.path)?;
    fs::write(writer.join("src/limit.ts"), "work in progress\n")?;
    fs::write(writer.join("TODO.md"), "untracked\n")?;
    let attempts = fork_attempts(&space, "payments", &[(1, 4101)])?;
    fs::write(
        attempts[0].worktree.join("src/charge.ts"),
        "charge with limit\n",
    )?;

    adopt_attempt(&space, &attempts[0])?;
    assert_eq!(
        fs::read_to_string(writer.join("src/limit.ts"))?,
        "work in progress\n"
    );
    assert_eq!(fs::read_to_string(writer.join("TODO.md"))?, "untracked\n");
    assert_eq!(
        fs::read_to_string(writer.join("src/charge.ts"))?,
        "charge with limit\n"
    );
    assert_eq!(status(&writer)?, "");
    assert_eq!(git(&writer, &["stash", "list"])?, "");
    Ok(())
}

#[test]
fn a_space_changed_after_the_fork_is_not_adopted_into() -> TestResult {
    let root = temp_root()?;
    let (space, writer) = space(&root.path)?;
    let attempts = fork_attempts(&space, "payments", &[(1, 4101)])?;
    fs::write(attempts[0].worktree.join("src/limit.ts"), "attempt\n")?;
    fs::write(writer.join("src/charge.ts"), "the writer kept going\n")?;
    let head = xgit::head_commit(&writer)?;

    let result = adopt_attempt(&space, &attempts[0]);
    assert!(
        matches!(result, Err(Error::ChangedSinceFork { .. })),
        "{result:?}"
    );
    assert_eq!(xgit::head_commit(&writer)?, head);
    assert_eq!(
        fs::read_to_string(writer.join("src/charge.ts"))?,
        "the writer kept going\n"
    );
    Ok(())
}

#[test]
fn merge_conflicts_are_found_without_touching_a_worktree() -> TestResult {
    let root = temp_root()?;
    let (space, writer) = space(&root.path)?;
    let attempts = fork_attempts(&space, "payments", &[(1, 4101)])?;
    fs::write(attempts[0].worktree.join("src/limit.ts"), "theirs\n")?;
    let tip = attempt_tip(&attempts[0])?;
    common::commit_file(&writer, "src/limit.ts", "ours\n", "ours")?;
    let head = xgit::head_commit(&writer)?;
    assert_eq!(
        xgit::merge_conflicts(&writer, &head, &tip)?,
        ["src/limit.ts"]
    );
    assert_eq!(status(&writer)?, "");
    assert!(xgit::merge_conflicts(&writer, &head, &head)?.is_empty());
    Ok(())
}
