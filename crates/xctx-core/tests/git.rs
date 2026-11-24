mod common;

use std::fs;
use std::path::Path;

use common::{TestResult, add_bare_remote, commit_file, git, make_repo, temp_root};
use xctx_core::Error;
use xctx_core::git::{self, MainWorktree};

#[test]
fn discovers_root_branch_dirty_state_and_remote() -> TestResult {
    let root = temp_root()?;
    let repo = root.path.join("app");
    make_repo(
        &repo,
        &[("README.md", "app\n"), ("src/main.rs", "fn main() {}\n")],
    )?;

    assert_eq!(git::repo_root(&repo.join("src"))?, repo);
    assert_eq!(git::current_branch(&repo)?.as_deref(), Some("main"));
    assert_eq!(git::default_branch(&repo)?, "main");
    assert!(!git::is_dirty(&repo)?);
    assert_eq!(git::remote_name(&repo)?, None);

    fs::write(repo.join("scratch.txt"), "x")?;
    assert!(git::is_dirty(&repo)?, "an untracked file is a change");
    fs::remove_file(repo.join("scratch.txt"))?;

    let remote = root.path.join("remote.git");
    add_bare_remote(&repo, &remote)?;
    assert_eq!(git::remote_name(&repo)?.as_deref(), Some("origin"));
    assert_eq!(git::remote_url(&repo, "origin")?, remote.to_string_lossy());

    git(&repo, &["checkout", "-q", "-b", "feature"])?;
    assert_eq!(
        git::default_branch(&repo)?,
        "main",
        "the remote's HEAD decides, not the checkout"
    );
    let base = git::head_commit(&repo)?;
    commit_file(&repo, "a.txt", "a", "a")?;
    commit_file(&repo, "b.txt", "b", "b")?;
    assert_eq!(git::commits_ahead(&repo, &base, "HEAD")?, 2);
    assert_eq!(git::commits_ahead(&repo, "main", "feature")?, 2);
    assert_eq!(git::merge_base(&repo, "main", "feature")?, base);
    Ok(())
}

#[test]
fn a_folder_that_is_not_a_repository_is_reported_as_such() -> TestResult {
    let root = temp_root()?;
    let plain = root.path.join("plain");
    fs::create_dir_all(&plain)?;
    assert!(matches!(git::repo_root(&plain), Err(Error::NotARepository(p)) if p == plain));
    assert!(matches!(
        git::main_worktree(&plain),
        Err(Error::NotARepository(_))
    ));
    let missing = root.path.join("missing");
    assert!(matches!(
        git::repo_root(&missing),
        Err(Error::NotARepository(_))
    ));
    Ok(())
}

#[test]
fn worktrees_are_created_on_new_or_existing_branches_and_removed_keeping_the_branch() -> TestResult
{
    let root = temp_root()?;
    let repo = root.path.join("app");
    make_repo(&repo, &[("README.md", "app\n")])?;
    let main = git::main_worktree(&repo)?;
    assert_eq!(
        main,
        MainWorktree {
            path: repo.clone(),
            bare: None
        }
    );
    let path = git::worktree_path(&main, "task");
    assert_eq!(path, root.path.join("app-worktrees").join("task"));

    let head = git::head_commit(&repo)?;
    let created = git::create_worktree(&repo, &path, "space/task", &head)?;
    assert!(created.branch_created);
    assert_eq!(git::current_branch(&path)?.as_deref(), Some("space/task"));
    assert_eq!(
        git::main_worktree(&path)?.path,
        repo,
        "found from inside the linked worktree"
    );
    let listed = git::list_worktrees(&repo)?;
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[1].branch.as_deref(), Some("space/task"));

    commit_file(&path, "work.txt", "w", "work")?;
    git::remove_worktree(&repo, &path, false)?;
    assert!(!path.exists());
    assert!(
        git::branch_exists(&repo, "space/task")?,
        "removing a worktree keeps its branch"
    );

    let again = git::create_worktree(&repo, &path, "space/task", &head)?;
    assert!(
        !again.branch_created,
        "an existing branch is checked out, not recreated"
    );
    assert!(path.join("work.txt").exists(), "with its commits");

    fs::write(path.join("dirty.txt"), "x")?;
    assert!(
        git::remove_worktree(&repo, &path, false).is_err(),
        "git refuses to remove a worktree with changes"
    );
    git::remove_worktree(&repo, &path, true)?;
    assert!(!path.exists());

    git::delete_branch(&repo, "space/task")?;
    assert!(!git::branch_exists(&repo, "space/task")?);
    Ok(())
}

#[test]
fn a_worktree_that_cannot_be_made_leaves_no_new_branch_behind() -> TestResult {
    let root = temp_root()?;
    let repo = root.path.join("app");
    make_repo(&repo, &[("README.md", "app\n")])?;
    let taken = root.path.join("taken");
    fs::create_dir_all(&taken)?;
    fs::write(taken.join("file"), "x")?;
    let result = git::create_worktree(&repo, &taken, "space/taken", "HEAD");
    assert!(
        matches!(&result, Err(Error::Command { args, .. }) if args.starts_with("worktree add")),
        "git's own error comes back: {result:?}"
    );
    assert!(!git::branch_exists(&repo, "space/taken")?);
    Ok(())
}

#[test]
fn a_bare_layout_main_worktree_is_the_bare_directory() -> TestResult {
    let root = temp_root()?;
    let source = root.path.join("source");
    make_repo(&source, &[("README.md", "app\n")])?;
    let bare = root.path.join("app").join(".bare");
    git(
        &root.path,
        &[
            "clone",
            "-q",
            "--bare",
            &source.to_string_lossy(),
            &bare.to_string_lossy(),
        ],
    )?;
    let linked = root.path.join("app").join("aaa");
    git(
        &bare,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "space/aaa",
            &linked.to_string_lossy(),
        ],
    )?;
    assert_eq!(
        git::main_worktree(&linked)?,
        MainWorktree {
            path: bare.clone(),
            bare: Some(bare)
        }
    );
    Ok(())
}

#[test]
fn removing_a_worktree_whose_folder_is_gone_prunes_it() -> TestResult {
    let root = temp_root()?;
    let repo = root.path.join("app");
    make_repo(&repo, &[("README.md", "app\n")])?;
    let path = root.path.join("elsewhere");
    git::create_worktree(&repo, &path, "space/gone", "HEAD")?;
    fs::remove_dir_all(&path)?;
    git::remove_worktree(&repo, &path, false)?;
    assert_eq!(git::list_worktrees(&repo)?.len(), 1);
    Ok(())
}

#[test]
fn pushes_a_branch_and_opens_a_pull_request_with_gh() -> TestResult {
    let root = temp_root()?;
    let repo = root.path.join("app");
    make_repo(&repo, &[("README.md", "app\n")])?;
    let remote = root.path.join("remote.git");
    add_bare_remote(&repo, &remote)?;
    git(&repo, &["checkout", "-q", "-b", "space/x"])?;
    commit_file(&repo, "x.txt", "x", "x")?;

    git::push_branch(&repo, "origin", "space/x")?;
    assert!(git::branch_exists(&remote, "space/x")?);

    let gh = common::fake_gh(&root.path)?;
    let url = git::open_pull_request(
        &gh,
        &repo,
        &git::PullRequestRequest {
            head: "space/x".into(),
            base: "main".into(),
            title: "Do x".into(),
            body: "do x well".into(),
        },
    )?;
    assert_eq!(url, "https://github.com/acme/repo/pull/7");
    let args = fs::read_to_string(root.path.join("gh-args"))?;
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "pr",
            "create",
            "--head",
            "space/x",
            "--base",
            "main",
            "--title",
            "Do x",
            "--body",
            "do x well"
        ]
    );

    let failing = common::failing_gh(&root.path)?;
    let err = git::open_pull_request(
        &failing,
        &repo,
        &git::PullRequestRequest {
            head: "space/x".into(),
            base: "main".into(),
            title: "t".into(),
            body: "b".into(),
        },
    );
    assert!(
        matches!(err, Err(Error::Command { message, .. }) if message == "no GitHub remote found")
    );

    let missing = git::open_pull_request(
        Path::new("/nonexistent/gh"),
        &repo,
        &git::PullRequestRequest {
            head: "space/x".into(),
            base: "main".into(),
            title: "t".into(),
            body: "b".into(),
        },
    );
    assert!(matches!(missing, Err(Error::Spawn { .. })));
    Ok(())
}
