//! Against a real omp. Run by hand: `OMP_BIN=/path/to/omp cargo test -p xctx-omp --test smoke -- --ignored`.
//! It never sends a prompt, so it costs no tokens and needs no login.

use std::time::Duration;

use xctx_omp::{
    Block, Command, Effect, Entry, Frame, Incoming, LaunchSpec, OmpClient, SessionModel, ToolState,
};

#[test]
#[ignore = "needs a real omp (set OMP_BIN)"]
fn the_handshake_and_first_picture_work_against_real_omp() {
    let omp = std::env::var("OMP_BIN").unwrap_or_else(|_| "omp".to_owned());
    let cwd = tempfile::tempdir().expect("scratch dir");
    let mut spec = LaunchSpec::new(omp, cwd.path());
    spec.args = vec!["--no-session".into(), "--no-title".into()];
    let started = OmpClient::start(&spec).expect("omp starts and says ready");
    assert!(matches!(started.startup.first(), Some(Frame::Ready(_))));

    let mut model = SessionModel::new();
    for f in started.startup {
        model.apply(f);
    }
    while model.commands.is_empty() || model.model.is_none() {
        match started
            .frames
            .recv_timeout(Duration::from_secs(20))
            .expect("omp answers")
        {
            Incoming::Frame(f) => {
                model.apply(f);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
    assert!(model.commands.iter().any(|c| c.name == "session"));
    assert!(model.entries.is_empty());
}

#[test]
#[ignore = "needs a real omp (set OMP_BIN)"]
fn open_session_resumes_and_hydrates_real_omp_session() {
    let omp = std::env::var("OMP_BIN").unwrap_or_else(|_| "omp".to_owned());
    let cwd_real = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).expect("canonicalize cwd");

    // Discover omp's active provider + model id from an empty session so the synthetic session
    // uses a real model that omp can restore without spending any tokens.
    let (provider, model_id) = {
        let mut probe_spec = LaunchSpec::new(&omp, &cwd_real);
        probe_spec.args = vec!["--no-session".into(), "--no-title".into()];
        let started = OmpClient::start(&probe_spec).expect("probe omp starts");
        let mut model = SessionModel::new();
        for f in started.startup {
            model.apply(f);
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while model.model.is_none() {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .expect("timed out waiting for probe get_state");
            if let Incoming::Frame(f) = started.frames.recv_timeout(remaining).expect("probe frame")
            {
                model.apply(f);
            }
        }
        let info = model.model.expect("default model");
        (info.provider, info.id)
    };

    let session_dir = tempfile::tempdir().expect("session dir");
    let session_id = "019c0000-0000-7000-8000-000000000001";
    let session_file = session_dir
        .path()
        .join(format!("2026-11-28T00-00-00-000Z_{session_id}.jsonl"));
    let cwd_str = cwd_real.to_string_lossy();
    let lines = [
        serde_json::json!({
            "type": "session",
            "version": 3,
            "id": session_id,
            "timestamp": "2026-11-28T00:00:00.000Z",
            "cwd": cwd_str,
        }),
        serde_json::json!({
            "type": "model_change",
            "id": "mc1",
            "parentId": null,
            "timestamp": "2026-11-28T00:00:00.100Z",
            "model": format!("{provider}/{model_id}"),
        }),
        serde_json::json!({
            "type": "message",
            "id": "m1",
            "parentId": "mc1",
            "timestamp": "2026-11-28T00:00:01.000Z",
            "message": {
                "role": "user",
                "content": "please inspect Cargo.toml",
                "timestamp": 1795737601000u64,
            }
        }),
        serde_json::json!({
            "type": "message",
            "id": "m2",
            "parentId": "m1",
            "timestamp": "2026-11-28T00:00:02.000Z",
            "message": {
                "role": "assistant",
                "content": [
                    {"type": "thinking", "thinking": "Reading the workspace manifest."},
                    {"type": "toolCall", "id": "tc-1", "name": "read", "arguments": {"path": "Cargo.toml"}},
                    {"type": "text", "text": "Checking Cargo.toml now."}
                ],
                "provider": provider,
                "model": model_id,
                "usage": {
                    "input": 100,
                    "output": 40,
                    "cacheRead": 0,
                    "cacheWrite": 0,
                    "totalTokens": 140,
                    "cost": {"input": 0.001, "output": 0.001, "cacheRead": 0.0, "cacheWrite": 0.0, "total": 0.002}
                },
                "stopReason": "toolUse",
                "timestamp": 1795737602000u64,
            }
        }),
        serde_json::json!({
            "type": "message",
            "id": "m3",
            "parentId": "m2",
            "timestamp": "2026-11-28T00:00:03.000Z",
            "message": {
                "role": "toolResult",
                "toolCallId": "tc-1",
                "toolName": "read",
                "content": [{"type": "text", "text": "[workspace]\nmembers = []"}],
                "isError": false,
                "timestamp": 1795737603000u64,
            }
        }),
    ];
    let body: String = lines.iter().map(|v| format!("{v}\n")).collect();
    std::fs::write(&session_file, body).expect("write session jsonl");

    // Resume twice in a row (simulating two consecutive app restarts) and verify both hydrate cleanly
    // without creating spurious empty session files.
    for _ in 0..2 {
        let mut spec = LaunchSpec::new(&omp, &cwd_real);
        spec.args = vec![
            "--session-dir".into(),
            session_dir.path().as_os_str().to_os_string(),
            "--no-title".into(),
        ];
        let started = OmpClient::start(&spec).expect("omp starts");
        let mut model = SessionModel::new();
        for f in started.startup {
            for effect in model.apply(f) {
                if let Effect::Send(cmd) = effect {
                    started.client.send(&cmd).expect("send startup effect");
                }
            }
        }
        let dir = session_dir.path().to_string_lossy().into_owned();
        started
            .client
            .send(&Command::open_session(&dir).with_id("open-1"))
            .expect("send open_session");

        let mut got_history = false;
        let mut got_state_after_open = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        while !(got_history && got_state_after_open) {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .expect("timed out waiting for resumed session history");
            match started.frames.recv_timeout(remaining).expect("omp answers") {
                Incoming::Frame(f) => {
                    if let Frame::Response(r) = &f {
                        if r.command == "get_state" && r.id.as_deref() == Some("session-state") {
                            got_state_after_open = true;
                        }
                        if r.command == "get_messages_page"
                            && r.data.get("nextCursor").and_then(|v| v.as_str()).is_none()
                        {
                            got_history = true;
                        }
                    }
                    for effect in model.apply(f) {
                        if let Effect::Send(cmd) = effect {
                            started.client.send(&cmd).expect("send follow-up effect");
                        }
                    }
                }
                other => panic!("unexpected frame: {other:?}"),
            }
        }

        assert_eq!(model.session.id.as_deref(), Some(session_id));
        assert_eq!(model.entries.len(), 2, "entries: {:?}", model.entries);
        assert!(matches!(
            &model.entries[0],
            Entry::User { text, .. } if text == "please inspect Cargo.toml"
        ));
        match &model.entries[1] {
            Entry::Assistant(a) => {
                assert!(
                    a.blocks
                        .iter()
                        .any(|b| matches!(b, Block::Thinking(t) if t.contains("manifest")))
                );
                assert!(
                    a.blocks
                        .iter()
                        .any(|b| matches!(b, Block::ToolCall(id) if id == "tc-1"))
                );
                assert!(
                    a.blocks
                        .iter()
                        .any(|b| matches!(b, Block::Text(t) if t.contains("Checking Cargo.toml")))
                );
            }
            other => panic!("expected assistant entry, got {other:?}"),
        }
        let tool = model.tools.get("tc-1").expect("tool tc-1 hydrated");
        assert_eq!(tool.state, ToolState::Done);
        assert!(tool.output.contains("[workspace]"));
        assert!(model.stats.tokens >= 140, "stats: {:?}", model.stats);
    }

    let jsonl_count = std::fs::read_dir(session_dir.path())
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .count();
    assert_eq!(
        jsonl_count, 1,
        "resuming should not create extra .jsonl files"
    );
}
