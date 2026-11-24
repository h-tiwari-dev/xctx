//! The Inbox on disk: `inbox.json` in the data directory, written atomically on every change.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use xctx_core::store::{data_dir, write_atomic};
use xctx_core::{EnvReport, SpaceDraft, SpaceId};

use crate::error::{InboxError, Result};
use crate::item::{InboxItem, ItemBody, ItemId, ItemKind, ItemState, MissingEnvVar, Source};

/// The Inbox's file in the data directory.
pub const INBOX_FILE: &str = "inbox.json";

/// How many decided items are kept for history; the oldest beyond this are dropped. Pending items are never
/// dropped.
pub const MAX_SETTLED_ITEMS: usize = 200;

/// How many notices from agents ([`Source::Agent`]) can be pending at once. Posting one more dismisses the
/// oldest pending agent notice, so an agent posting in a loop cannot grow the Inbox or the badge count without
/// limit.
pub const MAX_PENDING_AGENT_NOTICES: usize = 100;

const INBOX_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct InboxData {
    version: u32,
    next_id: u64,
    #[serde(default)]
    items: Vec<InboxItem>,
}

impl Default for InboxData {
    fn default() -> Self {
        InboxData {
            version: INBOX_VERSION,
            next_id: 1,
            items: Vec::new(),
        }
    }
}

/// Which items [`Inbox::list`] returns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    /// Only items about this space.
    pub space: Option<SpaceId>,
    /// Decided items too, not only pending ones.
    pub include_settled: bool,
}

fn system_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The Inbox, loaded from and saved to a data directory. Every change is on disk before the method returns; a
/// change that cannot be written is not kept.
#[derive(Debug)]
pub struct Inbox {
    path: PathBuf,
    data: InboxData,
    fresh: bool,
    clock: fn() -> u64,
}

impl Inbox {
    /// Open the Inbox in `dir`, creating the directory if needed. A missing `inbox.json` is an empty Inbox (see
    /// [`Inbox::is_fresh`]); one that cannot be parsed is an error, so a damaged file is never replaced.
    pub fn open(dir: impl AsRef<Path>) -> Result<Inbox> {
        let dir = dir.as_ref();
        fs::create_dir_all(dir).map_err(|source| xctx_core::Error::Io {
            path: dir.to_path_buf(),
            source,
        })?;
        let path = dir.join(INBOX_FILE);
        let (data, fresh) = match fs::read(&path) {
            Ok(bytes) => {
                let data: InboxData =
                    serde_json::from_slice(&bytes).map_err(|source| xctx_core::Error::Json {
                        path: path.clone(),
                        source,
                    })?;
                if data.version > INBOX_VERSION {
                    return Err(InboxError::UnsupportedVersion {
                        path,
                        found: data.version,
                        supported: INBOX_VERSION,
                    });
                }
                let mut data = data;
                data.next_id = data.next_id.max(next_free_id(&data.items));
                (data, false)
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => (InboxData::default(), true),
            Err(source) => return Err(xctx_core::Error::Io { path, source }.into()),
        };
        Ok(Inbox {
            path,
            data,
            fresh,
            clock: system_now,
        })
    }

    /// Open the Inbox in the xctx data directory (`$XCTX_DATA_DIR`, else `~/Library/Application Support/xctx`).
    pub fn open_default() -> Result<Inbox> {
        Inbox::open(data_dir()?)
    }

    /// True when there was no `inbox.json` when this Inbox was opened: the first run, when the app posts
    /// [`crate::welcome_notice`].
    pub fn is_fresh(&self) -> bool {
        self.fresh
    }

    /// The file this Inbox is kept in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Use `clock` (seconds since the Unix epoch) for timestamps instead of the system clock, for tests.
    pub fn set_clock(&mut self, clock: fn() -> u64) {
        self.clock = clock;
    }

    fn commit<R>(&mut self, change: impl FnOnce(&mut InboxData) -> Result<R>) -> Result<R> {
        let now = (self.clock)();
        let mut data = self.data.clone();
        let result = change(&mut data)?;
        prune(&mut data, now);
        // Nothing changed (e.g. a repeated mise check): no need to rewrite and fsync the file.
        if data == self.data {
            return Ok(result);
        }
        let bytes = serde_json::to_vec_pretty(&data).map_err(|source| xctx_core::Error::Json {
            path: self.path.clone(),
            source,
        })?;
        write_atomic(&self.path, &bytes)?;
        self.data = data;
        self.fresh = false;
        Ok(result)
    }

    /// Every item, oldest first.
    pub fn items(&self) -> &[InboxItem] {
        &self.data.items
    }

    /// The item with this id.
    pub fn get(&self, id: &ItemId) -> Option<&InboxItem> {
        self.data.items.iter().find(|i| &i.id == id)
    }

    /// The items `filter` selects, newest first.
    pub fn list(&self, filter: &Filter) -> Vec<&InboxItem> {
        self.data
            .items
            .iter()
            .rev()
            .filter(|i| filter.include_settled || i.is_pending())
            .filter(|i| {
                filter
                    .space
                    .as_ref()
                    .is_none_or(|s| i.space.as_ref() == Some(s))
            })
            .collect()
    }

    /// How many items wait for the user: the badge count.
    pub fn pending_count(&self) -> usize {
        self.data.items.iter().filter(|i| i.is_pending()).count()
    }

    /// Post a new pending item and return its id. A missing variable already pending for the same directory,
    /// `MISE_ENV` and name is not posted twice; its id is returned instead. A notice from [`Source::Agent`] beyond
    /// [`MAX_PENDING_AGENT_NOTICES`] dismisses the oldest pending agent notice.
    pub fn post(
        &mut self,
        body: ItemBody,
        space: Option<SpaceId>,
        source: Source,
    ) -> Result<ItemId> {
        if let ItemBody::MissingEnvVar(m) = &body
            && let Some(existing) = self.pending_missing(m)
        {
            return Ok(existing.id.clone());
        }
        let now = (self.clock)();
        self.commit(|data| {
            let id = ItemId::new(format!("inbox-{}", data.next_id));
            data.next_id += 1;
            data.items.push(InboxItem {
                id: id.clone(),
                space,
                source,
                state: ItemState::Pending,
                created_at: now,
                updated_at: now,
                note: None,
                body,
            });
            Ok(id)
        })
    }

    fn pending_missing(&self, m: &MissingEnvVar) -> Option<&InboxItem> {
        self.data.items.iter().find(|i| {
            i.is_pending()
                && matches!(&i.body, ItemBody::MissingEnvVar(o)
                    if o.dir == m.dir && o.mise_env == m.mise_env && o.var.name == m.var.name)
        })
    }

    /// Move a pending item to `to`, checking that the kind allows it. `check` sees the item before the move.
    fn settle(
        &mut self,
        id: &ItemId,
        to: ItemState,
        action: &'static str,
        allowed: fn(ItemKind) -> bool,
        note: Option<String>,
        check: impl FnOnce(&InboxItem) -> Result<()>,
    ) -> Result<InboxItem> {
        let now = (self.clock)();
        self.commit(|data| {
            let item = data
                .items
                .iter_mut()
                .find(|i| &i.id == id)
                .ok_or_else(|| InboxError::NotFound(id.clone()))?;
            if !item.is_pending() {
                return Err(InboxError::NotPending {
                    id: id.clone(),
                    state: item.state,
                });
            }
            if !allowed(item.kind()) {
                return Err(InboxError::WrongKind {
                    id: id.clone(),
                    kind: item.kind(),
                    action,
                });
            }
            check(item)?;
            item.state = to;
            item.updated_at = now;
            if note.is_some() {
                item.note = note;
            }
            Ok(item.clone())
        })
    }

    /// Approve a pending Plan Gate or space widening and return it as approved; the app then launches or widens.
    /// A Plan Gate with no repos cannot be approved, nor a widening that adds no repos and does not switch to
    /// production.
    pub fn approve(&mut self, id: &ItemId) -> Result<InboxItem> {
        self.settle(
            id,
            ItemState::Approved,
            "approve",
            ItemKind::is_gate,
            None,
            |item| match &item.body {
                ItemBody::PlanGate(p) if p.draft.repos.is_empty() => {
                    Err(InboxError::EmptyPlan(item.id.clone()))
                }
                ItemBody::SpaceWiden(w) if w.add_repos.is_empty() && !w.production => {
                    Err(InboxError::EmptyWiden(item.id.clone()))
                }
                ItemBody::AttemptProposal(a)
                    if a.count == 0 || a.count > xctx_core::MAX_ATTEMPTS =>
                {
                    Err(InboxError::BadAttemptCount {
                        id: item.id.clone(),
                        count: a.count,
                    })
                }
                _ => Ok(()),
            },
        )
    }

    /// Turn down a pending Plan Gate or space widening, with an optional note on why.
    pub fn reject(&mut self, id: &ItemId, note: Option<String>) -> Result<InboxItem> {
        self.settle(
            id,
            ItemState::Rejected,
            "reject",
            ItemKind::is_gate,
            note,
            |_| Ok(()),
        )
    }

    /// Mark a missing variable as set now.
    pub fn resolve(&mut self, id: &ItemId) -> Result<InboxItem> {
        self.settle(
            id,
            ItemState::Resolved,
            "resolve",
            |k| k == ItemKind::MissingEnvVar,
            None,
            |_| Ok(()),
        )
    }

    /// Put away a notice or a missing variable without acting on it. Gates must be approved or rejected.
    pub fn dismiss(&mut self, id: &ItemId) -> Result<InboxItem> {
        self.settle(
            id,
            ItemState::Dismissed,
            "dismiss",
            |k| !k.is_gate(),
            None,
            |_| Ok(()),
        )
    }

    /// Change a pending Plan Gate's draft (add or drop repos, retitle, reshape) before approving it.
    pub fn edit_plan(&mut self, id: &ItemId, edit: impl FnOnce(&mut SpaceDraft)) -> Result<()> {
        let now = (self.clock)();
        self.commit(|data| {
            let item = data
                .items
                .iter_mut()
                .find(|i| &i.id == id)
                .ok_or_else(|| InboxError::NotFound(id.clone()))?;
            if !item.is_pending() {
                return Err(InboxError::NotPending {
                    id: id.clone(),
                    state: item.state,
                });
            }
            let kind = item.kind();
            let ItemBody::PlanGate(plan) = &mut item.body else {
                return Err(InboxError::WrongKind {
                    id: id.clone(),
                    kind,
                    action: "edit",
                });
            };
            edit(&mut plan.draft);
            item.updated_at = now;
            Ok(())
        })
    }

    /// Bring the missing-variable items for one directory in line with a fresh mise inspection: post the
    /// variables `report` says are missing, and resolve pending ones for this directory and `MISE_ENV` that are
    /// not missing any more. A variable the user dismissed for this directory and `MISE_ENV` is not posted again
    /// and has no id in the result. Returns the ids of the items now pending for it. Writes at most once, and not
    /// at all when nothing changed.
    pub fn sync_missing(
        &mut self,
        repo: &str,
        dir: &Path,
        mise_env: Option<&str>,
        space: Option<SpaceId>,
        report: &EnvReport,
    ) -> Result<Vec<ItemId>> {
        let now = (self.clock)();
        let mise_env = mise_env.map(str::to_owned);
        self.commit(|data| {
            for item in data.items.iter_mut().filter(|i| i.is_pending()) {
                if let ItemBody::MissingEnvVar(m) = &item.body
                    && m.dir == dir
                    && m.mise_env == mise_env
                    && !report.missing.iter().any(|v| v.name == m.var.name)
                {
                    item.state = ItemState::Resolved;
                    item.updated_at = now;
                }
            }
            let mut ids = Vec::new();
            for var in &report.missing {
                let same_var = |i: &&InboxItem| {
                    matches!(&i.body, ItemBody::MissingEnvVar(m)
                        if m.dir == dir && m.mise_env == mise_env && m.var.name == var.name)
                };
                if let Some(existing) = data.items.iter().filter(same_var).find(|i| i.is_pending())
                {
                    ids.push(existing.id.clone());
                    continue;
                }
                if data
                    .items
                    .iter()
                    .filter(same_var)
                    .any(|i| i.state == ItemState::Dismissed)
                {
                    continue;
                }
                let id = ItemId::new(format!("inbox-{}", data.next_id));
                data.next_id += 1;
                data.items.push(InboxItem {
                    id: id.clone(),
                    space: space.clone(),
                    source: Source::App,
                    state: ItemState::Pending,
                    created_at: now,
                    updated_at: now,
                    note: None,
                    body: ItemBody::MissingEnvVar(MissingEnvVar {
                        repo: repo.to_owned(),
                        dir: dir.to_path_buf(),
                        mise_env: mise_env.clone(),
                        var: var.clone(),
                    }),
                });
                ids.push(id);
            }
            Ok(ids)
        })
    }
}

/// The lowest id number above every `inbox-<n>` in `items`, so a hand-edited `next_id` cannot repeat an id.
fn next_free_id(items: &[InboxItem]) -> u64 {
    items
        .iter()
        .filter_map(|i| i.id.as_str().strip_prefix("inbox-")?.parse::<u64>().ok())
        .max()
        .map_or(1, |n| n.saturating_add(1))
}

fn is_pending_agent_notice(item: &InboxItem) -> bool {
    item.is_pending() && item.source == Source::Agent && item.kind() == ItemKind::Notice
}

/// Dismiss the oldest pending agent notices beyond [`MAX_PENDING_AGENT_NOTICES`], then drop the oldest decided
/// items beyond [`MAX_SETTLED_ITEMS`].
fn prune(data: &mut InboxData, now: u64) {
    let agent_notices = data
        .items
        .iter()
        .filter(|i| is_pending_agent_notice(i))
        .count();
    let mut excess = agent_notices.saturating_sub(MAX_PENDING_AGENT_NOTICES);
    for item in data.items.iter_mut() {
        if excess == 0 {
            break;
        }
        if is_pending_agent_notice(item) {
            item.state = ItemState::Dismissed;
            item.updated_at = now;
            excess -= 1;
        }
    }

    let settled = data.items.iter().filter(|i| !i.is_pending()).count();
    let mut excess = settled.saturating_sub(MAX_SETTLED_ITEMS);
    if excess == 0 {
        return;
    }
    data.items.retain(|i| {
        if excess > 0 && !i.is_pending() {
            excess -= 1;
            false
        } else {
            true
        }
    });
}
