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
use xctx_omp::{
    AssistantEntry, Block, Entry, Frame, Queue, SessionModel, Subagent, ToolRun, ToolState,
};
use xctx_omp_view::transcript::RowKey;
use xctx_omp_view::{
    ChangeKind, ChangedFile, DraftMode, ModelChoice, OmpIntent, OmpView, PaneEscape, SessionChanges,
};
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
    assert!(shown(cx, "omp-session-bar"));
    assert!(shown(cx, "omp-hud-todos"));
    assert!(shown(cx, "omp-hud-fast"));

    click(cx, "omp-hud-fast");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::SetFastMode { enabled: true }]
    );

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

#[gpui::test]
fn copy_buttons_write_reply_tool_diff_code_block_and_transcript_to_clipboard(
    cx: &mut TestAppContext,
) {
    let mut model = SessionModel::new();
    model.session.id = Some("sess-42".into());
    model.entries.push(Entry::User {
        text: "Fix the bug".into(),
        images: 0,
        from_agent: false,
    });
    model.tools.insert(
        "edit-1".into(),
        ToolRun {
            id: "edit-1".into(),
            name: "edit".into(),
            args: json!({"path": "src/lib.rs"}),
            state: ToolState::Done,
            output: "ok".into(),
            details: json!({"diff": "-let a = 1;\n+let a = 2;\n"}),
        },
    );
    model.entries.push(Entry::Assistant(AssistantEntry {
        message_id: "m1".into(),
        blocks: vec![
            Block::ToolCall("edit-1".into()),
            Block::Text("Updated it:\n\n```rust\nlet a = 2;\n```".into()),
        ],
        streaming: false,
        model: Some("claude-sonnet".into()),
        ..AssistantEntry::default()
    }));

    let (host, cx) = host(cx, model);

    // Copy tool diff directly from the tool card header without expanding it.
    click(cx, "omp-copy-tool-edit-1");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some("-let a = 1;\n+let a = 2;\n".into())
    );
    assert!(!shown(cx, "omp-tool-expanded"));

    // Expand / collapse all tool cards from the session bar.
    click(cx, "omp-tools-toggle");
    assert!(shown(cx, "omp-tool-expanded"));
    click(cx, "omp-tools-toggle");
    assert!(!shown(cx, "omp-tool-expanded"));

    // Copy the fenced code block from its header bar.
    click(cx, "omp-copy-row-2-code-1");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some("let a = 2;".into())
    );

    // Copy the latest assistant reply from the session bar.
    click(cx, "omp-copy-last");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some("Updated it:\n\n```rust\nlet a = 2;\n```".into())
    );

    // Open Session info: copy ID, copy full transcript, and fire Compact / Fork / NewSession.
    click(cx, "omp-session-info");
    assert!(shown(cx, "omp-info-panel"));
    click(cx, "omp-info-copy-id");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some("sess-42".into())
    );
    click(cx, "omp-info-copy-transcript");
    let transcript = cx
        .read_from_clipboard()
        .and_then(|c| c.text())
        .expect("transcript copied");
    assert!(transcript.contains("## You"));
    assert!(transcript.contains("Fix the bug"));
    assert!(transcript.contains("-let a = 1;"));

    click(cx, "omp-action-compact");
    click(cx, "omp-action-fork");
    click(cx, "omp-action-new");
    assert_eq!(
        intents(&host, cx),
        [
            OmpIntent::Compact { instructions: None },
            OmpIntent::Fork,
            OmpIntent::NewSession,
        ]
    );
}

#[gpui::test]
fn changes_panel_lists_files_and_copies_single_or_all_diffs(cx: &mut TestAppContext) {
    let (host, cx) = host(cx, SessionModel::new());
    let view = view(&host, cx);
    view.update(cx, |v, cx| {
        v.set_changes(
            SessionChanges {
                branch: Some("feat/copy".into()),
                files: vec![
                    ChangedFile {
                        path: "src/main.rs".into(),
                        kind: ChangeKind::Modified,
                        additions: 1,
                        deletions: 1,
                        diff: "diff --git a/src/main.rs b/src/main.rs\n-old\n+new\n".into(),
                    },
                    ChangedFile {
                        path: "src/extra.rs".into(),
                        kind: ChangeKind::Untracked,
                        additions: 1,
                        deletions: 0,
                        diff: "+pub fn hi() {}\n".into(),
                    },
                ],
                error: None,
            },
            cx,
        );
    });
    cx.run_until_parked();

    click(cx, "omp-changes-toggle");
    assert!(shown(cx, "omp-changes-panel"));
    assert!(shown(cx, "omp-changes-files"));
    assert!(shown(cx, "omp-changes-diff"));

    click(cx, "omp-changes-copy-path");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some("src/main.rs".into())
    );

    click(cx, "omp-changes-copy-diff");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some("diff --git a/src/main.rs b/src/main.rs\n-old\n+new\n".into())
    );

    click(cx, "omp-changes-copy-all");
    let all = cx
        .read_from_clipboard()
        .and_then(|c| c.text())
        .expect("combined patch");
    assert!(all.contains("diff --git a/src/main.rs b/src/main.rs"));
    assert!(all.contains("+pub fn hi() {}"));
}

#[gpui::test]
fn subagent_tray_peeks_output_and_emits_cancel(cx: &mut TestAppContext) {
    let (host, cx) = host(cx, SessionModel::new());
    let mut model = SessionModel::new();
    model.subagents.insert(
        "ReadAB".into(),
        Subagent {
            id: "ReadAB".into(),
            agent: "scout".into(),
            task: "Inspect diff helpers".into(),
            status: "running".into(),
            tool_count: 3,
            tokens: 1200,
            duration_ms: 4000,
            current_tool: "grep".into(),
            recent_output: vec!["found plugin_host.rs".into()],
            ..Subagent::default()
        },
    );
    show(&host, cx, model);

    // Tray auto-opens when a subagent arrives.
    assert!(shown(cx, "omp-subagent-tray"));
    assert!(shown(cx, "omp-subagent-ReadAB"));

    // Clicking the row opens the peek card and lets us copy output or cancel.
    click(cx, "omp-subagent-ReadAB");
    assert!(shown(cx, "omp-subagent-peek"));
    click(cx, "omp-subagent-copy-output");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some("found plugin_host.rs".into())
    );

    click(cx, "omp-subagent-cancel-ReadAB");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::CancelSubagent {
            id: "ReadAB".into()
        }]
    );
}

#[gpui::test]
fn btw_panel_and_todo_drawer_render_and_copy(cx: &mut TestAppContext) {
    let (host, cx) = host(cx, SessionModel::new());
    let mut model = replay("btw");
    apply(
        &mut model,
        json!({"type": "btw_record", "record": {
            "id": "15dd27bd55324ae2",
            "question": "What is 2+2? Answer with one digit.",
            "answer": "4",
            "status": "done"
        }}),
    );
    apply(
        &mut model,
        json!({"type": "response", "command": "get_state", "success": true, "data": {
            "todoPhases": [{"name": "Ship", "tasks": [
                {"content": "Add copy", "status": "completed"},
                {"content": "Run ci", "status": "in_progress"}
            ]}]
        }}),
    );
    show(&host, cx, model);

    // Side questions panel auto-opens when btw records arrive.
    assert!(shown(cx, "omp-btw-panel"));
    click(cx, "omp-btw-copy-0");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some("4".into())
    );

    // Clicking the rail's todo indicator opens the checklist drawer and copies Markdown.
    click(cx, "omp-todos-toggle");
    assert!(shown(cx, "omp-todo-drawer"));
    click(cx, "omp-todo-copy");
    let md = cx
        .read_from_clipboard()
        .and_then(|c| c.text())
        .expect("todos copied");
    assert!(md.contains("- [x] Add copy"));
    assert!(md.contains("- [-] Run ci"));
}

#[gpui::test]
fn multiple_sessions_show_quick_strip_switcher_drawer_and_split_action(cx: &mut TestAppContext) {
    use std::cell::Cell;
    use std::rc::Rc;
    use xctx_omp_view::SessionEntry;

    let (host, cx) = host(cx, SessionModel::new());
    let view = view(&host, cx);
    let splits = Rc::new(Cell::new(0usize));
    let splits_cb = splits.clone();

    view.update(cx, |v, cx| {
        v.set_split_agent_handler(Rc::new(move |_, _| {
            splits_cb.set(splits_cb.get() + 1);
        }));
        v.set_sessions(
            vec![
                SessionEntry {
                    id: "sess-2".into(),
                    path: "/tmp/sess-2.jsonl".into(),
                    title: "Fix retry bug".into(),
                    preview: Some("Make checkout retries idempotent".into()),
                    message_count: 14,
                    updated_label: "2m ago".into(),
                    active: true,
                },
                SessionEntry {
                    id: "sess-1".into(),
                    path: "/tmp/sess-1.jsonl".into(),
                    title: "Initial design".into(),
                    preview: Some("Explore payment architecture".into()),
                    message_count: 8,
                    updated_label: "2h ago".into(),
                    active: false,
                },
            ],
            cx,
        );
    });
    cx.run_until_parked();

    // Multiple sessions immediately show the quick session strip with clickable pills.
    assert!(shown(cx, "omp-sessions-strip"));
    assert!(shown(cx, "omp-session-pill-0"));
    assert!(shown(cx, "omp-session-pill-1"));

    // Clicking an inactive session pill switches directly to that session file.
    click(cx, "omp-session-pill-1");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::SwitchSession {
            path: "/tmp/sess-1.jsonl".into()
        }]
    );

    // Clicking the Sessions toggle opens the full drawer with rows, New, Fork, and Split pane.
    click(cx, "omp-sessions-toggle");
    assert!(shown(cx, "omp-sessions-drawer"));
    assert!(!shown(cx, "omp-sessions-strip"));
    assert!(shown(cx, "omp-session-row-0"));
    assert!(shown(cx, "omp-session-row-1"));

    click(cx, "omp-sessions-split");
    assert_eq!(splits.get(), 1);

    click(cx, "omp-session-row-1");
    assert!(!shown(cx, "omp-sessions-drawer"));
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::SwitchSession {
            path: "/tmp/sess-1.jsonl".into()
        }]
    );
}

#[gpui::test]
fn text_selection_copies_via_cmd_c_even_when_composer_is_focused_and_clears_on_escape(
    cx: &mut TestAppContext,
) {
    use xctx_omp_view::SegmentId;

    let mut model = SessionModel::new();
    model.entries.push(Entry::User {
        text: "Check harsh_resume_new.pdf".into(),
        images: 0,
        from_agent: false,
    });
    model.entries.push(Entry::Assistant(AssistantEntry {
        message_id: "m1".into(),
        blocks: vec![Block::Text(
            "Rebuilt `harsh_resume_new.pdf` from `demo_ai_resume.tex`.".into(),
        )],
        streaming: false,
        ..AssistantEntry::default()
    }));
    let (host, cx) = host(cx, model);
    let view = view(&host, cx);

    // Select a phrase inside the rendered assistant paragraph (row 1, segment 0).
    let seg = SegmentId::new(1, 0);
    view.update(cx, |v, cx| {
        v.selection().set_range(seg, 8..28);
        cx.notify();
    });
    cx.run_until_parked();
    assert!(shown(cx, "omp-copy-selection"));
    assert!(shown(cx, "omp-clear-selection"));

    // Focus the composer (empty input selection) and press Cmd+C: must bubble to OmpView and copy transcript selection.
    let composer_focus = view.read_with(cx, |v, cx| v.focus_handle(cx));
    cx.update(|window, app| composer_focus.focus(window, app));
    cx.simulate_keystrokes("cmd-c");
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("harsh_resume_new.pdf")
    );

    // Clicking the floating Copy selection button also copies, and Escape clears the selection.
    cx.write_to_clipboard(gpui::ClipboardItem::new_string("stale".into()));
    click(cx, "omp-copy-selection");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("harsh_resume_new.pdf")
    );

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shown(cx, "omp-copy-selection"));
    assert!(view.read_with(cx, |v, _| !v.selection().has_selection()));
}

#[gpui::test]
fn in_session_search_highlights_navigates_and_closes_on_escape(cx: &mut TestAppContext) {
    let mut model = SessionModel::new();
    model.entries.push(Entry::User {
        text: "Please rebuild the resume pdf".into(),
        images: 0,
        from_agent: false,
    });
    model.entries.push(Entry::Assistant(AssistantEntry {
        message_id: "m1".into(),
        blocks: vec![Block::Text(
            "Done: harsh_resume_new.pdf compiled cleanly.".into(),
        )],
        streaming: false,
        ..AssistantEntry::default()
    }));
    let (_host, cx) = host(cx, model);

    assert!(!shown(cx, "omp-search-bar"));
    click(cx, "omp-search-toggle");
    assert!(shown(cx, "omp-search-bar"));

    cx.simulate_input("resume");
    cx.run_until_parked();
    assert!(shown(cx, "omp-search-count"));

    click(cx, "omp-search-next");
    click(cx, "omp-search-prev");
    assert!(shown(cx, "omp-search-bar"));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(!shown(cx, "omp-search-bar"));
}

#[gpui::test]
fn composer_history_and_edit_in_composer_recall_prompts(cx: &mut TestAppContext) {
    let mut model = SessionModel::new();
    model.entries.push(Entry::User {
        text: "first prompt".into(),
        images: 0,
        from_agent: false,
    });
    model.entries.push(Entry::User {
        text: "second prompt".into(),
        images: 0,
        from_agent: false,
    });
    let (host, cx) = host(cx, model);
    let view = view(&host, cx);

    let composer_focus = view.read_with(cx, |v, cx| v.focus_handle(cx));
    cx.update(|window, app| composer_focus.focus(window, app));

    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    click(cx, "omp-send");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::Prompt {
            text: "second prompt".into(),
            images: vec![],
        }]
    );

    // Clicking Edit on the first user message loads it into the composer ready to send.
    click(cx, "omp-edit-user-0");
    click(cx, "omp-send");
    assert_eq!(
        intents(&host, cx),
        [OmpIntent::Prompt {
            text: "first prompt".into(),
            images: vec![],
        }]
    );
}

#[gpui::test]
fn session_controls_open_file_export_and_new_session_work(cx: &mut TestAppContext) {
    use std::cell::RefCell;
    use std::rc::Rc;

    let mut model = SessionModel::new();
    model.session.id = Some("sess-9".into());
    model.session.file = Some("/tmp/sess-9.jsonl".into());
    model.tools.insert(
        "read-1".into(),
        ToolRun {
            id: "read-1".into(),
            name: "read".into(),
            args: json!({"path": "src/main.rs"}),
            output: "fn main() {}\n".into(),
            details: serde_json::Value::Null,
            state: ToolState::Done,
        },
    );
    model.entries.push(Entry::Assistant(AssistantEntry {
        message_id: "m1".into(),
        blocks: vec![Block::ToolCall("read-1".into())],
        streaming: false,
        ..AssistantEntry::default()
    }));
    let (host, cx) = host(cx, model);
    let view = view(&host, cx);

    let opened: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));
    let opened_cb = opened.clone();
    view.update(cx, |v, cx| {
        v.set_open_file_handler(Rc::new(move |path, _, _| {
            opened_cb.borrow_mut().push(path.to_owned());
        }));
        v.set_changes(
            SessionChanges {
                branch: Some("main".into()),
                files: vec![ChangedFile {
                    path: "src/lib.rs".into(),
                    kind: ChangeKind::Modified,
                    additions: 2,
                    deletions: 1,
                    diff: "@@ -1 +1 @@\n-old\n+new\n".into(),
                }],
                error: None,
            },
            cx,
        );
    });
    cx.run_until_parked();

    // Quick "+" button on the session bar starts a new session.
    click(cx, "omp-bar-new");
    assert_eq!(intents(&host, cx), [OmpIntent::NewSession]);

    // Tool card Open button fires the host open_file callback.
    click(cx, "omp-open-tool-read-1");
    assert_eq!(opened.borrow().as_slice(), ["src/main.rs"]);

    // Changes panel Open button also fires the host open_file callback.
    click(cx, "omp-changes-toggle");
    assert!(shown(cx, "omp-changes-panel"));
    click(cx, "omp-changes-open-file");
    assert_eq!(opened.borrow().as_slice(), ["src/main.rs", "src/lib.rs"]);

    // Session info drawer Export HTML emits OmpIntent::ExportHtml.
    click(cx, "omp-session-info");
    assert!(shown(cx, "omp-info-panel"));
    click(cx, "omp-action-export");
    assert_eq!(intents(&host, cx), [OmpIntent::ExportHtml]);
}
