mod common;

use std::fs;
use std::path::Path;

use common::{TestResult, add_bare_remote, commit_file, git, make_repo, make_repos, temp_root};
use xctx_core::git as xgit;
use xctx_core::space::{
    DraftRepo, PullRequestOutcome, ShipOptions, ShipOutcome, ShipSkipReason, SpaceDraft,
    SpaceShape, SpaceStatus, WidenRequest,
};
use xctx_core::{Error, Store, discard, launch, ship, widen};

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

    // The second repository fails after the first worktree is made: the folder its worktree goes in is a file.
    fs::write(repos.code.join("web").join("checkout-web-worktrees"), "x")?;
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
    assert!(
        !repos.code.join("payments-worktrees").exists(),
        "the worktrees folder the launch made is removed with its worktree"
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
fn launch_never_reuses_an_existing_space_branch() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    git(&repos.payments, &["checkout", "-q", "-b", "space/retry"])?;
    commit_file(&repos.payments, "retry.ts", "", "earlier work")?;
    git(&repos.payments, &["checkout", "-q", "main"])?;
    commit_file(&repos.payments, "main.ts", "", "main moved on")?;
    let earlier_tip = xgit::resolve_commit(&repos.payments, "space/retry")?;
    let head = xgit::head_commit(&repos.payments)?;

    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("retry", &[&repos.payments]))?;
    assert_eq!(
        space.id.as_str(),
        "retry-2",
        "a space/<slug> branch the store does not know is taken"
    );
    let repo = &space.repos[0];
    assert!(repo.branch_created);
    assert_eq!(repo.branch, "space/retry-2");
    assert_eq!(repo.base_commit, head);
    assert!(!repo.worktree.join("retry.ts").exists());

    discard(&mut store, &space.id, false)?;
    assert_eq!(
        xgit::resolve_commit(&repos.payments, "space/retry")?,
        earlier_tip,
        "the existing branch is untouched"
    );

    // A leftover worktree folder takes its slug too, even with no branch behind it.
    fs::create_dir_all(repos.code.join("payments-worktrees").join("leftover"))?;
    let next = launch(&mut store, draft("leftover", &[&repos.payments]))?;
    assert_eq!(next.id.as_str(), "leftover-2");
    Ok(())
}

#[test]
fn launch_gives_repositories_with_the_same_folder_name_unique_names() -> TestResult {
    let root = temp_root()?;
    let work_api = root.path.join("work").join("api");
    let oss_api = root.path.join("oss").join("api");
    make_repo(&work_api, &[("README.md", "work api\n")])?;
    make_repo(&oss_api, &[("README.md", "oss api\n")])?;
    let mut store = Store::open(root.path.join("data"))?;

    let space = launch(&mut store, draft("api change", &[&work_api, &oss_api]))?;
    let names: Vec<&str> = space.repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["api", "api-2"]);
    assert_eq!(space.repos[0].repo, work_api);
    assert_eq!(space.repos[1].repo, oss_api);
    assert_ne!(space.repos[0].worktree, space.repos[1].worktree);
    for repo in &space.repos {
        assert!(repo.worktree.is_dir());
        assert_eq!(
            xgit::current_branch(&repo.worktree)?.as_deref(),
            Some("space/api-change")
        );
    }
    Ok(())
}

#[test]
fn launch_in_a_bare_layout_branches_from_the_default_branch_not_another_space() -> TestResult {
    let root = temp_root()?;
    let source = root.path.join("source");
    make_repo(&source, &[("README.md", "app\n")])?;
    let layout = root.path.join("app");
    fs::create_dir_all(&layout)?;
    let bare = layout.join(".bare");
    git(
        &layout,
        &[
            "clone",
            "-q",
            "--bare",
            &source.to_string_lossy(),
            &bare.to_string_lossy(),
        ],
    )?;
    let main = layout.join("main");
    let aaa = layout.join("aaa");
    git(
        &bare,
        &["worktree", "add", "-q", &main.to_string_lossy(), "main"],
    )?;
    git(
        &bare,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "space/aaa",
            &aaa.to_string_lossy(),
        ],
    )?;
    commit_file(&aaa, "aaa.ts", "", "another space's work")?;
    let listed = xgit::list_worktrees(&main)?;
    assert_eq!(
        listed[0].path, aaa,
        "git lists the other space's worktree before main"
    );
    let main_head = xgit::head_commit(&main)?;

    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("bare work", &[&main]))?;
    let repo = &space.repos[0];
    assert_eq!(
        repo.repo, bare,
        "the bare directory, which outlives any worktree"
    );
    assert_eq!(repo.worktree, layout.join("bare-work"));
    assert_eq!(repo.base_branch, "main");
    assert_eq!(repo.base_commit, main_head);
    assert!(!repo.worktree.join("aaa.ts").exists());
    assert_eq!(xgit::head_commit(&repo.worktree)?, main_head);

    fs::remove_dir_all(&aaa)?;
    discard(&mut store, &space.id, false)?;
    assert!(!repo.worktree.exists());
    assert!(xgit::branch_exists(&bare, "space/bare-work")?);
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
fn a_worktree_git_will_not_remove_keeps_its_mise_file() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("payments locked", &[&repos.payments]))?;
    let worktree = space.repos[0].worktree.clone();
    git(
        &repos.payments,
        &["worktree", "lock", &worktree.to_string_lossy()],
    )?;

    let result = discard(&mut store, &space.id, true);
    assert!(matches!(result, Err(Error::Command { .. })), "{result:?}");
    assert!(worktree.join(space.mise_file_name()).exists());
    let exclude = fs::read_to_string(repos.payments.join(".git/info/exclude"))?;
    assert!(exclude.contains(&space.mise_file_name()), "{exclude}");
    assert_eq!(
        store.space(&space.id).map(|s| s.status),
        Some(SpaceStatus::Active)
    );
    Ok(())
}

#[test]
fn discard_survives_a_repository_and_worktree_both_deleted_by_hand() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("payments vanished", &[&repos.payments]))?;
    fs::remove_dir_all(&space.repos[0].worktree)?;
    fs::remove_dir_all(&repos.payments)?;

    let report = discard(&mut store, &space.id, false)?;
    assert!(report.removed[0].already_gone);
    assert_eq!(
        store.space(&space.id).map(|s| s.status),
        Some(SpaceStatus::Discarded)
    );
    Ok(())
}

#[test]
fn ship_does_not_count_upstream_commits_merged_into_the_space() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("payments merge", &[&repos.payments]))?;
    let worktree = space.repos[0].worktree.clone();
    commit_file(&worktree, "work.ts", "", "space work")?;
    commit_file(&repos.payments, "upstream1.ts", "", "upstream one")?;
    commit_file(&repos.payments, "upstream2.ts", "", "upstream two")?;
    git(&worktree, &["merge", "-q", "--no-edit", "main"])?;

    let report = ship(&mut store, &space.id, &ShipOptions::default())?;
    assert_eq!(
        report.repos[0].commits_ahead, 2,
        "the space's commit and its merge commit"
    );
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

fn widen_repos(repos: &[&Path]) -> WidenRequest {
    WidenRequest {
        repos: repos
            .iter()
            .map(|r| DraftRepo::new(*r, "widened"))
            .collect(),
        production: false,
    }
}

#[test]
fn widen_adds_a_worktree_and_mise_file_for_a_new_repo() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("checkout limits", &[&repos.payments]))?;
    let payments_src = repos.payments.join("src");

    let report = widen(
        &mut store,
        &space.id,
        widen_repos(&[&repos.checkout_web, &payments_src, &repos.checkout_web]),
    )?;
    assert_eq!(
        report.added_repos.len(),
        1,
        "repos already in the space and repeats are skipped"
    );
    assert!(!report.production_enabled);
    let web = &report.added_repos[0];
    assert_eq!(web.name, "checkout-web");
    assert_eq!(web.repo, repos.checkout_web);
    assert_eq!(web.branch, "space/checkout-limits");
    assert_eq!(web.reason, "widened");
    assert!(web.branch_created);
    assert_eq!(web.base_commit, xgit::head_commit(&repos.checkout_web)?);
    assert_eq!(
        xgit::current_branch(&web.worktree)?.as_deref(),
        Some("space/checkout-limits")
    );
    let mise = fs::read_to_string(web.worktree.join(space.mise_file_name()))?;
    assert!(mise.contains("PORT = \"4001\""), "{mise}");
    assert!(mise.contains("XCTX_SPACE = \"checkout-limits\""), "{mise}");
    assert!(!xgit::is_dirty(&web.worktree)?, "the mise file is ignored");

    let saved = Store::open(root.path.join("data"))?
        .space(&space.id)
        .cloned()
        .ok_or("the widened space is saved")?;
    assert_eq!(saved.repos.len(), 2);
    assert_eq!(
        saved.repos[0], space.repos[0],
        "existing repos are untouched"
    );
    assert_eq!(&saved.repos[1], web);
    assert!(!saved.production);
    assert_eq!(saved.port, space.port);
    Ok(())
}

#[test]
fn widen_to_production_sets_the_flag_and_the_terminal_mise_env() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("prod hotfix", &[&repos.payments]))?;
    assert!(!space.production);
    assert_eq!(space.effective_mise_env(), "prod-hotfix");

    let report = widen(
        &mut store,
        &space.id,
        WidenRequest {
            repos: Vec::new(),
            production: true,
        },
    )?;
    assert!(report.added_repos.is_empty());
    assert!(report.production_enabled);
    let saved = Store::open(root.path.join("data"))?
        .space(&space.id)
        .cloned()
        .ok_or("the widened space is saved")?;
    assert!(saved.production);
    assert_eq!(
        saved.mise_env(),
        "prod-hotfix",
        "the slug still names its mise file"
    );
    assert_eq!(saved.effective_mise_env(), "production,prod-hotfix");
    let env = saved.env();
    assert!(env.contains(&("MISE_ENV".into(), "production,prod-hotfix".into())));
    assert!(env.contains(&("PORT".into(), "4001".into())));
    assert!(env.contains(&("XCTX_SPACE".into(), "prod-hotfix".into())));
    assert_eq!(saved.repos, space.repos);

    // Adding a repo later keeps production on.
    let report = widen(&mut store, &space.id, widen_repos(&[&repos.checkout_web]))?;
    assert!(report.production_enabled);
    assert_eq!(report.added_repos.len(), 1);
    Ok(())
}

#[test]
fn widen_refuses_requests_that_change_nothing_and_spaces_that_are_not_active() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("nothing new", &[&repos.payments]))?;

    assert!(matches!(
        widen(&mut store, &space.id, WidenRequest::default()),
        Err(Error::EmptyWiden(_))
    ));
    assert!(matches!(
        widen(&mut store, &space.id, widen_repos(&[&repos.payments])),
        Err(Error::EmptyWiden(_))
    ));
    let production = WidenRequest {
        repos: Vec::new(),
        production: true,
    };
    widen(&mut store, &space.id, production.clone())?;
    assert!(
        matches!(
            widen(&mut store, &space.id, production.clone()),
            Err(Error::EmptyWiden(_))
        ),
        "production is already on"
    );
    assert!(matches!(
        widen(&mut store, &space.id, widen_repos(&[&repos.notes])),
        Err(Error::NotARepository(_))
    ));

    assert!(matches!(
        widen(
            &mut store,
            &xctx_core::SpaceId::new("nope"),
            production.clone()
        ),
        Err(Error::SpaceNotFound(_))
    ));
    discard(&mut store, &space.id, false)?;
    assert!(matches!(
        widen(&mut store, &space.id, widen_repos(&[&repos.checkout_web])),
        Err(Error::SpaceNotActive { .. })
    ));
    assert!(!xgit::branch_exists(
        &repos.checkout_web,
        "space/nothing-new"
    )?);
    Ok(())
}

#[test]
fn a_failed_widen_removes_what_it_made_and_leaves_the_space_as_it_was() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let api = root.path.join("code").join("api");
    make_repo(&api, &[("README.md", "api\n")])?;
    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("rollback check", &[&repos.payments]))?;
    let before = store.space(&space.id).cloned();

    // api's worktree is made, then checkout-web fails: the folder its worktree goes in is a file.
    fs::write(repos.code.join("web").join("checkout-web-worktrees"), "x")?;
    let result = widen(
        &mut store,
        &space.id,
        WidenRequest {
            repos: vec![
                DraftRepo::new(&api, "widened"),
                DraftRepo::new(&repos.checkout_web, "widened"),
            ],
            production: true,
        },
    );
    assert!(matches!(result, Err(Error::Command { .. })), "{result:?}");
    assert!(
        !root
            .path
            .join("code")
            .join("api-worktrees")
            .join("rollback-check")
            .exists()
    );
    assert!(!root.path.join("code").join("api-worktrees").exists());
    assert!(!xgit::branch_exists(&api, "space/rollback-check")?);
    assert!(!xgit::branch_exists(
        &repos.checkout_web,
        "space/rollback-check"
    )?);
    assert_eq!(xgit::list_worktrees(&api)?.len(), 1);
    let exclude = fs::read_to_string(api.join(".git/info/exclude"))?;
    assert!(!exclude.contains("mise.rollback-check.toml"), "{exclude}");

    assert_eq!(store.space(&space.id).cloned(), before);
    assert_eq!(
        Store::open(root.path.join("data"))?
            .space(&space.id)
            .cloned(),
        before
    );
    let existing = &space.repos[0];
    assert!(existing.worktree.join(space.mise_file_name()).exists());
    assert!(xgit::branch_exists(&repos.payments, &existing.branch)?);
    assert_eq!(xgit::list_worktrees(&repos.payments)?.len(), 2);
    Ok(())
}

#[test]
fn a_widen_the_store_cannot_save_is_rolled_back() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let data = root.path.join("data");
    let mut store = Store::open(&data)?;
    let space = launch(&mut store, draft("store failure", &[&repos.payments]))?;

    fs::set_permissions(&data, fs::Permissions::from_mode(0o555))?;
    let result = widen(&mut store, &space.id, widen_repos(&[&repos.checkout_web]));
    fs::set_permissions(&data, fs::Permissions::from_mode(0o755))?;
    assert!(matches!(result, Err(Error::Io { .. })), "{result:?}");
    assert!(
        !repos
            .code
            .join("web")
            .join("checkout-web-worktrees")
            .join("store-failure")
            .exists()
    );
    assert!(
        !repos
            .code
            .join("web")
            .join("checkout-web-worktrees")
            .exists()
    );
    assert!(!xgit::branch_exists(
        &repos.checkout_web,
        "space/store-failure"
    )?);
    assert_eq!(store.space(&space.id), Some(&space));
    assert_eq!(Store::open(&data)?.space(&space.id), Some(&space));
    Ok(())
}

#[test]
fn widen_never_takes_over_an_existing_space_branch() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("taken branch", &[&repos.payments]))?;
    git(&repos.checkout_web, &["branch", "space/taken-branch"])?;

    let result = widen(&mut store, &space.id, widen_repos(&[&repos.checkout_web]));
    assert!(
        matches!(&result, Err(Error::SpaceBranchTaken { repo, branch })
            if repo == &repos.checkout_web && branch == "space/taken-branch"),
        "{result:?}"
    );
    assert_eq!(store.space(&space.id), Some(&space));
    assert_eq!(xgit::list_worktrees(&repos.checkout_web)?.len(), 1);
    Ok(())
}

#[test]
fn widen_rolls_back_when_a_space_branch_appears_while_it_runs() -> TestResult {
    use std::os::unix::fs::PermissionsExt;

    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let api = root.path.join("code").join("api");
    make_repo(&api, &[("README.md", "api\n")])?;
    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("late branch", &[&repos.payments]))?;

    // Making api's worktree runs its post-checkout hook, which makes checkout-web's space branch after widen
    // checked that it was free.
    let hook = api.join(".git").join("hooks").join("post-checkout");
    fs::create_dir_all(hook.parent().ok_or("hooks folder")?)?;
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\nunset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE GIT_COMMON_DIR\n\
             git -C '{}' branch space/late-branch\n",
            repos.checkout_web.display()
        ),
    )?;
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755))?;

    let result = widen(
        &mut store,
        &space.id,
        widen_repos(&[&api, &repos.checkout_web]),
    );
    assert!(
        matches!(&result, Err(Error::SpaceBranchTaken { repo, branch })
            if repo == &repos.checkout_web && branch == "space/late-branch"),
        "{result:?}"
    );
    assert!(!root.path.join("code").join("api-worktrees").exists());
    assert!(!xgit::branch_exists(&api, "space/late-branch")?);
    assert!(
        !repos
            .code
            .join("web")
            .join("checkout-web-worktrees")
            .exists(),
        "the worktree on the branch widen did not make is removed"
    );
    assert!(
        xgit::branch_exists(&repos.checkout_web, "space/late-branch")?,
        "a branch widen did not make is kept"
    );
    assert_eq!(xgit::list_worktrees(&repos.checkout_web)?.len(), 1);
    assert_eq!(store.space(&space.id), Some(&space));
    Ok(())
}

#[test]
fn widen_gives_repos_with_a_folder_name_already_in_the_space_unique_names() -> TestResult {
    let root = temp_root()?;
    let work_api = root.path.join("work").join("api");
    let oss_api = root.path.join("oss").join("api");
    let fork_api = root.path.join("fork").join("api");
    for (dir, text) in [(&work_api, "work"), (&oss_api, "oss"), (&fork_api, "fork")] {
        make_repo(dir, &[("README.md", text)])?;
    }
    let mut store = Store::open(root.path.join("data"))?;
    let space = launch(&mut store, draft("api names", &[&work_api]))?;

    let report = widen(&mut store, &space.id, widen_repos(&[&oss_api, &fork_api]))?;
    let names: Vec<&str> = report.added_repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["api-2", "api-3"]);
    let saved = store.space(&space.id).ok_or("the space is saved")?;
    let names: Vec<&str> = saved.repos.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["api", "api-2", "api-3"]);
    let worktrees: std::collections::HashSet<_> = saved.repos.iter().map(|r| &r.worktree).collect();
    assert_eq!(worktrees.len(), 3);
    Ok(())
}

#[test]
fn spaces_saved_before_the_production_flag_load_as_not_production() -> TestResult {
    let root = temp_root()?;
    let repos = make_repos(&root.path)?;
    let data = root.path.join("data");
    let mut store = Store::open(&data)?;
    let space = launch(&mut store, draft("old store", &[&repos.payments]))?;

    let path = data.join("spaces.json");
    let mut json: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    let saved = json
        .get_mut("spaces")
        .and_then(|s| s.get_mut(0))
        .and_then(|s| s.as_object_mut())
        .ok_or("spaces.json has the space")?;
    assert!(saved.remove("production").is_some());
    fs::write(&path, serde_json::to_vec_pretty(&json)?)?;

    let reopened = Store::open(&data)?;
    let loaded = reopened.space(&space.id).ok_or("the space loads")?;
    assert!(!loaded.production);
    assert_eq!(loaded, &space);
    assert_eq!(loaded.effective_mise_env(), "old-store");
    Ok(())
}
