//! The shared-space store in temporary data directories.

use std::fs;

use xctx_channel::{
    ChannelError, ContractDraft, FindingSeverity, MAX_MESSAGES, MessageKind, SHARED_FILE,
    SPACES_DIR, SharedSpaceStore,
};
use xctx_core::Redactor;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn draft(spec: &str) -> ContractDraft {
    ContractDraft {
        title: "rate-limit headers".into(),
        summary: "what the api sends when it limits".into(),
        spec: spec.into(),
    }
}

fn secrets() -> Redactor {
    Redactor::new([("STRIPE_KEY".to_owned(), "sk_live_abc123".to_owned())])
}

#[test]
fn a_new_space_starts_empty_and_writes_nothing_until_it_changes() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let store = SharedSpaceStore::open_for_space(tmp.path(), "rate-limit")?;
    assert_eq!(
        store.path(),
        tmp.path()
            .join(SPACES_DIR)
            .join("rate-limit")
            .join(SHARED_FILE)
    );
    assert!(!store.path().exists());
    let none = Redactor::default();
    assert_eq!(store.contract(&none), None);
    assert!(
        store
            .list_messages(None, None, None, None, &none)
            .is_empty()
    );
    assert!(store.findings(false, &none).is_empty());
    assert!(store.notes(&none).is_empty());
    Ok(())
}

#[test]
fn unsafe_slugs_are_refused() -> TestResult {
    let tmp = tempfile::tempdir()?;
    for slug in ["", "..", "../escape", "a/b", ".hidden"] {
        assert!(matches!(
            SharedSpaceStore::open_for_space(tmp.path(), slug),
            Err(ChannelError::InvalidSlug(_))
        ));
    }
    assert!(!tmp.path().join(SPACES_DIR).exists());
    Ok(())
}

#[test]
fn contract_versions_and_agreements_survive_reopening() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let mut store = SharedSpaceStore::open_for_space(tmp.path(), "rl")?;
    store.set_clock(|| 100);
    assert!(matches!(
        store.agree_contract("writer-web"),
        Err(ChannelError::NoContract)
    ));
    assert_eq!(
        store.set_contract("writer-api", draft("X-RateLimit: int"))?,
        1
    );
    assert_eq!(store.agree_contract("writer-web")?, 1);
    assert_eq!(store.agree_contract("writer-web")?, 1);

    let reopened = SharedSpaceStore::open_for_space(tmp.path(), "rl")?;
    let none = Redactor::default();
    let contract = reopened.contract(&none).ok_or("no contract")?;
    assert_eq!(contract.version, 1);
    assert_eq!(contract.agreed_by, ["writer-api", "writer-web"]);
    assert_eq!(contract.updated_at, 100);

    let mut store = reopened;
    assert_eq!(
        store.set_contract("writer-web", draft("X-RateLimit: u32"))?,
        2
    );
    let contract = store.contract(&none).ok_or("no contract")?;
    assert_eq!(contract.agreed_by, ["writer-web"]);
    assert_eq!(
        store.set_contract("writer-api", draft("X-RateLimit: u32"))?,
        2,
        "an identical draft is not a new revision"
    );
    Ok(())
}

#[test]
fn every_agent_facing_read_is_masked() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let mut store = SharedSpaceStore::open_for_space(tmp.path(), "rl")?;
    store.set_contract(
        "writer-api",
        ContractDraft {
            title: "billing".into(),
            summary: "test key sk_live_abc123".into(),
            spec: "Authorization: Bearer sk_live_abc123".into(),
        },
    )?;
    store.post_message(
        "writer-api",
        None,
        MessageKind::Finding,
        "use sk_live_abc123",
    )?;
    store.add_finding(
        "probe",
        FindingSeverity::Error,
        Some("api"),
        None,
        "sk_live_abc123 leaked in logs",
    )?;
    store.add_note("writer-web", "remember sk_live_abc123")?;

    let r = secrets();
    let contract = store.contract(&r).ok_or("no contract")?;
    let messages = store.list_messages(None, None, None, None, &r);
    let findings = store.findings(false, &r);
    let notes = store.notes(&r);
    let shown = format!("{contract:?} {messages:?} {findings:?} {notes:?}");
    assert!(!shown.contains("sk_live_abc123"), "leaked: {shown}");
    assert_eq!(shown.matches("[REDACTED:STRIPE_KEY]").count(), 5);
    Ok(())
}

#[test]
fn channel_posts_filter_and_stay_bounded_on_disk() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let mut store = SharedSpaceStore::open_for_space(tmp.path(), "rl")?;
    let q = store.post_message(
        "writer-web",
        Some("writer-api"),
        MessageKind::Question,
        "u32?",
    )?;
    store.post_message("writer-api", None, MessageKind::Status, "on it")?;
    let none = Redactor::default();
    let to_web = store.list_messages(Some("writer-web"), None, None, None, &none);
    assert_eq!(to_web.len(), 1, "only the broadcast reaches writer-web");
    let questions = store.list_messages(None, Some(MessageKind::Question), None, None, &none);
    assert_eq!(questions[0].id, q);
    assert_eq!(questions[0].to.as_deref(), Some("writer-api"));

    for i in 0..MAX_MESSAGES {
        store.post_message("tests", None, MessageKind::Status, &format!("run {i}"))?;
    }
    let reopened = SharedSpaceStore::open_for_space(tmp.path(), "rl")?;
    let all = reopened.list_messages(None, None, None, None, &none);
    assert_eq!(all.len(), MAX_MESSAGES);
    assert_eq!(all[0].body, "run 0");

    let mut reopened = reopened;
    let next = reopened.post_message("tests", None, MessageKind::Status, "after")?;
    assert_eq!(
        next,
        (MAX_MESSAGES + 3) as u64,
        "ids keep counting after reopening"
    );
    Ok(())
}

#[test]
fn findings_and_notes_persist_and_resolve() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let mut store = SharedSpaceStore::open_for_space(tmp.path(), "rl")?;
    let a = store.add_finding(
        "tests",
        FindingSeverity::Error,
        Some("api"),
        Some("src/limit.rs"),
        "limit test fails",
    )?;
    let b = store.add_finding("reviewer", FindingSeverity::Warning, None, None, "naming")?;
    store.add_note("writer-api", "limiter lives in middleware")?;
    store.resolve_finding(a)?;
    assert!(matches!(
        store.resolve_finding(42),
        Err(ChannelError::FindingNotFound(42))
    ));

    let reopened = SharedSpaceStore::open_for_space(tmp.path(), "rl")?;
    let none = Redactor::default();
    let open: Vec<u64> = reopened
        .findings(true, &none)
        .iter()
        .map(|f| f.id)
        .collect();
    assert_eq!(open, [b]);
    assert_eq!(reopened.findings(false, &none).len(), 2);
    assert_eq!(reopened.notes(&none)[0].body, "limiter lives in middleware");
    Ok(())
}

#[test]
fn no_op_changes_do_not_rewrite_the_file() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let mut store = SharedSpaceStore::open_for_space(tmp.path(), "rl")?;
    store.set_contract("writer-api", draft("x"))?;
    let id = store.add_finding("tests", FindingSeverity::Info, None, None, "f")?;
    store.resolve_finding(id)?;
    // Replace the file with a marker: a no-op that wrote would put real JSON back.
    fs::write(store.path(), b"marker")?;
    store.agree_contract("writer-api")?;
    store.set_contract("writer-api", draft("x"))?;
    store.resolve_finding(id)?;
    assert_eq!(fs::read(store.path())?, b"marker");
    Ok(())
}

#[test]
fn a_failed_write_keeps_nothing() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let mut store = SharedSpaceStore::open_for_space(tmp.path(), "rl")?;
    let dir = store.path().parent().ok_or("no parent")?.to_path_buf();
    fs::remove_dir_all(&dir)?;
    assert!(matches!(
        store.add_note("writer", "lost"),
        Err(ChannelError::Store(_))
    ));
    assert!(store.notes(&Redactor::default()).is_empty());
    fs::create_dir_all(&dir)?;
    assert_eq!(
        store.add_note("writer", "kept")?,
        1,
        "the failed note's id is not used up"
    );
    Ok(())
}

#[test]
fn rejected_changes_leave_the_store_unchanged() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let mut store = SharedSpaceStore::open_for_space(tmp.path(), "rl")?;
    assert!(matches!(
        store.post_message("writer", None, MessageKind::Status, "  "),
        Err(ChannelError::EmptyField("body"))
    ));
    assert!(matches!(
        store.set_contract("", draft("x")),
        Err(ChannelError::EmptyField("author"))
    ));
    assert!(!store.path().exists());
    Ok(())
}

#[test]
fn damaged_and_newer_files_are_refused_and_left_alone() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let path = tmp.path().join(SPACES_DIR).join("rl").join(SHARED_FILE);
    fs::create_dir_all(path.parent().ok_or("no parent")?)?;

    fs::write(&path, b"{ not json")?;
    assert!(matches!(
        SharedSpaceStore::open(&path),
        Err(ChannelError::Corrupt { .. })
    ));
    assert_eq!(fs::read(&path)?, b"{ not json");

    fs::write(&path, br#"{"version": 1, "channel": "wrong shape"}"#)?;
    assert!(matches!(
        SharedSpaceStore::open(&path),
        Err(ChannelError::Corrupt { .. })
    ));

    let newer = br#"{"version": 2, "something": "new"}"#;
    fs::write(&path, newer)?;
    match SharedSpaceStore::open_for_space(tmp.path(), "rl") {
        Err(ChannelError::UnsupportedVersion {
            found, supported, ..
        }) => assert_eq!((found, supported), (2, 1)),
        other => return Err(format!("expected UnsupportedVersion, got {other:?}").into()),
    }
    assert_eq!(fs::read(&path)?, newer);
    Ok(())
}

#[test]
fn version_zero_files_are_refused_and_left_alone() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let path = tmp.path().join(SPACES_DIR).join("rl").join(SHARED_FILE);
    fs::create_dir_all(path.parent().ok_or("no parent")?)?;
    let zero = br#"{"version": 0}"#;
    fs::write(&path, zero)?;
    assert!(matches!(
        SharedSpaceStore::open(&path),
        Err(ChannelError::UnsupportedVersion { found: 0, .. })
    ));
    assert_eq!(fs::read(&path)?, zero);
    Ok(())
}

#[test]
fn agreeing_to_a_contract_version_refuses_stale_versions() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let mut store = SharedSpaceStore::open_for_space(tmp.path(), "rl")?;
    assert!(matches!(
        store.agree_contract_version("writer-web", Some(1)),
        Err(ChannelError::NoContract)
    ));
    store.set_contract("writer-api", draft("X-RateLimit: int"))?;
    assert_eq!(store.agree_contract_version("writer-web", Some(1))?, 1);

    assert_eq!(
        store.set_contract("writer-api", draft("X-RateLimit: u32"))?,
        2
    );
    let before = fs::read(store.path())?;
    assert!(matches!(
        store.agree_contract_version("writer-web", Some(1)),
        Err(ChannelError::StaleContract {
            current: 2,
            given: 1
        })
    ));
    assert_eq!(
        fs::read(store.path())?,
        before,
        "a stale agreement writes nothing"
    );
    let none = Redactor::default();
    let contract = store.contract(&none).ok_or("no contract")?;
    assert!(!contract.is_agreed_by("writer-web"));

    assert_eq!(store.agree_contract_version("writer-web", None)?, 2);
    assert_eq!(store.agree_contract_version("tests", Some(2))?, 2);
    let reopened = SharedSpaceStore::open_for_space(tmp.path(), "rl")?;
    let contract = reopened.contract(&none).ok_or("no contract")?;
    assert_eq!(contract.agreed_by, ["writer-api", "writer-web", "tests"]);
    Ok(())
}

#[test]
fn hand_edited_next_ids_cannot_repeat_ids() -> TestResult {
    let tmp = tempfile::tempdir()?;
    let mut store = SharedSpaceStore::open_for_space(tmp.path(), "rl")?;
    store.post_message("a", None, MessageKind::Status, "one")?;
    store.post_message("a", None, MessageKind::Status, "two")?;
    store.add_note("a", "n")?;
    store.add_finding("a", FindingSeverity::Info, None, None, "f")?;
    let text = fs::read_to_string(store.path())?
        .replace("\"next_id\": 3", "\"next_id\": 1")
        .replace("\"next_note_id\": 2", "\"next_note_id\": 1")
        .replace("\"next_finding_id\": 2", "\"next_finding_id\": 1");
    fs::write(store.path(), text)?;
    let mut reopened = SharedSpaceStore::open_for_space(tmp.path(), "rl")?;
    assert_eq!(
        reopened.post_message("a", None, MessageKind::Status, "three")?,
        3
    );
    assert_eq!(reopened.add_note("a", "n2")?, 2);
    assert_eq!(
        reopened.add_finding("a", FindingSeverity::Info, None, None, "f2")?,
        2
    );
    Ok(())
}
