//! A space's attempts on disk: `<data dir>/spaces/<slug>/attempts.json`, next to its `graph.json`, written
//! atomically. A missing file is a space with no attempts; a damaged one is an error and is left alone.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use xctx_core::SpaceId;
use xctx_core::store::write_atomic;

use crate::error::{AttemptError, Result};
use crate::set::AttemptSet;

/// The directory under the data directory with one directory per space (shared with `xctx-graph`).
pub const SPACES_DIR: &str = "spaces";

/// The attempts file in a space's directory.
pub const ATTEMPTS_FILE: &str = "attempts.json";

/// The newest version this build reads, and the one it writes.
pub const ATTEMPTS_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct Document {
    version: u32,
    #[serde(flatten)]
    set: AttemptSet,
}

/// Where space `slug` keeps its attempts. Fails with [`AttemptError::InvalidSlug`] unless `slug` is non-empty
/// ASCII letters, digits, `-` or `_`, so it cannot leave the spaces directory.
pub fn attempts_path(data_dir: &Path, slug: &str) -> Result<PathBuf> {
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err(AttemptError::InvalidSlug(slug.to_owned()));
    }
    Ok(data_dir.join(SPACES_DIR).join(slug).join(ATTEMPTS_FILE))
}

/// The attempts of `space` in `data_dir`: an empty set when it has never had any.
pub fn load(data_dir: &Path, space: &SpaceId) -> Result<AttemptSet> {
    let path = attempts_path(data_dir, space.as_str())?;
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(AttemptSet::new(space.clone())),
        Err(source) => return Err(xctx_core::Error::Io { path, source }.into()),
    };
    let doc: Document =
        serde_json::from_slice(&bytes).map_err(|source| xctx_core::Error::Json {
            path: path.clone(),
            source,
        })?;
    if doc.version > ATTEMPTS_VERSION {
        return Err(AttemptError::UnsupportedVersion {
            path,
            found: doc.version,
            supported: ATTEMPTS_VERSION,
        });
    }
    Ok(doc.set)
}

/// Save `set` to its space's file in `data_dir`, creating the space's directory if needed.
pub fn save(data_dir: &Path, set: &AttemptSet) -> Result<()> {
    let path = attempts_path(data_dir, set.space.as_str())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| xctx_core::Error::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let doc = Document {
        version: ATTEMPTS_VERSION,
        set: set.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&doc).map_err(|source| xctx_core::Error::Json {
        path: path.clone(),
        source,
    })?;
    write_atomic(&path, &bytes)?;
    Ok(())
}
