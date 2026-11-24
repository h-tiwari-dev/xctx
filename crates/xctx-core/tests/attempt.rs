//! Forking, adopting and discarding attempts on temporary repositories.

mod common;

use std::fs;
use std::path::Path;

use common::{TestResult, git, make_repo, make_repos, temp_root};
use xctx_core::attempt::{attempt_changes, attempt_number_taken, attempt_tip};
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

#[test]
fn discarding_one_attempt_keeps_the_mise_file_ignored_for_the_space_and_the_others() -> TestResult {
    let root = temp_root()?;
    let (space, writer) = space(&root.path)?;
    let attempts = fork_attempts(&space, "payments", &[(1, 4101), (2, 4102)])?;
    fs::write(attempts[1].worktree.join("src/limit.ts"), "token bucket\n")?;
    discard_attempt(space.id.as_str(), &attempts[0])?;
    assert_eq!(
        status(&writer)?,
        "",
        "the space's mise file is still ignored"
    );
    assert_eq!(status(&attempts[1].worktree)?, "M src/limit.ts");
    let changes = attempt_changes(&attempts[1])?;
    let paths: Vec<&str> = changes.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, ["src/limit.ts"]);
    adopt_attempt(&space, &attempts[1])?;
    assert_eq!(
        fs::read_to_string(writer.join("src/limit.ts"))?,
        "token bucket\n"
    );
    Ok(())
}

#[test]
fn a_snapshot_of_a_sparse_checkout_keeps_the_files_outside_it() -> TestResult {
    let root = temp_root()?;
    let repo = root.path.join("sparse");
    make_repo(&repo, &[("src/a.ts", "a\n"), ("docs/guide.md", "guide\n")])?;
    git(&repo, &["sparse-checkout", "set", "src"])?;
    assert!(!repo.join("docs/guide.md").exists());
    let head = xgit::head_commit(&repo)?;
    assert_eq!(xgit::snapshot_commit(&repo, "s")?, head, "nothing changed");
    fs::write(repo.join("src/a.ts"), "changed\n")?;
    let snapshot = xgit::snapshot_commit(&repo, "s")?;
    let files = git(&repo, &["ls-tree", "-r", "--name-only", &snapshot])?;
    assert_eq!(files, "docs/guide.md\nsrc/a.ts");
    Ok(())
}

#[test]
fn a_snapshot_leaves_out_skip_worktree_and_assume_unchanged_contents() -> TestResult {
    let root = temp_root()?;
    let repo = root.path.join("flags");
    make_repo(
        &repo,
        &[
            ("a.ts", "a\n"),
            ("local.env", "committed\n"),
            ("same.txt", "committed\n"),
        ],
    )?;
    git(&repo, &["update-index", "--skip-worktree", "local.env"])?;
    git(&repo, &["update-index", "--assume-unchanged", "same.txt"])?;
    fs::write(repo.join("local.env"), "my own settings\n")?;
    fs::write(repo.join("same.txt"), "assumed unchanged\n")?;
    let head = xgit::head_commit(&repo)?;
    assert_eq!(xgit::snapshot_commit(&repo, "s")?, head, "nothing to take");
    fs::write(repo.join("a.ts"), "changed\n")?;
    let snapshot = xgit::snapshot_commit(&repo, "s")?;
    for (file, text) in [
        ("a.ts", "changed"),
        ("local.env", "committed"),
        ("same.txt", "committed"),
    ] {
        assert_eq!(git(&repo, &["show", &format!("{snapshot}:{file}")])?, text);
    }
    Ok(())
}

/// Give the writer a stash of the user's own, and return `git stash list`.
fn user_stash(writer: &Path) -> TestResult<String> {
    fs::write(writer.join("README.md"), "mine, for later\n")?;
    git(writer, &["stash", "push", "-q", "-m", "the user's own"])?;
    let list = git(writer, &["stash", "list", "--format=%H %s"])?;
    assert!(list.contains("the user's own"), "{list}");
    Ok(list)
}

#[test]
fn adopting_into_a_sparse_checkout_keeps_the_files_outside_it() -> TestResult {
    let root = temp_root()?;
    let (space, writer) = space(&root.path)?;
    fs::create_dir_all(writer.join("docs"))?;
    common::commit_file(&writer, "docs/guide.md", "guide\n", "docs")?;
    git(&writer, &["sparse-checkout", "set", "src"])?;
    let attempts = fork_attempts(&space, "payments", &[(1, 4101)])?;
    assert_eq!(
        fs::read_to_string(attempts[0].worktree.join("docs/guide.md"))?,
        "guide\n",
        "the attempt has the file the writer's checkout leaves out"
    );
    fs::write(attempts[0].worktree.join("src/limit.ts"), "attempt\n")?;
    adopt_attempt(&space, &attempts[0])?;
    assert_eq!(
        git(&writer, &["ls-tree", "--name-only", "HEAD", "docs/"])?,
        "docs/guide.md"
    );
    assert_eq!(
        fs::read_to_string(writer.join("src/limit.ts"))?,
        "attempt\n"
    );
    Ok(())
}

#[test]
fn a_skip_worktree_change_is_neither_adopted_over_nor_a_reason_to_touch_the_user_s_stash()
-> TestResult {
    let root = temp_root()?;
    let (space, writer) = space(&root.path)?;
    let stashes = user_stash(&writer)?;
    // A local-only change git is told to leave alone: neither a snapshot nor a stash sees it.
    git(
        &writer,
        &["update-index", "--skip-worktree", "src/charge.ts"],
    )?;
    fs::write(writer.join("src/charge.ts"), "local only\n")?;
    let attempts = fork_attempts(&space, "payments", &[(1, 4101)])?;
    let forked = fs::read_to_string(attempts[0].worktree.join("src/charge.ts"))?;
    fs::write(attempts[0].worktree.join("src/limit.ts"), "attempt\n")?;
    adopt_attempt(&space, &attempts[0])?;
    assert_eq!(git(&writer, &["stash", "list", "--format=%H %s"])?, stashes);
    assert_eq!(forked, "", "the attempt starts from the committed file");
    assert_eq!(git(&writer, &["show", "HEAD:src/charge.ts"])?, "");
    assert_eq!(
        fs::read_to_string(writer.join("src/charge.ts"))?,
        "local only\n"
    );
    assert_eq!(
        fs::read_to_string(writer.join("src/limit.ts"))?,
        "attempt\n"
    );
    Ok(())
}

#[test]
fn adopting_leaves_the_user_s_stash_alone_when_it_stashes_and_when_it_refuses() -> TestResult {
    let root = temp_root()?;
    let (space, writer) = space(&root.path)?;
    let stashes = user_stash(&writer)?;
    fs::write(writer.join("src/limit.ts"), "work in progress\n")?;
    let attempts = fork_attempts(&space, "payments", &[(1, 4101), (2, 4102)])?;
    fs::write(attempts[0].worktree.join("src/charge.ts"), "charge\n")?;

    fs::write(writer.join("NEW.md"), "written after the fork\n")?;
    let refused = adopt_attempt(&space, &attempts[0]);
    assert!(
        matches!(refused, Err(Error::ChangedSinceFork { .. })),
        "{refused:?}"
    );
    assert_eq!(git(&writer, &["stash", "list", "--format=%H %s"])?, stashes);
    fs::remove_file(writer.join("NEW.md"))?;

    adopt_attempt(&space, &attempts[0])?;
    assert_eq!(git(&writer, &["stash", "list", "--format=%H %s"])?, stashes);
    assert_eq!(
        fs::read_to_string(writer.join("src/limit.ts"))?,
        "work in progress\n"
    );
    assert_eq!(status(&writer)?, "");
    let backup = xctx_core::attempt::pre_adopt_ref(space.id.as_str(), 1);
    assert!(!git(&writer, &["rev-parse", "--verify", &backup])?.is_empty());
    Ok(())
}

fn parents(dir: &Path) -> TestResult<usize> {
    Ok(git(dir, &["rev-list", "--parents", "-n", "1", "HEAD"])?
        .split_whitespace()
        .count()
        - 1)
}

#[test]
fn adopting_fast_forwards_whatever_merge_ff_says() -> TestResult {
    let root = temp_root()?;
    let (space, writer) = space(&root.path)?;
    git(&writer, &["config", "merge.ff", "false"])?;
    let attempts = fork_attempts(&space, "payments", &[(1, 4101)])?;
    fs::write(attempts[0].worktree.join("src/limit.ts"), "attempt\n")?;
    let report = adopt_attempt(&space, &attempts[0])?;
    assert!(report.fast_forward);
    assert_eq!(parents(&writer)?, 1, "no merge commit");
    Ok(())
}

#[test]
fn adopting_merges_when_it_cannot_fast_forward_even_with_merge_ff_only() -> TestResult {
    let root = temp_root()?;
    let (space, writer) = space(&root.path)?;
    git(&writer, &["config", "merge.ff", "only"])?;
    fs::write(writer.join("src/limit.ts"), "writer\n")?;
    let attempts = fork_attempts(&space, "payments", &[(1, 4101)])?;
    fs::write(attempts[0].worktree.join("src/charge.ts"), "attempt\n")?;
    // The writer commits exactly what was forked, so nothing is lost but the branch moved on.
    git(&writer, &["commit", "-q", "-am", "writer"])?;
    let report = adopt_attempt(&space, &attempts[0])?;
    assert!(!report.fast_forward);
    assert_eq!(parents(&writer)?, 2, "a merge commit");
    assert_eq!(
        fs::read_to_string(writer.join("src/charge.ts"))?,
        "attempt\n"
    );
    Ok(())
}

#[test]
fn one_file_diffs_read_names_literally_and_follow_renames() -> TestResult {
    let root = temp_root()?;
    let repo = root.path.join("names");
    make_repo(
        &repo,
        &[
            ("a*.txt", "star\n"),
            ("ab.txt", "plain\n"),
            ("old.txt", "one\ntwo\nthree\nfour\n"),
        ],
    )?;
    let base = xgit::head_commit(&repo)?;
    git(&repo, &["config", "diff.external", "false"])?;
    git(&repo, &["config", "color.diff", "always"])?;
    fs::write(repo.join("a*.txt"), "star changed\n")?;
    fs::write(repo.join("ab.txt"), "plain changed\n")?;
    git(&repo, &["mv", "old.txt", "new.txt"])?;
    fs::write(repo.join("new.txt"), "one\ntwo\nthree\nfour\nfive\n")?;
    let head = xgit::snapshot_commit(&repo, "s")?;

    let diff = xgit::diff_file(&repo, &base, &head, "a*.txt")?;
    assert!(diff.contains("+star changed"), "{diff}");
    assert!(!diff.contains("plain"), "the name is not a pattern: {diff}");
    assert!(!diff.contains('\x1b'), "no colour: {diff}");
    let renamed = xgit::diff_file(&repo, &base, &head, "new.txt")?;
    assert!(renamed.contains("rename from old.txt"), "{renamed}");
    assert!(renamed.contains("+five"), "{renamed}");
    let stat = xgit::diff_stat(&repo, &base, &head)?;
    let paths: Vec<&str> = stat.iter().map(|c| c.path.as_str()).collect();
    assert_eq!(paths, ["a*.txt", "ab.txt", "new.txt"]);
    Ok(())
}

#[test]
fn numbers_left_behind_by_another_fork_are_reported_taken() -> TestResult {
    let root = temp_root()?;
    let (space, _) = space(&root.path)?;
    let repo = &space.repos[0].repo;
    git(repo, &["branch", &format!("attempt/{}/1", space.id)])?;
    let slug = space.id.as_str();
    assert!(attempt_number_taken(repo, slug, 1)?);
    assert!(!attempt_number_taken(repo, slug, 2)?);
    let made = fork_attempts(&space, "payments", &[(2, 4102)])?;
    fs::write(made[0].worktree.join("x"), "")?;
    assert!(attempt_number_taken(repo, slug, 2)?);
    Ok(())
}
