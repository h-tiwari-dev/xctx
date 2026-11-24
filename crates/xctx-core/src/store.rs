//! The store on disk: spaces (with their ports), the indexed root folders, and one opaque layout document per
//! space, kept under the data directory. Every write goes to a temporary file that is renamed into place, so a
//! crash never leaves a half-written file.
//!
//! Layout on disk:
//! - `spaces.json`: the [`Store`]'s spaces and roots.
//! - `layouts/<space id>.json`: whatever the app saved with [`Store::save_layout`].

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, io_error};
use crate::space::{self, Space, SpaceId, SpaceStatus};

/// The environment variable that moves the data directory, for tests and for running a second copy of the app.
pub const DATA_DIR_ENV: &str = "XCTX_DATA_DIR";

const SPACES_FILE: &str = "spaces.json";
const LAYOUTS_DIR: &str = "layouts";
const STORE_VERSION: u32 = 1;

/// The data directory: `$XCTX_DATA_DIR` when set and non-empty, else `~/Library/Application Support/xctx`.
pub fn data_dir() -> Result<PathBuf> {
    resolve_data_dir(std::env::var_os(DATA_DIR_ENV), std::env::var_os("HOME"))
}

/// [`data_dir`] from explicit values of `XCTX_DATA_DIR` and `HOME`, so it can be decided without reading the
/// process environment.
pub fn resolve_data_dir(
    xctx_data_dir: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf> {
    if let Some(dir) = xctx_data_dir.filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = home.filter(|h| !h.is_empty()).ok_or(Error::NoDataDir)?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("Application Support")
        .join("xctx"))
}

/// Write `bytes` to `path` through a temporary file in the same directory and a rename, so readers see either the
/// old file or the new one. The parent directory must exist.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(io_error(parent))?;
    file.write_all(bytes).map_err(io_error(file.path()))?;
    file.as_file().sync_all().map_err(io_error(file.path()))?;
    file.persist(path).map_err(|e| io_error(path)(e.error))?;
    Ok(())
}

/// What `spaces.json` holds. Fields it does not know are ignored, and a file without `version` (as the Swift
/// prototype wrote, with the same `roots` and `spaces`) reads as version 1.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct StoreData {
    #[serde(default = "first_version")]
    version: u32,
    #[serde(default)]
    roots: Vec<PathBuf>,
    #[serde(default)]
    spaces: Vec<Space>,
}

fn first_version() -> u32 {
    1
}

/// The spaces and indexed roots, loaded from and saved to a data directory. Every change is written to disk before
/// the method returns.
#[derive(Debug)]
pub struct Store {
    dir: PathBuf,
    data: StoreData,
}

impl Store {
    /// Open the store in `dir`, creating the directory if needed. A missing `spaces.json` is an empty store; one
    /// that cannot be parsed is an error, so a damaged file is never silently replaced.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Store> {
        let dir = dir.into();
        fs::create_dir_all(&dir).map_err(io_error(&dir))?;
        let path = dir.join(SPACES_FILE);
        let data = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|source| Error::Json {
                path: path.clone(),
                source,
            })?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => StoreData {
                version: STORE_VERSION,
                ..StoreData::default()
            },
            Err(e) => return Err(io_error(path)(e)),
        };
        Ok(Store { dir, data })
    }

    /// Open the store in [`data_dir`].
    pub fn open_default() -> Result<Store> {
        Store::open(data_dir()?)
    }

    /// The data directory this store lives in.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Write `data` to `spaces.json`.
    fn write(&self, data: &StoreData) -> Result<()> {
        let path = self.dir.join(SPACES_FILE);
        let bytes = serde_json::to_vec_pretty(data).map_err(|source| Error::Json {
            path: path.clone(),
            source,
        })?;
        write_atomic(&path, &bytes)
    }

    /// Apply `change` to a copy of the data and keep it only once it is on disk, so memory never holds what the
    /// file does not.
    fn commit<R>(&mut self, change: impl FnOnce(&mut StoreData) -> R) -> Result<R> {
        let mut data = self.data.clone();
        let result = change(&mut data);
        self.write(&data)?;
        self.data = data;
        Ok(result)
    }

    /// Every space, in the order they were created, including shipped and discarded ones.
    pub fn spaces(&self) -> &[Space] {
        &self.data.spaces
    }

    /// The spaces that are still active: the living spaces Home shows.
    pub fn active_spaces(&self) -> impl Iterator<Item = &Space> {
        self.data
            .spaces
            .iter()
            .filter(|s| s.status == SpaceStatus::Active)
    }

    /// The space with this id.
    pub fn space(&self, id: &SpaceId) -> Option<&Space> {
        self.data.spaces.iter().find(|s| &s.id == id)
    }

    /// Save `space`, replacing the stored space with the same id or adding it at the end.
    pub fn upsert_space(&mut self, space: Space) -> Result<()> {
        self.commit(
            |data| match data.spaces.iter_mut().find(|s| s.id == space.id) {
                Some(existing) => *existing = space,
                None => data.spaces.push(space),
            },
        )
    }

    /// Forget a space and its saved layout. Its worktrees and branches are not touched. Returns the space, or
    /// `None` when there was none with this id.
    pub fn remove_space(&mut self, id: &SpaceId) -> Result<Option<Space>> {
        let Some(index) = self.data.spaces.iter().position(|s| &s.id == id) else {
            return Ok(None);
        };
        let space = self.commit(|data| data.spaces.remove(index))?;
        self.remove_layout(id)?;
        Ok(Some(space))
    }

    /// The folders the repo index searches.
    pub fn roots(&self) -> &[PathBuf] {
        &self.data.roots
    }

    /// Add a folder to index. Returns false when it was already there.
    pub fn add_root(&mut self, root: impl Into<PathBuf>) -> Result<bool> {
        let root = root.into();
        if self.data.roots.contains(&root) {
            return Ok(false);
        }
        self.commit(|data| data.roots.push(root))?;
        Ok(true)
    }

    /// Stop indexing a folder. Returns false when it was not there.
    pub fn remove_root(&mut self, root: &Path) -> Result<bool> {
        if !self.data.roots.iter().any(|r| r == root) {
            return Ok(false);
        }
        self.commit(|data| data.roots.retain(|r| r != root))?;
        Ok(true)
    }

    /// A short readable slug for `task`, unique among every space in the store (shipped and discarded ones too,
    /// since their `space/<slug>` branches are kept).
    pub fn unique_slug(&self, task: &str) -> String {
        space::unique_slug(task, |slug| {
            self.data.spaces.iter().any(|s| s.id.as_str() == slug)
        })
    }

    /// Which active space holds each port.
    pub fn port_allocations(&self) -> BTreeMap<u16, SpaceId> {
        self.active_spaces()
            .map(|s| (s.port, s.id.clone()))
            .collect()
    }

    /// The lowest port in [`space::PORT_RANGE`] no active space holds. Shipped and discarded spaces give their port
    /// back.
    pub fn allocate_port(&self) -> Result<u16> {
        space::allocate_port(self.active_spaces().map(|s| s.port))
    }

    fn layout_path(&self, id: &SpaceId) -> PathBuf {
        let name: String = id
            .as_str()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.dir.join(LAYOUTS_DIR).join(format!("{name}.json"))
    }

    /// Save the app's layout document for a space: the pane tree, agent sessions, anything that must survive a
    /// restart. The store does not look inside it.
    pub fn save_layout(&self, id: &SpaceId, layout: &serde_json::Value) -> Result<()> {
        let path = self.layout_path(id);
        let dir = self.dir.join(LAYOUTS_DIR);
        fs::create_dir_all(&dir).map_err(io_error(&dir))?;
        let bytes = serde_json::to_vec_pretty(layout).map_err(|source| Error::Json {
            path: path.clone(),
            source,
        })?;
        write_atomic(&path, &bytes)
    }

    /// The layout document last saved for a space, or `None` when there is none.
    pub fn load_layout(&self, id: &SpaceId) -> Result<Option<serde_json::Value>> {
        let path = self.layout_path(id);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|source| Error::Json { path, source }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(io_error(path)(e)),
        }
    }

    /// Delete a space's layout document, if there is one.
    pub fn remove_layout(&self, id: &SpaceId) -> Result<()> {
        let path = self.layout_path(id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io_error(path)(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_prefers_xctx_data_dir_then_home() {
        assert_eq!(
            resolve_data_dir(Some("/tmp/x".into()), Some("/Users/me".into())).ok(),
            Some(PathBuf::from("/tmp/x"))
        );
        assert_eq!(
            resolve_data_dir(Some("".into()), Some("/Users/me".into())).ok(),
            Some(PathBuf::from("/Users/me/Library/Application Support/xctx"))
        );
        assert!(matches!(
            resolve_data_dir(None, None),
            Err(Error::NoDataDir)
        ));
    }

    #[test]
    fn write_atomic_replaces_the_file_and_leaves_no_temp_files() -> Result<()> {
        let dir = tempfile::tempdir().map_err(io_error("tempdir"))?;
        let path = dir.path().join("a.json");
        write_atomic(&path, b"one")?;
        write_atomic(&path, b"two")?;
        assert_eq!(fs::read(&path).map_err(io_error(&path))?, b"two");
        let entries = fs::read_dir(dir.path())
            .map_err(io_error(dir.path()))?
            .count();
        assert_eq!(entries, 1);
        Ok(())
    }

    #[test]
    fn a_change_that_cannot_be_written_is_not_kept() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().map_err(io_error("tempdir"))?;
        let mut store = Store::open(dir.path())?;
        let read_only = fs::Permissions::from_mode(0o555);
        fs::set_permissions(dir.path(), read_only).map_err(io_error(dir.path()))?;
        let result = store.add_root("/code");
        let writable = fs::Permissions::from_mode(0o755);
        fs::set_permissions(dir.path(), writable).map_err(io_error(dir.path()))?;
        assert!(matches!(result, Err(Error::Io { .. })), "{result:?}");
        assert!(store.roots().is_empty());
        Ok(())
    }

    #[test]
    fn a_prototype_spaces_file_without_a_version_is_read() -> Result<()> {
        let dir = tempfile::tempdir().map_err(io_error("tempdir"))?;
        let legacy = br#"{
  "inbox" : [

  ],
  "roots" : [
    "\/Users\/me\/Documents"
  ],
  "spaces" : [

  ]
}"#;
        fs::write(dir.path().join(SPACES_FILE), legacy).map_err(io_error(dir.path()))?;
        let store = Store::open(dir.path())?;
        assert_eq!(store.roots(), [PathBuf::from("/Users/me/Documents")]);
        assert!(store.spaces().is_empty());
        assert_eq!(
            fs::read(dir.path().join(SPACES_FILE)).map_err(io_error(dir.path()))?,
            legacy,
            "opening does not rewrite the file"
        );
        Ok(())
    }

    #[test]
    fn a_damaged_spaces_file_is_an_error_not_an_empty_store() -> Result<()> {
        let dir = tempfile::tempdir().map_err(io_error("tempdir"))?;
        fs::write(dir.path().join(SPACES_FILE), b"{not json").map_err(io_error(dir.path()))?;
        assert!(matches!(Store::open(dir.path()), Err(Error::Json { .. })));
        Ok(())
    }
}
