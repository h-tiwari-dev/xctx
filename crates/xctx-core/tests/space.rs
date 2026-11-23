mod common;

use std::fs;
use std::path::Path;

use common::{TestResult, add_bare_remote, commit_file, git, make_repos, temp_root};
use xctx_core::git as xgit;
use xctx_core::space::{
    DraftRepo, PullRequestOutcome, ShipOptions, ShipOutcome, ShipSkipReason, SpaceDraft,
    SpaceShape, SpaceStatus,
};
use xctx_core::{Error, Store, discard, launch, ship};

fn draft(task: &str, repos: &[&Path]) -> SpaceDraft {
    let mut draft = SpaceDraft::new(task);
    for repo in repos {
        draft.add_repo(DraftRepo::new(*repo, "test"));
    }
    draft
}

#[test]
fn launch_makes_worktrees_mise_files_and_saves_the_space() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut store = Store::open(root.path.join("data"))?;

    let proposal = draft(
        "Add rate limiting to checkout",
        &[&repos.payments, &repos.checkout_web],
    );
    assert!(
        !root.path.join("code").join("payments-worktrees").exists(),
        "a draft touches nothing on disk"
    );
    let space = launch(&mut store, proposal)?;

    assert_eq!(space.id.as_str(), "rate-limiting-checkout");
    assert_eq!(space.title, "Add rate limiting to");
    assert_eq!(space.status, SpaceStatus::Active);
    assert_eq!(space.shape, SpaceShape::Dev);
    assert_eq!(space.port, 4001);
    assert_eq!(space.branch(), "space/rate-limiting-checkout");
    assert_eq!(space.mise_env(), "rate-limiting-checkout");
    let env = space.env();
    assert!(env.contains(&("PORT".into(), "4001".into())));
    assert!(env.contains(&("MISE_ENV".into(), "rate-limiting-checkout".into())));

    let head = xgit::head_commit(&repos.payments)?;
    let pay = &space.repos[0];
    assert_eq!(pay.name, "payments");
    assert_eq!(pay.repo, repos.payments);
    assert_eq!(
        pay.worktree,
        repos
            .code
            .join("payments-worktrees")
            .join("rate-limiting-checkout")
    );
    assert_eq!(pay.base_branch, "main");
    assert_eq!(pay.base_commit, head);
    assert!(pay.branch_created);
    assert_eq!(
        xgit::current_branch(&pay.worktree)?.as_deref(),
        Some("space/rate-limiting-checkout")
    );
    for repo in &space.repos {
        let mise = fs::read_to_string(repo.worktree.join("mise.rate-limiting-checkout.toml"))?;
        assert!(mise.contains("PORT = \"4001\""), "{mise}");
        assert!(!xgit::is_dirty(&repo.worktree)?, "the mise file is ignored");
        assert!(
            !xgit::is_dirty(&repo.repo)?,
            "the main checkout is untouched"
        );
    }

    let reopened = Store::open(root.path.join("data"))?;
    assert_eq!(reopened.space(&space.id), Some(&space));

    let second = launch(
        &mut store,
        draft("add rate limiting to checkout", &[&repos.payments]),
    )?;
    assert_eq!(second.id.as_str(), "rate-limiting-checkout-2");
    assert_eq!(second.port, 4002, "ports never clash between active spaces");
    assert_eq!(store.port_allocations().len(), 2);
    Ok(())
}

#[test]
fn launch_rejects_empty_drafts_and_dedupes_repositories() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut store = Store::open(root.path.join("data"))?;
    assert!(matches!(
        launch(&mut store, SpaceDraft::new("nothing")),
        Err(Error::EmptyDraft)
    ));

    let src = repos.payments.join("src");
    let space = launch(&mut store, draft("twice", &[&repos.payments, &src]))?;
    assert_eq!(
        space.repos.len(),
        1,
        "two paths in one repository make one worktree"
    );
    Ok(())
}

#[test]
fn a_failed_launch_removes_what_it_made_and_saves_nothing() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut store = Store::open(root.path.join("data"))?;

    let result = launch(
        &mut store,
        draft("broken", &[&repos.payments, &repos.notes]),
    );
    assert!(matches!(result, Err(Error::NotARepository(_))));
    assert!(store.spaces().is_empty());
    assert!(!xgit::branch_exists(&repos.payments, "space/broken")?);

    // The second repository fails after the first worktree is made: its path is already taken.
    let blocker = repos
        .code
        .join("web")
        .join("checkout-web-worktrees")
        .join("blocked");
    fs::create_dir_all(&blocker)?;
    fs::write(blocker.join("file"), "x")?;
    let result = launch(
        &mut store,
        draft("blocked", &[&repos.payments, &repos.checkout_web]),
    );
    assert!(matches!(result, Err(Error::Command { .. })), "{result:?}");
    assert!(
        !repos
            .code
            .join("payments-worktrees")
            .join("blocked")
            .exists()
    );
    assert!(!xgit::branch_exists(&repos.payments, "space/blocked")?);
    assert!(!xgit::branch_exists(&repos.checkout_web, "space/blocked")?);
    let exclude = fs::read_to_string(repos.payments.join(".git/info/exclude"))?;
    assert!(!exclude.contains("mise.blocked.toml"), "{exclude}");
    assert_eq!(xgit::list_worktrees(&repos.payments)?.len(), 1);
    assert!(store.spaces().is_empty());
    Ok(())
}

#[test]
fn launch_checks_out_an_existing_space_branch_as_it_is() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let base = xgit::head_commit(&repos.payments)?;
    git(&repos.payments, &["checkout", "-q", "-b", "space/retry"])?;
    commit_file(&repos.payments, "retry.ts", "", "earlier work")?;
    git(&repos.payments, &["checkout", "-q", "main"])?;
    commit_file(&repos.payments, "main.ts", "", "main moved on")?;

    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("retry", &[&repos.payments]))?;
    let repo = &space.repos[0];
    assert!(!repo.branch_created);
    assert_eq!(
        repo.base_commit, base,
        "the base is where the branch left main"
    );
    assert!(repo.worktree.join("retry.ts").exists());

    discard(&mut store, &space.id, false)?;
    assert!(
        xgit::branch_exists(&repos.payments, "space/retry")?,
        "an existing branch survives"
    );
    Ok(())
}

#[test]
fn discard_removes_worktrees_and_keeps_branches() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(
        &mut store,
        draft("checkout payments", &[&repos.payments, &repos.checkout_web]),
    )?;
    let worktree = space.repos[0].worktree.clone();
    fs::write(worktree.join("untracked.txt"), "x")?;

    let refused = discard(&mut store, &space.id, false);
    assert!(
        matches!(&refused, Err(Error::DirtyWorktrees { repos }) if repos == &["payments".to_owned()]),
        "{refused:?}"
    );
    assert!(worktree.exists(), "nothing was removed");

    let report = discard(&mut store, &space.id, true)?;
    assert_eq!(report.removed.len(), 2);
    assert!(report.removed.iter().all(|r| !r.already_gone));
    for repo in &space.repos {
        assert!(!repo.worktree.exists());
        assert!(xgit::branch_exists(&repo.repo, &repo.branch)?);
        let exclude = fs::read_to_string(repo.repo.join(".git/info/exclude"))?;
        assert!(!exclude.contains(&space.mise_file_name()), "{exclude}");
    }
    assert_eq!(
        store.space(&space.id).map(|s| s.status),
        Some(SpaceStatus::Discarded)
    );
    assert!(
        store.port_allocations().is_empty(),
        "the port is free again"
    );
    assert!(matches!(
        discard(&mut store, &space.id, false),
        Err(Error::SpaceNotActive { .. })
    ));
    let next = launch(&mut store, draft("checkout payments", &[&repos.payments]))?;
    assert_eq!(
        next.id.as_str(),
        "checkout-payments-2",
        "slugs are never reused"
    );
    assert_eq!(next.port, 4001);
    Ok(())
}

#[test]
fn ship_without_a_remote_keeps_the_commits_on_the_branch() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(
        &mut store,
        draft(
            "Add rate limiting to checkout",
            &[&repos.payments, &repos.checkout_web],
        ),
    )?;
    let worktree = space.repos[0].worktree.clone();

    fs::write(worktree.join("new.txt"), "x")?;
    assert!(
        matches!(
            ship(&mut store, &space.id, &ShipOptions::default()),
            Err(Error::DirtyWorktrees { .. })
        ),
        "uncommitted work blocks shipping"
    );
    git(&worktree, &["add", "new.txt"])?;
    git(&worktree, &["commit", "-q", "-m", "work"])?;

    let report = ship(&mut store, &space.id, &ShipOptions::default())?;
    let lines: Vec<String> = report.repos.iter().map(ToString::to_string).collect();
    assert_eq!(
        lines,
        [
            "payments: 1 commit kept on space/rate-limiting-checkout (no remote)",
            "checkout-web: no new commits",
        ]
    );
    assert_eq!(
        report.repos[0].outcome,
        ShipOutcome::Skipped(ShipSkipReason::NoRemote)
    );
    assert!(!worktree.exists());
    assert!(xgit::branch_exists(
        &repos.payments,
        "space/rate-limiting-checkout"
    )?);
    assert_eq!(
        Store::open(root.path.join("data"))?
            .space(&space.id)
            .map(|s| s.status),
        Some(SpaceStatus::Shipped)
    );
    assert!(matches!(
        ship(&mut store, &space.id, &ShipOptions::default()),
        Err(Error::SpaceNotActive { .. })
    ));
    Ok(())
}

#[test]
fn ship_pushes_and_opens_a_pull_request_where_there_is_work_and_a_remote() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let pay_remote = root.path.join("payments.git");
    let web_remote = root.path.join("checkout-web.git");
    add_bare_remote(&repos.payments, &pay_remote)?;
    add_bare_remote(&repos.checkout_web, &web_remote)?;
    let gh_dir = root.path.join("bin");
    fs::create_dir_all(&gh_dir)?;
    let gh = common::fake_gh(&gh_dir)?;

    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(
        &mut store,
        draft(
            "Add rate limiting to checkout",
            &[&repos.payments, &repos.checkout_web],
        ),
    )?;
    commit_file(&space.repos[0].worktree, "limit.ts", "x", "limit")?;
    commit_file(&space.repos[0].worktree, "more.ts", "x", "more")?;

    let report = ship(&mut store, &space.id, &ShipOptions { gh: Some(gh) })?;
    assert_eq!(report.repos[0].commits_ahead, 2);
    assert_eq!(
        report.repos[0].outcome,
        ShipOutcome::Pushed {
            remote: "origin".into(),
            pull_request: PullRequestOutcome::Opened {
                url: "https://github.com/acme/repo/pull/7".into()
            },
        }
    );
    assert_eq!(
        report.repos[0].to_string(),
        "payments: pushed 2 commits to origin/space/rate-limiting-checkout, opened https://github.com/acme/repo/pull/7"
    );
    assert_eq!(
        report.repos[1].outcome,
        ShipOutcome::Skipped(ShipSkipReason::NoNewCommits)
    );
    assert!(xgit::branch_exists(
        &pay_remote,
        "space/rate-limiting-checkout"
    )?);
    assert!(
        !xgit::branch_exists(&web_remote, "space/rate-limiting-checkout")?,
        "nothing to push for checkout-web"
    );
    let args = fs::read_to_string(gh_dir.join("gh-args"))?;
    assert_eq!(
        args.lines().collect::<Vec<_>>(),
        [
            "pr",
            "create",
            "--head",
            "space/rate-limiting-checkout",
            "--base",
            "main",
            "--title",
            "Add rate limiting to",
            "--body",
            "Add rate limiting to checkout",
        ]
    );
    assert_eq!(report.removed.len(), 2);
    assert!(space.repos.iter().all(|r| !r.worktree.exists()));
    Ok(())
}

#[test]
fn ship_reports_a_missing_or_failing_gh_without_stopping() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    add_bare_remote(&repos.payments, &root.path.join("payments.git"))?;
    add_bare_remote(&repos.checkout_web, &root.path.join("checkout-web.git"))?;
    let mut store = Store::open(root.path.join("data"))?;

    let first = launch(&mut store, draft("payments one", &[&repos.payments]))?;
    commit_file(&first.repos[0].worktree, "a.ts", "a", "a")?;
    let report = ship(&mut store, &first.id, &ShipOptions { gh: None })?;
    assert_eq!(
        report.repos[0].outcome,
        ShipOutcome::Pushed {
            remote: "origin".into(),
            pull_request: PullRequestOutcome::GhNotFound,
        }
    );

    let second = launch(&mut store, draft("checkout two", &[&repos.checkout_web]))?;
    commit_file(&second.repos[0].worktree, "b.ts", "b", "b")?;
    let failing = common::failing_gh(&root.path)?;
    let report = ship(&mut store, &second.id, &ShipOptions { gh: Some(failing) })?;
    assert_eq!(
        report.repos[0].outcome,
        ShipOutcome::Pushed {
            remote: "origin".into(),
            pull_request: PullRequestOutcome::Failed {
                message: "no GitHub remote found".into()
            },
        }
    );
    assert_eq!(
        store.space(&second.id).map(|s| s.status),
        Some(SpaceStatus::Shipped)
    );
    Ok(())
}

#[test]
fn a_failed_push_leaves_the_space_active_with_its_worktrees() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    git(
        &repos.payments,
        &[
            "remote",
            "add",
            "origin",
            &root.path.join("missing.git").to_string_lossy(),
        ],
    )?;
    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("payments push", &[&repos.payments]))?;
    commit_file(&space.repos[0].worktree, "a.ts", "a", "a")?;

    let result = ship(&mut store, &space.id, &ShipOptions { gh: None });
    assert!(matches!(result, Err(Error::Command { .. })), "{result:?}");
    assert!(space.repos[0].worktree.exists());
    assert_eq!(
        store.space(&space.id).map(|s| s.status),
        Some(SpaceStatus::Active)
    );
    Ok(())
}

#[test]
fn ship_and_discard_survive_a_worktree_deleted_by_hand() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("payments gone", &[&repos.payments]))?;
    fs::remove_dir_all(&space.repos[0].worktree)?;

    let report = discard(&mut store, &space.id, false)?;
    assert!(report.removed[0].already_gone);
    assert_eq!(xgit::list_worktrees(&repos.payments)?.len(), 1);
    Ok(())
}

#[test]
fn unknown_spaces_are_reported() -> TestResult {
    let root = temp_root()?;
    let mut store = Store::open(root.path.join("data"))?;
    let id = xctx_core::SpaceId::new("nope");
    assert!(matches!(
        ship(&mut store, &id, &ShipOptions::default()),
        Err(Error::SpaceNotFound(_))
    ));
    assert!(matches!(
        discard(&mut store, &id, true),
        Err(Error::SpaceNotFound(_))
    ));
    Ok(())
}
