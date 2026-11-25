//! Planning and running tool calls with small shell scripts as fake tools, in temporary directories.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};
use xctx_core::Redactor;
use xctx_plugin::{
    BUILTIN_MANIFESTS, CallContext, Effect, Invocation, LoopAction, Manifest, PluginError,
    match_events, pane_argv, plan_call, run_plan,
};

const SECOND: Duration = Duration::from_secs(1);

fn args(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

fn ctx(worktree: &Path) -> CallContext {
    CallContext {
        worktree: worktree.to_owned(),
        space: "fix-login".to_owned(),
        port: Some(4123),
        mise_env: "fix-login".to_owned(),
        space_production: false,
        secrets: BTreeMap::new(),
        redactor: Redactor::default(),
    }
}

fn builtin(name: &str) -> Manifest {
    let text = BUILTIN_MANIFESTS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, t)| *t)
        .unwrap_or_default();
    Manifest::parse(text, name).unwrap_or_else(|e| panic!("{e}"))
}

/// A manifest with one tool running `sh <worktree>/<script>` plus `extra` argv elements.
fn script_manifest(extra: &str, params: &str, more: &str) -> Manifest {
    let text = format!(
        "name = \"fake\"\nversion = \"1\"\n{more}\n[pane]\nkind = \"terminal\"\n\n[[tools]]\nname = \"fake.run\"\n\
         effect = \"read\"\nrun = [\"/bin/sh\", \"{{WORKTREE}}/tool.sh\"{extra}]\nparams = [{params}]\n"
    );
    Manifest::parse(&text, "fake").unwrap_or_else(|e| panic!("{e}"))
}

fn write_script(dir: &Path, body: &str) {
    fs::write(dir.join("tool.sh"), body).expect("write script");
}

fn argv_of(plan: &xctx_plugin::CallPlan) -> Vec<String> {
    plan.argv().map(<[String]>::to_vec).unwrap_or_default()
}

#[test]
fn placeholder_values_stay_single_literal_arguments() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = script_manifest(
        ", \"{value}\", \"--label=<{value}>\", \"{port}\", \"--port={PORT}\"",
        "{ name = \"value\", type = \"string\", required = true }, { name = \"port\", type = \"integer\" }",
        "",
    );
    let worktree = dir.path().display().to_string();
    for nasty in [
        "; rm -rf /",
        "two words",
        "{PORT} {WORKTREE} {other}",
        "line\nbreak",
        "$(touch pwned)",
        "*",
        "",
    ] {
        let plan = plan_call(
            &m,
            "fake.run",
            &args(json!({ "value": nasty })),
            &ctx(dir.path()),
        )
        .expect("plan");
        assert_eq!(
            argv_of(&plan),
            [
                "/bin/sh".to_owned(),
                format!("{worktree}/tool.sh"),
                nasty.to_owned(),
                format!("--label=<{nasty}>"),
                "--port=4123".to_owned(),
            ],
            "{nasty:?}: the absent optional `{{port}}` element is dropped and nothing else changes"
        );
    }

    // Run one through a real process: the script sees exactly its arguments, and nothing ran in a shell.
    write_script(
        dir.path(),
        "for a in \"$@\"; do printf '<%s>\\n' \"$a\"; done\n",
    );
    let canary = dir.path().join("canary");
    fs::write(&canary, "still here").expect("canary");
    let value = format!("x; rm -f {} $(touch pwned)", canary.display());
    let plan = plan_call(
        &m,
        "fake.run",
        &args(json!({ "value": value, "port": 7 })),
        &ctx(dir.path()),
    )
    .expect("plan");
    let out = run_plan(&plan, 10 * SECOND, 64 * 1024).expect("run");
    assert!(out.success(), "{out:?}");
    assert_eq!(
        out.stdout,
        format!("<{value}>\n<--label=<{value}>>\n<7>\n<--port=4123>\n")
    );
    assert!(canary.exists());
    assert!(!dir.path().join("pwned").exists());
}

#[test]
fn values_that_would_be_options_are_refused_unless_allowed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let c = ctx(dir.path());
    let m = script_manifest(
        ", \"{file}\", \"{name}.txt\", \"--label={label}\", \"{flags}\", \"{n}\", \"{opts}\"",
        "{ name = \"file\", type = \"path\" }, { name = \"name\", type = \"string\" }, \
         { name = \"label\", type = \"string\" }, { name = \"flags\", type = \"strings\" }, \
         { name = \"n\", type = \"integer\" }, { name = \"opts\", type = \"strings\", allow_dash = true }",
        "",
    );
    for call in [
        json!({ "file": "--output=/etc/passwd" }),
        json!({ "file": "-" }),
        json!({ "name": "-rf" }),
        json!({ "flags": ["ok", "--exec=evil"] }),
    ] {
        let err = plan_call(&m, "fake.run", &args(call.clone()), &c).expect_err("refused");
        assert!(
            matches!(&err, PluginError::BadParam { message, .. } if message.contains("option")),
            "{call}: {err}"
        );
    }
    let plan = plan_call(
        &m,
        "fake.run",
        &args(json!({ "file": "a-b", "label": "-x", "n": -3, "opts": ["-la", "--all"] })),
        &c,
    )
    .expect("allowed");
    assert_eq!(
        argv_of(&plan)[2..],
        ["a-b", "--label=-x", "-3", "-la", "--all"]
    );

    // shell.run's arguments may be options; its program may not.
    let terminal = builtin("terminal");
    let plan = plan_call(
        &terminal,
        "shell.run",
        &args(json!({ "command": "ls", "args": ["-la"] })),
        &c,
    )
    .expect("allowed");
    assert_eq!(argv_of(&plan), ["ls", "-la"]);
    assert!(
        plan_call(
            &terminal,
            "shell.run",
            &args(json!({ "command": "-la" })),
            &c
        )
        .is_err()
    );
}

#[test]
fn integer_bounds_are_checked_when_planning() {
    let dir = tempfile::tempdir().expect("tempdir");
    let api = builtin("api-client");
    let c = ctx(dir.path());
    let replay = |times: i64| {
        plan_call(
            &api,
            "http.replay",
            &args(json!({ "id": "h1", "times": times })),
            &c,
        )
    };
    for bad in [0, -1, 101] {
        assert!(
            matches!(replay(bad), Err(PluginError::BadParam { .. })),
            "{bad}"
        );
    }
    let plan = replay(100).expect("allowed");
    assert!(matches!(&plan.invocation, Invocation::Builtin { args, .. } if args["times"] == 100));
    let schema = api.tool("http.replay").expect("tool").input_schema();
    assert_eq!(schema["properties"]["times"]["minimum"], 1);
    assert_eq!(schema["properties"]["times"]["maximum"], 100);
}

#[test]
fn shell_run_passes_its_program_and_args_without_a_shell() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = builtin("terminal");
    let call = args(json!({ "command": "/bin/echo", "args": ["a; rm -rf /", "b c", "{args}"] }));
    let plan = plan_call(&m, "shell.run", &call, &ctx(dir.path())).expect("plan");
    assert_eq!(
        argv_of(&plan),
        ["/bin/echo", "a; rm -rf /", "b c", "{args}"]
    );
    // It runs any program, so it is a write: the app gates it in the Inbox.
    assert_eq!(plan.effect, Effect::Write);
    let out = run_plan(&plan, 10 * SECOND, 1024).expect("run");
    assert_eq!(out.stdout, "a; rm -rf / b c {args}\n");

    let only = plan_call(
        &m,
        "shell.run",
        &args(json!({ "command": "pwd" })),
        &ctx(dir.path()),
    )
    .expect("plan");
    assert_eq!(argv_of(&only), ["pwd"]);
    let out = run_plan(&only, 10 * SECOND, 1024).expect("run");
    let cwd = fs::canonicalize(dir.path()).expect("canonical");
    assert_eq!(out.stdout.trim(), cwd.display().to_string());
}

#[test]
fn bad_arguments_are_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = builtin("terminal");
    let c = ctx(dir.path());
    let refused = |call: Value| plan_call(&m, "shell.run", &args(call), &c).expect_err("refused");
    assert!(matches!(
        refused(json!({})),
        PluginError::MissingParam { .. }
    ));
    assert!(matches!(
        refused(json!({ "command": null })),
        PluginError::MissingParam { .. }
    ));
    assert!(matches!(
        refused(json!({ "command": "ls", "shell": true })),
        PluginError::UnknownParam { .. }
    ));
    assert!(matches!(
        refused(json!({ "command": 3 })),
        PluginError::BadParam { .. }
    ));
    assert!(matches!(
        refused(json!({ "command": "" })),
        PluginError::BadParam { .. }
    ));
    assert!(matches!(
        refused(json!({ "command": "ls", "args": "-la" })),
        PluginError::BadParam { .. }
    ));
    assert!(matches!(
        refused(json!({ "command": "ls", "args": [1] })),
        PluginError::BadParam { .. }
    ));
    assert!(matches!(
        refused(json!({ "command": "ls\0x" })),
        PluginError::BadParam { .. }
    ));
    assert!(matches!(
        plan_call(&m, "shell.nope", &Map::new(), &c),
        Err(PluginError::UnknownTool { .. })
    ));

    let typed = script_manifest(
        ", \"{n}\", \"{flag}\"",
        "{ name = \"n\", type = \"integer\" }, { name = \"flag\", type = \"boolean\" }",
        "",
    );
    let plan = plan_call(
        &typed,
        "fake.run",
        &args(json!({ "n": -3, "flag": true })),
        &c,
    )
    .expect("plan");
    assert_eq!(argv_of(&plan)[2..], ["-3", "true"]);
    assert!(matches!(
        plan_call(&typed, "fake.run", &args(json!({ "n": 1.5 })), &c),
        Err(PluginError::BadParam { .. })
    ));
    assert!(matches!(
        plan_call(&typed, "fake.run", &args(json!({ "flag": "yes" })), &c),
        Err(PluginError::BadParam { .. })
    ));

    let mut no_port = ctx(dir.path());
    no_port.port = None;
    let port_tool = script_manifest(", \"{PORT}\"", "", "");
    assert!(matches!(
        plan_call(&port_tool, "fake.run", &Map::new(), &no_port),
        Err(PluginError::MissingContext { .. })
    ));
}

#[test]
fn production_is_refused_unless_the_space_and_the_scope_both_allow_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let default_scope = script_manifest("", "", "");
    let prod_scope = script_manifest("", "", "[scope]\nmise_envs = [\"space\", \"production\"]\n");
    let staging_only = script_manifest("", "", "[scope]\nmise_envs = [\"staging\"]\n");

    let mut c = ctx(dir.path());
    assert!(plan_call(&default_scope, "fake.run", &Map::new(), &c).is_ok());
    assert!(matches!(
        plan_call(&staging_only, "fake.run", &Map::new(), &c),
        Err(PluginError::ScopeRefused { .. })
    ));

    // MISE_ENV says production but the space was never widened.
    c.mise_env = "production,fix-login".to_owned();
    for m in [&default_scope, &prod_scope] {
        let err = plan_call(m, "fake.run", &Map::new(), &c).expect_err("refused");
        assert!(err.to_string().contains("not widened"), "{err}");
    }

    // Widened, but the default scope never reaches production.
    c.space_production = true;
    let err = plan_call(&default_scope, "fake.run", &Map::new(), &c).expect_err("refused");
    assert!(
        err.to_string().contains("does not include production"),
        "{err}"
    );
    let plan = plan_call(&prod_scope, "fake.run", &Map::new(), &c).expect("allowed");
    assert_eq!(
        plan.env.get("MISE_ENV").map(String::as_str),
        Some("production,fix-login")
    );

    // Built-in plugins are scoped to the space too.
    let api = builtin("api-client");
    let call = args(json!({ "method": "GET", "path": "/health" }));
    assert!(matches!(
        plan_call(&api, "http.send", &call, &c),
        Err(PluginError::ScopeRefused { .. })
    ));

    c.mise_env = "someone-else".to_owned();
    c.space_production = false;
    assert!(matches!(
        plan_call(&default_scope, "fake.run", &Map::new(), &c),
        Err(PluginError::ScopeRefused { .. })
    ));
    c.mise_env = String::new();
    assert!(matches!(
        plan_call(&default_scope, "fake.run", &Map::new(), &c),
        Err(PluginError::ScopeRefused { .. })
    ));
}

#[test]
fn builtins_are_planned_but_not_run() {
    let dir = tempfile::tempdir().expect("tempdir");
    let db = builtin("database");
    let plan = plan_call(
        &db,
        "sql.write",
        &args(json!({ "statement": "DELETE FROM t", "reason": "x" })),
        &ctx(dir.path()),
    )
    .expect("plan");
    assert_eq!(plan.effect, Effect::Write);
    assert_eq!(plan.builtin(), Some("sql.write"));
    assert!(
        matches!(&plan.invocation, Invocation::Builtin { args, .. } if args["statement"] == "DELETE FROM t")
    );
    assert!(matches!(
        run_plan(&plan, SECOND, 1024),
        Err(PluginError::Builtin { .. })
    ));
}

#[test]
fn secrets_reach_the_tool_and_are_masked_in_its_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_script(
        dir.path(),
        "echo \"token=$MY_TOKEN env=$MISE_ENV port=$PORT space=$XCTX_SPACE\"\necho \"$MY_TOKEN\" >&2\n",
    );
    let m = script_manifest("", "", "secrets = [\"MY_TOKEN\"]");
    let mut c = ctx(dir.path());
    assert!(matches!(
        plan_call(&m, "fake.run", &Map::new(), &c),
        Err(PluginError::MissingSecret { .. })
    ));

    let secret = "s3cr3t-value-9876".to_owned();
    c.secrets.insert("MY_TOKEN".to_owned(), secret.clone());
    c.redactor = Redactor::new([("MY_TOKEN".to_owned(), secret.clone())]);
    let plan = plan_call(&m, "fake.run", &Map::new(), &c).expect("plan");
    let out = run_plan(&plan, 10 * SECOND, 4096).expect("run");
    assert_eq!(
        out.stdout,
        "token=[REDACTED:MY_TOKEN] env=fix-login port=4123 space=fix-login\n"
    );
    assert_eq!(out.stderr, "[REDACTED:MY_TOKEN]\n");
    assert!(!out.stdout.contains(&secret) && !out.stderr.contains(&secret));

    // A secret straddling the cap is masked whole, never cut into a visible prefix.
    write_script(dir.path(), "printf 'abcdefgh%s tail' \"$MY_TOKEN\"\n");
    let out = run_plan(&plan, 10 * SECOND, 12).expect("run");
    assert!(out.truncated);
    assert!(out.stdout.len() <= 12);
    assert!(!out.stdout.contains("s3cr"), "{:?}", out.stdout);
}

#[test]
fn a_secret_repeated_across_the_cap_never_shows_a_prefix() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A long secret, masked into something much shorter, repeated well past the cap and its margin: masking
    // what was read shrinks it below the cap, so a cut that fell inside the last secret would show its start.
    let secret: String = (0..2000)
        .map(|i| char::from(b'a' + (i % 26) as u8))
        .collect();
    write_script(
        dir.path(),
        "printf 'head '\ni=0; while [ $i -lt 100 ]; do printf '%s' \"$MY_TOKEN\"; i=$((i+1)); done\n",
    );
    let m = script_manifest("", "", "secrets = [\"MY_TOKEN\"]");
    let mut c = ctx(dir.path());
    c.secrets.insert("MY_TOKEN".to_owned(), secret.clone());
    c.redactor = Redactor::new([("MY_TOKEN".to_owned(), secret.clone())]);
    let plan = plan_call(&m, "fake.run", &Map::new(), &c).expect("plan");
    for cap in [1000, 3000, 10_000, 40_000] {
        let out = run_plan(&plan, 30 * SECOND, cap).expect("run");
        assert!(out.truncated, "{cap}");
        assert!(out.stdout.len() <= cap, "{cap}");
        assert!(out.stdout.starts_with("head "), "{cap}: {:?}", out.stdout);
        let rest = out.stdout["head ".len()..].replace("[REDACTED:MY_TOKEN]", "");
        assert!(
            !rest.contains("abcd") && rest.len() < "[REDACTED:MY_TOKEN]".len(),
            "{cap}: a part of the secret shows: {rest:?}"
        );
    }
}

#[test]
fn what_the_tool_leaves_running_is_killed_when_the_call_ends() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_script(dir.path(), "sleep 600 >/dev/null 2>&1 &\necho $!\n");
    let m = script_manifest("", "", "");
    let plan = plan_call(&m, "fake.run", &Map::new(), &ctx(dir.path())).expect("plan");
    let start = Instant::now();
    let out = run_plan(&plan, 30 * SECOND, 1024).expect("run");
    assert!(start.elapsed() < 5 * SECOND, "took {:?}", start.elapsed());
    assert!(out.success() && !out.timed_out, "{out:?}");
    let pid: i32 = out.stdout.trim().parse().expect("the sleep's pid");
    // Killed by the time the call returns; launchd (its new parent) reaps it a moment later.
    let gone = |pid: i32| {
        let status = std::process::Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .expect("ps");
        let stat = String::from_utf8_lossy(&status.stdout);
        stat.trim().is_empty() || stat.trim().starts_with('Z')
    };
    let deadline = Instant::now() + 5 * SECOND;
    while !gone(pid) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(gone(pid), "the background sleep {pid} is still running");
}

#[test]
fn output_is_capped_and_flagged() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_script(
        dir.path(),
        "i=0; while [ $i -lt 2000 ]; do echo 0123456789abcdef; i=$((i+1)); done; echo done >&2\n",
    );
    let m = script_manifest("", "", "");
    let plan = plan_call(&m, "fake.run", &Map::new(), &ctx(dir.path())).expect("plan");
    let out = run_plan(&plan, 30 * SECOND, 1000).expect("run");
    assert!(out.success());
    assert!(out.truncated);
    assert_eq!(out.stdout.len(), 1000);
    assert_eq!(out.stderr, "done\n");

    let out = run_plan(&plan, 30 * SECOND, 1 << 20).expect("run");
    assert!(!out.truncated);
    assert_eq!(out.stdout.len(), 2000 * 17);
}

#[test]
fn a_timeout_kills_the_tool_and_what_it_started() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_script(dir.path(), "echo started\nsleep 30 &\nsleep 30\n");
    let m = script_manifest("", "", "");
    let plan = plan_call(&m, "fake.run", &Map::new(), &ctx(dir.path())).expect("plan");
    let start = Instant::now();
    let out = run_plan(&plan, Duration::from_millis(300), 1024).expect("run");
    assert!(start.elapsed() < 10 * SECOND, "took {:?}", start.elapsed());
    assert!(out.timed_out);
    assert_eq!(out.status, None);
    assert_eq!(out.stdout, "started\n");

    // The tool exits at once but leaves a child holding its output open: killed at the deadline.
    write_script(dir.path(), "sleep 30 &\necho bye\n");
    let start = Instant::now();
    let out = run_plan(&plan, Duration::from_millis(300), 1024).expect("run");
    assert!(start.elapsed() < 10 * SECOND, "took {:?}", start.elapsed());
    assert_eq!(out.status, Some(0));
    assert_eq!(out.stdout, "bye\n");
}

#[test]
fn a_missing_program_is_a_spawn_error_and_a_failing_one_is_a_status() {
    let dir = tempfile::tempdir().expect("tempdir");
    let m = builtin("terminal");
    let c = ctx(dir.path());
    let plan = plan_call(
        &m,
        "shell.run",
        &args(json!({ "command": "/no/such/program" })),
        &c,
    )
    .expect("plan");
    assert!(matches!(
        run_plan(&plan, SECOND, 1024),
        Err(PluginError::Spawn { .. })
    ));
    let plan = plan_call(
        &m,
        "shell.run",
        &args(json!({ "command": "/bin/sh", "args": ["-c", "exit 3"] })),
        &c,
    )
    .expect("plan");
    let out = run_plan(&plan, 10 * SECOND, 1024).expect("run");
    assert_eq!(out.status, Some(3));
    assert!(!out.success());
}

#[test]
fn events_fire_on_tool_output_lines() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_script(
        dir.path(),
        "echo 'GET /health 200'\necho '{\"check\": {\"status\": \"failed\"}}'\necho 'ERROR: database is down' >&2\n",
    );
    let m = script_manifest(
        "",
        "",
        "[[events]]\nname = \"check.failed\"\naction = \"fail\"\nmatch = { field = \"check.status\", equals = \"failed\" }\n\
         [[events]]\nname = \"log.error\"\naction = \"notice\"\nmatch = { regex = \"^ERROR\" }\n\
         [[events]]\nname = \"check.ok\"\naction = \"pass\"\nmatch = { regex = \" 200$\" }\n",
    );
    let plan = plan_call(&m, "fake.run", &Map::new(), &ctx(dir.path())).expect("plan");
    let out = run_plan(&plan, 10 * SECOND, 4096).expect("run");
    let fired = match_events(&m, out.lines());
    let names: Vec<(&str, LoopAction)> =
        fired.iter().map(|e| (e.name.as_str(), e.action)).collect();
    assert_eq!(
        names,
        [
            ("check.ok", LoopAction::Pass),
            ("check.failed", LoopAction::Fail),
            ("log.error", LoopAction::Notice)
        ]
    );
    assert_eq!(fired[2].line, "ERROR: database is down");
    assert_eq!(xctx_plugin::loop_outcome(&fired), Some(LoopAction::Fail));

    // Pane lines go through the same matching, for example the API client's status events.
    let api = builtin("api-client");
    let fired = match_events(
        &api,
        [
            "{\"event\": \"response.status\", \"status\": 503}",
            "plain text",
        ],
    );
    assert_eq!(fired.len(), 1);
    assert_eq!(fired[0].name, "response.status");
    assert_eq!(fired[0].action, LoopAction::Notice);
}

#[test]
fn pane_commands_fill_the_space_placeholders() {
    let dir = tempfile::tempdir().expect("tempdir");
    let text = "name = \"tail\"\nversion = \"1\"\n[pane]\nkind = \"logs\"\nrun = [\"tail\", \"-f\", \"{WORKTREE}/log/{SPACE}.log\"]\n";
    let m = Manifest::parse(text, "t").expect("parses");
    let argv = pane_argv(&m.pane, &ctx(dir.path())).expect("filled");
    assert_eq!(
        argv,
        Some(vec![
            "tail".into(),
            "-f".into(),
            format!("{}/log/fix-login.log", dir.path().display())
        ])
    );
    assert_eq!(
        pane_argv(&builtin("terminal").pane, &ctx(dir.path())).expect("ok"),
        None
    );
    assert_eq!(
        xctx_plugin::fill_context("http://localhost:{PORT}", &ctx(dir.path())).expect("filled"),
        "http://localhost:4123"
    );
}
