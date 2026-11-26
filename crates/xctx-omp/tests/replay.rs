//! Recorded omp conversations (`tests/fixtures`, made by `fixtures/record/record.py` against a real omp) pushed
//! through the model. These pin the frame shapes the model depends on.

mod common;

use xctx_omp::{Block, Effect, Entry, GateKind, Phase, SessionModel, ToolState};

fn assistant_text(model: &SessionModel) -> String {
    model
        .entries
        .iter()
        .filter_map(|e| match e {
            Entry::Assistant(a) => Some(a),
            _ => None,
        })
        .flat_map(|a| a.blocks.iter())
        .filter_map(|b| match b {
            Block::Text(t) => Some(t.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_plain_turn_streams_into_one_assistant_entry_with_its_tool() {
    let (m, effects) = common::replay("plain");
    assert_eq!(m.phase(), Phase::Idle);
    assert!(m.settled);
    assert_eq!(assistant_text(&m), "hello from a.txt");

    let tool = m.tools.values().next().expect("one tool");
    assert_eq!(tool.name, "read");
    assert_eq!(tool.state, ToolState::Done);
    assert!(tool.output.contains("hello"), "{:?}", tool.output);

    let Some(Entry::Assistant(a)) = m.entries.iter().find(|e| matches!(e, Entry::Assistant(_)))
    else {
        panic!("assistant entry");
    };
    assert!(!a.streaming);
    assert!(
        a.blocks
            .iter()
            .any(|b| matches!(b, Block::ToolCall(id) if *id == tool.id))
    );

    assert!(m.session.id.is_some() && m.session.file.is_some());
    assert!(effects.iter().any(|e| matches!(e, Effect::SessionBound(_))));
    assert_eq!(m.model.as_ref().map(|x| x.id.as_str()), Some("auto-smart"));
    assert!(m.context.is_some());
    assert!(m.stats.tokens > 0 && m.stats.tool_calls == 1);
    assert!(m.commands.len() > 10, "slash commands arrive on ready");
}

#[test]
fn the_phase_goes_running_then_idle_around_a_turn() {
    let (_, effects) = common::replay("plain");
    let phases: Vec<Phase> = effects
        .iter()
        .filter_map(|e| match e {
            Effect::PhaseChanged(p) => Some(*p),
            _ => None,
        })
        .collect();
    assert_eq!(phases, [Phase::Running, Phase::Idle]);
}

#[test]
fn a_tool_approval_is_a_gate_and_needs_you_until_answered() {
    let (mut m, effects) = common::replay("permission");
    let gate = effects
        .iter()
        .find_map(|e| match e {
            Effect::GateRaised(g) => Some(g.clone()),
            _ => None,
        })
        .expect("gate");
    assert_eq!(
        gate.kind,
        GateKind::Approval {
            tool: "bash".into(),
            detail: "Command: echo hi".into()
        }
    );
    // The fixture answered "cancel" (a deny), but the model is only told about what omp said.
    assert_eq!(m.phase(), Phase::NeedsYou);
    let cleared = m.resolve_ui(&gate.request_id);
    assert!(cleared.contains(&Effect::GateCleared(gate.request_id.clone())));
    assert!(cleared.contains(&Effect::PhaseChanged(Phase::Idle)));
    assert!(
        m.resolve_ui(&gate.request_id).is_empty(),
        "answering twice does nothing"
    );

    let tool = m.tools.values().next().expect("tool");
    assert_eq!(tool.name, "bash");
    assert_eq!(
        tool.state,
        ToolState::Failed,
        "a denied tool is a failed tool"
    );
}

#[test]
fn an_ask_form_is_a_form_gate_whose_html_parses() {
    let (m, effects) = common::replay("ask");
    let gate = effects
        .iter()
        .find_map(|e| match e {
            Effect::GateRaised(g) => Some(g),
            _ => None,
        })
        .expect("gate");
    assert_eq!(gate.kind, GateKind::Form);
    let request = m.next_gate().expect("pending");
    let html = request.html().expect("form html");
    let form = xctx_omp::form::Form::parse(html);
    assert_eq!(form.fields[0].name, "colour");

    let mut answers = xctx_omp::form::Answers::default();
    answers.chosen.insert("colour".into(), vec!["blue".into()]);
    let data = form.answer(&answers).expect("valid");
    assert_eq!(
        serde_json::Value::Object(data),
        serde_json::json!({"colour": "blue"})
    );
}

#[test]
fn queued_messages_show_up_in_the_queue_and_leave_it() {
    let mut m = SessionModel::new();
    let mut saw_steering = false;
    let mut saw_follow_up = false;
    for f in common::frames("queue") {
        m.apply(f);
        saw_steering |= !m.queue.steering.is_empty();
        saw_follow_up |= !m.queue.follow_up.is_empty();
    }
    assert!(
        saw_steering || saw_follow_up,
        "the queue was visible while the turn ran"
    );
    assert!(m.queue.steering.is_empty() && m.queue.follow_up.is_empty());
    let users: Vec<&str> = m
        .entries
        .iter()
        .filter_map(|e| match e {
            Entry::User { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(users.len(), 3, "{users:?}");
    assert!(users.contains(&"After that, say DONE."));
}

#[test]
fn btw_records_are_kept_apart_from_the_conversation() {
    let (m, _) = common::replay("btw");
    assert_eq!(m.btw.len(), 1);
    assert_eq!(m.btw[0].question, "What is 2+2? Answer with one digit.");
    assert!(m.entries.is_empty(), "a side question is not a message");
}

#[test]
fn slash_command_output_becomes_plain_notices() {
    let (m, _) = common::replay("commands");
    let notices: Vec<&str> = m
        .entries
        .iter()
        .filter_map(|e| match e {
            Entry::Notice { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(notices.len() >= 4, "{notices:?}");
    assert!(notices.iter().any(|t| t.starts_with("No background jobs")));
    assert!(
        notices.iter().all(|t| !t.contains('\u{1b}')),
        "terminal codes are stripped"
    );
}

#[test]
fn subagent_frames_build_a_tray() {
    let (m, _) = common::replay("subagent");
    assert_eq!(
        m.subagents.len(),
        2,
        "{:?}",
        m.subagents.keys().collect::<Vec<_>>()
    );
    let sub = &m.subagents["ReadAB"];
    assert_eq!(sub.agent, "scout");
    assert_eq!(sub.status, "completed");
    assert!(
        sub.session_file
            .as_deref()
            .is_some_and(|f| f.ends_with("ReadAB.jsonl"))
    );
    assert!(sub.tool_count >= 1);
    assert!(sub.parent_tool_call_id.starts_with("toolu_"));
    // The subagent's own events never become the parent's transcript.
    assert!(m.entries.iter().all(|e| match e {
        Entry::User { text, .. } => !text.starts_with("Complete assignment"),
        _ => true,
    }));
}

#[test]
fn every_fixture_replays_without_leaving_a_streaming_message() {
    for name in [
        "plain",
        "ask",
        "permission",
        "queue",
        "btw",
        "commands",
        "subagent",
    ] {
        let (m, _) = common::replay(name);
        for e in &m.entries {
            if let Entry::Assistant(a) = e {
                assert!(!a.streaming, "{name}: {a:?}");
            }
        }
    }
}
