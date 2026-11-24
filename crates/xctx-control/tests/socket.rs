//! The server, client and CLI talking over real sockets in temporary directories.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use xctx_control::protocol::{
    AgentParams, ContractAgreeParams, LoopAutonomy, LoopSetAutonomyParams, PaneParams,
    PaneSplitParams, PaneSplitResult, SpaceListResult, SpaceSummary,
};
use xctx_control::{
    Call, Capability, Client, ControlError, ErrorCode, Incoming, PROTOCOL_VERSION, Responder,
    RpcError, Server, ServerConfig,
};
use xctx_layout::{PaneId, Shape, SplitAxis};

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// Leave a socket file at `path` that nothing listens on, as a crashed window does. A child process another
/// test forks can hold the dropped listener open until it execs, so wait until connecting is refused.
fn stale_socket(path: &Path) -> std::io::Result<()> {
    drop(UnixListener::bind(path)?);
    for _ in 0..500 {
        if UnixStream::connect(path).is_err() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(10));
    }
    Err(std::io::Error::other(
        "the dropped listener never went away",
    ))
}

fn temp() -> std::io::Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("control.sock");
    Ok((dir, path))
}

/// A handler that answers like a tiny window with one space.
fn window(incoming: Incoming) {
    let (call, responder) = incoming.into_parts();
    match call {
        Call::SpaceList => responder.ok(&SpaceListResult {
            spaces: vec![SpaceSummary {
                id: "rate-limit".into(),
                title: "Rate limit".into(),
                port: 4001,
                shape: Shape::Dev,
                repos: vec![],
            }],
        }),
        Call::PaneSplit(p) if p.space == "rate-limit" => responder.ok(&PaneSplitResult {
            pane: PaneId(p.pane.map_or(1, |p| p.0) + 10),
        }),
        Call::PaneClose(PaneParams { space, .. }) | Call::PaneFocus(PaneParams { space, .. }) => {
            responder.err(RpcError::not_found(format!("no space named {space}")))
        }
        other => responder.err(RpcError::internal(format!("unexpected {other:?}"))),
    }
}

fn serve(path: &Path) -> Result<Server, ControlError> {
    Server::bind(path, ServerConfig::default(), window)
}

#[test]
fn ping_is_answered_by_the_server_itself() -> TestResult {
    let (_dir, path) = temp()?;
    let _server = Server::bind(&path, ServerConfig::default(), |_| {
        panic!("ping must not reach the handler")
    })?;
    let pong = Client::connect(&path)?.ping()?;
    assert_eq!(pong.protocol, PROTOCOL_VERSION);
    assert_eq!(pong.pid, std::process::id());
    Ok(())
}

#[test]
fn calls_reach_the_handler_and_results_come_back() -> TestResult {
    let (_dir, path) = temp()?;
    let _server = serve(&path)?;
    let mut client = Client::connect(&path)?;
    let list: SpaceListResult = client.call_as(&Call::SpaceList)?;
    assert_eq!(list.spaces[0].id, "rate-limit");
    let split: PaneSplitResult = client.call_as(&Call::PaneSplit(PaneSplitParams {
        space: "rate-limit".into(),
        pane: Some(PaneId(2)),
        axis: SplitAxis::Horizontal,
        kind: None,
        repo: None,
    }))?;
    assert_eq!(split.pane, PaneId(12));
    Ok(())
}

#[test]
fn handler_errors_arrive_with_their_code() -> TestResult {
    let (_dir, path) = temp()?;
    let _server = serve(&path)?;
    let result = Client::connect(&path)?.call(&Call::PaneClose(PaneParams {
        space: "gone".into(),
        pane: PaneId(1),
    }));
    match result {
        Err(ControlError::Rpc(e)) => {
            assert_eq!(e.kind(), Some(ErrorCode::NotFound));
            assert!(e.message.contains("gone"));
        }
        other => panic!("expected an rpc error, got {other:?}"),
    }
    Ok(())
}

#[test]
fn methods_outside_the_granted_capabilities_are_forbidden() -> TestResult {
    let (_dir, path) = temp()?;
    let config = ServerConfig {
        capabilities: [Capability::Read].into_iter().collect(),
        ..ServerConfig::default()
    };
    let _server = Server::bind(&path, config, window)?;
    let mut client = Client::connect(&path)?;
    assert!(client.call(&Call::SpaceList).is_ok());
    let result = client.call(&Call::PaneFocus(PaneParams {
        space: "rate-limit".into(),
        pane: PaneId(1),
    }));
    assert!(
        matches!(&result, Err(ControlError::Rpc(e)) if e.kind() == Some(ErrorCode::Forbidden)),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn the_default_config_forbids_pane_agents_to_resume_agents_or_raise_loop_autonomy() -> TestResult {
    let steering = [
        Call::GraphResume(AgentParams {
            space: "rate-limit".into(),
            agent: "writer-api".into(),
        }),
        Call::LoopSetAutonomy(LoopSetAutonomyParams {
            space: "rate-limit".into(),
            autonomy: LoopAutonomy::UntilChecksPass,
            max_iterations: None,
            max_turns: None,
        }),
    ];
    let pause = Call::GraphPause(AgentParams {
        space: "rate-limit".into(),
        agent: "writer-api".into(),
    });
    let answer_all = |incoming: Incoming| incoming.respond(Ok(json!({})));

    let (_dir, path) = temp()?;
    assert!(
        !ServerConfig::default()
            .capabilities
            .contains(&Capability::Steer)
    );
    let _server = Server::bind(&path, ServerConfig::default(), answer_all)?;
    let mut client = Client::connect(&path)?;
    for call in &steering {
        let result = client.call(call);
        assert!(
            matches!(&result, Err(ControlError::Rpc(e))
                if e.kind() == Some(ErrorCode::Forbidden) && e.message.contains("Steer")),
            "{}: {result:?}",
            call.method()
        );
    }
    assert!(client.call(&pause).is_ok(), "pausing stays with Layout");

    let (_steer_dir, steer_path) = temp()?;
    let trusted = ServerConfig {
        capabilities: Capability::ALL.into_iter().collect(),
        ..ServerConfig::default()
    };
    let _trusted_server = Server::bind(&steer_path, trusted, answer_all)?;
    let mut trusted_client = Client::connect(&steer_path)?;
    for call in &steering {
        assert!(trusted_client.call(call).is_ok(), "{}", call.method());
    }
    Ok(())
}

#[test]
fn an_unanswered_request_times_out_and_a_dropped_one_fails() -> TestResult {
    let (_dir, path) = temp()?;
    let parked: Arc<Mutex<Vec<Responder>>> = Arc::default();
    let config = ServerConfig {
        reply_timeout: Duration::from_millis(100),
        ..ServerConfig::default()
    };
    let _server = Server::bind(&path, config, {
        let parked = Arc::clone(&parked);
        move |incoming: Incoming| {
            let (call, responder) = incoming.into_parts();
            if call == Call::SpaceList {
                assert!(!responder.is_cancelled(), "cancelled before the timeout");
                if let Ok(mut parked) = parked.lock() {
                    parked.push(responder);
                }
            } else {
                drop(responder);
            }
        }
    })?;
    let mut client = Client::connect(&path)?;
    let timed_out = client.call(&Call::SpaceList);
    assert!(
        matches!(&timed_out, Err(ControlError::Rpc(e)) if e.kind() == Some(ErrorCode::Timeout)),
        "{timed_out:?}"
    );
    let dropped = client.call(&Call::PaneFocus(PaneParams {
        space: "s".into(),
        pane: PaneId(1),
    }));
    assert!(
        matches!(&dropped, Err(ControlError::Rpc(e)) if e.kind() == Some(ErrorCode::InternalError)),
        "{dropped:?}"
    );
    // The parked request knows the client stopped waiting; answering after the timeout is harmless.
    if let Ok(mut parked) = parked.lock() {
        assert_eq!(parked.len(), 1);
        for responder in parked.drain(..) {
            assert!(responder.is_cancelled());
            responder.ok(&json!({}));
        }
    }
    assert!(client.ping().is_ok());
    Ok(())
}

#[test]
fn notifications_get_no_response() -> TestResult {
    let (_dir, path) = temp()?;
    let seen = Arc::new(Mutex::new(0));
    let _server = Server::bind(&path, ServerConfig::default(), {
        let seen = Arc::clone(&seen);
        move |incoming: Incoming| {
            if let Ok(mut n) = seen.lock() {
                *n += 1;
            }
            incoming.respond(Ok(json!({})));
        }
    })?;
    let mut client = Client::connect(&path)?;
    client.notify(&Call::SpaceList)?;
    // The next line on the socket is ping's answer, not one for the notification.
    assert!(client.ping().is_ok());
    assert_eq!(*seen.lock().map_err(|e| e.to_string())?, 1);
    Ok(())
}

#[test]
fn bad_lines_get_errors_and_the_connection_stays_usable() -> TestResult {
    let (_dir, path) = temp()?;
    let _server = serve(&path)?;
    let stream = UnixStream::connect(&path)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut exchange = |line: &str| -> Result<Value, Box<dyn std::error::Error>> {
        writeln!(writer, "{line}")?;
        let mut reply = String::new();
        reader.read_line(&mut reply)?;
        Ok(serde_json::from_str(&reply)?)
    };
    let reply = exchange("this is not json")?;
    assert_eq!(reply["error"]["code"], json!(-32700));
    assert_eq!(reply["id"], Value::Null);
    let reply = exchange(r#"{"jsonrpc":"2.0","id":"a","method":"pane.explode"}"#)?;
    assert_eq!(reply["error"]["code"], json!(-32601));
    assert_eq!(reply["id"], json!("a"));
    let reply =
        exchange(r#"{"jsonrpc":"2.0","id":2,"method":"pane.close","params":{"space":"s"}}"#)?;
    assert_eq!(reply["error"]["code"], json!(-32602));
    let reply = exchange(r#"{"jsonrpc":"2.0","id":3,"method":"ping"}"#)?;
    assert_eq!(reply["result"]["protocol"], json!(PROTOCOL_VERSION));
    assert_eq!(reply["id"], json!(3));
    Ok(())
}

#[test]
fn clients_are_served_concurrently() -> TestResult {
    let (_dir, path) = temp()?;
    let _server = serve(&path)?;
    let handles: Vec<_> = (0..8)
        .map(|_| {
            let path = path.clone();
            thread::spawn(move || -> Result<usize, ControlError> {
                let mut client = Client::connect(&path)?;
                let mut n = 0;
                for _ in 0..20 {
                    let list: SpaceListResult = client.call_as(&Call::SpaceList)?;
                    n += list.spaces.len();
                }
                Ok(n)
            })
        })
        .collect();
    for handle in handles {
        let n = handle.join().map_err(|_| "client thread panicked")??;
        assert_eq!(n, 20);
    }
    Ok(())
}

#[test]
fn the_socket_is_private_and_removed_on_drop() -> TestResult {
    let (_dir, path) = temp()?;
    let server = serve(&path)?;
    let mode = std::fs::metadata(&path)?.permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    server.shutdown();
    assert!(!path.exists());
    assert!(matches!(
        Client::connect(&path),
        Err(ControlError::NotRunning(_))
    ));
    Ok(())
}

#[test]
fn a_live_socket_is_not_taken_over_but_a_stale_one_is() -> TestResult {
    let (_dir, path) = temp()?;
    let first = serve(&path)?;
    assert!(matches!(serve(&path), Err(ControlError::AlreadyRunning(_))));
    assert!(
        Client::connect(&path)?.ping().is_ok(),
        "the first server still answers"
    );
    drop(first);

    // A socket file nothing listens on, as a crashed window leaves behind.
    stale_socket(&path)?;
    assert!(path.exists());
    let _second = serve(&path)?;
    assert!(Client::connect(&path)?.ping().is_ok());
    Ok(())
}

#[test]
fn stopping_a_server_whose_socket_was_replaced_leaves_the_new_one_alone() -> TestResult {
    let (_dir, path) = temp()?;
    let old = serve(&path)?;
    std::fs::remove_file(&path)?;
    // Something that does not take the lock, such as an older xctx, binds the path.
    let _new = UnixListener::bind(&path)?;
    let started = std::time::Instant::now();
    drop(old);
    assert!(started.elapsed() < Duration::from_secs(2), "shutdown hung");
    assert!(path.exists());
    assert!(UnixStream::connect(&path).is_ok());
    Ok(())
}

#[test]
fn two_servers_binding_at_once_leave_one_running_and_refuse_the_other() -> TestResult {
    for _ in 0..20 {
        let (_dir, path) = temp()?;
        // A stale socket both will want to clear.
        stale_socket(&path)?;
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let racers: Vec<_> = (0..2)
            .map(|_| {
                let barrier = Arc::clone(&barrier);
                let path = path.clone();
                thread::spawn(move || {
                    barrier.wait();
                    serve(&path)
                })
            })
            .collect();
        let mut results = Vec::new();
        for racer in racers {
            results.push(racer.join().map_err(|_| "bind thread panicked")?);
        }
        let running: Vec<_> = results.iter().filter(|r| r.is_ok()).collect();
        assert_eq!(running.len(), 1, "{results:?}");
        assert!(
            results
                .iter()
                .any(|r| matches!(r, Err(ControlError::AlreadyRunning(_)))),
            "{results:?}"
        );
        assert!(
            Client::connect(&path)?.ping().is_ok(),
            "the winner's socket is still there"
        );
    }
    Ok(())
}

#[test]
fn a_held_lock_refuses_to_bind_without_touching_the_socket() -> TestResult {
    let (_dir, path) = temp()?;
    let holder = serve(&path)?;
    assert!(path.with_file_name("control.sock.lock").exists());
    // The holder's socket is gone and a stale one sits in its place; a second server must leave it alone.
    std::fs::remove_file(&path)?;
    stale_socket(&path)?;
    let before = std::fs::symlink_metadata(&path)?;
    assert!(matches!(serve(&path), Err(ControlError::AlreadyRunning(_))));
    let after = std::fs::symlink_metadata(&path)?;
    assert_eq!(
        std::os::unix::fs::MetadataExt::ino(&before),
        std::os::unix::fs::MetadataExt::ino(&after)
    );
    drop(holder);
    let _next = serve(&path)?;
    assert!(Client::connect(&path)?.ping().is_ok());
    Ok(())
}

#[test]
fn a_file_that_is_not_a_socket_is_left_alone() -> TestResult {
    let (_dir, path) = temp()?;
    std::fs::write(&path, b"precious")?;
    assert!(matches!(serve(&path), Err(ControlError::Io { .. })));
    assert_eq!(std::fs::read(&path)?, b"precious");
    Ok(())
}

#[test]
fn the_parent_directory_is_created() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("nested").join("control.sock");
    let _server = serve(&path)?;
    assert!(Client::connect(&path)?.ping().is_ok());
    Ok(())
}

#[test]
fn xctx_ctl_calls_the_window_with_the_space_from_the_environment() -> TestResult {
    let (_dir, path) = temp()?;
    let seen: Arc<Mutex<Vec<Call>>> = Arc::default();
    let _server = Server::bind(&path, ServerConfig::default(), {
        let seen = Arc::clone(&seen);
        move |incoming: Incoming| {
            if let Ok(mut seen) = seen.lock() {
                seen.push(incoming.call().clone());
            }
            window(incoming);
        }
    })?;
    let ctl = env!("CARGO_BIN_EXE_xctx-ctl");

    let out = Command::new(ctl)
        .args(["--compact", "pane.split", "axis=vertical", "pane=3"])
        .env("XCTX_CONTROL_SOCKET", &path)
        .env("XCTX_SPACE", "rate-limit")
        .output()?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout)?.trim(), r#"{"pane":13}"#);

    let out = Command::new(ctl)
        .args(["--socket"])
        .arg(&path)
        .args(["pane.focus", "space=gone", "pane=1"])
        .env_remove("XCTX_SPACE")
        .output()?;
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8(out.stderr)?.contains("no space named gone"));

    let out = Command::new(ctl)
        .args(["pane.close"])
        .env("XCTX_CONTROL_SOCKET", &path)
        .env_remove("XCTX_SPACE")
        .output()?;
    assert_eq!(
        out.status.code(),
        Some(2),
        "a missing space is a usage error"
    );

    let calls = seen.lock().map_err(|e| e.to_string())?.clone();
    assert_eq!(calls.len(), 2);
    assert!(
        matches!(&calls[0], Call::PaneSplit(p) if p.space == "rate-limit" && p.pane == Some(PaneId(3)))
    );
    Ok(())
}

#[test]
fn xctx_ctl_takes_the_calling_agent_from_the_environment() -> TestResult {
    let (_dir, path) = temp()?;
    let seen: Arc<Mutex<Vec<Call>>> = Arc::default();
    let _server = Server::bind(&path, ServerConfig::default(), {
        let seen = Arc::clone(&seen);
        move |incoming: Incoming| {
            if let Ok(mut seen) = seen.lock() {
                seen.push(incoming.call().clone());
            }
            incoming.respond(Ok(json!({})));
        }
    })?;
    let out = Command::new(env!("CARGO_BIN_EXE_xctx-ctl"))
        .args(["contract.agree", "version=2"])
        .env("XCTX_CONTROL_SOCKET", &path)
        .env("XCTX_SPACE", "rate-limit")
        .env("XCTX_AGENT_ID", "writer-api")
        .output()?;
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let calls = seen.lock().map_err(|e| e.to_string())?.clone();
    assert_eq!(
        calls,
        [Call::ContractAgree(ContractAgreeParams {
            space: "rate-limit".into(),
            agent: "writer-api".into(),
            version: Some(2),
        })]
    );
    Ok(())
}

#[test]
fn xctx_ctl_says_when_no_window_is_running() -> TestResult {
    let (_dir, path) = temp()?;
    let out = Command::new(env!("CARGO_BIN_EXE_xctx-ctl"))
        .arg("ping")
        .env("XCTX_CONTROL_SOCKET", &path)
        .output()?;
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8(out.stderr)?.contains("not running"));
    Ok(())
}

/// A fake window that answers every request line with `reply`, for as many connections as come.
fn fake_window(path: &Path, reply: &'static str) -> std::io::Result<()> {
    let listener = UnixListener::bind(path)?;
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { return };
            let Ok(read_half) = stream.try_clone() else {
                return;
            };
            let mut writer = stream;
            for line in BufReader::new(read_half).lines() {
                if line.is_err() || writeln!(writer, "{reply}").is_err() {
                    break;
                }
            }
        }
    });
    Ok(())
}

#[test]
fn a_null_id_is_accepted_only_on_an_error() -> TestResult {
    let (_dir, path) = temp()?;
    fake_window(&path, r#"{"jsonrpc":"2.0","id":null,"result":{}}"#)?;
    let result = Client::connect(&path)?.call(&Call::SpaceList);
    assert!(
        matches!(&result, Err(ControlError::Protocol(_))),
        "{result:?}"
    );

    let (_dir, path) = temp()?;
    fake_window(
        &path,
        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"bad line"}}"#,
    )?;
    let result = Client::connect(&path)?.call(&Call::SpaceList);
    assert!(
        matches!(&result, Err(ControlError::Rpc(e)) if e.kind() == Some(ErrorCode::ParseError)),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn xctx_ctl_exits_1_when_the_window_answers_nonsense() -> TestResult {
    let (_dir, path) = temp()?;
    fake_window(&path, "not json")?;
    let out = Command::new(env!("CARGO_BIN_EXE_xctx-ctl"))
        .arg("ping")
        .env("XCTX_CONTROL_SOCKET", &path)
        .output()?;
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8(out.stderr)?.contains("protocol error"));
    Ok(())
}
