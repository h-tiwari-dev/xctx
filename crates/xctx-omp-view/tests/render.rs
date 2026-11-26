//! The omp pane in GPUI's headless test window, fed by conversations recorded from a real omp
//! (`crates/xctx-omp/tests/fixtures`) and decoded the way the client decodes them: the transcript and its tool
//! cards, the queue chips, the composer's modes and menus, the HUD, and the error banner.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::{
    ClipboardItem, Context, Entity, ExternalPaths, FileDropEvent, Focusable, ImageFormat,
    IntoElement, Modifiers, Render, Subscription, TestAppContext, VisualTestContext, Window, div,
    prelude::*,
};
use serde_json::{Value, json};
use xctx_omp::codec::Decoder;
use xctx_omp::{Block, Entry, Frame, Queue, SessionModel};
use xctx_omp_view::transcript::RowKey;
use xctx_omp_view::{DraftMode, ModelChoice, OmpIntent, OmpView, PaneEscape};
use xctx_ui::{MotionSettings, Theme};

/// A 1×1 transparent PNG.
const PNG: [u8; 67] = [
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

/// What omp said in fixture `name`, decoded as `OmpClient` decodes it.
fn frames(name: &str) -> Vec<Frame> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../xctx-omp/tests/fixtures")
        .join(format!("{name}.jsonl"));
    let text = std::fs::read_to_string(&path).expect("fixture");
    let mut decoder = Decoder::default();
    text.lines()
        .filter_map(|l| l.strip_prefix("< "))
        .filter_map(|l| decoder.push_line(l).expect("decodes"))
        .map(Frame::parse)
        .collect()
}

fn replay(name: &str) -> SessionModel {
    let mut model = SessionModel::new();
    for frame in frames(name) {
        model.apply(frame);
    }
    model
}

/// Replay `name` until `stop` holds for the model.
fn replay_until(name: &str, stop: impl Fn(&SessionModel) -> bool) -> SessionModel {
    let mut model = SessionModel::new();
    for frame in frames(name) {
        model.apply(frame);
        if stop(&model) {
            return model;
        }
    }
    panic!("{name} never reached the state");
}

fn apply(model: &mut SessionModel, frame: Value) {
    model.apply(Frame::parse(frame));
}

struct Host {
    view: Entity<OmpView>,
    intents: Vec<OmpIntent>,
    escapes: usize,
    _subscriptions: [Subscription; 2],
}

impl Render for Host {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.view.clone())
    }
}

fn host(cx: &mut TestAppContext, model: SessionModel) -> (Entity<Host>, &mut VisualTestContext) {
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        MotionSettings::set_reduced(cx, true);
    });
    let (host, cx) = cx.add_window_view(|_, cx| {
        let view = cx.new(|cx| OmpView::new(Arc::new(model), cx));
        let intents = cx.subscribe(&view, |host: &mut Host, _, intent: &OmpIntent, _| {
            host.intents.push(intent.clone());
        });
        let escapes = cx.subscribe(&view, |host: &mut Host, _, _: &PaneEscape, _| {
            host.escapes += 1;
        });
        Host {
            view,
            intents: Vec::new(),
            escapes: 0,
            _subscriptions: [intents, escapes],
        }
    });
    let view = host.read_with(cx, |h, _| h.view.clone());
    cx.update(|window, cx| view.focus_handle(cx).focus(window, cx));
    cx.run_until_parked();
    (host, cx)
}

fn view(host: &Entity<Host>, cx: &mut VisualTestContext) -> Entity<OmpView> {
    host.read_with(cx, |h, _| h.view.clone())
}

fn show(host: &Entity<Host>, cx: &mut VisualTestContext, model: SessionModel) {
    let view = view(host, cx);
    view.update(cx, |v, cx| v.set_session(Arc::new(model), cx));
    cx.run_until_parked();
}

fn intents(host: &Entity<Host>, cx: &mut VisualTestContext) -> Vec<OmpIntent> {
    host.update(cx, |h, _| std::mem::take(&mut h.intents))
}

fn draft(host: &Entity<Host>, cx: &mut VisualTestContext) -> String {
    let view = view(host, cx);
    view.read_with(cx, |v, cx| v.composer().read(cx).text(cx).to_owned())
}

fn shown(cx: &mut VisualTestContext, selector: &'static str) -> bool {
    cx.debug_bounds(selector).is_some()
}

fn click(cx: &mut VisualTestContext, selector: &'static str) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("{selector} is not drawn"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

#[gpui::test]
fn the_plain_fixture_draws_the_turn_with_a_collapsed_read_card(cx: &mut TestAppContext) {
    let model = replay("plain");
    let (host, cx) = host(cx, model);

    let rows = view(&host, cx).read_with(cx, |v, _| v.transcript().rows().to_vec());
    assert!(matches!(rows.first(), Some(RowKey::User(_))), "{rows:?}");
    assert!(
        rows.iter().any(|r| matches!(r, RowKey::Block { .. })),
        "{rows:?}"
    );

    for selector in [
        "omp-user-message",
        "omp-thinking",
        "omp-assistant-text",
        "omp-tool-read",
        "omp-hud-model",
        "omp-hud-context",
        "omp-hud-cost",
        "omp-composer",
        "omp-send",
    ] {
        assert!(shown(cx, selector), "{selector} is not drawn");
    }
    assert!(!shown(cx, "omp-empty"));
    assert!(!shown(cx, "omp-banner"));
    assert!(!shown(cx, "omp-working"), "the turn is over");
    assert!(
        !shown(cx, "omp-tool-expanded"),
        "tool cards start collapsed"
    );

    click(cx, "omp-tool-read");
    assert!(shown(cx, "omp-tool-expanded"));
    click(cx, "omp-tool-read");
    assert!(!shown(cx, "omp-tool-expanded"));
}

#[gpui::test]
fn a_running_turn_shows_the_working_indicator_and_stop_aborts(cx: &mut TestAppContext) {
    let running = replay_until("plain", |m| {
        m.is_running()
            && m.entries.iter().any(|e| {
                matches!(e, Entry::Assistant(a) if a.blocks.iter().any(|b| matches!(b, Block::Thinking(t) if !t.is_empty())))
            })
    });
    let (host, cx) = host(cx, running);
    assert!(shown(cx, "omp-working"));
    assert!(shown(cx, "omp-working-elapsed"));
    assert!(shown(cx, "omp-thinking"));
    assert!(shown(cx, "omp-send-steer") && shown(cx, "omp-send-follow-up"));
    assert!(!shown(cx, "omp-send"));

    click(cx, "omp-stop");
    assert_eq!(intents(&host, cx), [OmpIntent::Abort]);

    cx.simulate_keystrokes("escape");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::Abort],
        "Esc stops a running turn"
    );

    show(&host, cx, replay("plain"));
    assert!(!shown(cx, "omp-working"));
    assert!(shown(cx, "omp-tool-read"));
}

#[gpui::test]
fn the_queue_fixture_shows_chips_that_promote_remove_and_edit(cx: &mut TestAppContext) {
    let queued = replay_until("queue", |m| {
        !m.queue.steering.is_empty() && !m.queue.follow_up.is_empty()
    });
    let (host, cx) = host(cx, queued);
    assert!(shown(cx, "omp-queue"));
    assert!(shown(cx, "omp-queue-chip"));

    // Chip 0 is the steering message, chip 1 the follow-up; only a follow-up can be promoted.
    assert!(!shown(cx, "omp-queue-promote-0"));
    click(cx, "omp-queue-promote-1");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::PromoteQueued {
            message: "After that, say DONE.".into()
        }]
    );

    click(cx, "omp-queue-remove-0");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::RemoveQueued {
            message: "Stop counting at 5.".into(),
            queue: Queue::Steering
        }]
    );

    click(cx, "omp-queue-edit-1");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::EditQueued {
            message: "After that, say DONE.".into(),
            queue: Queue::FollowUp
        }]
    );
    assert_eq!(draft(&host, cx), "After that, say DONE.");

    cx.simulate_keystrokes("enter");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::Steer {
            text: "After that, say DONE.".into(),
            images: vec![]
        }],
        "Enter steers while the agent runs"
    );
    cx.simulate_input("then summarise");
    cx.simulate_keystrokes("alt-enter");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::FollowUp {
            text: "then summarise".into(),
            images: vec![]
        }],
        "Alt+Enter queues a follow-up"
    );

    show(&host, cx, replay("queue"));
    assert!(!shown(cx, "omp-queue"), "the queue drained by the end");
}

#[gpui::test]
fn composer_modes_get_their_border_and_send_their_intent(cx: &mut TestAppContext) {
    let (host, cx) = host(cx, replay("plain"));
    let mode = |host: &Entity<Host>, cx: &mut VisualTestContext| {
        let view = view(host, cx);
        view.read_with(cx, |v, cx| v.composer().read(cx).mode(cx))
    };

    cx.simulate_input("!!git status");
    assert!(shown(cx, "omp-mode-bash"));
    assert_eq!(
        mode(&host, cx),
        DraftMode::Bash {
            exclude_from_context: true
        }
    );
    cx.simulate_keystrokes("enter");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::Bash {
            command: "git status".into(),
            exclude_from_context: true
        }]
    );
    assert_eq!(draft(&host, cx), "");
    assert!(!shown(cx, "omp-mode-bash"));

    cx.simulate_input("$ print(1)");
    assert!(shown(cx, "omp-mode-python"));
    cx.simulate_keystrokes("enter");
    assert!(intents(&host, cx).is_empty(), "omp's RPC cannot run python");
    assert!(shown(cx, "omp-composer-note"));
    cx.simulate_keystrokes("escape");
    assert_eq!(draft(&host, cx), "", "Esc clears an idle draft");

    cx.simulate_input("?what is 2+2");
    assert!(shown(cx, "omp-mode-btw"));
    cx.simulate_keystrokes("enter");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::Btw {
            question: "what is 2+2".into()
        }]
    );

    cx.simulate_input("/compac");
    assert!(shown(cx, "omp-slash-menu"));
    cx.simulate_keystrokes("tab");
    assert_eq!(draft(&host, cx), "/compact ");
    assert!(!shown(cx, "omp-slash-menu"));
    cx.simulate_input("keep tests");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::SlashCommand {
            name: "compact".into(),
            args: "keep tests".into()
        }]
    );

    cx.simulate_input("fix it");
    cx.simulate_keystrokes("enter");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::Prompt {
            text: "fix it".into(),
            images: vec![]
        }]
    );

    cx.simulate_keystrokes("escape");
    assert_eq!(
        host.read_with(cx, |h, _| h.escapes),
        1,
        "Esc on an empty idle composer goes to the shell"
    );
}

#[gpui::test]
fn file_completion_comes_from_the_host_callback(cx: &mut TestAppContext) {
    let (host, cx) = host(cx, replay("plain"));
    let view = view(&host, cx);
    view.update(cx, |v, cx| {
        v.set_file_completer(
            std::rc::Rc::new(|query: &str| {
                ["src/main.rs", "src/lib.rs", "README.md"]
                    .into_iter()
                    .filter(|p| p.starts_with(query))
                    .map(Into::into)
                    .collect()
            }),
            cx,
        )
    });
    cx.simulate_input("look at @src/");
    assert!(shown(cx, "omp-file-menu"));
    assert!(shown(cx, "omp-menu-row-1"));
    assert!(!shown(cx, "omp-menu-row-2"), "README.md does not match");
    cx.simulate_keystrokes("down enter");
    assert_eq!(draft(&host, cx), "look at @src/lib.rs ");
    assert!(!shown(cx, "omp-file-menu"));
    assert!(intents(&host, cx).is_empty(), "accepting is not sending");
}

#[gpui::test]
fn pasted_and_dropped_images_are_sent_with_the_message(cx: &mut TestAppContext) {
    let (host, cx) = host(cx, replay("plain"));
    cx.write_to_clipboard(ClipboardItem::new_image(&gpui::Image::from_bytes(
        ImageFormat::Png,
        PNG.to_vec(),
    )));
    cx.simulate_keystrokes("cmd-v");
    assert!(shown(cx, "omp-attachment"));
    assert_eq!(draft(&host, cx), "", "an image paste types nothing");

    let dir = tempfile::tempdir().expect("temp dir");
    let png = dir.path().join("shot.png");
    std::fs::write(&png, PNG).expect("write png");
    let notes = dir.path().join("notes.txt");
    std::fs::write(&notes, "x").expect("write txt");
    let target = cx.debug_bounds("omp-view").expect("pane").center();
    // GPUI ignores hover after keyboard input until the mouse moves, and a file drop is not a mouse move.
    cx.simulate_mouse_move(target, None, Modifiers::none());
    cx.simulate_event(FileDropEvent::Entered {
        position: target,
        paths: ExternalPaths(vec![png, notes].into()),
    });
    cx.simulate_event(FileDropEvent::Submit { position: target });
    cx.run_until_parked();
    let view = view(&host, cx);
    let (count, note) = view.read_with(cx, |v, cx| {
        let composer = v.composer().read(cx);
        (composer.images().len(), composer.note().map(str::to_owned))
    });
    assert_eq!(count, 2, "the PNG was attached ({note:?})");
    assert_eq!(
        note.as_deref(),
        Some("Only images can be attached: notes.txt")
    );

    cx.simulate_input("what is this?");
    cx.simulate_keystrokes("enter");
    let sent = intents(&host, cx);
    let [OmpIntent::Prompt { text, images }] = sent.as_slice() else {
        panic!("{sent:?}");
    };
    assert_eq!(text, "what is this?");
    assert_eq!(images.len(), 2);
    assert!(images.iter().all(|i| i.mime_type == "image/png"));
    assert!(
        !shown(cx, "omp-attachment"),
        "sent images leave the composer"
    );
}

#[gpui::test]
fn the_error_banner_covers_turn_errors_retries_frame_errors_and_exit(cx: &mut TestAppContext) {
    let mut model = replay("plain");
    apply(
        &mut model,
        json!({"type": "message_start", "message": {"role": "user", "content": "again"}}),
    );
    apply(
        &mut model,
        json!({"type": "prompt_result", "id": "p2", "agentInvoked": true, "status": "error",
               "error": {"message": "429 rate limited", "retryable": true}}),
    );
    let (host, cx) = host(cx, model.clone());
    assert!(shown(cx, "omp-banner"));
    assert!(shown(cx, "omp-banner-turn-failed"));
    let view = view(&host, cx);
    let title = view.read_with(cx, |v, _| v.banner().map(|b| b.title));
    assert_eq!(title.as_deref(), Some("429 rate limited"));
    click(cx, "omp-banner-dismiss");
    assert!(!shown(cx, "omp-banner"), "dismissed");

    apply(&mut model, json!({"type": "agent_start"}));
    apply(
        &mut model,
        json!({"type": "auto_retry_start", "attempt": 1, "errorMessage": "overloaded"}),
    );
    show(&host, cx, model.clone());
    assert!(shown(cx, "omp-banner-retrying"));

    apply(&mut model, json!({"type": "rpc_frame_error"}));
    apply(&mut model, json!({"type": "agent_end"}));
    show(&host, cx, model);
    assert!(shown(cx, "omp-banner-transport"));

    view.update(cx, |v, cx| v.malformed_frame("{not json", cx));
    cx.run_until_parked();
    assert!(shown(cx, "omp-banner-transport"));

    view.update(cx, |v, cx| {
        v.client_exited(Some(1), "Error: session file locked\n", cx)
    });
    cx.run_until_parked();
    assert!(shown(cx, "omp-banner-exited"));
    let banner = view.read_with(cx, |v, _| v.banner()).expect("banner");
    assert_eq!(banner.title, "omp exited with code 1");
    assert_eq!(banner.detail.as_deref(), Some("Error: session file locked"));
}

#[gpui::test]
fn the_hud_reads_out_the_session_and_switches_model_and_effort(cx: &mut TestAppContext) {
    let mut model = replay("plain");
    apply(
        &mut model,
        json!({"type": "response", "command": "get_state", "success": true, "data": {
            "model": {"provider": "cursor", "id": "auto-smart", "name": "Auto"},
            "thinkingLevel": "low", "thinkingLevels": ["off", "low", "high"],
            "sessionName": "fix-retry",
            "todoPhases": [{"name": "Do", "tasks": [
                {"content": "a", "status": "completed"}, {"content": "b", "status": "pending"}]}]
        }}),
    );
    let (host, cx) = host(cx, model);
    assert!(shown(cx, "omp-hud-session"));
    assert!(shown(cx, "omp-hud-todos"));

    click(cx, "omp-hud-effort");
    assert!(shown(cx, "omp-effort-menu"));
    click(cx, "omp-effort-high");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::SetThinkingLevel {
            level: "high".into()
        }]
    );
    assert!(!shown(cx, "omp-effort-menu"));

    click(cx, "omp-hud-model");
    assert_eq!(intents(&host, cx), [OmpIntent::ListModels]);
    assert!(shown(cx, "omp-model-menu"));
    let view = view(&host, cx);
    view.update(cx, |v, cx| {
        v.set_model_choices(
            ModelChoice::from_picker(&json!({"rows": [
                {"provider": "cursor", "id": "auto-smart", "name": "Auto", "current": true},
                {"provider": "anthropic", "id": "claude-opus-5-5", "name": "Claude Opus 5.5"}
            ]})),
            cx,
        )
    });
    cx.run_until_parked();
    click(cx, "omp-model-1");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::SetModel {
            provider: "anthropic".into(),
            model_id: "claude-opus-5-5".into()
        }]
    );
}

#[gpui::test]
fn a_new_session_shows_the_empty_state(cx: &mut TestAppContext) {
    let (_host, cx) = host(cx, SessionModel::new());
    assert!(shown(cx, "omp-empty"));
    assert!(!shown(cx, "omp-user-message"));
    assert!(shown(cx, "omp-composer"));
}
