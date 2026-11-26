//! The client against `fake-omp`, which replays a recorded conversation over real pipes.

mod common;

use std::time::Duration;

use xctx_omp::{
    Command, Entry, Frame, Incoming, LaunchSpec, OmpClient, OmpError, Phase, SessionModel,
};

fn fake(fixture: &str) -> LaunchSpec {
    let mut spec = LaunchSpec::new(env!("CARGO_BIN_EXE_fake-omp"), std::env::temp_dir());
    spec.args = vec!["--fixture".into(), common::fixture(fixture).into()];
    spec.handshake_timeout = Duration::from_secs(10);
    spec
}

/// Start, apply the handshake frames, send `commands`, then apply frames until `done` says stop.
fn run(
    spec: &LaunchSpec,
    commands: &[Command],
    mut done: impl FnMut(&SessionModel) -> bool,
) -> (SessionModel, Option<Incoming>) {
    let started = OmpClient::start(spec).expect("starts");
    let mut model = SessionModel::new();
    for f in started.startup {
        model.apply(f);
    }
    for c in commands {
        started.client.send(c).expect("send");
    }
    let mut end = None;
    loop {
        match started
            .frames
            .recv_timeout(Duration::from_secs(10))
            .expect("frame in time")
        {
            Incoming::Frame(f) => {
                for effect in model.apply(f) {
                    if let xctx_omp::Effect::Send(c) = effect {
                        let _ = started.client.send(&c);
                    }
                }
                if done(&model) {
                    break;
                }
            }
            Incoming::Malformed(_) => {}
            gone @ Incoming::Exited { .. } => {
                end = Some(gone);
                break;
            }
        }
    }
    (model, end)
}

/// The turn is over and the follow-up `get_state` has answered.
fn assistant_ended(model: &SessionModel) -> bool {
    model.settled && model.phase() == Phase::Idle && model.session.id.is_some()
}

#[test]
fn a_session_over_real_pipes_ends_up_the_same_as_the_replay() {
    let spec = fake("plain");
    let prompt = Command::prompt(
        "Read a.txt with the read tool and reply with only its first line.",
        &[],
        None,
    );
    let (live, _) = run(&spec, &[prompt], assistant_ended);
    let (replayed, _) = common::replay("plain");
    assert_eq!(live.entries, replayed.entries);
    assert_eq!(live.session, replayed.session);
}

#[test]
fn large_frames_arrive_whole_through_rpc_chunks() {
    let mut spec = fake("plain");
    spec.env.push(("FAKE_OMP_CHUNK".into(), "100".into()));
    let prompt = Command::prompt("x", &[], None);
    let (live, _) = run(&spec, &[prompt], assistant_ended);
    let (replayed, _) = common::replay("plain");
    assert_eq!(
        live.entries, replayed.entries,
        "chunked frames reassemble to the same conversation"
    );
}

#[test]
fn a_child_that_exits_reports_it_and_a_missing_program_is_a_spawn_error() {
    let started = OmpClient::start(&fake("plain")).expect("starts");
    let mut client = started.client;
    client.close_input();
    let exit = loop {
        let incoming = started
            .frames
            .recv_timeout(Duration::from_secs(10))
            .expect("frame");
        if let Incoming::Exited { code, .. } = incoming {
            break code;
        }
    };

    assert_eq!(exit, Some(0));

    let missing = LaunchSpec::new("/nonexistent/omp", std::env::temp_dir());
    assert!(matches!(
        OmpClient::start(&missing),
        Err(OmpError::Spawn { .. })
    ));
}

#[test]
fn a_child_that_never_says_ready_times_out_and_one_that_dies_is_exited_early() {
    let empty = tempfile::NamedTempFile::new().expect("temp file");
    let mut silent = LaunchSpec::new(env!("CARGO_BIN_EXE_fake-omp"), std::env::temp_dir());
    silent.args = vec!["--fixture".into(), empty.path().into()];
    silent.handshake_timeout = Duration::from_millis(300);
    assert!(matches!(
        OmpClient::start(&silent),
        Err(OmpError::Timeout("ready"))
    ));

    let mut dies = LaunchSpec::new("/usr/bin/false", std::env::temp_dir());
    dies.handshake_timeout = Duration::from_secs(5);
    assert!(matches!(
        OmpClient::start(&dies),
        Err(OmpError::ExitedEarly)
    ));
}

#[test]
fn a_gate_raised_over_the_wire_can_be_answered() {
    let spec = fake("ask");
    let prompt = Command::prompt("ask me", &[], None);
    let started = OmpClient::start(&spec).expect("starts");
    let mut model = SessionModel::new();
    for f in started.startup {
        model.apply(f);
    }
    started.client.send(&prompt).unwrap();
    let request_id = loop {
        match started
            .frames
            .recv_timeout(Duration::from_secs(10))
            .expect("frame")
        {
            Incoming::Frame(f) => {
                model.apply(f.clone());
                if let Frame::Ui(r) = &f
                    && r.blocks()
                {
                    break r.id.clone();
                }
            }
            Incoming::Exited { .. } => panic!("exited before the form"),
            Incoming::Malformed(_) => {}
        }
    };
    assert_eq!(model.phase(), Phase::NeedsYou);
    let form = xctx_omp::form::Form::parse(model.next_gate().unwrap().html().unwrap());
    let mut answers = xctx_omp::form::Answers::default();
    answers.chosen.insert("colour".into(), vec!["blue".into()]);
    let data = form.answer(&answers).unwrap();
    started
        .client
        .send(&Command::ui_form(&request_id, data))
        .unwrap();
    model.resolve_ui(&request_id);
    assert_eq!(model.phase(), Phase::Running);
    // The script goes on after the answer and finishes the turn.
    loop {
        match started
            .frames
            .recv_timeout(Duration::from_secs(10))
            .expect("frame")
        {
            Incoming::Frame(f) => {
                model.apply(f);
                if model.settled && model.phase() == Phase::Idle {
                    break;
                }
            }
            Incoming::Exited { .. } => break,
            Incoming::Malformed(_) => {}
        }
    }
    assert!(
        model
            .entries
            .iter()
            .any(|e| matches!(e, Entry::Assistant(_)))
    );
}
