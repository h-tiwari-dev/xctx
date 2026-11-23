//! A space's whole life without a window, against real git repositories in a temporary directory: draft, launch,
//! the pane programs it starts, reshape, persistence across a reopened store, then discard or ship.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use xctx_core::{ShipOptions, SpaceShape, SpaceStatus, Store};
use xctx_layout::{PaneKind, Shape};

use crate::lifecycle::{
    dirty_repos, discard_message, discard_space, launch_space, repo_for_draft, reshape_space,
    scan_index, ship_report_lines, ship_space,
};
use crate::persist::restore_layout;
use crate::spawn::{AgentStart, ProgramSearch, pane_launch};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// Run git in `dir` with a throwaway identity and no signing.
fn git(dir: &Path, args: &[&str]) -> TestResult<String> {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=xctx test",
            "-c",
            "user.email=test@xctx.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn make_repo(dir: &Path, readme: &str) -> TestResult {
    fs::create_dir_all(dir)?;
    fs::write(dir.join("README.md"), readme)?;
    git(dir, &["init", "-q", "-b", "main"])?;
    git(dir, &["add", "."])?;
    git(dir, &["commit", "-q", "-m", "first commit"])?;
    Ok(())
}

/// A temporary root with a store, `code/payments` and `code/web` repositories, and a fake `pi` on a private
/// `PATH`, so nothing outside the directory is read or written.
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    payments: PathBuf,
    web: PathBuf,
    search: ProgramSearch,
}

impl Fixture {
    fn new() -> TestResult<Fixture> {
        let dir = tempfile::tempdir()?;
        let root = fs::canonicalize(dir.path())?;
        let payments = root.join("code/payments");
        let web = root.join("code/web");
        make_repo(
            &payments,
            "payments service: checkout retry and rate limits\n",
        )?;
        make_repo(&web, "web frontend\n")?;
        let bin = root.join("bin");
        fs::create_dir_all(&bin)?;
        let pi = bin.join("pi");
        fs::write(&pi, "#!/bin/sh\n")?;
        fs::set_permissions(&pi, fs::Permissions::from_mode(0o755))?;
        let search = ProgramSearch {
            path: vec![bin],
            home: Some(root.join("home")),
            shell: Some("/bin/zsh".to_owned()),
            system_dirs: Vec::new(),
        };
        Ok(Fixture {
            _dir: dir,
            root,
            payments,
            web,
            search,
        })
    }

    fn data_dir(&self) -> PathBuf {
        self.root.join("data")
    }

    fn store(&self) -> TestResult<Store> {
        Ok(Store::open(self.data_dir())?)
    }
}

#[test]
fn a_space_lives_from_draft_to_discard() -> TestResult {
    let fixture = Fixture::new()?;
    let mut store = fixture.store()?;
    store.add_root(fixture.root.join("code"))?;

    // Draft: the index proposes payments; web is added by path. Nothing is on disk yet.
    let index = scan_index(store.roots().to_vec());
    let mut draft = index.draft("make payments checkout retry safe");
    assert_eq!(
        draft.repos.first().map(|r| r.path.clone()),
        Some(fixture.payments.clone())
    );
    draft.add_repo(repo_for_draft(&fixture.web.to_string_lossy(), None)?);
    assert_eq!(draft.repos.len(), 2);
    assert!(store.spaces().is_empty());

    // Launch: worktrees, the space, and its first layout saved.
    let launched = launch_space(&mut store, draft)?;
    assert_eq!(launched.warning, None);
    let mut space = launched.space;
    let mut layout = launched.layout;
    assert_eq!(launched.panes, layout.panes());
    assert_eq!(space.repos.len(), 2);
    for repo in &space.repos {
        assert!(repo.worktree.is_dir(), "{}", repo.worktree.display());
        assert_eq!(repo.branch, space.branch());
    }
    let agents = layout.agent_panes();
    assert_eq!(agents.len(), 2);
    assert!(store.load_layout(&space.id)?.is_some());

    // The programs launch starts: pi with the task in each repo's worktree, shells beside them.
    for pane in &launched.panes {
        let start = AgentStart::Task(space.task.clone());
        let launch = pane_launch(&space, &pane.role, &start, &fixture.search);
        let worktree = space
            .repos
            .iter()
            .find(|r| r.name == pane.role.repo)
            .map(|r| r.worktree.clone());
        assert_eq!(launch.config.cwd, worktree);
        let env = &launch.config.env;
        assert_eq!(
            env.get("MISE_ENV").map(String::as_str),
            Some(space.id.as_str())
        );
        assert_eq!(
            env.get("XCTX_SPACE").map(String::as_str),
            Some(space.id.as_str())
        );
        assert_eq!(env.get("PORT"), Some(&space.port.to_string()));
        match pane.role.kind {
            PaneKind::Agent => {
                assert_eq!(
                    launch.config.program,
                    fixture.root.join("bin/pi").to_string_lossy()
                );
                assert_eq!(launch.config.args, ["--".to_owned(), space.task.clone()]);
            }
            _ => assert_eq!(launch.config.program, "/bin/zsh"),
        }
    }

    // Reshape to Review: agents stay, shells make way for git panes, and both shape and layout are saved.
    let (reshaped, warning) = reshape_space(&mut store, &mut space, &mut layout, Shape::Review)?;
    assert_eq!(warning, None);
    assert_eq!(layout.agent_panes(), agents);
    assert!(reshaped.added.iter().all(|p| p.role.kind == PaneKind::Git));
    assert!(
        reshaped
            .removed
            .iter()
            .all(|p| p.role.kind == PaneKind::Shell)
    );
    assert_eq!(space.shape, SpaceShape::Review);
    let split_pane = layout.split_focused(xctx_layout::SplitAxis::Vertical)?;
    crate::persist::save_layout(&store, &space.id, &layout)?;

    // Quit and reopen: the store has the space, and the saved layout comes back as it was.
    drop(store);
    let mut store = fixture.store()?;
    let reopened = store
        .active_spaces()
        .find(|s| s.id == space.id)
        .cloned()
        .ok_or("the space did not survive reopening")?;
    assert_eq!(reopened.shape, SpaceShape::Review);
    let restored = restore_layout(store.load_layout(&reopened.id), &reopened)?;
    assert_eq!(restored.fallback, None);
    assert_eq!(restored.layout, layout);
    assert!(restored.layout.role(split_pane.id).is_some());

    // Reopened agents resume their sessions.
    for pane in restored.layout.panes() {
        let launch = pane_launch(
            &reopened,
            &pane.role,
            &AgentStart::Continue,
            &fixture.search,
        );
        if pane.role.kind == PaneKind::Agent {
            assert_eq!(launch.config.args, ["--continue"]);
        }
    }

    // Discard: refused while a worktree is dirty, then forced; worktrees go, branches stay.
    let payments_worktree = reopened.repos[0].worktree.clone();
    fs::write(payments_worktree.join("scratch.txt"), "wip\n")?;
    assert_eq!(dirty_repos(&reopened)?, ["payments"]);
    let refused = discard_space(&mut store, &reopened.id, false);
    assert!(
        matches!(refused, Err(xctx_core::Error::DirtyWorktrees { .. })),
        "{refused:?}"
    );
    assert!(payments_worktree.is_dir());
    let report = discard_space(&mut store, &reopened.id, true)?;
    assert_eq!(report.removed.len(), 2);
    assert!(!payments_worktree.exists());
    assert_eq!(store.active_spaces().count(), 0);
    assert_eq!(store.load_layout(&reopened.id)?, None);
    let branch = reopened.branch();
    assert_eq!(
        git(&fixture.payments, &["branch", "--list", &branch])?.trim_start_matches(['*', ' ']),
        branch
    );
    let message = discard_message(&report);
    assert!(message.contains(&branch), "{message}");
    Ok(())
}

#[test]
fn shipping_without_a_remote_keeps_the_work_on_the_branch() -> TestResult {
    let fixture = Fixture::new()?;
    let mut store = fixture.store()?;
    let mut draft = xctx_core::SpaceDraft::new("tidy payments readme");
    draft.add_repo(repo_for_draft(&fixture.payments.to_string_lossy(), None)?);
    let launched = launch_space(&mut store, draft)?;
    let space = launched.space;
    let worktree = space.repos[0].worktree.clone();
    fs::write(worktree.join("NOTES.md"), "notes\n")?;
    git(&worktree, &["add", "."])?;
    git(&worktree, &["commit", "-q", "-m", "notes"])?;

    let report = ship_space(
        &mut store,
        &space.id,
        Some("Tidy the payments readme"),
        &ShipOptions::default(),
    )?;
    let lines = ship_report_lines(&report);
    assert!(lines[0].contains("no remote"), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("kept branch")), "{lines:?}");
    let shipped = store.space(&space.id).ok_or("the space was forgotten")?;
    assert_eq!(shipped.status, SpaceStatus::Shipped);
    assert_eq!(shipped.title, "Tidy the payments readme");
    assert!(!worktree.exists());
    assert_eq!(store.load_layout(&space.id)?, None);
    Ok(())
}
