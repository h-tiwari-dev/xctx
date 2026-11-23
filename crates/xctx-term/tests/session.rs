//! Real PTY integration tests for `TerminalSession`.

use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use xctx_term::{Color, Rgb, TermError, TerminalConfig, TerminalEvent, TerminalSession};

const TIMEOUT: Duration = Duration::from_secs(10);

fn spawn(config: TerminalConfig) -> (TerminalSession, Receiver<TerminalEvent>) {
    let (events_tx, events_rx) = mpsc::channel();
    let session = TerminalSession::spawn(config, move |event| {
        let _ = events_tx.send(event);
    })
    .expect("spawn session");
    (session, events_rx)
}

fn sh(script: &str) -> TerminalConfig {
    TerminalConfig::new("/bin/sh").args(["-c", script])
}

/// Waits until the visible text satisfies `predicate`, returning it.
fn wait_for_text(session: &TerminalSession, predicate: impl Fn(&str) -> bool) -> String {
    let deadline = Instant::now() + TIMEOUT;
    loop {
        let text = session.snapshot().text();
        if predicate(&text) {
            return text;
        }
        assert!(Instant::now() < deadline, "timed out; screen was:\n{text}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn process_is_gone(pid: u32) -> bool {
    let pid = libc::pid_t::try_from(pid).expect("pid fits");
    // A zombie still answers kill(pid, 0); only a reaped pid gives ESRCH.
    let result = unsafe { libc::kill(pid, 0) };
    result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

fn wait_until_gone(pid: u32, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if process_is_gone(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn spawn_renders_truecolor_output() {
    let (session, _events) = spawn(sh(r#"printf '\033[38;2;255;0;128mhello\033[0m\n'"#));
    wait_for_text(&session, |text| text.contains("hello"));
    let snapshot = session.snapshot();
    let row = (0..snapshot.rows)
        .find(|&row| snapshot.row_text(row).starts_with("hello"))
        .expect("row with hello");
    let h = snapshot.cell(row, 0).expect("cell");
    assert_eq!(h.c, 'h');
    assert_eq!(h.fg, Color::Rgb(Rgb::new(255, 0, 128)));
    assert!(session.wait_for_exit(TIMEOUT));
}

#[test]
fn write_reaches_program_stdin() {
    let (session, _events) = spawn(sh(r#"read line; echo "got:$line""#));
    session.write(b"abc\r").expect("write");
    wait_for_text(&session, |text| text.contains("got:abc"));
    assert!(session.wait_for_exit(TIMEOUT));
}

#[test]
fn exit_status_and_event_are_reported() {
    let (session, events) = spawn(sh("exit 3"));
    assert!(session.wait_for_exit(TIMEOUT));
    assert!(!session.is_alive());
    let status = session.exit_status().expect("status collected");
    assert_eq!(status.code(), Some(3));

    let deadline = Instant::now() + TIMEOUT;
    let mut saw_exit = false;
    while let Ok(event) = events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        if let TerminalEvent::Exited(Some(status)) = event {
            assert_eq!(status.code(), Some(3));
            saw_exit = true;
            break;
        }
    }
    assert!(saw_exit, "no Exited event");
    assert!(
        wait_until_gone(session.pid(), TIMEOUT),
        "child left as a zombie"
    );
}

#[test]
fn wakeup_events_arrive_for_output() {
    let (session, events) = spawn(sh("echo hi"));
    let saw_wakeup = events
        .iter()
        .take_while(|event| !matches!(event, TerminalEvent::Exited(_)))
        .any(|event| event == TerminalEvent::Wakeup);
    assert!(saw_wakeup);
    drop(session);
}

#[test]
fn resize_updates_grid_and_program_window_size() {
    let (session, _events) = spawn(sh("read x; stty size"));
    session.resize(100, 30, 9, 18).expect("resize");
    let snapshot = session.snapshot();
    assert_eq!((snapshot.cols, snapshot.rows), (100, 30));
    session.write(b"\r").expect("write");
    wait_for_text(&session, |text| text.contains("30 100"));
}

#[test]
fn default_environment_advertises_truecolor() {
    let (session, _events) =
        spawn(sh(r#"echo "$TERM $COLORTERM $XCTX_TEST""#).env("XCTX_TEST", "set"));
    wait_for_text(&session, |text| {
        text.contains("xterm-256color truecolor set")
    });
}

#[test]
fn cwd_is_applied() {
    let dir = std::env::temp_dir()
        .canonicalize()
        .expect("temp dir")
        .join(format!("xctx-term-cwd-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let (session, _events) = spawn(sh("pwd -P").cwd(&dir));
    let expected = dir.to_string_lossy().into_owned();
    wait_for_text(&session, |text| text.replace('\n', "").contains(&expected));
    assert!(session.wait_for_exit(TIMEOUT));
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn query_replies_are_written_back_to_the_program() {
    // The program asks for primary device attributes and prints the 5-byte reply.
    let (session, _events) = spawn(sh(
        r#"stty -echo -icanon; printf '\033[c'; reply=$(dd bs=1 count=5 2>/dev/null | od -An -c | tr -s ' '); echo "reply:$reply""#,
    ));
    wait_for_text(&session, |text| text.contains("reply: 033 [ ? 6 c"));
}

#[test]
fn output_scrolls_into_history() {
    let (session, _events) = spawn(sh("seq 1 100").size(20, 10));
    wait_for_text(&session, |text| text.contains("100"));
    assert!(session.history_size() >= 90);
    session.scroll_lines(5);
    assert_eq!(session.display_offset(), 5);
    session.scroll_to_bottom();
    assert_eq!(session.display_offset(), 0);
}

#[test]
fn spawn_reports_missing_program() {
    let result = TerminalSession::spawn(
        TerminalConfig::new("/nonexistent/xctx-term-program"),
        |_| {},
    );
    assert!(matches!(result, Err(TermError::Spawn { .. })));
}

#[test]
fn spawn_reports_missing_cwd() {
    let result = TerminalSession::spawn(sh("true").cwd("/nonexistent/xctx-term-dir"), |_| {});
    assert!(matches!(result, Err(TermError::Spawn { .. })));
}

#[test]
fn drop_hangs_up_and_reaps_the_program() {
    let (session, _events) = spawn(TerminalConfig::new("/bin/sleep").args(["30"]));
    let pid = session.pid();
    assert!(session.is_alive());
    drop(session);
    assert!(
        wait_until_gone(pid, Duration::from_secs(5)),
        "sleep survived drop"
    );
}

#[test]
fn drop_kills_a_program_that_ignores_hangup() {
    let (session, _events) = spawn(sh(r#"trap '' HUP; echo ready; sleep 30"#));
    wait_for_text(&session, |text| text.contains("ready"));
    let pid = session.pid();
    let started = Instant::now();
    drop(session);
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "drop blocked"
    );
    assert!(
        wait_until_gone(pid, Duration::from_secs(6)),
        "program survived drop"
    );
}

#[test]
fn write_after_exit_eventually_fails() {
    let (session, _events) = spawn(sh("exit 0"));
    assert!(session.wait_for_exit(TIMEOUT));
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match session.write(b"x") {
            Err(TermError::Closed) => break,
            Ok(()) => {
                assert!(Instant::now() < deadline, "writes kept succeeding");
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(err) => panic!("unexpected error: {err}"),
        }
    }
}

#[test]
fn background_threads_release_the_session_after_drop() {
    use std::sync::Arc;
    for config in [TerminalConfig::new("/bin/sleep").args(["30"]), sh("exit 0")] {
        let sentinel = Arc::new(());
        let held = Arc::clone(&sentinel);
        let session = TerminalSession::spawn(config, move |_| {
            let _ = &held;
        })
        .expect("spawn session");
        drop(session);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Arc::strong_count(&sentinel) > 1 {
            assert!(
                Instant::now() < deadline,
                "a session thread is still running"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn full_screen_program_uses_alt_screen_and_quits() {
    let vim = "/usr/bin/vim";
    if !std::path::Path::new(vim).exists() {
        return;
    }
    // -u NONE -i NONE: no user config, no viminfo, nothing outside the test is read or written.
    let config = TerminalConfig::new(vim)
        .args(["-u", "NONE", "-i", "NONE", "-N", "-n"])
        .cwd(std::env::temp_dir())
        .size(80, 24);
    let (session, _events) = spawn(config);
    wait_for_text(&session, |text| {
        text.lines().filter(|line| line.starts_with('~')).count() > 5
    });
    assert!(session.modes().alt_screen);
    session.write(b"ihello\x1b").expect("write");
    wait_for_text(&session, |text| text.starts_with("hello"));
    session.write(b":q!\r").expect("write");
    assert!(session.wait_for_exit(TIMEOUT));
    assert_eq!(
        session.exit_status().and_then(|status| status.code()),
        Some(0)
    );
}

#[test]
fn ctrl_c_interrupts_the_foreground_program() {
    // Only works when the PTY is the program's controlling terminal.
    let (session, _events) = spawn(sh(
        r#"trap 'echo caught; exit 0' INT; echo ready; while :; do sleep 0.1; done"#,
    ));
    wait_for_text(&session, |text| text.contains("ready"));
    session.write(b"\x03").expect("write");
    wait_for_text(&session, |text| text.contains("caught"));
    assert!(session.wait_for_exit(TIMEOUT));
}

#[test]
fn session_and_snapshot_can_cross_threads() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<TerminalSession>();
    assert_send_sync::<xctx_term::GridSnapshot>();
}
