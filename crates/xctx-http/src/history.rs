//! A space's HTTP history and its file: `<data dir>/spaces/<slug>/http.json`, next to the space's `graph.json`
//! and `attempts.json`, written atomically. A missing file is an empty history; a damaged one (or one from a
//! newer xctx) is an error, is left alone, and is never overwritten by [`save`]: move it aside with
//! [`quarantine`] to start afresh.
//!
//! The file never holds credentials: [`save`] masks sensitive headers and URL credentials
//! ([`Exchange::masked_for_disk`]). The raw values live only in the in-memory [`HttpHistory`], so a request
//! loaded from disk that needed them cannot be replayed ([`crate::replay`] says which parts are missing).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use xctx_core::SpaceId;
use xctx_core::store::write_atomic;

use crate::error::{HttpError, Result};
use crate::exchange::Exchange;

/// The directory under the data directory with one directory per space (shared with `xctx-graph`).
pub const SPACES_DIR: &str = "spaces";

/// The history file in a space's directory.
pub const HTTP_FILE: &str = "http.json";

/// The newest version this build reads, and the one it writes.
pub const HTTP_VERSION: u32 = 1;

/// The most exchanges a history keeps; recording more drops the oldest.
pub const MAX_HISTORY: usize = 200;

/// The exchanges of one space, oldest first, at most [`MAX_HISTORY`] of them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpHistory {
    /// The space they belong to.
    pub space: SpaceId,
    /// The exchanges, oldest first.
    pub exchanges: Vec<Exchange>,
    /// The id the next recorded exchange gets.
    pub next_id: u64,
}

impl HttpHistory {
    /// An empty history for `space`.
    pub fn new(space: SpaceId) -> HttpHistory {
        HttpHistory {
            space,
            exchanges: Vec::new(),
            next_id: 1,
        }
    }

    /// Add `exchange` with the next id, dropping the oldest exchanges beyond [`MAX_HISTORY`]. Returns its id.
    pub fn record(&mut self, mut exchange: Exchange) -> u64 {
        let id = self.next_id.max(1);
        self.next_id = id.saturating_add(1);
        exchange.id = id;
        self.exchanges.push(exchange);
        if self.exchanges.len() > MAX_HISTORY {
            let excess = self.exchanges.len() - MAX_HISTORY;
            self.exchanges.drain(..excess);
        }
        id
    }

    /// [`HttpHistory::record`] each of `exchanges` in order. Returns their ids.
    pub fn record_all(&mut self, exchanges: impl IntoIterator<Item = Exchange>) -> Vec<u64> {
        exchanges.into_iter().map(|e| self.record(e)).collect()
    }

    /// The exchange with id `id`, if it is still kept.
    pub fn get(&self, id: u64) -> Option<&Exchange> {
        self.exchanges.iter().find(|e| e.id == id)
    }

    /// The most recent exchange.
    pub fn latest(&self) -> Option<&Exchange> {
        self.exchanges.last()
    }
}

#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    #[serde(flatten)]
    history: HttpHistory,
}

/// Where space `slug` keeps its HTTP history. Fails with [`HttpError::InvalidSlug`] unless `slug` is non-empty
/// ASCII letters, digits, `-` or `_`, so it cannot leave the spaces directory.
pub fn http_path(data_dir: &Path, slug: &str) -> Result<PathBuf> {
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err(HttpError::InvalidSlug(slug.to_owned()));
    }
    Ok(data_dir.join(SPACES_DIR).join(slug).join(HTTP_FILE))
}

/// The HTTP history of `space` in `data_dir`: empty when it has never sent anything. Fails on a damaged file
/// (leaving it in place), a file from a newer xctx, or one naming another space. Keeps only the newest
/// [`MAX_HISTORY`] exchanges, and raises `next_id` above every stored id so ids are never reused.
pub fn load(data_dir: &Path, space: &SpaceId) -> Result<HttpHistory> {
    let path = http_path(data_dir, space.as_str())?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(HttpHistory::new(space.clone()));
        }
        Err(source) => return Err(xctx_core::Error::Io { path, source }.into()),
    };
    let value: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|source| xctx_core::Error::Json {
            path: path.clone(),
            source,
        })?;
    let version = value
        .get("version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    if version > u64::from(HTTP_VERSION) {
        return Err(HttpError::UnsupportedVersion {
            path,
            found: u32::try_from(version).unwrap_or(u32::MAX),
            supported: HTTP_VERSION,
        });
    }
    let doc: Document = serde_json::from_value(value).map_err(|source| xctx_core::Error::Json {
        path: path.clone(),
        source,
    })?;
    let mut history = doc.history;
    if history.space != *space {
        return Err(HttpError::WrongSpace {
            path,
            found: history.space.as_str().to_owned(),
            expected: space.as_str().to_owned(),
        });
    }
    if history.exchanges.len() > MAX_HISTORY {
        let excess = history.exchanges.len() - MAX_HISTORY;
        history.exchanges.drain(..excess);
    }
    let max = history.exchanges.iter().map(|e| e.id).max().unwrap_or(0);
    history.next_id = history.next_id.max(max.saturating_add(1)).max(1);
    Ok(history)
}

/// The name a damaged history file gets from [`quarantine`]: `http.json.bad`.
pub const QUARANTINE_SUFFIX: &str = ".bad";

impl HttpHistory {
    /// A copy to write to disk: every exchange as [`Exchange::masked_for_disk`] makes it.
    pub fn masked_for_disk(&self) -> HttpHistory {
        HttpHistory {
            space: self.space.clone(),
            exchanges: self
                .exchanges
                .iter()
                .map(Exchange::masked_for_disk)
                .collect(),
            next_id: self.next_id,
        }
    }
}

/// Save `history` to its space's file in `data_dir`, creating the space's directory if needed. Sensitive
/// headers and URL credentials are masked in the file ([`HttpHistory::masked_for_disk`]); `history` itself
/// keeps the raw values.
///
/// Refuses, with the error [`load`] would give, to replace a file that is damaged, from a newer xctx, or of
/// another space: call [`quarantine`] on it first. Saves are not locked: the caller must not run two saves of
/// the same space at once (the app saves from one place, holding the space's history lock).
pub fn save(data_dir: &Path, history: &HttpHistory) -> Result<()> {
    let path = http_path(data_dir, history.space.as_str())?;
    check_replaceable(&path, &history.space)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| xctx_core::Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let doc = Document {
        version: HTTP_VERSION,
        history: history.masked_for_disk(),
    };
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|source| xctx_core::Error::Json {
        path: path.clone(),
        source,
    })?;
    write_atomic(&path, &bytes)?;
    Ok(())
}

/// Just enough of a history file to tell whether it may be replaced, without keeping its exchanges.
#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    version: u64,
    space: SpaceId,
    #[allow(dead_code)]
    exchanges: serde::de::IgnoredAny,
}

/// Fails when the file at `path` exists and is not a history of `space` this build reads.
fn check_replaceable(path: &Path, space: &SpaceId) -> Result<()> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(xctx_core::Error::Io {
                path: path.to_path_buf(),
                source,
            }
            .into());
        }
    };
    let probe: Probe = serde_json::from_slice(&bytes).map_err(|source| {
        // A newer xctx may have changed the shape; say so rather than calling it damaged.
        match serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|v| v.get("version").and_then(serde_json::Value::as_u64))
        {
            Some(found) if found > u64::from(HTTP_VERSION) => HttpError::UnsupportedVersion {
                path: path.to_path_buf(),
                found: u32::try_from(found).unwrap_or(u32::MAX),
                supported: HTTP_VERSION,
            },
            _ => xctx_core::Error::Json {
                path: path.to_path_buf(),
                source,
            }
            .into(),
        }
    })?;
    if probe.version > u64::from(HTTP_VERSION) {
        return Err(HttpError::UnsupportedVersion {
            path: path.to_path_buf(),
            found: u32::try_from(probe.version).unwrap_or(u32::MAX),
            supported: HTTP_VERSION,
        });
    }
    if probe.space != *space {
        return Err(HttpError::WrongSpace {
            path: path.to_path_buf(),
            found: probe.space.as_str().to_owned(),
            expected: space.as_str().to_owned(),
        });
    }
    Ok(())
}

/// Move the history file at `path` (one [`load`] or [`save`] refused) aside to `http.json.bad`, or
/// `http.json.bad.2`, `.bad.3`, ... when that is taken, so nothing is lost and the next [`save`] starts a new
/// file. Returns where it went. Fails when `path` does not exist or cannot be renamed.
pub fn quarantine(path: &Path) -> Result<PathBuf> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| HTTP_FILE.to_owned());
    let mut n = 1u32;
    let target = loop {
        let candidate = if n == 1 {
            path.with_file_name(format!("{name}{QUARANTINE_SUFFIX}"))
        } else {
            path.with_file_name(format!("{name}{QUARANTINE_SUFFIX}.{n}"))
        };
        if !candidate.exists() || n == u32::MAX {
            break candidate;
        }
        n += 1;
    };
    fs::rename(path, &target).map_err(|source| xctx_core::Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(target)
}
