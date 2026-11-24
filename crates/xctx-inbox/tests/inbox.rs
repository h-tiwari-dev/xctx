//! The Inbox store in temporary data directories.

use std::fs;
use std::path::{Path, PathBuf};

use xctx_core::{DraftRepo, EnvReport, RequiredVar, SpaceDraft, SpaceId, SpaceShape};
use xctx_inbox::{
    Filter, INBOX_FILE, Inbox, InboxError, ItemBody, ItemId, ItemKind, ItemState, Level,
    MAX_PENDING_AGENT_NOTICES, MAX_SETTLED_ITEMS, MissingEnvVar, Notice, PlanGate, Source,
    SpaceWiden, welcome_notice,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn plan(repos: &[&str]) -> ItemBody {
    let mut draft = SpaceDraft::new("add rate limiting to the api");
    for r in repos {
        draft.add_repo(DraftRepo::new(
            format!("/code/{r}"),
            format!("name matches {r}"),
        ));
    }
    ItemBody::PlanGate(PlanGate {
        draft,
        rationale: Some("the limiter lives in the api".into()),
    })
}

fn notice(title: &str) -> ItemBody {
    ItemBody::Notice(Notice {
        title: title.into(),
        body: None,
        level: Level::Info,
    })
}

fn missing(dir: &str, name: &str) -> ItemBody {
    ItemBody::MissingEnvVar(MissingEnvVar {
        repo: "api".into(),
        dir: PathBuf::from(dir),
        mise_env: Some("rate-limit".into()),
        var: RequiredVar {
            name: name.into(),
            help: Some("from 1Password".into()),
            required_in: None,
        },
    })
}

fn report(names: &[&str]) -> EnvReport {
    EnvReport {
        missing: names
            .iter()
            .map(|n| RequiredVar {
                name: (*n).into(),
                help: None,
                required_in: None,
            })
            .collect(),
        secrets: Default::default(),
    }
}

#[test]
fn a_new_inbox_is_fresh_and_empty_and_survives_a_reopen() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    assert!(inbox.is_fresh());
    assert!(inbox.items().is_empty());
    assert!(
        !dir.path().join(INBOX_FILE).exists(),
        "opening writes nothing"
    );

    let id = inbox.post(ItemBody::Notice(welcome_notice()), None, Source::App)?;
    assert!(!inbox.is_fresh());
    let reopened = Inbox::open(dir.path())?;
    assert!(!reopened.is_fresh());
    assert_eq!(reopened.items(), inbox.items());
    assert_eq!(reopened.get(&id).map(|i| i.kind()), Some(ItemKind::Notice));
    Ok(())
}

#[test]
fn ids_are_never_reused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    let a = inbox.post(notice("a"), None, Source::Agent)?;
    let b = inbox.post(notice("b"), None, Source::Agent)?;
    assert_ne!(a, b);
    let mut reopened = Inbox::open(dir.path())?;
    let c = reopened.post(notice("c"), None, Source::Agent)?;
    assert!(c != a && c != b);
    Ok(())
}

#[test]
fn a_plan_gate_is_edited_then_approved_once() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    let id = inbox.post(plan(&["api"]), None, Source::ParentAgent)?;
    inbox.edit_plan(&id, |draft| {
        draft.add_repo(DraftRepo::new("/code/web", "added by the user"));
        draft.shape = SpaceShape::Review;
    })?;
    let approved = inbox.approve(&id)?;
    assert_eq!(approved.state, ItemState::Approved);
    let ItemBody::PlanGate(p) = &approved.body else {
        return Err("not a plan".into());
    };
    assert_eq!(p.draft.repos.len(), 2);
    assert_eq!(p.draft.shape, SpaceShape::Review);

    assert!(matches!(
        inbox.approve(&id),
        Err(InboxError::NotPending {
            state: ItemState::Approved,
            ..
        })
    ));
    assert!(matches!(
        inbox.edit_plan(&id, |_| {}),
        Err(InboxError::NotPending { .. })
    ));
    assert_eq!(Inbox::open(dir.path())?.get(&id), Some(&approved));
    Ok(())
}

#[test]
fn an_empty_plan_cannot_be_approved_but_can_be_rejected() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    let id = inbox.post(plan(&[]), None, Source::ParentAgent)?;
    assert!(matches!(inbox.approve(&id), Err(InboxError::EmptyPlan(_))));
    assert_eq!(inbox.get(&id).map(|i| i.state), Some(ItemState::Pending));
    let rejected = inbox.reject(&id, Some("wrong repo".into()))?;
    assert_eq!(rejected.state, ItemState::Rejected);
    assert!(rejected.detail().contains("Note: wrong repo"));
    Ok(())
}

#[test]
fn each_kind_allows_only_its_own_decisions() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    let gate = inbox.post(plan(&["api"]), None, Source::ParentAgent)?;
    let widen = inbox.post(
        ItemBody::SpaceWiden(SpaceWiden {
            space: SpaceId::new("rate-limit"),
            add_repos: vec![],
            production: true,
            reason: "repro needs prod data".into(),
        }),
        Some(SpaceId::new("rate-limit")),
        Source::Agent,
    )?;
    let note = inbox.post(notice("tests pass"), None, Source::Agent)?;
    let var = inbox.post(missing("/code/api", "KEY"), None, Source::App)?;

    let wrong = |r: Result<_, InboxError>| matches!(r, Err(InboxError::WrongKind { .. }));
    assert!(
        wrong(inbox.dismiss(&gate)),
        "gates are decided, not dismissed"
    );
    assert!(wrong(inbox.resolve(&widen)));
    assert!(wrong(inbox.approve(&note)));
    assert!(wrong(inbox.approve(&var)));
    assert!(matches!(
        inbox.edit_plan(&widen, |_| {}),
        Err(InboxError::WrongKind { .. })
    ));

    assert_eq!(inbox.approve(&widen)?.state, ItemState::Approved);
    assert_eq!(inbox.dismiss(&note)?.state, ItemState::Dismissed);
    assert_eq!(inbox.resolve(&var)?.state, ItemState::Resolved);
    assert!(matches!(
        inbox.approve(&ItemId::new("inbox-999")),
        Err(InboxError::NotFound(_))
    ));
    Ok(())
}

#[test]
fn list_is_newest_first_and_filters_by_space_and_state() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    let space = SpaceId::new("rate-limit");
    let a = inbox.post(notice("a"), Some(space.clone()), Source::Agent)?;
    let b = inbox.post(notice("b"), None, Source::Agent)?;
    let c = inbox.post(notice("c"), Some(space.clone()), Source::Agent)?;
    inbox.dismiss(&c)?;

    let ids = |items: Vec<&xctx_inbox::InboxItem>| {
        items.into_iter().map(|i| i.id.clone()).collect::<Vec<_>>()
    };
    assert_eq!(ids(inbox.list(&Filter::default())), [b.clone(), a.clone()]);
    assert_eq!(
        ids(inbox.list(&Filter {
            space: Some(space),
            include_settled: true
        })),
        [c, a]
    );
    assert_eq!(inbox.pending_count(), 2);
    Ok(())
}

#[test]
fn a_pending_missing_var_is_posted_once() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    let first = inbox.post(missing("/code/api", "KEY"), None, Source::App)?;
    let again = inbox.post(missing("/code/api", "KEY"), None, Source::App)?;
    let other = inbox.post(missing("/code/web", "KEY"), None, Source::App)?;
    assert_eq!(first, again);
    assert_ne!(first, other);
    inbox.resolve(&first)?;
    let after = inbox.post(missing("/code/api", "KEY"), None, Source::App)?;
    assert_ne!(after, first, "once resolved, a new report is a new item");
    Ok(())
}

#[test]
fn sync_missing_posts_new_vars_and_resolves_fixed_ones() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    let api = Path::new("/code/api");
    let space = Some(SpaceId::new("rate-limit"));
    let first = inbox.sync_missing(
        "api",
        api,
        Some("rate-limit"),
        space.clone(),
        &report(&["A", "B"]),
    )?;
    assert_eq!(first.len(), 2);
    // Another directory's items are not touched.
    let web = inbox.post(missing("/code/web", "A"), None, Source::App)?;

    let second = inbox.sync_missing(
        "api",
        api,
        Some("rate-limit"),
        space.clone(),
        &report(&["B", "C"]),
    )?;
    assert_eq!(second[0], first[1], "B is still the same item");
    let state = |inbox: &Inbox, id: &ItemId| inbox.get(id).map(|i| i.state);
    assert_eq!(state(&inbox, &first[0]), Some(ItemState::Resolved));
    assert_eq!(state(&inbox, &second[1]), Some(ItemState::Pending));
    assert_eq!(state(&inbox, &web), Some(ItemState::Pending));

    // A different MISE_ENV is a different environment.
    let prod = inbox.sync_missing("api", api, Some("production"), space, &report(&[]))?;
    assert!(prod.is_empty());
    assert_eq!(state(&inbox, &second[0]), Some(ItemState::Pending));
    Ok(())
}

#[test]
fn a_dismissed_missing_var_is_not_posted_again_by_sync() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    let api = Path::new("/code/api");
    let space = Some(SpaceId::new("rate-limit"));
    let first = inbox.sync_missing(
        "api",
        api,
        Some("rate-limit"),
        space.clone(),
        &report(&["A", "B"]),
    )?;
    inbox.dismiss(&first[0])?;
    let count = inbox.items().len();

    let again = inbox.sync_missing(
        "api",
        api,
        Some("rate-limit"),
        space.clone(),
        &report(&["A", "B"]),
    )?;
    assert_eq!(
        again,
        [first[1].clone()],
        "A was dismissed, so it has no id"
    );
    assert_eq!(inbox.items().len(), count, "nothing new was posted");
    assert_eq!(inbox.pending_count(), 1);
    assert_eq!(
        inbox.get(&first[0]).map(|i| i.state),
        Some(ItemState::Dismissed)
    );

    // The dismissal is for that directory and MISE_ENV only.
    let prod = inbox.sync_missing("api", api, Some("production"), space, &report(&["A"]))?;
    assert_eq!(prod.len(), 1);
    assert_ne!(prod[0], first[0]);
    Ok(())
}

#[test]
fn sync_missing_writes_nothing_when_nothing_changed() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    let api = Path::new("/code/api");
    let space = Some(SpaceId::new("rate-limit"));
    let first = inbox.sync_missing(
        "api",
        api,
        Some("rate-limit"),
        space.clone(),
        &report(&["A"]),
    )?;
    let before = fs::read(dir.path().join(INBOX_FILE))?;

    // A read-only directory makes any write fail, so these succeed only if they write nothing.
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555))?;
    let same = inbox.sync_missing(
        "api",
        api,
        Some("rate-limit"),
        space.clone(),
        &report(&["A"]),
    );
    let other_dir = inbox.sync_missing("web", Path::new("/code/web"), None, None, &report(&[]));
    let changed = inbox.sync_missing("api", api, Some("rate-limit"), space, &report(&["A", "B"]));
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755))?;

    assert_eq!(same?, first);
    assert!(other_dir?.is_empty());
    assert!(changed.is_err(), "a real change still writes");
    assert_eq!(fs::read(dir.path().join(INBOX_FILE))?, before);
    Ok(())
}

#[test]
fn pending_agent_notices_are_capped_by_dismissing_the_oldest() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    inbox.set_clock(|| 7);
    let app_notice = inbox.post(notice("from the app"), None, Source::App)?;
    let gate = inbox.post(plan(&["api"]), None, Source::Agent)?;
    let mut agent_ids = Vec::new();
    for n in 0..MAX_PENDING_AGENT_NOTICES + 5 {
        agent_ids.push(inbox.post(notice(&format!("n{n}")), None, Source::Agent)?);
    }

    let pending_agent_notices = |inbox: &Inbox| {
        inbox
            .items()
            .iter()
            .filter(|i| i.is_pending() && i.source == Source::Agent && i.kind() == ItemKind::Notice)
            .count()
    };
    assert_eq!(pending_agent_notices(&inbox), MAX_PENDING_AGENT_NOTICES);
    for id in &agent_ids[..5] {
        let item = inbox.get(id).ok_or("oldest notice was dropped")?;
        assert_eq!((item.state, item.updated_at), (ItemState::Dismissed, 7));
    }
    for id in &agent_ids[5..] {
        assert_eq!(inbox.get(id).map(|i| i.state), Some(ItemState::Pending));
    }
    let state = |id: &ItemId| inbox.get(id).map(|i| i.state);
    assert_eq!(state(&app_notice), Some(ItemState::Pending));
    assert_eq!(state(&gate), Some(ItemState::Pending));
    assert_eq!(inbox.pending_count(), MAX_PENDING_AGENT_NOTICES + 2);
    assert_eq!(
        pending_agent_notices(&Inbox::open(dir.path())?),
        MAX_PENDING_AGENT_NOTICES
    );
    Ok(())
}

#[test]
fn a_hand_edited_next_id_cannot_repeat_an_id() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    let a = inbox.post(notice("a"), None, Source::User)?;
    let b = inbox.post(notice("b"), None, Source::User)?;
    let path = dir.path().join(INBOX_FILE);
    let mut file: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    file["next_id"] = 1.into();
    fs::write(&path, serde_json::to_vec(&file)?)?;

    let mut reopened = Inbox::open(dir.path())?;
    let c = reopened.post(notice("c"), None, Source::User)?;
    assert!(c != a && c != b, "{c} repeats an id");
    assert_eq!(c.as_str(), "inbox-3");
    Ok(())
}

#[test]
fn a_widening_that_changes_nothing_cannot_be_approved() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    let space = SpaceId::new("rate-limit");
    let id = inbox.post(
        ItemBody::SpaceWiden(SpaceWiden {
            space: space.clone(),
            add_repos: vec![],
            production: false,
            reason: "no reason".into(),
        }),
        Some(space),
        Source::Agent,
    )?;
    assert!(matches!(inbox.approve(&id), Err(InboxError::EmptyWiden(_))));
    assert_eq!(inbox.get(&id).map(|i| i.state), Some(ItemState::Pending));
    assert_eq!(inbox.reject(&id, None)?.state, ItemState::Rejected);
    Ok(())
}

#[test]
fn old_decided_items_are_pruned_but_pending_ones_kept() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    let keep = inbox.post(plan(&["api"]), None, Source::ParentAgent)?;
    let mut first_settled = None;
    for n in 0..MAX_SETTLED_ITEMS + 5 {
        let id = inbox.post(notice(&format!("n{n}")), None, Source::Agent)?;
        inbox.dismiss(&id)?;
        first_settled.get_or_insert(id);
    }
    let settled = inbox.items().iter().filter(|i| !i.is_pending()).count();
    assert_eq!(settled, MAX_SETTLED_ITEMS);
    assert!(inbox.get(&keep).is_some());
    assert!(first_settled.is_some_and(|id| inbox.get(&id).is_none()));
    Ok(())
}

#[test]
fn timestamps_come_from_the_clock() -> TestResult {
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    inbox.set_clock(|| 1_000);
    let id = inbox.post(notice("a"), None, Source::User)?;
    inbox.set_clock(|| 2_000);
    let item = inbox.dismiss(&id)?;
    assert_eq!((item.created_at, item.updated_at), (1_000, 2_000));
    Ok(())
}

#[test]
fn a_damaged_or_newer_file_is_an_error_and_left_alone() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join(INBOX_FILE);
    fs::write(&path, b"{not json")?;
    assert!(matches!(Inbox::open(dir.path()), Err(InboxError::Store(_))));
    assert_eq!(fs::read(&path)?, b"{not json");
    fs::write(&path, br#"{"version": 99, "next_id": 1, "items": []}"#)?;
    assert!(matches!(
        Inbox::open(dir.path()),
        Err(InboxError::UnsupportedVersion { found: 99, .. })
    ));
    Ok(())
}

#[test]
fn a_change_that_cannot_be_written_is_not_kept() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555))?;
    let result = inbox.post(notice("a"), None, Source::Agent);
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755))?;
    assert!(result.is_err());
    assert!(inbox.items().is_empty());
    assert!(inbox.is_fresh());
    Ok(())
}

#[test]
fn an_attempt_proposal_is_a_gate_with_a_bounded_count() -> TestResult {
    use xctx_inbox::AttemptProposal;
    let dir = tempfile::tempdir()?;
    let mut inbox = Inbox::open(dir.path())?;
    let proposal = |count| {
        ItemBody::AttemptProposal(AttemptProposal {
            space: SpaceId::new("rate-limit"),
            agent: "writer-api".into(),
            count,
            instructions: vec!["token bucket".into()],
            rationale: Some("two designs look equally good".into()),
        })
    };
    let good = inbox.post(proposal(2), Some(SpaceId::new("rate-limit")), Source::Agent)?;
    let item = inbox.get(&good).ok_or("posted")?;
    assert_eq!(item.kind(), ItemKind::AttemptProposal);
    assert!(item.kind().is_gate());
    assert_eq!(item.title(), "Fork 2 attempts of writer-api in rate-limit");
    assert!(item.detail().contains("- attempt 1: token bucket"));
    assert!(matches!(
        inbox.dismiss(&good),
        Err(InboxError::WrongKind { .. })
    ));
    assert_eq!(inbox.approve(&good)?.state, ItemState::Approved);

    for count in [0, 5] {
        let bad = inbox.post(proposal(count), None, Source::Agent)?;
        assert!(matches!(
            inbox.approve(&bad),
            Err(InboxError::BadAttemptCount { .. })
        ));
        assert_eq!(inbox.reject(&bad, None)?.state, ItemState::Rejected);
    }
    Ok(())
}
