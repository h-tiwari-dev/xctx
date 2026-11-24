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
            control: None,
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
    let launched = launch_space(&mut store, draft, Vec::new())?;
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
    let launched = launch_space(&mut store, draft, Vec::new())?;
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

#[test]
fn an_approved_widen_request_adds_a_worktree_and_a_writer() -> TestResult {
    use xctx_control::protocol::SpaceWidenProposeParams;
    use xctx_inbox::{Inbox, ItemBody, ItemState, Source};

    use crate::control::widen_proposal;
    use crate::inbox_view::{widen_message, widen_refusal, widen_request};
    use crate::space_graph::{SpaceGraphState, add_writer_lanes};

    let fixture = Fixture::new()?;
    let mut store = fixture.store()?;
    store.add_root(fixture.root.join("code"))?;
    let index = scan_index(store.roots().to_vec());
    let mut draft = index.draft("make payments checkout retry safe");
    draft.repos.retain(|r| r.path == fixture.payments);
    assert_eq!(draft.repos.len(), 1);
    let space = launch_space(&mut store, draft, Vec::new())?.space;
    let mut graph = SpaceGraphState::open(&fixture.data_dir(), &space)?;
    assert_eq!(graph.graph().writers().count(), 1);

    // Propose: an Inbox item, and nothing widened yet.
    let spaces: Vec<_> = store.active_spaces().cloned().collect();
    let params = SpaceWidenProposeParams {
        space: space.id.as_str().to_owned(),
        repos: Some(vec!["web".to_owned()]),
        production: false,
        rationale: Some("the web client reads the retry header".to_owned()),
    };
    let widen = widen_proposal(&spaces, |_| false, &index, &params, None)?;
    assert_eq!(widen.add_repos.len(), 1);
    let mut inbox = Inbox::open(fixture.root.join("inbox"))?;
    let item = inbox.post(
        ItemBody::SpaceWiden(widen),
        Some(space.id.clone()),
        Source::Agent,
    )?;
    assert_eq!(
        store
            .active_spaces()
            .find(|s| s.id == space.id)
            .map(|s| s.repos.len()),
        Some(1)
    );

    // Approve: the steps the Inbox runs, in order.
    let approving = std::collections::HashSet::new();
    assert_eq!(
        widen_refusal(&approving, &item, &spaces, |_| false, &space.id),
        None
    );
    let refused = std::collections::HashSet::from([item.clone()]);
    assert!(widen_refusal(&refused, &item, &spaces, |_| false, &space.id).is_some());
    let request = match &inbox.get(&item).ok_or("the item is gone")?.body {
        ItemBody::SpaceWiden(widen) => widen_request(widen),
        other => return Err(format!("not a widen request: {other:?}").into()),
    };
    let report = xctx_core::widen(&mut store, &space.id, request)?;
    inbox.approve(&item)?;
    add_writer_lanes(&mut graph, &space.id, &report.added_repos)?;

    let widened = store
        .active_spaces()
        .find(|s| s.id == space.id)
        .cloned()
        .ok_or("the space is gone")?;
    assert_eq!(widened.repos.len(), 2);
    let web = widened
        .repos
        .iter()
        .find(|r| r.name == "web")
        .ok_or("web was not added")?;
    assert!(web.worktree.is_dir(), "{}", web.worktree.display());
    assert_eq!(web.branch, space.branch());
    assert_eq!(inbox.get(&item).map(|i| i.state), Some(ItemState::Approved));
    let writers: Vec<Option<&str>> = graph.graph().writers().map(|w| w.role.repo()).collect();
    assert_eq!(writers, [Some("payments"), Some("web")]);
    let reloaded = SpaceGraphState::open(&fixture.data_dir(), &widened)?;
    assert_eq!(reloaded.graph(), graph.graph());
    assert_eq!(
        widen_message(&space.id, &report, false),
        format!("Widened {}: added web", space.id)
    );

    // A second approval of the same repos changes nothing.
    assert!(
        xctx_core::widen(
            &mut store,
            &space.id,
            xctx_core::WidenRequest {
                repos: report
                    .added_repos
                    .iter()
                    .map(|r| xctx_core::DraftRepo::new(r.repo.clone(), "again"))
                    .collect(),
                production: false,
            },
        )
        .is_err()
    );
    Ok(())
}

#[test]
fn attempts_fork_report_diff_and_go_without_a_trace() -> TestResult {
    use xctx_control::protocol::{AttemptDiffParams, AttemptReportParams, ReproStatus};
    use xctx_core::Redactor;

    use crate::attempts::{
        SpaceAttempts, allocate_attempt_ports, attempt_diff, attempt_list, check_report_token,
        delete_pre_adopt_refs, diff_target, live_attempts_refusal, new_attempt_token,
        plan_fork_skipping_leftovers, record_report,
    };

    let fixture = Fixture::new()?;
    let mut store = fixture.store()?;
    let mut draft = xctx_core::SpaceDraft::new("make payments checkout retry safe");
    draft.add_repo(repo_for_draft(&fixture.payments.to_string_lossy(), None)?);
    let space = launch_space(&mut store, draft, Vec::new())?.space;
    let worktree = space.repos[0].worktree.clone();
    fs::write(worktree.join("wip.txt"), "uncommitted work\n")?;

    // A failed fork left attempt 1's branch behind: planning skips it.
    let slug = space.id.as_str();
    git(
        &fixture.payments,
        &["branch", &xctx_core::attempt::attempt_branch(slug, 1)],
    )?;
    let mut attempts = SpaceAttempts::new(Some(fixture.data_dir()));
    let plan = plan_fork_skipping_leftovers(
        attempts.load(&space.id)?,
        slug,
        "writer-payments",
        "payments",
        &worktree,
        vec![Some("use a token bucket".into()), None],
    )?;
    assert_eq!(plan.numbers, [2, 3]);
    git(
        &fixture.payments,
        &["branch", "-D", &xctx_core::attempt::attempt_branch(slug, 1)],
    )?;
    // Planned again now that it is gone, for the numbers the rest of the test uses.
    let plan = plan_fork_skipping_leftovers(
        attempts.load(&space.id)?,
        slug,
        "writer-payments",
        "payments",
        &worktree,
        vec![Some("use a token bucket".into()), None],
    )?;
    assert_eq!(plan.numbers, [1, 2]);

    // Allocate ports outside the space's, fork, record with a report token each.
    let taken: Vec<u16> = store.port_allocations().into_keys().collect();
    let ports = allocate_attempt_ports(plan.numbers.len(), taken)?;
    assert!(!ports.contains(&space.port));
    let pairs: Vec<(u32, u16)> = plan.numbers.iter().copied().zip(ports).collect();
    let made = xctx_core::fork_attempts(&space, "payments", &pairs)?;
    let tokens = [new_attempt_token()?, new_attempt_token()?];
    attempts.change(&space.id, |set| {
        set.record_fork(&plan, made.clone(), "xctx-s-writer-payments", 1)?;
        set.set_token(1, tokens[0].clone())?;
        set.set_token(2, tokens[1].clone())
    })?;
    assert!(made[0].worktree.join("wip.txt").is_file());
    assert!(live_attempts_refusal(&space.id, attempts.load(&space.id)?, "ship").is_some());

    // Attempt 1 works and reports; its diff is masked.
    fs::write(made[0].worktree.join("wip.txt"), "token tok-999999\n")?;
    let set = attempts.load(&space.id)?;
    let params = AttemptDiffParams {
        space: space.id.as_str().to_owned(),
        attempt: 1,
        path: Some("wip.txt".into()),
    };
    let target = diff_target(&space.id, set, &params)?;
    let redactor = Redactor::new([("T".to_owned(), "tok-999999".to_owned())]);
    let diff = attempt_diff(&target, params.path.as_deref(), &redactor)?;
    assert_eq!(
        diff.files
            .iter()
            .map(|f| f.path.as_str())
            .collect::<Vec<_>>(),
        ["wip.txt"]
    );
    let text = diff.diff.unwrap_or_default();
    assert!(
        text.contains("[REDACTED:T]") && !text.contains("tok-999999"),
        "{text}"
    );
    let report = |attempt| AttemptReportParams {
        space: space.id.as_str().to_owned(),
        attempt,
        summary: "swapped the limiter".into(),
        tests_passed: Some(3),
        tests_failed: None,
        test_command: None,
        repro: ReproStatus::Fixed,
        gave_up: false,
        token: Some(tokens[attempt as usize - 1].clone()),
    };
    // Attempt 2's token does not report for attempt 1.
    let first_attempt = attempts
        .load(&space.id)?
        .get(1)
        .cloned()
        .ok_or("no attempt 1")?;
    assert!(check_report_token(&space.id, &first_attempt, Some(&tokens[1])).is_err());
    check_report_token(&space.id, &first_attempt, report(1).token.as_deref())?;
    let first = attempts.change(&space.id, |set| record_report(set, &report(1), 2))?;
    assert!(!first.round_finished);
    let second = attempts.change(&space.id, |set| record_report(set, &report(2), 3))?;
    assert!(second.round_finished);
    let listed = attempt_list(attempts.load(&space.id)?, Some(1), &redactor);
    assert_eq!(listed.attempts.len(), 2);

    // Adopt 1 (the space did not change since the fork), discard 2: nothing of either is left.
    let adopted = xctx_core::adopt_attempt(&space, &made[0])?;
    assert!(adopted.fast_forward);
    assert_eq!(
        fs::read_to_string(worktree.join("wip.txt"))?,
        "token tok-999999\n"
    );
    attempts.change(&space.id, |set| set.mark_adopted(1, 4).map(|_| ()))?;
    for attempt in &made {
        xctx_core::discard_attempt(space.id.as_str(), attempt)?;
        assert!(!attempt.worktree.exists());
    }
    attempts.change(&space.id, |set| set.mark_discarded(2, 5).map(|_| ()))?;
    assert_eq!(
        live_attempts_refusal(&space.id, attempts.load(&space.id)?, "ship"),
        None
    );
    let mut reread = SpaceAttempts::new(Some(fixture.data_dir()));
    assert_eq!(reread.live_ports([&space.id]), Vec::<u16>::new());

    // The space's uncommitted work before the adoption is kept as a ref until the space ends.
    let backup = xctx_core::attempt::pre_adopt_ref(slug, 1);
    git(&fixture.payments, &["rev-parse", "--verify", &backup])?;
    assert_eq!(delete_pre_adopt_refs(&fixture.payments, slug)?, 1);
    assert!(git(&fixture.payments, &["rev-parse", "--verify", &backup]).is_err());
    assert_eq!(delete_pre_adopt_refs(&fixture.payments, slug)?, 0);
    assert_eq!(delete_pre_adopt_refs(&fixture.root.join("gone"), slug)?, 0);
    Ok(())
}
