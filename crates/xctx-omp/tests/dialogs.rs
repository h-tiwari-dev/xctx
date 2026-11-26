//! The dialog registry through its public surface: what frame an answer produces, or which error it is, plus the
//! recorded `ask` and `permission` conversations replayed step by step.

mod common;

use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::TempDir;
use xctx_omp::codec::Decoder;
use xctx_omp::form::Answers;
use xctx_omp::{
    Answer, Change, DangerClass, DialogError, Dialogs, Frame, Kind, Origin, SessionModel,
};

fn ui(request: Value) -> Frame {
    let mut request = request;
    request["type"] = json!("extension_ui_request");
    Frame::parse(request)
}

fn registry() -> (Dialogs, TempDir) {
    let dir = tempfile::tempdir().expect("temp dir");
    (Dialogs::new(dir.path()), dir)
}

fn raise(d: &mut Dialogs, request: Value, now: Instant) {
    let id = request["id"].as_str().expect("id").to_owned();
    assert_eq!(d.apply(&ui(request), now), Some(Change::Raised(id)));
}

fn form_registry(html: &str) -> (Dialogs, TempDir, Instant) {
    let (mut d, dir) = registry();
    let now = Instant::now();
    raise(
        &mut d,
        json!({"id": "f", "method": "form", "html": html}),
        now,
    );
    (d, dir, now)
}

fn text(pairs: &[(&str, &str)]) -> Answer {
    let mut a = Answers::default();
    for (k, v) in pairs {
        a.text.insert((*k).into(), (*v).into());
    }
    Answer::Form(a)
}

fn chosen(name: &str, values: &[&str]) -> Answers {
    let mut a = Answers::default();
    a.chosen.insert(
        name.into(),
        values.iter().map(|v| (*v).to_string()).collect(),
    );
    a
}

/// The names of the fields an invalid form answer was refused for; the request must still be waiting.
fn refused(d: &mut Dialogs, answer: Answer, now: Instant) -> Vec<String> {
    let err = d.answer("f", answer, Origin::Pane, now).unwrap_err();
    let DialogError::Invalid(fields) = err else {
        panic!("not a field error: {err:?}");
    };
    assert!(d.get("f").is_some(), "an invalid answer leaves it pending");
    fields.into_iter().map(|f| f.name).collect()
}

fn sent(d: &mut Dialogs, id: &str, answer: Answer, origin: Origin, now: Instant) -> Value {
    d.answer(id, answer, origin, now)
        .expect("a valid answer")
        .as_value()
        .clone()
}

// ---- forms: every rule is enforced before a frame exists -----------------------------------------------------

const TEXT_FORM: &str = r#"<form>
    <label>Name <input name="n" required minlength="3" maxlength="5"></label>
    <label>Age <input type="number" name="age" min="1" max="120"></label>
    <label>Code <input name="code" pattern="[A-Z]{2}[0-9]"></label>
    <label>Mail <input type="email" name="mail"></label>
    <label>Site <input type="url" name="site"></label>
    </form>"#;

#[test]
fn a_required_field_left_blank_is_refused() {
    let (mut d, _t, now) = form_registry(TEXT_FORM);
    assert_eq!(refused(&mut d, text(&[("n", "   ")]), now), ["n"]);
    assert_eq!(
        refused(&mut d, Answer::Form(Answers::default()), now),
        ["n"]
    );
}

#[test]
fn minlength_and_maxlength_are_refused_either_side() {
    let (mut d, _t, now) = form_registry(TEXT_FORM);
    assert_eq!(refused(&mut d, text(&[("n", "ab")]), now), ["n"]);
    assert_eq!(refused(&mut d, text(&[("n", "abcdef")]), now), ["n"]);
}

#[test]
fn number_min_max_and_non_numbers_are_refused() {
    let (mut d, _t, now) = form_registry(TEXT_FORM);
    assert_eq!(
        refused(&mut d, text(&[("n", "abc"), ("age", "0")]), now),
        ["age"]
    );
    assert_eq!(
        refused(&mut d, text(&[("n", "abc"), ("age", "121")]), now),
        ["age"]
    );
    assert_eq!(
        refused(&mut d, text(&[("n", "abc"), ("age", "old")]), now),
        ["age"]
    );
    assert_eq!(
        refused(&mut d, text(&[("n", "abc"), ("age", "NaN")]), now),
        ["age"]
    );
}

#[test]
fn a_pattern_must_match_the_whole_value() {
    let (mut d, _t, now) = form_registry(TEXT_FORM);
    assert_eq!(
        refused(&mut d, text(&[("n", "abc"), ("code", "AB12")]), now),
        ["code"]
    );
    assert_eq!(
        refused(&mut d, text(&[("n", "abc"), ("code", "ab1")]), now),
        ["code"]
    );
}

#[test]
fn email_and_url_must_look_like_one() {
    let (mut d, _t, now) = form_registry(TEXT_FORM);
    let bad = text(&[("n", "abc"), ("mail", "nope"), ("site", "not a url")]);
    assert_eq!(refused(&mut d, bad, now), ["mail", "site"]);
}

#[test]
fn every_failing_field_is_reported_at_once() {
    let (mut d, _t, now) = form_registry(TEXT_FORM);
    let bad = text(&[("n", "a"), ("age", "500"), ("code", "x"), ("mail", "y")]);
    assert_eq!(refused(&mut d, bad, now), ["n", "age", "code", "mail"]);
}

#[test]
fn a_valid_form_becomes_the_frame_omp_expects_and_settles_the_request() {
    let (mut d, _t, now) = form_registry(TEXT_FORM);
    let good = text(&[
        ("n", "abcd"),
        ("age", "42"),
        ("code", "AB1"),
        ("mail", "me@example.com"),
        ("site", "https://example.com"),
    ]);
    let frame = sent(&mut d, "f", good, Origin::Pane, now);
    assert_eq!(
        frame,
        json!({"type": "extension_ui_response", "id": "f", "data": {
            "n": "abcd", "age": 42, "code": "AB1", "mail": "me@example.com", "site": "https://example.com"}})
    );
    assert!(d.pending().is_empty());
}

const RADIO: &str = r#"<form><fieldset><legend>Pick</legend>
    <label><input type="radio" name="c" value="a" required> A</label>
    <label><input type="radio" name="c" value="b"> B</label>
    <label><input type="radio" name="c" value="z" disabled> Z</label></fieldset></form>"#;

#[test]
fn radio_rules_required_one_only_options_only_and_other() {
    let (mut d, _t, now) = form_registry(RADIO);
    let go = |a: Answers| Answer::Form(a);
    assert_eq!(
        refused(&mut d, go(Answers::default()), now),
        ["c"],
        "required"
    );
    assert_eq!(
        refused(&mut d, go(chosen("c", &["a", "b"])), now),
        ["c"],
        "one radio only"
    );
    assert_eq!(
        refused(&mut d, go(chosen("c", &["q"])), now),
        ["c"],
        "not an option"
    );
    assert_eq!(
        refused(&mut d, go(chosen("c", &["z"])), now),
        ["c"],
        "disabled option"
    );

    let mut other_and_pick = chosen("c", &["a"]);
    other_and_pick.other.insert("c".into(), "green".into());
    assert_eq!(
        refused(&mut d, go(other_and_pick), now),
        ["c"],
        "an option and Other together"
    );

    let mut duplicate = Answers::default();
    duplicate.other.insert("c".into(), "a".into());
    assert_eq!(
        refused(&mut d, go(duplicate), now),
        ["c"],
        "Other that repeats an option"
    );

    let mut free = Answers::default();
    free.other.insert("c".into(), "green".into());
    assert_eq!(
        sent(&mut d, "f", go(free), Origin::Pane, now)["data"],
        json!({"c": "green"}),
        "free text is allowed on a group of two or more without an Other option"
    );
}

#[test]
fn other_text_is_refused_when_the_form_has_its_own_other_option() {
    let (mut d, _t, now) = form_registry(
        r#"<form><fieldset><legend>Pick</legend>
        <label><input type="radio" name="c" value="a"> A</label>
        <label><input type="radio" name="c" value="other"> Other</label></fieldset></form>"#,
    );
    let mut typed = Answers::default();
    typed.other.insert("c".into(), "green".into());
    assert_eq!(refused(&mut d, Answer::Form(typed), now), ["c"]);
}

#[test]
fn checkbox_groups_answer_with_an_array_and_a_required_one_needs_a_tick() {
    let (mut d, _t, now) = form_registry(
        r#"<form><fieldset><legend>Pick</legend>
        <label><input type="checkbox" name="t" value="a" required> A</label>
        <label><input type="checkbox" name="t" value="b"> B</label></fieldset></form>"#,
    );
    assert_eq!(
        refused(&mut d, Answer::Form(Answers::default()), now),
        ["t"]
    );
    assert_eq!(
        refused(&mut d, Answer::Form(chosen("t", &["nope"])), now),
        ["t"]
    );
    let frame = sent(
        &mut d,
        "f",
        Answer::Form(chosen("t", &["a", "b"])),
        Origin::Pane,
        now,
    );
    assert_eq!(frame["data"], json!({"t": ["a", "b"]}));
}

#[test]
fn a_form_takes_form_values_or_a_dismissal_and_nothing_else() {
    let (mut d, _t, now) = form_registry(RADIO);
    for wrong in [
        Answer::Choice("a".into()),
        Answer::Confirm(true),
        Answer::Text("a".into()),
    ] {
        let err = d.answer("f", wrong, Origin::Pane, now).unwrap_err();
        assert!(matches!(err, DialogError::WrongAnswer { .. }), "{err:?}");
    }
    assert_eq!(
        sent(&mut d, "f", Answer::Cancel, Origin::Pane, now),
        json!({"type": "extension_ui_response", "id": "f", "cancelled": true})
    );
}

// ---- select, confirm, input, editor ---------------------------------------------------------------------------

#[test]
fn a_select_answer_must_be_one_of_its_options() {
    let (mut d, _t) = registry();
    let now = Instant::now();
    raise(
        &mut d,
        json!({"id": "s", "method": "select", "title": "Branch?", "options": ["main", "dev"]}),
        now,
    );
    assert!(matches!(
        d.get("s").map(|p| &p.kind),
        Some(Kind::Select { .. })
    ));
    assert_eq!(
        d.answer("s", Answer::Choice("nope".into()), Origin::Pane, now),
        Err(DialogError::InvalidChoice("nope".into()))
    );
    assert!(matches!(
        d.answer("s", Answer::Confirm(true), Origin::Pane, now),
        Err(DialogError::WrongAnswer { .. })
    ));
    assert!(d.get("s").is_some());
    assert_eq!(
        sent(&mut d, "s", Answer::Choice("dev".into()), Origin::Pane, now),
        json!({"type": "extension_ui_response", "id": "s", "value": "dev"})
    );
}

#[test]
fn confirm_input_and_editor_take_their_own_kind_of_answer() {
    let (mut d, _t) = registry();
    let now = Instant::now();
    raise(
        &mut d,
        json!({"id": "c", "method": "confirm", "title": "Sure?", "message": "m"}),
        now,
    );
    raise(
        &mut d,
        json!({"id": "i", "method": "input", "title": "Token", "secret": true}),
        now,
    );
    raise(
        &mut d,
        json!({"id": "e", "method": "editor", "title": "Edit", "prefill": "x"}),
        now,
    );
    assert!(matches!(
        d.get("i").map(|p| &p.kind),
        Some(Kind::Input { secret: true, .. })
    ));

    assert!(matches!(
        d.answer("c", Answer::Text("yes".into()), Origin::Pane, now),
        Err(DialogError::WrongAnswer { .. })
    ));
    assert!(matches!(
        d.answer("i", Answer::Confirm(true), Origin::Pane, now),
        Err(DialogError::WrongAnswer { .. })
    ));
    assert!(matches!(
        d.answer("e", Answer::Choice("x".into()), Origin::Pane, now),
        Err(DialogError::WrongAnswer { .. })
    ));
    assert_eq!(d.pending().len(), 3, "refused answers settle nothing");

    assert_eq!(
        sent(&mut d, "c", Answer::Confirm(false), Origin::Pane, now),
        json!({"type": "extension_ui_response", "id": "c", "confirmed": false})
    );
    assert_eq!(
        sent(
            &mut d,
            "i",
            Answer::Text("hunter2".into()),
            Origin::Pane,
            now
        ),
        json!({"type": "extension_ui_response", "id": "i", "value": "hunter2"})
    );
    assert_eq!(
        sent(&mut d, "e", Answer::Text("a\nb".into()), Origin::Pane, now),
        json!({"type": "extension_ui_response", "id": "e", "value": "a\nb"})
    );
}

#[test]
fn an_answer_never_shows_its_text_in_debug_output() {
    let shown = format!("{:?}", Answer::Text("hunter2".into()));
    assert!(!shown.contains("hunter2"), "{shown}");
}

#[test]
fn a_dialog_with_unrecorded_answer_shape_can_only_be_dismissed() {
    let (mut d, _t) = registry();
    let now = Instant::now();
    raise(
        &mut d,
        json!({"id": "q", "method": "ask", "questions": [{"q": "x"}]}),
        now,
    );
    assert!(matches!(
        d.answer("q", Answer::Text("x".into()), Origin::Pane, now),
        Err(DialogError::WrongAnswer { .. })
    ));
    assert_eq!(
        sent(&mut d, "q", Answer::Cancel, Origin::Pane, now),
        json!({"type": "extension_ui_response", "id": "q", "cancelled": true})
    );
}

#[test]
fn presentation_requests_never_wait_for_an_answer() {
    let (mut d, _t) = registry();
    let now = Instant::now();
    for request in [
        json!({"id": "w", "method": "setWidget", "widgetKey": "x"}),
        json!({"id": "n", "method": "notify", "message": "hi"}),
        json!({"id": "", "method": "confirm", "title": "no id"}),
    ] {
        assert_eq!(d.apply(&ui(request), now), None);
    }
    assert!(d.pending().is_empty());
    assert!(matches!(
        d.answer("w", Answer::Cancel, Origin::Pane, now),
        Err(DialogError::Unknown(_))
    ));
}

#[test]
fn a_repeated_request_id_is_one_request() {
    let (mut d, _t) = registry();
    let now = Instant::now();
    let request = json!({"id": "c", "method": "confirm", "title": "Sure?"});
    raise(&mut d, request.clone(), now);
    assert_eq!(d.apply(&ui(request.clone()), now), None);
    assert_eq!(d.pending().len(), 1);
    sent(&mut d, "c", Answer::Confirm(true), Origin::Pane, now);
    assert_eq!(
        d.apply(&ui(request), now),
        None,
        "an answered id does not come back"
    );
    assert!(d.pending().is_empty());
}

// ---- timeout and cancel ---------------------------------------------------------------------------------------

#[test]
fn a_timeout_is_a_deadline_the_host_polls_and_a_late_answer_is_an_error() {
    let (mut d, _t) = registry();
    let t0 = Instant::now();
    raise(
        &mut d,
        json!({"id": "a", "method": "confirm", "title": "A", "timeout": 5000}),
        t0,
    );
    raise(
        &mut d,
        json!({"id": "b", "method": "confirm", "title": "B", "timeout": 2000}),
        t0,
    );
    raise(
        &mut d,
        json!({"id": "c", "method": "confirm", "title": "C"}),
        t0,
    );
    raise(
        &mut d,
        json!({"id": "z", "method": "confirm", "title": "Z", "timeout": 0}),
        t0,
    );

    assert_eq!(
        d.get("a").and_then(|p| p.deadline),
        Some(t0 + Duration::from_secs(5))
    );
    assert_eq!(d.get("c").and_then(|p| p.deadline), None);
    assert_eq!(
        d.get("z").and_then(|p| p.deadline),
        None,
        "zero means no timeout"
    );
    assert_eq!(d.next_deadline(), Some(t0 + Duration::from_secs(2)));
    assert_eq!(
        d.get("a")
            .and_then(|p| p.remaining(t0 + Duration::from_secs(1))),
        Some(Duration::from_secs(4))
    );
    assert_eq!(
        d.get("a")
            .and_then(|p| p.remaining(t0 + Duration::from_secs(9))),
        Some(Duration::ZERO)
    );

    assert!(d.expire(t0 + Duration::from_millis(1999)).is_empty());
    assert_eq!(d.expire(t0 + Duration::from_secs(2)), ["b"]);
    assert_eq!(d.next_deadline(), Some(t0 + Duration::from_secs(5)));
    assert_eq!(
        d.answer(
            "b",
            Answer::Confirm(true),
            Origin::Pane,
            t0 + Duration::from_secs(3)
        ),
        Err(DialogError::TimedOut("b".into()))
    );

    // "a" is past its deadline but the host has not polled yet: the answer is still refused.
    assert_eq!(
        d.answer(
            "a",
            Answer::Confirm(true),
            Origin::Inbox,
            t0 + Duration::from_secs(6)
        ),
        Err(DialogError::TimedOut("a".into()))
    );
    assert!(d.get("a").is_none());

    // Requests without a deadline stay answerable however late.
    let late = t0 + Duration::from_secs(3600);
    assert!(d.expire(late).is_empty());
    assert!(
        d.answer("c", Answer::Confirm(true), Origin::Pane, late)
            .is_ok()
    );
}

#[test]
fn a_cancel_frame_closes_its_target_and_the_answer_is_an_error() {
    let (mut d, _t) = registry();
    let now = Instant::now();
    raise(
        &mut d,
        json!({"id": "a", "method": "select", "title": "A", "options": ["x"]}),
        now,
    );
    raise(
        &mut d,
        json!({"id": "b", "method": "confirm", "title": "B"}),
        now,
    );

    assert_eq!(
        d.apply(
            &ui(json!({"id": "k1", "method": "cancel", "targetId": "a"})),
            now
        ),
        Some(Change::Cancelled("a".into()))
    );
    assert_eq!(
        d.pending()
            .iter()
            .map(|p| p.id.as_str())
            .collect::<Vec<_>>(),
        ["b"]
    );
    assert_eq!(
        d.answer("a", Answer::Choice("x".into()), Origin::Pane, now),
        Err(DialogError::Cancelled("a".into()))
    );
    assert_eq!(
        d.apply(
            &ui(json!({"id": "k2", "method": "cancel", "targetId": "ghost"})),
            now
        ),
        None,
        "cancelling something unknown changes nothing"
    );
    assert_eq!(d.pending().len(), 1);
}

#[test]
fn replacing_the_session_forgets_every_request() {
    let (mut d, _t) = registry();
    let now = Instant::now();
    raise(
        &mut d,
        json!({"id": "a", "method": "confirm", "title": "A"}),
        now,
    );
    d.clear();
    assert!(d.pending().is_empty());
    assert_eq!(
        d.answer("a", Answer::Confirm(true), Origin::Pane, now),
        Err(DialogError::Unknown("a".into()))
    );
}

// ---- one request, one answer ------------------------------------------------------------------------------------

#[test]
fn the_second_answer_from_the_pane_or_the_inbox_is_an_error_not_a_frame() {
    let (mut d, _t) = registry();
    let now = Instant::now();
    raise(
        &mut d,
        json!({"id": "a", "method": "confirm", "title": "A"}),
        now,
    );
    raise(
        &mut d,
        json!({"id": "b", "method": "confirm", "title": "B"}),
        now,
    );

    let first = sent(&mut d, "a", Answer::Confirm(true), Origin::Inbox, now);
    assert_eq!(first["confirmed"], json!(true));
    assert_eq!(
        d.answer("a", Answer::Confirm(false), Origin::Pane, now),
        Err(DialogError::AlreadyAnswered("a".into()))
    );
    assert_eq!(
        d.answer("a", Answer::Cancel, Origin::Inbox, now),
        Err(DialogError::AlreadyAnswered("a".into()))
    );
    assert_eq!(d.pending().len(), 1, "the other request is untouched");
    assert!(
        d.answer("b", Answer::Confirm(false), Origin::Pane, now)
            .is_ok()
    );
    assert_eq!(
        d.answer("never-seen", Answer::Cancel, Origin::Pane, now),
        Err(DialogError::Unknown("never-seen".into()))
    );
}

// ---- tool approvals ---------------------------------------------------------------------------------------------

fn approval(command: &str) -> Value {
    json!({"id": "p", "method": "select",
        "title": format!("Allow tool: bash\nCommand: {command}"),
        "options": ["Approve", "Approve bash for this session", "Deny"]})
}

#[test]
fn an_allow_tool_select_becomes_a_permission_with_tool_and_command() {
    let (mut d, _t) = registry();
    let now = Instant::now();
    raise(&mut d, approval("echo hi"), now);
    let Some(Kind::Permission(p)) = d.get("p").map(|p| &p.kind) else {
        panic!("not a permission");
    };
    assert_eq!(p.tool, "bash");
    assert_eq!(p.command.as_deref(), Some("echo hi"));
    assert_eq!(p.detail, "Command: echo hi");
    assert_eq!(p.approve_once(), Some("Approve"));
    assert_eq!(p.deny(), Some("Deny"));
    assert!(p.danger.is_none() && p.one_click_allowed());
}

#[test]
fn a_multi_line_command_is_kept_whole() {
    let (mut d, _t) = registry();
    raise(&mut d, approval("cat <<EOF\nhello\nEOF"), Instant::now());
    let Some(Kind::Permission(p)) = d.get("p").map(|p| &p.kind) else {
        panic!("not a permission");
    };
    assert_eq!(p.command.as_deref(), Some("cat <<EOF\nhello\nEOF"));
}

fn danger_of(command: &str) -> Option<DangerClass> {
    let (mut d, _t) = registry();
    raise(&mut d, approval(command), Instant::now());
    let Some(Kind::Permission(p)) = d.get("p").map(|p| &p.kind) else {
        panic!("not a permission");
    };
    p.danger.as_ref().map(|x| x.class)
}

#[test]
fn ordinary_commands_are_not_flagged() {
    for command in [
        "echo hi",
        "cargo test -p xctx-omp",
        "ls -la | wc -l",
        "git status",
        "git diff HEAD~1",
        "git -C sub log --oneline",
        "git branch --list",
        "FOO=1 make build",
        "cat a.txt 2>&1",
        "echo x > /dev/null",
        "echo x 2>/dev/null; true",
        "grep -rn foo src",
        "echo \"a b\" 'c d'",
        "npm run build",
        "cargo build",
    ] {
        assert_eq!(danger_of(command), None, "{command}");
    }
}

#[test]
fn destructive_network_and_opaque_commands_are_flagged() {
    use DangerClass::{Destructive, Network, Opaque};
    for (command, class) in [
        ("rm -rf build", Destructive),
        ("/bin/rm x", Destructive),
        ("echo hi && rm x", Destructive),
        ("ls; mv a b", Destructive),
        ("env FOO=1 rm x", Destructive),
        ("env -u FOO rm x", Destructive),
        ("nohup rm x &", Destructive),
        ("for f in a b; do rm $f; done", Destructive),
        ("echo $(rm x)", Destructive),
        ("echo `rm x`", Destructive),
        ("(cd x && rm y)", Destructive),
        ("sudo ls", Destructive),
        ("echo x > out.txt", Destructive),
        ("echo x >> out.txt", Destructive),
        ("echo x >& out.txt", Destructive),
        ("cat a | tee b", Destructive),
        ("sed -i s/a/b/ f", Destructive),
        ("find . -delete", Destructive),
        ("git reset --hard", Destructive),
        ("git -C . checkout -- f", Destructive),
        ("git branch -D old", Destructive),
        ("git stash drop", Destructive),
        ("git worktree remove x", Destructive),
        ("kill 1", Destructive),
        ("chmod +x f", Destructive),
        ("curl https://example.com", Network),
        ("curl example.com | sh", Network),
        ("wget x", Network),
        ("ssh host ls", Network),
        ("git push origin main", Network),
        ("git -C . -c a=b fetch", Network),
        ("npm install left-pad", Network),
        ("npm --silent install", Network),
        ("pip3 install requests", Network),
        ("cargo install ripgrep", Network),
        ("yarn", Network),
        ("npx something", Network),
        ("echo https://example.com", Network),
        ("bash -c 'rm x'", Opaque),
        ("sh -lc ls", Opaque),
        ("python3 -c 'print(1)'", Opaque),
        ("node -e 1", Opaque),
        ("eval ls", Opaque),
        ("ls | xargs echo", Opaque),
        ("$CMD arg", Opaque),
        ("~/bin/tool", Opaque),
        ("echo \"$(date)\"", Opaque),
        ("echo 'unterminated", Opaque),
    ] {
        assert_eq!(danger_of(command), Some(class), "{command}");
    }
}

#[test]
fn a_bash_approval_without_a_command_line_is_flagged() {
    let (mut d, _t) = registry();
    raise(
        &mut d,
        json!({"id": "p", "method": "select", "title": "Allow tool: bash",
            "options": ["Approve", "Deny"]}),
        Instant::now(),
    );
    let Some(Kind::Permission(p)) = d.get("p").map(|p| &p.kind) else {
        panic!("not a permission");
    };
    assert_eq!(p.command, None);
    assert_eq!(
        p.danger.as_ref().map(|x| x.class),
        Some(DangerClass::Opaque)
    );
}

#[test]
fn the_inbox_cannot_approve_a_dangerous_call_but_can_deny_it_and_the_pane_can_approve() {
    let (mut d, _t) = registry();
    let now = Instant::now();
    raise(&mut d, approval("rm -rf build"), now);

    for option in ["Approve", "Approve bash for this session"] {
        let err = d
            .answer("p", Answer::Choice(option.into()), Origin::Inbox, now)
            .unwrap_err();
        assert!(
            matches!(err, DialogError::NeedsReview { ref tool, .. } if tool == "bash"),
            "{err:?}"
        );
        assert!(d.get("p").is_some(), "a refused approval leaves it pending");
    }
    assert!(matches!(
        d.answer("p", Answer::Choice("Maybe".into()), Origin::Pane, now),
        Err(DialogError::InvalidChoice(_))
    ));
    assert_eq!(
        sent(
            &mut d,
            "p",
            Answer::Choice("Approve".into()),
            Origin::Pane,
            now
        ),
        json!({"type": "extension_ui_response", "id": "p", "value": "Approve"}),
        "in the pane the user has read the exact call"
    );

    let mut denied = approval("rm -rf build");
    denied["id"] = json!("p2");
    raise(&mut d, denied, now);
    assert_eq!(
        sent(
            &mut d,
            "p2",
            Answer::Choice("Deny".into()),
            Origin::Inbox,
            now
        )["value"],
        json!("Deny")
    );
}

#[test]
fn the_inbox_can_approve_an_unflagged_call_with_one_click() {
    let (mut d, _t) = registry();
    let now = Instant::now();
    raise(&mut d, approval("echo hi"), now);
    assert_eq!(
        sent(
            &mut d,
            "p",
            Answer::Choice("Approve".into()),
            Origin::Inbox,
            now
        ),
        json!({"type": "extension_ui_response", "id": "p", "value": "Approve"})
    );
}

#[test]
fn dismissing_an_approval_is_the_frame_omp_treats_as_a_deny_even_when_dangerous() {
    let (mut d, _t) = registry();
    let now = Instant::now();
    raise(&mut d, approval("rm -rf build"), now);
    assert_eq!(
        sent(&mut d, "p", Answer::Cancel, Origin::Inbox, now),
        json!({"type": "extension_ui_response", "id": "p", "cancelled": true})
    );
}

fn file_approval(tool: &str, detail: &str) -> Value {
    json!({"id": "p", "method": "select",
        "title": format!("Allow tool: {tool}\n{detail}"),
        "options": ["Approve", "Deny"]})
}

fn file_danger(d: &mut Dialogs, tool: &str, detail: &str) -> Option<DangerClass> {
    let mut request = file_approval(tool, detail);
    request["id"] = json!(format!("{tool}:{detail}"));
    let id = request["id"].as_str().unwrap_or_default().to_owned();
    raise(d, request, Instant::now());
    let Some(Kind::Permission(p)) = d.get(&id).map(|p| &p.kind) else {
        panic!("not a permission");
    };
    p.danger.as_ref().map(|x| x.class)
}

#[test]
fn write_and_edit_are_flagged_outside_the_worktree_only() {
    let dir = tempfile::tempdir().expect("temp dir");
    let outside = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir(dir.path().join("src")).expect("mkdir");
    std::fs::write(dir.path().join("src/a.rs"), "").expect("write");
    let mut d = Dialogs::new(dir.path());
    let root = dir.path().display();
    let out = outside.path().display();

    for tool in ["write", "edit", "ast_edit"] {
        assert_eq!(
            file_danger(&mut d, tool, "Path: src/a.rs"),
            None,
            "{tool} relative, existing"
        );
        assert_eq!(
            file_danger(&mut d, tool, "Path: src/new/deep.rs"),
            None,
            "{tool} new file"
        );
        assert_eq!(
            file_danger(&mut d, tool, &format!("Path: {root}/src/a.rs")),
            None
        );
        assert_eq!(
            file_danger(&mut d, tool, "file_path: src/a.rs"),
            None,
            "{tool} key spelling"
        );
    }
    let flagged = Some(DangerClass::OutsideWorktree);
    assert_eq!(
        file_danger(&mut d, "write", &format!("Path: {out}/x")),
        flagged
    );
    assert_eq!(file_danger(&mut d, "write", "Path: /etc/hosts"), flagged);
    assert_eq!(file_danger(&mut d, "edit", "Path: ../x"), flagged);
    assert_eq!(file_danger(&mut d, "edit", "Path: src/../../x"), flagged);
    assert_eq!(file_danger(&mut d, "edit", "Path: ~/.zshrc"), flagged);
    assert_eq!(file_danger(&mut d, "edit", "Path:"), flagged);
    assert_eq!(
        file_danger(&mut d, "write", "Something else"),
        flagged,
        "no path to check"
    );
    assert_eq!(
        file_danger(&mut d, "write", &format!("Path: src/a.rs\nPath: {out}/x")),
        flagged,
        "every path counts"
    );
    // Other tools are not classified; the Inbox still shows them in full.
    assert_eq!(file_danger(&mut d, "read", "Path: /etc/hosts"), None);
}

#[cfg(unix)]
#[test]
fn a_symlink_that_leaves_the_worktree_is_outside() {
    let dir = tempfile::tempdir().expect("temp dir");
    let outside = tempfile::tempdir().expect("temp dir");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).expect("symlink");
    std::os::unix::fs::symlink(outside.path().join("missing"), dir.path().join("dangling"))
        .expect("symlink");
    let mut d = Dialogs::new(dir.path());
    let flagged = Some(DangerClass::OutsideWorktree);
    assert_eq!(file_danger(&mut d, "write", "Path: link/x"), flagged);
    assert_eq!(file_danger(&mut d, "write", "Path: dangling"), flagged);
}

// ---- recorded conversations -------------------------------------------------------------------------------------

/// One line of a fixture: what omp said, or what the recording client answered.
enum Line {
    Said(Frame),
    Sent(Value),
}

fn transcript(name: &str) -> Vec<Line> {
    let body = std::fs::read_to_string(common::fixture(name)).expect("fixture");
    let mut decoder = Decoder::default();
    let mut lines = Vec::new();
    for line in body.lines() {
        if let Some(said) = line.strip_prefix("< ") {
            if let Some(value) = decoder.push_line(said).expect("decodes") {
                lines.push(Line::Said(Frame::parse(value)));
            }
        } else if let Some(sent) = line.strip_prefix("> ") {
            let value: Value = serde_json::from_str(sent).expect("json");
            if value["type"] == "extension_ui_response" {
                lines.push(Line::Sent(value));
            }
        }
    }
    lines
}

/// The ids waiting on the user after each step, with immediate repeats collapsed.
struct Steps {
    seen: Vec<Vec<String>>,
}

impl Steps {
    fn note(&mut self, d: &Dialogs, model: &SessionModel) {
        let ids: Vec<String> = d.pending().iter().map(|p| p.id.clone()).collect();
        let model_ids: Vec<String> = model
            .pending_ui
            .iter()
            .filter(|r| r.blocks())
            .map(|r| r.id.clone())
            .collect();
        assert_eq!(ids, model_ids, "the registry and the session model agree");
        if self.seen.last() != Some(&ids) {
            self.seen.push(ids);
        }
    }
}

/// Replay a fixture through the registry and the model the way a host would, answering each request with
/// `answer_for(recorded response)`, and return the ids pending after every step.
fn replay(name: &str, origin: Origin, answer_for: impl Fn(&Value) -> Answer) -> Vec<Vec<String>> {
    let (mut d, _t) = registry();
    let mut model = SessionModel::new();
    let mut steps = Steps { seen: Vec::new() };
    let now = Instant::now();
    for line in transcript(name) {
        match line {
            Line::Said(frame) => {
                d.apply(&frame, now);
                model.apply(frame);
            }
            Line::Sent(recorded) => {
                let id = recorded["id"].as_str().expect("id").to_owned();
                let ours = d
                    .answer(&id, answer_for(&recorded), origin, now)
                    .expect("the recorded answer is valid");
                assert_eq!(ours.as_value(), &recorded, "the frame omp was sent");
                model.resolve_ui(&id);
                assert_eq!(
                    d.answer(&id, answer_for(&recorded), origin, now),
                    Err(DialogError::AlreadyAnswered(id.clone())),
                    "a second answer is refused"
                );
            }
        }
        steps.note(&d, &model);
    }
    steps.seen
}

#[test]
fn replaying_ask_jsonl_the_form_is_pending_until_the_recorded_answer_and_the_frame_matches() {
    let seen = replay("ask", Origin::Pane, |recorded| {
        let colour = recorded["data"]["colour"].as_str().expect("colour");
        Answer::Form(chosen("colour", &[colour]))
    });
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(seen[0].is_empty());
    assert_eq!(seen[1], ["15dd27a370190919"]);
    assert!(seen[2].is_empty());
}

#[test]
fn replaying_permission_jsonl_a_cancel_is_the_recorded_deny_and_the_inbox_may_send_it() {
    let seen = replay("permission", Origin::Inbox, |recorded| {
        assert_eq!(recorded["cancelled"], json!(true));
        Answer::Cancel
    });
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert!(seen[0].is_empty());
    assert_eq!(seen[1], ["15dd27a87ca5884c"]);
    assert!(seen[2].is_empty());
}

#[test]
fn the_recorded_permission_is_a_bash_echo_the_inbox_may_approve_with_one_click() {
    let (mut d, _t) = registry();
    let now = Instant::now();
    for line in transcript("permission") {
        if let Line::Said(frame) = line {
            d.apply(&frame, now);
            if !d.pending().is_empty() {
                break;
            }
        }
    }
    let Some(Kind::Permission(p)) = d.pending().first().map(|p| &p.kind) else {
        panic!("no permission pending");
    };
    assert_eq!(
        (p.tool.as_str(), p.command.as_deref()),
        ("bash", Some("echo hi"))
    );
    assert!(p.one_click_allowed());
    let id = d.pending()[0].id.clone();
    let frame = sent(
        &mut d,
        &id,
        Answer::Choice("Approve".into()),
        Origin::Inbox,
        now,
    );
    assert_eq!(frame["value"], json!("Approve"));
}
