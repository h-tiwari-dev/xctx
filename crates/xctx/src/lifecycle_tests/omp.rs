//! RPC agents (omp's `--mode rpc-ui`) in a space's life, against `fake-omp` replaying the hand-written fixtures in
//! `crates/xctx/tests/fixtures/omp` (and omp's recorded permission prompt): open, quit, reopen and resume a
//! persisted session; crash and end-of-output restarts; two spaces' agents on their own session folders;
//! `agent.send` and `agent.read` over a real control socket; and a tool approval that blocks the agent and posts
//! an Inbox item. Everything lives in the fixture's temporary directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use xctx_control::protocol::{
    AgentDelivery, AgentProcess, AgentReadParams, AgentReadResult, AgentSendParams,
    AgentSendResult, WhileRunning,
};
use xctx_control::{Call, Client, RpcError, Server, ServerConfig};
use xctx_core::{Redactor, Space, SpaceDraft, Store};
use xctx_graph::{AgentNode, AgentNodeId, AgentState};
use xctx_inbox::{Inbox, ItemBody, ItemState};
use xctx_layout::{PaneKind, PaneRole};
use xctx_plugin::RunnerManifest;

use super::{Fixture, TestResult};
use crate::agent_session::AgentSession;
use crate::lifecycle::{launch_space, repo_for_draft};
use crate::omp_agent::{
    AgentStatus, AgentUpdate, MAX_RESTARTS, OmpAgent, Opening, agent_session_dir, rpc_agent_read,
    rpc_agent_send, session_lease_held,
};
use crate::space_graph::SpaceGraphState;
use crate::spawn::{
    AgentStart, PaneAgent, pane_launch_for_runner, rpc_launch, terminal_resume_launch,
};

const WAIT: Duration = Duration::from_secs(10);
const WRITER: &str = "writer-payments";

/// The `fake-omp` binary `cargo test --workspace` builds beside this test binary.
fn fake_omp() -> TestResult<PathBuf> {
    let exe = std::env::current_exe()?;
    exe.ancestors()
        .skip(1)
        .map(|dir| dir.join("fake-omp"))
        .find(|path| path.is_file())
        .ok_or_else(|| {
            format!(
                "no fake-omp above {}: run cargo build -p xctx-omp --bin fake-omp (cargo test --workspace builds it)",
                exe.display()
            )
            .into()
        })
}

fn ours(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/omp")
        .join(format!("{name}.jsonl"))
}

fn recorded(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../xctx-omp/tests/fixtures")
        .join(format!("{name}.jsonl"))
}

/// An RPC runner whose program is `fake-omp` replaying `fixture`.
fn runner(fixture: &Path) -> TestResult<RunnerManifest> {
    let text = format!(
        "name = \"omp\"\nprogram = {:?}\ninterface = \"rpc\"\ntask = {{ style = \"after-double-dash\" }}\n\
         resume = [\"--continue\"]\nrpc_args = [\"--fixture\", {:?}]\n",
        fake_omp()?.to_string_lossy(),
        fixture.to_string_lossy()
    );
    Ok(RunnerManifest::parse(&text, "test")?)
}

fn launch(fixture: &Fixture, store: &mut Store, task: &str) -> TestResult<Space> {
    let mut draft = SpaceDraft::new(task);
    draft.add_repo(repo_for_draft(&fixture.payments.to_string_lossy(), None)?);
    Ok(launch_space(store, draft, Vec::new())?.space)
}

fn role() -> PaneRole {
    PaneRole::new(PaneKind::Agent, "payments".to_owned())
}

fn writer() -> PaneAgent {
    PaneAgent {
        id: WRITER.to_owned(),
        role: "writer".to_owned(),
    }
}

/// The payments writer of `space` running `fake-omp` on `script`, with every command it is sent logged.
fn agent(
    fixture: &Fixture,
    space: &Space,
    script: &Path,
    session: Option<String>,
) -> TestResult<(OmpAgent, PathBuf)> {
    let dir = agent_session_dir(&fixture.data_dir(), &space.id, WRITER);
    fs::create_dir_all(&dir)?;
    let mut launch = rpc_launch(
        space,
        &role(),
        &fixture.search,
        &runner(script)?,
        Some(&writer()),
        &dir,
    )?;
    let name = script
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let log = fixture.root.join(format!("{}-{name}.log", space.id));
    launch
        .env
        .insert("FAKE_OMP_LOG".into(), log.to_string_lossy().into_owned());
    let agent = OmpAgent::new(
        launch,
        space.id.clone(),
        Some(AgentNodeId::new(WRITER)),
        "payments writer",
        session,
    );
    Ok((agent, log))
}

/// Apply what the child sends until `done` holds, returning every update on the way.
fn pump(
    agent: &mut OmpAgent,
    mut done: impl FnMut(&OmpAgent, &[AgentUpdate]) -> bool,
) -> TestResult<Vec<AgentUpdate>> {
    let deadline = Instant::now() + WAIT;
    let mut all = Vec::new();
    loop {
        all.extend(agent.recv(Duration::from_millis(50)).unwrap_or_default());
        if done(agent, &all) {
            return Ok(all);
        }
        if Instant::now() > deadline {
            return Err(format!("timed out; status {:?}, updates {all:?}", agent.status()).into());
        }
    }
}

/// The commands the fake was sent, in order.
fn sent(log: &Path) -> TestResult<Vec<Value>> {
    Ok(fs::read_to_string(log)?
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

fn node(graph: &SpaceGraphState) -> TestResult<AgentNode> {
    Ok(graph
        .graph()
        .node(&AgentNodeId::new(WRITER))
        .cloned()
        .ok_or("the space has no payments writer")?)
}

fn exited(updates: &[AgentUpdate]) -> Option<(Option<i32>, bool)> {
    updates.iter().find_map(|u| match u {
        AgentUpdate::Exited { code, unexpected } => Some((*code, *unexpected)),
        _ => None,
    })
}

#[test]
fn an_rpc_agent_records_its_session_once_persisted_and_resumes_it_after_a_reopen() -> TestResult {
    let fixture = Fixture::new()?;
    let mut store = fixture.store()?;
    let space = launch(&fixture, &mut store, "make payments checkout retry safe")?;
    let dir = agent_session_dir(&fixture.data_dir(), &space.id, WRITER);
    let mut graph = SpaceGraphState::open(&fixture.data_dir(), &space)?;
    let none = Redactor::default();

    let (mut omp, log) = agent(&fixture, &space, &ours("first-run"), None)?;
    let mut updates = omp.start_blocking(Opening::Task("make checkout retries safe".into()));
    updates.extend(pump(&mut omp, |a, _| *a.status() == AgentStatus::Running)?);
    let problems = omp.apply_updates(&updates, Some(&mut graph), None, &none);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(node(&graph)?.state, AgentState::Running, "agent_start");
    // omp named a session at startup, but its file is not on disk until the turn persists.
    assert_eq!(omp.session_id(), None);

    let updates = pump(&mut omp, |a, _| a.session_id().is_some())?;
    omp.apply_updates(&updates, Some(&mut graph), None, &none);
    assert_eq!(omp.status(), &AgentStatus::Done, "session_settled");
    let saved = node(&graph)?;
    assert_eq!(saved.state, AgentState::Done);
    assert_eq!(saved.session.as_deref(), Some("sess-1"));
    assert!(dir.join("sess-1.jsonl").is_file());
    assert!(
        omp.transcript_tail(10)
            .contains("Retries now reuse one idempotency key."),
        "{}",
        omp.transcript_tail(10)
    );
    assert!(
        sent(&log)?
            .iter()
            .any(|c| c["type"] == "prompt" && c["message"] == "make checkout retries safe")
    );

    // Quit: its input closes, omp exits on its own, and that is a stop, not a failure.
    omp.close();
    let updates = pump(&mut omp, |a, _| a.exit().is_some())?;
    assert_eq!(exited(&updates), Some((Some(0), false)));
    assert_eq!(omp.status(), &AgentStatus::Stopped);
    assert!(!omp.may_restart());
    drop(omp);
    drop(graph);

    // Reopen: the graph read from disk has the session, and the new child resumes the agent's folder.
    let mut graph = SpaceGraphState::open(&fixture.data_dir(), &space)?;
    let session = node(&graph)?.session;
    let (mut omp, log) = agent(&fixture, &space, &ours("resume"), session)?;
    let mut updates = omp.start_blocking(Opening::Resume);
    updates.extend(pump(&mut omp, |_, all| {
        all.iter()
            .any(|u| matches!(u, AgentUpdate::SessionRecorded { id, .. } if id == "sess-1"))
    })?);
    omp.apply_updates(&updates, Some(&mut graph), None, &none);
    let dir_text = dir.to_string_lossy().into_owned();
    assert!(
        sent(&log)?
            .iter()
            .any(|c| c["type"] == "open_session" && c["sessionDir"] == dir_text.as_str()),
        "open_session on the agent's own folder"
    );
    assert_eq!(omp.session_id(), Some("sess-1"));
    assert_eq!(omp.status(), &AgentStatus::Idle);
    assert_eq!(node(&graph)?.session.as_deref(), Some("sess-1"));
    Ok(())
}

#[test]
fn a_crashed_or_closed_rpc_agent_fails_and_restarts_on_its_session_a_bounded_number_of_times()
-> TestResult {
    let fixture = Fixture::new()?;
    let mut store = fixture.store()?;
    let space = launch(&fixture, &mut store, "make payments checkout retry safe")?;
    let mut graph = SpaceGraphState::open(&fixture.data_dir(), &space)?;
    let none = Redactor::default();

    let (mut omp, log) = agent(&fixture, &space, &ours("crash"), Some("sess-1".into()))?;
    let mut opening = Opening::Resume;
    for crash in 0..=MAX_RESTARTS {
        let mut updates = omp.start_blocking(opening);
        updates.extend(pump(&mut omp, |a, _| a.exit().is_some())?);
        assert_eq!(exited(&updates), Some((Some(3), true)), "crash {crash}");
        let AgentStatus::Failed { reason } = omp.status().clone() else {
            return Err(format!("not failed: {:?}", omp.status()).into());
        };
        assert!(reason.contains("exited (3)"), "{reason}");
        omp.apply_updates(&updates, Some(&mut graph), None, &none);
        assert!(matches!(node(&graph)?.state, AgentState::Failed { .. }));
        match omp.restart_opening() {
            Some(next) => opening = next,
            None => {
                assert_eq!(
                    crash, MAX_RESTARTS,
                    "it stops restarting only after the limit"
                );
                break;
            }
        }
        assert_eq!(opening, Opening::Resume);
    }
    let opened = sent(&log)?
        .iter()
        .filter(|c| c["type"] == "open_session")
        .count();
    assert_eq!(
        opened,
        MAX_RESTARTS as usize + 1,
        "every restart resumed its session"
    );

    // A child that closes its output on its own is a failure too, and may be started again.
    let (mut omp, _) = agent(&fixture, &space, &ours("eof"), None)?;
    let mut updates = omp.start_blocking(Opening::Fresh);
    updates.extend(pump(&mut omp, |a, _| a.exit().is_some())?);
    assert_eq!(exited(&updates), Some((Some(0), true)));
    assert!(
        matches!(omp.status(), AgentStatus::Failed { reason } if reason.contains("exited (0)"))
    );
    assert_eq!(omp.restart_opening(), Some(Opening::Resume));
    Ok(())
}

#[test]
fn agents_of_two_spaces_keep_their_sessions_in_their_own_folders() -> TestResult {
    let fixture = Fixture::new()?;
    let mut store = fixture.store()?;
    let first = launch(&fixture, &mut store, "make payments checkout retry safe")?;
    let second = launch(&fixture, &mut store, "add payments rate limits")?;
    let mut files = Vec::new();
    for space in [&first, &second] {
        let (mut omp, _) = agent(&fixture, space, &ours("first-run"), None)?;
        let mut updates = omp.start_blocking(Opening::Task("make checkout retries safe".into()));
        updates.extend(pump(&mut omp, |a, _| a.session_id().is_some())?);
        let file = updates
            .iter()
            .find_map(|u| match u {
                AgentUpdate::SessionRecorded { file, .. } => Some(file.clone()),
                _ => None,
            })
            .ok_or("no session recorded")?;
        let dir = agent_session_dir(&fixture.data_dir(), &space.id, WRITER);
        assert!(
            file.starts_with(&dir) && file.is_file(),
            "{}",
            file.display()
        );
        assert!(dir.starts_with(fixture.data_dir().join("omp-sessions")));
        files.push(file);
    }
    assert_ne!(files[0].parent(), files[1].parent());
    Ok(())
}

#[test]
fn agent_send_and_read_reach_an_rpc_agent_over_the_control_socket() -> TestResult {
    let fixture = Fixture::new()?;
    let mut store = fixture.store()?;
    let space = launch(&fixture, &mut store, "make payments checkout retry safe")?;
    let (mut omp, log) = agent(&fixture, &space, &ours("control"), None)?;
    omp.start_blocking(Opening::Fresh);
    assert_eq!(omp.status(), &AgentStatus::Idle);
    let shared = Arc::new(Mutex::new(omp));

    let handled = Arc::clone(&shared);
    let space_id = space.id.clone();
    let redactor = Redactor::new([("TOKEN".to_owned(), "tok-0123456789".to_owned())]);
    let path = fixture.root.join("control.sock");
    let _server = Server::bind(&path, ServerConfig::default(), move |incoming| {
        let (call, responder) = incoming.into_parts();
        let Ok(mut omp) = handled.lock() else {
            responder.err(RpcError::internal("the agent's lock is poisoned"));
            return;
        };
        match call {
            Call::AgentSend(p) => {
                match rpc_agent_send(
                    &mut omp,
                    &AgentNodeId::new(&p.agent),
                    &p.text,
                    p.while_running,
                ) {
                    Ok(sent) => responder.ok(&sent),
                    Err(e) => responder.err(e),
                }
            }
            Call::AgentRead(p) => responder.ok(&rpc_agent_read(
                &omp,
                &space_id,
                &AgentNodeId::new(&p.agent),
                p.line_count() as usize,
                &redactor,
            )),
            other => responder.err(RpcError::internal(format!("unexpected {other:?}"))),
        }
    })?;
    let mut client = Client::connect(&path)?;
    let mut send = |text: &str, while_running| -> TestResult<AgentSendResult> {
        Ok(client.call_as(&Call::AgentSend(AgentSendParams {
            space: space.id.as_str().to_owned(),
            agent: WRITER.to_owned(),
            text: text.to_owned(),
            while_running,
        }))?)
    };
    let pump_shared = |done: fn(&OmpAgent) -> bool| -> TestResult {
        let mut omp = shared.lock().map_err(|_| "the agent's lock is poisoned")?;
        pump(&mut omp, |a, _| done(a))?;
        Ok(())
    };

    assert_eq!(
        send("ship it", None)?.delivered,
        Some(AgentDelivery::Prompt),
        "idle"
    );
    pump_shared(|a| *a.status() == AgentStatus::Running)?;
    assert_eq!(
        send("also the docs", None)?.delivered,
        Some(AgentDelivery::Steer)
    );
    assert_eq!(
        send("then the changelog", Some(WhileRunning::FollowUp))?.delivered,
        Some(AgentDelivery::FollowUp)
    );
    pump_shared(|a| *a.status() == AgentStatus::Done)?;

    let read: AgentReadResult = client.call_as(&Call::AgentRead(AgentReadParams {
        space: space.id.as_str().to_owned(),
        agent: WRITER.to_owned(),
        lines: Some(40),
    }))?;
    assert_eq!(read.process, AgentProcess::Running);
    assert!(
        read.text.contains("> ship it with token [REDACTED:TOKEN]")
            && read
                .text
                .contains("Shipped; [REDACTED:TOKEN] stays in the vault.")
            && !read.text.contains("tok-0123456789"),
        "{}",
        read.text
    );
    let commands: Vec<(String, String)> = sent(&log)?
        .iter()
        .filter(|c| c.get("message").is_some())
        .map(|c| {
            (
                c["type"].as_str().unwrap_or_default().to_owned(),
                c["message"].as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        commands,
        [
            ("prompt".to_owned(), "ship it".to_owned()),
            ("steer".to_owned(), "also the docs".to_owned()),
            ("follow_up".to_owned(), "then the changelog".to_owned()),
        ]
    );
    Ok(())
}

#[test]
fn a_tool_approval_blocks_the_agent_and_waits_in_the_inbox_masked() -> TestResult {
    let fixture = Fixture::new()?;
    let mut store = fixture.store()?;
    let space = launch(&fixture, &mut store, "make payments checkout retry safe")?;
    let mut graph = SpaceGraphState::open(&fixture.data_dir(), &space)?;
    let mut inbox = Inbox::open(fixture.root.join("inbox"))?;
    let redactor = Redactor::new([("SHELL_CMD".to_owned(), "echo hi".to_owned())]);

    let (mut omp, log) = agent(&fixture, &space, &recorded("permission"), None)?;
    let mut updates = omp.start_blocking(Opening::Task(
        "Run the shell command `echo hi` with the bash tool and reply with its output.".into(),
    ));
    updates.extend(pump(&mut omp, |a, _| {
        matches!(a.status(), AgentStatus::Blocked { .. })
    })?);
    let AgentStatus::Blocked { reason } = omp.status().clone() else {
        return Err("not blocked".into());
    };
    assert!(
        reason.contains("Allow bash?") && reason.contains("Command: echo hi"),
        "{reason}"
    );
    let gate = updates
        .iter()
        .find_map(|u| match u {
            AgentUpdate::GateRaised(gate) => Some(gate.clone()),
            _ => None,
        })
        .ok_or("no gate raised")?;
    assert_eq!(gate.request_id, "15dd27a87ca5884c");
    assert!(
        gate.body.contains("Tool: bash")
            && gate.body.contains("Command: echo hi")
            && gate
                .body
                .contains(r#"{"command":"echo hi","timeout":3600}"#),
        "the exact tool and arguments: {}",
        gate.body
    );

    let problems = omp.apply_updates(&updates, Some(&mut graph), Some(&mut inbox), &redactor);
    assert!(problems.is_empty(), "{problems:?}");
    let AgentState::Blocked { reason } = node(&graph)?.state else {
        return Err("the graph does not say blocked".into());
    };
    assert!(
        reason.contains("[REDACTED:SHELL_CMD]") && !reason.contains("echo hi"),
        "{reason}"
    );
    let item_id = omp
        .inbox_item(&gate.request_id)
        .cloned()
        .ok_or("no Inbox item")?;
    let item = inbox.get(&item_id).ok_or("the Inbox item is gone")?;
    assert_eq!(item.state, ItemState::Pending);
    assert_eq!(item.space.as_ref(), Some(&space.id));
    let ItemBody::Notice(notice) = &item.body else {
        return Err(format!("not a notice: {:?}", item.body).into());
    };
    assert_eq!(notice.title, "payments writer asks: Allow bash?");
    let body = notice.body.clone().unwrap_or_default();
    assert!(
        body.contains("Tool: bash")
            && body.contains("[REDACTED:SHELL_CMD]")
            && !body.contains("echo hi"),
        "{body}"
    );

    // The user denies it: the gate clears, the Inbox item goes, the turn ends.
    let cleared = omp.respond_ui(&xctx_omp::Command::ui_cancelled(&gate.request_id))?;
    assert!(cleared.contains(&AgentUpdate::GateCleared(gate.request_id.clone())));
    omp.apply_updates(&cleared, Some(&mut graph), Some(&mut inbox), &redactor);
    assert_eq!(
        inbox.get(&item_id).map(|i| i.state),
        Some(ItemState::Dismissed)
    );
    let updates = pump(&mut omp, |a, _| *a.status() == AgentStatus::Done)?;
    omp.apply_updates(&updates, Some(&mut graph), Some(&mut inbox), &redactor);
    assert_eq!(node(&graph)?.state, AgentState::Done);
    assert!(
        sent(&log)?
            .iter()
            .any(|c| c["type"] == "extension_ui_response"
                && c["id"] == gate.request_id.as_str()
                && c["cancelled"] == true)
    );
    assert!(
        omp.transcript_tail(20)
            .contains("The shell call was denied")
    );
    Ok(())
}

#[test]
fn rpc_and_terminal_launches_share_one_environment_and_never_widen_the_agent() -> TestResult {
    let fixture = Fixture::new()?;
    let mut store = fixture.store()?;
    let space = launch(&fixture, &mut store, "make payments checkout retry safe")?;
    let script = ours("first-run");
    let runner = runner(&script)?;
    let dir = agent_session_dir(&fixture.data_dir(), &space.id, WRITER);
    let rpc = rpc_launch(
        &space,
        &role(),
        &fixture.search,
        &runner,
        Some(&writer()),
        &dir,
    )?;
    let terminal = pane_launch_for_runner(
        &space,
        &role(),
        &AgentStart::Fresh,
        &AgentSession::Unnamed,
        &fixture.search,
        Some(&runner),
    )
    .with_agent(Some(&writer()));
    let mut pty_env = terminal.config.env.clone();
    assert_eq!(pty_env.remove("TERM").as_deref(), Some("xterm-256color"));
    assert_eq!(pty_env.remove("COLORTERM").as_deref(), Some("truecolor"));
    assert_eq!(
        rpc.env, pty_env,
        "one environment for both, less the terminal's own"
    );
    assert_eq!(
        rpc.env.get("XCTX_AGENT_ID").map(String::as_str),
        Some(WRITER)
    );
    assert_eq!(rpc.env.get("PORT"), Some(&space.port.to_string()));
    assert_eq!(Some(rpc.cwd.clone()), terminal.config.cwd);
    assert_eq!(rpc.cwd, space.repos[0].worktree);
    let dir_text = dir.to_string_lossy().into_owned();
    assert_eq!(
        rpc.args(),
        [
            "--fixture".to_owned(),
            script.to_string_lossy().into_owned(),
            "--session-dir".to_owned(),
            dir_text.clone(),
        ]
    );
    for flag in ["--auto-approve", "--approval-mode", "--no-ui", "--add-dir"] {
        assert!(!rpc.args().iter().any(|a| a.starts_with(flag)), "{flag}");
    }

    let resume = terminal_resume_launch(
        &space,
        &role(),
        &fixture.search,
        &runner,
        Some(&writer()),
        Some("sess-1"),
        &dir,
    );
    assert_eq!(resume.notice, None);
    assert_eq!(
        resume.config.args,
        [
            "--resume".to_owned(),
            "sess-1".to_owned(),
            "--session-dir".to_owned(),
            dir_text
        ]
    );
    assert_eq!(resume.config.env, terminal.config.env);

    // The lease check asks a probe child that holds no session.
    let free = rpc_launch(
        &space,
        &role(),
        &fixture.search,
        &self::runner(&ours("lease-free"))?,
        None,
        &dir,
    )?;
    assert!(free.probe_args().contains(&"--no-session".to_owned()));
    assert_eq!(session_lease_held(&free, "sess-1"), Ok(false));
    let held = rpc_launch(
        &space,
        &role(),
        &fixture.search,
        &self::runner(&ours("lease-held"))?,
        None,
        &dir,
    )?;
    assert_eq!(session_lease_held(&held, "sess-1"), Ok(true));
    Ok(())
}
