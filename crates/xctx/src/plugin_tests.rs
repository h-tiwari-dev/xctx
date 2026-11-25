//! Plugins end to end without a window, in temporary folders: a user plugin whose tool is a shell script, the API
//! client against a local test server, a sqlite database read directly and written only once approved, repo
//! plugin trust, mounts across a restart, and events reaching the loop, the shared space and the Inbox.
//!
//! Nothing here touches the network, the user's plugins, or Chrome.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Instant;

use serde_json::{Map, Value, json};
use xctx_control::ErrorCode;
use xctx_core::{Redactor, Space, SpaceId, SpaceRepo, SpaceShape, SpaceStatus};
use xctx_graph::{AutonomyLevel, LoopStage, LoopStatus};
use xctx_inbox::{Filter, Inbox, ItemBody, PluginGateKind};
use xctx_layout::PaneId;
use xctx_plugin::LoopAction;

use crate::plugin_host::{
    LoopEffect, Mount, MountCheck, MountSettings, PaneEffect, PluginHost, Prepared,
};
use crate::space_graph::SpaceGraphState;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A temporary data directory, user plugins folder and space with one repo whose worktree exists.
struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    data: PathBuf,
    plugins: PathBuf,
    space: Space,
}

impl Fixture {
    fn new() -> TestResult<Fixture> {
        let dir = tempfile::tempdir()?;
        let root = fs::canonicalize(dir.path())?;
        let data = root.join("data");
        let plugins = root.join("plugins");
        let worktree = root.join("api-worktrees/probe-it");
        fs::create_dir_all(&data)?;
        fs::create_dir_all(&plugins)?;
        fs::create_dir_all(&worktree)?;
        let space = Space {
            id: SpaceId::new("probe-it"),
            title: "probe it".into(),
            task: "probe the api".into(),
            shape: SpaceShape::Dev,
            status: SpaceStatus::Active,
            port: 4099,
            created_at: 0,
            repos: vec![SpaceRepo {
                name: "api".into(),
                repo: root.join("api"),
                worktree,
                branch: "space/probe-it".into(),
                base_branch: "main".into(),
                base_commit: "abc".into(),
                branch_created: true,
                reason: "named".into(),
            }],
            production: false,
        };
        Ok(Fixture {
            _dir: dir,
            root,
            data,
            plugins,
            space,
        })
    }

    fn host(&self) -> PluginHost {
        PluginHost::new(Some(self.data.clone()), Some(self.plugins.clone()))
    }

    fn worktree(&self) -> &Path {
        &self.space.repos[0].worktree
    }

    fn mount(
        &self,
        host: &mut PluginHost,
        pane: u64,
        plugin: &str,
        settings: MountSettings,
    ) -> TestResult {
        host.add_mount(
            &self.space.id,
            Mount {
                pane: PaneId(pane),
                plugin: plugin.into(),
                repo: "api".into(),
                settings,
            },
        )?;
        Ok(())
    }

    fn graph(&self) -> TestResult<SpaceGraphState> {
        Ok(SpaceGraphState::open(&self.data, &self.space)?)
    }
}

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn executable(path: &Path, script: &str) -> TestResult {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(path, script)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

/// A user plugin `probe` whose one tool runs a script that prints JSON, its environment and a secret.
fn write_probe_plugin(fx: &Fixture) -> TestResult<PathBuf> {
    let script = fx.root.join("bin/probe.sh");
    executable(
        &script,
        "#!/bin/sh\n\
         echo \"{\\\"event\\\":\\\"probe.down\\\",\\\"path\\\":\\\"$1\\\"}\"\n\
         echo \"env $MISE_ENV $PORT $XCTX_SPACE\"\n\
         echo \"token $API_TOKEN\" >&2\n\
         pwd\n",
    )?;
    let folder = fx.plugins.join("probe");
    fs::create_dir_all(&folder)?;
    fs::write(
        folder.join("plugin.toml"),
        format!(
            r#"
name = "probe"
version = "0.1.0"
description = "Probe the space's API"
secrets = ["API_TOKEN"]

[pane]
kind = "terminal"
run = []

[[tools]]
name = "probe.run"
description = "Probe a path"
effect = "read"
run = ["{script}", "{{path}}"]
params = [{{ name = "path", type = "string", required = true }}]

[[events]]
name = "probe.down"
match = {{ field = "event", equals = "probe.down" }}
action = "fail"

[[events]]
name = "probe.ok"
match = {{ regex = "all good" }}
action = "pass"
"#,
            script = script.display()
        ),
    )?;
    Ok(script)
}

#[test]
fn a_user_plugin_tool_runs_masked_and_its_event_stops_the_loop() -> TestResult {
    let fx = Fixture::new()?;
    write_probe_plugin(&fx)?;
    let mut host = fx.host();
    let secrets: BTreeMap<String, String> =
        [("API_TOKEN".to_owned(), "tok-4f9e2a".to_owned())].into();
    host.learn_secrets(&fx.space.id, &secrets);
    let redactor = Redactor::new(secrets.clone());

    // Not mounted yet: refused, naming how to mount it.
    let refused = host.prepare_call(
        &fx.space,
        "probe",
        "probe.run",
        &args(json!({"path": "/health"})),
        &redactor,
        false,
        None,
    );
    assert!(
        matches!(refused, Err(e) if e.kind() == Some(ErrorCode::Conflict) && e.message.contains("plugin.mount"))
    );

    fx.mount(&mut host, 5, "probe", MountSettings::default())?;
    let Prepared::Run(job) = host.prepare_call(
        &fx.space,
        "probe",
        "probe.run",
        &args(json!({"path": "/health; rm -rf /"})),
        &redactor,
        false,
        None,
    )?
    else {
        return Err("a read tool should run".into());
    };
    let output = job.run()?;
    assert!(
        output.text.contains("env probe-it 4099 probe-it"),
        "{}",
        output.text
    );
    assert!(
        output.text.contains("token [REDACTED:API_TOKEN]"),
        "{}",
        output.text
    );
    assert!(!output.text.contains("tok-4f9e2a"));
    // The argument stayed one argv element: no shell ran it.
    assert!(
        output.text.contains(r#""path":"/health; rm -rf /""#),
        "{}",
        output.text
    );
    assert!(output.text.contains(&fx.worktree().display().to_string()));
    assert_eq!(output.events.len(), 1);
    assert_eq!(output.events[0].0, "probe.down");

    // A missing argument and an unknown tool are refused before anything runs.
    for (tool, call_args) in [
        ("probe.run", json!({})),
        ("probe.nope", json!({"path": "/"})),
    ] {
        let refused = host.prepare_call(
            &fx.space,
            "probe",
            tool,
            &args(call_args),
            &redactor,
            false,
            None,
        );
        assert!(
            matches!(refused, Err(e) if e.kind() == Some(ErrorCode::InvalidParams)),
            "{tool}"
        );
    }

    // The event fails the running loop at its stage, records a finding and tells the Inbox.
    let mut state = fx.graph()?;
    state.change(|doc| {
        doc.loop_run.set_autonomy(AutonomyLevel::UntilChecksPass);
        doc.loop_run
            .start()
            .map_err(|e| xctx_control::RpcError::conflict(e.to_string()))
    })?;
    let mut inbox = Inbox::open(fx.root.join("inbox"))?;
    let (event, detail) = &output.events[0];
    let now = Instant::now();
    let report = host.fire_event(
        Some(&mut state),
        Some(&mut inbox),
        &fx.space,
        "probe",
        event,
        detail,
        &redactor,
        now,
    )?;
    assert_eq!(report.action, Some(LoopAction::Fail));
    assert_eq!(report.loop_effect, LoopEffect::Blocked);
    assert!(
        matches!(state.loop_run().status(), LoopStatus::Blocked { reason } if reason.contains("probe.down"))
    );
    assert_eq!(state.loop_run().stage(), LoopStage::Write);
    let findings = state.shared().findings(true, &redactor);
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].author, "plugin:probe");
    assert!(report.notice.is_some());

    // The same event again moments later does nothing.
    let again = host.fire_event(
        Some(&mut state),
        Some(&mut inbox),
        &fx.space,
        "probe",
        event,
        detail,
        &redactor,
        now,
    )?;
    assert!(again.debounced);
    assert_eq!(state.shared().findings(true, &redactor).len(), 1);

    // A loop that runs again moves on when a pass event fires.
    state.change(|doc| {
        doc.loop_run
            .start()
            .map_err(|e| xctx_control::RpcError::conflict(e.to_string()))
    })?;
    let passed = host.fire_event(
        Some(&mut state),
        Some(&mut inbox),
        &fx.space,
        "probe",
        "probe.ok",
        "all good",
        &redactor,
        now,
    )?;
    assert_eq!(passed.action, Some(LoopAction::Pass));
    assert_eq!(state.loop_run().stage(), LoopStage::Test);
    Ok(())
}

#[test]
fn a_pass_event_under_ask_each_step_waits_for_approval_in_the_inbox() -> TestResult {
    let fx = Fixture::new()?;
    write_probe_plugin(&fx)?;
    let mut host = fx.host();
    let mut state = fx.graph()?;
    state.change(|doc| {
        doc.loop_run
            .start()
            .map_err(|e| xctx_control::RpcError::conflict(e.to_string()))
    })?;
    let mut inbox = Inbox::open(fx.root.join("inbox"))?;
    let report = host.fire_event(
        Some(&mut state),
        Some(&mut inbox),
        &fx.space,
        "probe",
        "probe.ok",
        "all good",
        &Redactor::default(),
        Instant::now(),
    )?;
    assert!(matches!(
        report.loop_effect,
        LoopEffect::Advanced(xctx_graph::LoopDecision::NeedsApproval(LoopStage::Test))
    ));
    let item = report.notice.ok_or("no notice")?;
    let posted = inbox.get(&item).ok_or("missing")?;
    assert!(matches!(&posted.body, ItemBody::Notice(n) if n.title.starts_with("Loop ready for")));
    Ok(())
}

/// Serve each of `responses` to one connection on a local port, in order.
fn serve(responses: Vec<&'static str>) -> TestResult<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    thread::spawn(move || {
        for response in responses {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = Vec::new();
            let mut chunk = [0u8; 1024];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut chunk) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => request.extend_from_slice(&chunk[..n]),
                }
            }
            let _ = stream.write_all(response.as_bytes());
        }
    });
    Ok(port)
}

#[test]
fn the_api_client_sends_records_and_raises_its_status_event() -> TestResult {
    let fx = Fixture::new()?;
    let port = serve(vec![
        "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\nboom",
        "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
    ])?;
    let mut host = fx.host();
    fx.mount(
        &mut host,
        3,
        "api-client",
        MountSettings {
            base_url: Some(format!("http://127.0.0.1:{port}")),
            ..MountSettings::default()
        },
    )?;
    let redactor = Redactor::default();
    let Prepared::Run(job) = host.prepare_call(
        &fx.space,
        "api-client",
        "http.send",
        &args(json!({"method": "get", "path": "/health"})),
        &redactor,
        false,
        None,
    )?
    else {
        return Err("http.send reads".into());
    };
    let output = job.run()?;
    assert_eq!(
        output.json["outcome"]["response"]["status"],
        json!(500),
        "{}",
        output.json
    );
    assert!(output.text.contains("boom"));
    assert_eq!(
        output
            .events
            .iter()
            .map(|e| e.0.as_str())
            .collect::<Vec<_>>(),
        ["response.status"]
    );
    host.apply_effect(&fx.space.id, job.env.pane, &output.effect);
    assert!(matches!(
        output.effect,
        PaneEffect::Http {
            selected: Some(1),
            ..
        }
    ));
    let history = xctx_http::load(&fx.data, &fx.space.id)?;
    assert_eq!(history.exchanges.len(), 1);

    // Replay it from the history; the pane sees both exchanges.
    let Prepared::Run(replay) = host.prepare_call(
        &fx.space,
        "api-client",
        "http.replay",
        &args(json!({"id": "1"})),
        &redactor,
        false,
        None,
    )?
    else {
        return Err("http.replay reads".into());
    };
    let replayed = replay.run()?;
    assert!(replayed.events.is_empty());
    host.apply_effect(&fx.space.id, replay.env.pane, &replayed.effect);
    let data = host.http_data(&fx.space, &redactor);
    assert_eq!(data.history.len(), 2);
    assert_eq!(data.selected, Some(2));
    assert_eq!(data.base_url, Some(format!("http://127.0.0.1:{port}")));

    // response.status is a notice for the built-in API client: the Inbox hears, the loop does not move.
    let mut state = fx.graph()?;
    let mut inbox = Inbox::open(fx.root.join("inbox"))?;
    let report = host.fire_event(
        Some(&mut state),
        Some(&mut inbox),
        &fx.space,
        "api-client",
        "response.status",
        &output.events[0].1,
        &redactor,
        Instant::now(),
    )?;
    assert_eq!(report.action, Some(LoopAction::Notice));
    assert_eq!(report.loop_effect, LoopEffect::Unchanged);
    assert!(report.notice.is_some());
    assert_eq!(state.loop_run().status(), &LoopStatus::Idle);

    // Only the space's own hosts may be reached.
    let Prepared::Run(outside) = host.prepare_call(
        &fx.space,
        "api-client",
        "http.send",
        &args(json!({"method": "GET", "path": "http://example.invalid/"})),
        &redactor,
        false,
        None,
    )?
    else {
        return Err("http.send reads".into());
    };
    assert!(outside.run().is_err());
    Ok(())
}

fn sqlite3(db: &Path, sql: &str) -> TestResult<bool> {
    match Command::new("sqlite3").arg(db).arg(sql).output() {
        Ok(out) if out.status.success() => Ok(true),
        Ok(out) => Err(String::from_utf8_lossy(&out.stderr).into_owned().into()),
        Err(_) => Ok(false),
    }
}

#[test]
fn the_database_reads_directly_and_writes_only_once_approved() -> TestResult {
    let fx = Fixture::new()?;
    let db = fx.worktree().join("dev.sqlite3");
    if !sqlite3(
        &db,
        "CREATE TABLE users (name TEXT); INSERT INTO users VALUES ('ada'), ('bob');",
    )? {
        eprintln!("skipping: sqlite3 is not installed");
        return Ok(());
    }
    let mut host = fx.host();
    fx.mount(
        &mut host,
        4,
        "database",
        MountSettings {
            database: Some("dev.sqlite3".into()),
            ..MountSettings::default()
        },
    )?;
    let redactor = Redactor::default();
    let count = |host: &mut PluginHost| -> TestResult<Value> {
        let Prepared::Run(job) = host.prepare_call(
            &fx.space,
            "database",
            "sql.read",
            &args(json!({"query": "SELECT count(*) AS n FROM users"})),
            &redactor,
            false,
            None,
        )?
        else {
            return Err("sql.read reads".into());
        };
        let out = job.run()?;
        host.apply_effect(&fx.space.id, job.env.pane, &out.effect);
        Ok(out.json["rows"][0][0].clone())
    };
    assert_eq!(count(&mut host)?, json!(2));
    let data = host.sql_data(&fx.space, PaneId(4), &redactor);
    assert_eq!(data.engine, "sqlite");
    assert_eq!(data.result.map(|t| t.columns), Some(vec!["n".to_owned()]));

    // A write through sql.read is refused by the read-only guard.
    let Prepared::Run(sneaky) = host.prepare_call(
        &fx.space,
        "database",
        "sql.read",
        &args(json!({"query": "DELETE FROM users"})),
        &redactor,
        false,
        None,
    )?
    else {
        return Err("sql.read reads".into());
    };
    assert!(sneaky.run().is_err());

    // sql.write becomes a gate with its arguments shown; nothing runs until it is approved.
    let write = args(json!({"statement": "INSERT INTO users VALUES ('cy')", "reason": "seed"}));
    let Prepared::Gate(gate, pending) = host.prepare_call(
        &fx.space,
        "database",
        "sql.write",
        &write,
        &redactor,
        false,
        None,
    )?
    else {
        return Err("sql.write must wait in the Inbox".into());
    };
    match &gate.kind {
        PluginGateKind::WriteCall { tool, args } => {
            assert_eq!(tool, "sql.write");
            assert!(args.contains("INSERT INTO users"));
        }
        other => return Err(format!("unexpected gate {other:?}").into()),
    }
    assert_eq!(gate.rationale.as_deref(), Some("seed"));
    assert_eq!(pending.pane, Some(PaneId(4)));
    assert_eq!(count(&mut host)?, json!(2));

    let Prepared::Run(approved) = host.prepare_call(
        &fx.space,
        "database",
        &pending.tool,
        &pending.args,
        &redactor,
        true,
        pending.pane,
    )?
    else {
        return Err("an approved write runs".into());
    };
    approved.run()?;
    assert_eq!(count(&mut host)?, json!(3));
    Ok(())
}

#[test]
fn repo_plugins_need_trust_again_when_their_manifest_changes() -> TestResult {
    let fx = Fixture::new()?;
    let folder = fx.worktree().join(".xctx/plugins/seed");
    fs::create_dir_all(&folder)?;
    let manifest = |run: &str| {
        format!(
            "name = \"seed\"\nversion = \"1\"\ndescription = \"seed data\"\n[pane]\nkind = \"terminal\"\nrun = [\"{run}\"]\n"
        )
    };
    fs::write(folder.join("plugin.toml"), manifest("make"))?;
    let mut host = fx.host();
    let (check, _) = host.mount_check(&fx.space, "seed", false)?;
    assert_eq!(check, MountCheck::NeedsTrust { repo: "api".into() });
    host.trust_repo_plugin(&fx.space, "seed")?;
    assert_eq!(
        host.mount_check(&fx.space, "seed", false)?.0,
        MountCheck::Ready
    );
    // Trust is recorded on disk, for a host started later.
    assert_eq!(
        fx.host().mount_check(&fx.space, "seed", false)?.0,
        MountCheck::Ready
    );
    fs::write(folder.join("plugin.toml"), manifest("rm"))?;
    assert_eq!(
        fx.host().mount_check(&fx.space, "seed", false)?.0,
        MountCheck::NeedsTrust { repo: "api".into() }
    );
    Ok(())
}

#[test]
fn connectors_with_writes_ask_before_an_agent_mounts_them() -> TestResult {
    let fx = Fixture::new()?;
    let host = fx.host();
    assert!(matches!(
        host.mount_check(&fx.space, "jira", true)?.0,
        MountCheck::NeedsApproval(_)
    ));
    assert_eq!(
        host.mount_check(&fx.space, "jira", false)?.0,
        MountCheck::Ready
    );
    assert_eq!(
        host.mount_check(&fx.space, "logs", true)?.0,
        MountCheck::Ready
    );
    let unknown = host.mount_check(&fx.space, "nope", true);
    assert!(matches!(unknown, Err(e) if e.kind() == Some(ErrorCode::NotFound)));
    Ok(())
}

#[test]
fn mounts_survive_a_restart_and_logs_tail_into_the_pane() -> TestResult {
    let fx = Fixture::new()?;
    let log = fx.worktree().join("app.log");
    fs::write(&log, "INFO up\nERROR down\n")?;
    let mut host = fx.host();
    let settings = MountSettings {
        log_file: Some("app.log".into()),
        ..MountSettings::default()
    };
    fx.mount(&mut host, 6, "logs", settings.clone())?;
    let mut later = fx.host();
    let mounted = later.mounted(&fx.space);
    assert_eq!(mounted.len(), 1);
    assert_eq!(mounted[0].kind, "logs");
    assert_eq!(mounted[0].settings, settings);

    let registry = later.registry(&fx.space);
    let manifest = registry.usable("logs").ok_or("no logs")?.manifest.clone();
    let mount = later.mount_of(&fx.space.id, "logs", None)?;
    assert!(
        later
            .start_backend(&fx.space, &mount, &manifest, &Redactor::default(), None)
            .is_none()
    );
    let deadline = Instant::now() + std::time::Duration::from_secs(10);
    while later
        .logs_data(&fx.space.id, PaneId(6), &Redactor::default())
        .is_none_or(|d| d.lines.len() < 2)
    {
        if Instant::now() > deadline {
            return Err("the tail never read the file".into());
        }
        thread::sleep(std::time::Duration::from_millis(20));
    }
    // logs.query reads the pane's buffer.
    let Prepared::Run(job) = later.prepare_call(
        &fx.space,
        "logs",
        "logs.query",
        &args(json!({"level": "error"})),
        &Redactor::default(),
        false,
        None,
    )?
    else {
        return Err("logs.query reads".into());
    };
    let out = job.run()?;
    assert!(out.text.contains("ERROR down"), "{}", out.text);
    assert!(!out.text.contains("INFO up"), "{}", out.text);

    later.remove_mount(&fx.space.id, PaneId(6))?;
    assert!(later.stop_backend(&fx.space, PaneId(6)).is_none());
    assert!(fx.host().mounted(&fx.space).is_empty());
    assert!(
        Inbox::open(fx.root.join("inbox"))?
            .list(&Filter::default())
            .iter()
            .all(|i| i.space.is_none())
    );
    Ok(())
}
