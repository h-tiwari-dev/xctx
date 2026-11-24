//! The shared space on disk: `<data dir>/spaces/<slug>/shared.json`, written atomically on every change.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use xctx_core::Redactor;
use xctx_core::store::write_atomic;

use crate::channel::{Channel, ChannelMessage, MessageKind};
use crate::contract::{Contract, ContractDraft};
use crate::error::{ChannelError, Result, validate_slug};
use crate::notes::{Finding, FindingSeverity, Note, SharedNotes};

/// The directory under the data directory that holds one directory per space.
pub const SPACES_DIR: &str = "spaces";

/// The shared space's file in its space's directory.
pub const SHARED_FILE: &str = "shared.json";

const SHARED_VERSION: u32 = 1;

/// Everything `shared.json` holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SharedSpace {
    version: u32,
    #[serde(default)]
    contract: Option<Contract>,
    #[serde(default)]
    channel: Channel,
    #[serde(default)]
    notes: SharedNotes,
}

impl Default for SharedSpace {
    fn default() -> Self {
        SharedSpace {
            version: SHARED_VERSION,
            contract: None,
            channel: Channel::default(),
            notes: SharedNotes::default(),
        }
    }
}

fn system_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// One space's contract, channel, findings and notes, loaded from and saved to `shared.json`. Every change is on
/// disk before the method returns; a change that cannot be written is not kept, and a change that changes nothing
/// is not written.
///
/// Reads meant for agents or the UI take a [`Redactor`] and return masked copies.
#[derive(Debug)]
pub struct SharedSpaceStore {
    path: PathBuf,
    data: SharedSpace,
    clock: fn() -> u64,
}

impl SharedSpaceStore {
    /// Open the shared space kept in the file `path`, creating its directory if needed. A missing file is an empty
    /// shared space (written on the first change); one that cannot be parsed, comes from a newer xctx or has
    /// version 0 is an error, and the file is left as it is.
    pub fn open(path: impl Into<PathBuf>) -> Result<SharedSpaceStore> {
        let path = path.into();
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            fs::create_dir_all(dir).map_err(|source| xctx_core::Error::Io {
                path: dir.to_path_buf(),
                source,
            })?;
        }
        let data = match fs::read(&path) {
            Ok(bytes) => parse(&path, &bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => SharedSpace::default(),
            Err(source) => return Err(xctx_core::Error::Io { path, source }.into()),
        };
        Ok(SharedSpaceStore {
            path,
            data,
            clock: system_now,
        })
    }

    /// Open the shared space of the space `slug`: `<data_dir>/spaces/<slug>/shared.json`. A slug that could
    /// escape that directory is refused.
    pub fn open_for_space(data_dir: impl AsRef<Path>, slug: &str) -> Result<SharedSpaceStore> {
        validate_slug(slug)?;
        SharedSpaceStore::open(
            data_dir
                .as_ref()
                .join(SPACES_DIR)
                .join(slug)
                .join(SHARED_FILE),
        )
    }

    /// The file this shared space is kept in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Use `clock` (seconds since the Unix epoch) for timestamps instead of the system clock, for tests.
    pub fn set_clock(&mut self, clock: fn() -> u64) {
        self.clock = clock;
    }

    /// Apply `change` to a copy, write the copy, and only then keep it.
    fn commit<R>(&mut self, change: impl FnOnce(&mut SharedSpace, u64) -> Result<R>) -> Result<R> {
        let now = (self.clock)();
        let mut data = self.data.clone();
        let result = change(&mut data, now)?;
        if data == self.data {
            return Ok(result);
        }
        let bytes = serde_json::to_vec_pretty(&data).map_err(|source| xctx_core::Error::Json {
            path: self.path.clone(),
            source,
        })?;
        write_atomic(&self.path, &bytes)?;
        self.data = data;
        Ok(result)
    }

    /// The current contract, masked by `redactor`, or `None` before anyone set one.
    pub fn contract(&self, redactor: &Redactor) -> Option<Contract> {
        self.data.contract.as_ref().map(|c| c.redacted(redactor))
    }

    /// Write a new revision of the contract by `author` and return its version. The version goes up by one and
    /// only `author` has agreed to it. A draft identical to the current contract changes nothing and returns the
    /// current version.
    pub fn set_contract(&mut self, author: &str, draft: ContractDraft) -> Result<u32> {
        self.commit(|data, now| {
            match Contract::revise(data.contract.as_ref(), author, draft, now)? {
                Some(next) => {
                    let version = next.version;
                    data.contract = Some(next);
                    Ok(version)
                }
                None => Ok(data.contract.as_ref().map_or(0, |c| c.version)),
            }
        })
    }

    /// Record that `agent` agrees to the current contract and return the version agreed to. Agreeing twice changes
    /// nothing. Fails with [`ChannelError::NoContract`] when there is no contract. Same as
    /// [`SharedSpaceStore::agree_contract_version`] without a version check.
    pub fn agree_contract(&mut self, agent: &str) -> Result<u32> {
        self.agree_contract_version(agent, None)
    }

    /// Record that `agent` agrees to the current contract, but only if it is `expected_version` (when given), and
    /// return the version agreed to. Fails with [`ChannelError::StaleContract`] when the contract was revised
    /// after the agent read `expected_version`, and with [`ChannelError::NoContract`] when there is no contract.
    /// Agreeing twice changes nothing.
    pub fn agree_contract_version(
        &mut self,
        agent: &str,
        expected_version: Option<u32>,
    ) -> Result<u32> {
        self.commit(|data, now| {
            let contract = data.contract.as_mut().ok_or(ChannelError::NoContract)?;
            contract.agree_version(agent, expected_version, now)?;
            Ok(contract.version)
        })
    }

    /// Post a message to the channel and return its id; `to: None` broadcasts to the space. See [`Channel::post`].
    pub fn post_message(
        &mut self,
        from: &str,
        to: Option<&str>,
        kind: MessageKind,
        body: &str,
    ) -> Result<u64> {
        self.commit(|data, now| data.channel.post(from, to, kind, body, now))
    }

    /// Channel messages, filtered and masked as [`Channel::list`] describes.
    pub fn list_messages(
        &self,
        to: Option<&str>,
        kind: Option<MessageKind>,
        since_id: Option<u64>,
        limit: Option<usize>,
        redactor: &Redactor,
    ) -> Vec<ChannelMessage> {
        self.data.channel.list(to, kind, since_id, limit, redactor)
    }

    /// Record an unresolved finding and return its id. See [`SharedNotes::add_finding`].
    pub fn add_finding(
        &mut self,
        author: &str,
        severity: FindingSeverity,
        repo: Option<&str>,
        file: Option<&str>,
        summary: &str,
    ) -> Result<u64> {
        self.commit(|data, now| {
            data.notes
                .add_finding(author, severity, repo, file, summary, now)
        })
    }

    /// Mark a finding resolved; resolving it again changes nothing. Fails with [`ChannelError::FindingNotFound`]
    /// for an unknown id.
    pub fn resolve_finding(&mut self, id: u64) -> Result<()> {
        self.commit(|data, _| data.notes.resolve_finding(id).map(|_| ()))
    }

    /// Record a note and return its id.
    pub fn add_note(&mut self, author: &str, body: &str) -> Result<u64> {
        self.commit(|data, now| data.notes.add_note(author, body, now))
    }

    /// The findings, oldest first and masked; only unresolved ones when `unresolved_only`.
    pub fn findings(&self, unresolved_only: bool, redactor: &Redactor) -> Vec<Finding> {
        self.data.notes.findings(unresolved_only, redactor)
    }

    /// The notes, oldest first and masked.
    pub fn notes(&self, redactor: &Redactor) -> Vec<Note> {
        self.data.notes.notes(redactor)
    }
}

/// Parse `shared.json`, refusing damaged files, files from a newer xctx and files with version 0.
fn parse(path: &Path, bytes: &[u8]) -> Result<SharedSpace> {
    // Check the version on its own first, so a newer file with a changed shape reports its version rather than a
    // parse error.
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    let corrupt = |source| ChannelError::Corrupt {
        path: path.to_path_buf(),
        source,
    };
    let Versioned { version } = serde_json::from_slice(bytes).map_err(corrupt)?;
    if version == 0 || version > SHARED_VERSION {
        return Err(ChannelError::UnsupportedVersion {
            path: path.to_path_buf(),
            found: version,
            supported: SHARED_VERSION,
        });
    }
    let mut data: SharedSpace = serde_json::from_slice(bytes).map_err(corrupt)?;
    data.channel.heal_next_id();
    data.notes.heal_next_ids();
    Ok(data)
}
