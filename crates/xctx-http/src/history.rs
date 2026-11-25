//! A space's HTTP history and its file: `<data dir>/spaces/<slug>/http.json`, next to the space's `graph.json`
//! and `attempts.json`, written atomically. A missing file is an empty history; a damaged one is an error and
//! is left alone.

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

/// Save `history` to its space's file in `data_dir`, creating the space's directory if needed.
pub fn save(data_dir: &Path, history: &HttpHistory) -> Result<()> {
    let path = http_path(data_dir, history.space.as_str())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| xctx_core::Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let doc = Document {
        version: HTTP_VERSION,
        history: history.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|source| xctx_core::Error::Json {
        path: path.clone(),
        source,
    })?;
    write_atomic(&path, &bytes)?;
    Ok(())
}
