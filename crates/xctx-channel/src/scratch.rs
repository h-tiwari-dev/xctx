//! A space's scratch directory for intermediate artifacts (generated payloads, probe output, logs) that agents
//! share but that do not belong in any repo: `/tmp/xctx-scratch/<slug>/`, removed on Ship or Discard.
//!
//! Artifact paths come from agents, so every path is checked to be a plain relative path that stays inside the
//! directory, and no artifact is read or written through a symbolic link.
//!
//! The base (`/tmp/xctx-scratch`) sits in a directory every local user can write to, so before anything is read,
//! written, listed or removed, the base and the space's directory are checked to be real directories owned by
//! this user, and they are created readable only by this user.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Component, Path, PathBuf};

use xctx_core::store::write_atomic;

use crate::error::{ChannelError, Result, validate_slug};

/// Where scratch directories live unless a test or caller picks another base.
pub const DEFAULT_SCRATCH_BASE: &str = "/tmp/xctx-scratch";

/// The mode of every directory a scratch directory creates: only this user can list, read or add files.
const PRIVATE_DIR_MODE: u32 = 0o700;

/// One space's scratch directory. Nothing is created until the first artifact is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScratchDir {
    base: PathBuf,
    root: PathBuf,
}

fn io_error(path: &Path, source: io::Error) -> ChannelError {
    xctx_core::Error::Io {
        path: path.to_path_buf(),
        source,
    }
    .into()
}

fn invalid(path: &str, reason: &'static str) -> ChannelError {
    ChannelError::InvalidArtifactPath {
        path: path.to_owned(),
        reason,
    }
}

fn unsafe_dir(path: &Path, reason: &'static str) -> ChannelError {
    ChannelError::UnsafeScratchDir {
        path: path.to_path_buf(),
        reason,
    }
}

/// `rel` as a relative path of plain names, or why it is refused. `.` components are dropped.
fn validate_artifact_path(rel: &str) -> Result<PathBuf> {
    if rel.is_empty() {
        return Err(invalid(rel, "it is empty"));
    }
    if rel.contains('\0') {
        return Err(invalid(rel, "it contains a NUL byte"));
    }
    let mut clean = PathBuf::new();
    for component in Path::new(rel).components() {
        match component {
            Component::Normal(name) => clean.push(name),
            Component::CurDir => {}
            Component::ParentDir => return Err(invalid(rel, "it contains `..`")),
            Component::RootDir | Component::Prefix(_) => {
                return Err(invalid(rel, "it is absolute"));
            }
        }
    }
    if clean.as_os_str().is_empty() {
        return Err(invalid(rel, "it names no file"));
    }
    Ok(clean)
}

/// True when `path` exists and is a symbolic link.
fn is_symlink(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_symlink()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_error(path, e)),
    }
}

fn current_uid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Whether `path` exists, refusing it unless it is a directory (not a symbolic link to one) owned by this user.
fn check_private_dir(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => {
            let kind = meta.file_type();
            if kind.is_symlink() {
                Err(unsafe_dir(path, "it is a symbolic link"))
            } else if !kind.is_dir() {
                Err(unsafe_dir(path, "it is not a directory"))
            } else if meta.uid() != current_uid() {
                Err(unsafe_dir(path, "it is owned by another user"))
            } else {
                Ok(true)
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(io_error(path, e)),
    }
}

/// Create `path` (and, when `recursive`, its missing ancestors) readable only by this user, unless it already is
/// a directory this user owns. Whatever ends up at `path` is checked again, so one planted meanwhile is refused.
fn ensure_private_dir(path: &Path, recursive: bool) -> Result<()> {
    if check_private_dir(path)? {
        return Ok(());
    }
    match fs::DirBuilder::new()
        .recursive(recursive)
        .mode(PRIVATE_DIR_MODE)
        .create(path)
    {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(io_error(path, e)),
    }
    if check_private_dir(path)? {
        Ok(())
    } else {
        Err(io_error(
            path,
            io::Error::new(
                io::ErrorKind::NotFound,
                "the directory vanished after creation",
            ),
        ))
    }
}

impl ScratchDir {
    /// The scratch directory of the space `slug` under [`DEFAULT_SCRATCH_BASE`].
    pub fn for_space(slug: &str) -> Result<ScratchDir> {
        ScratchDir::in_base(DEFAULT_SCRATCH_BASE, slug)
    }

    /// The scratch directory of the space `slug` under `base`, for tests and callers that keep scratch elsewhere.
    /// A slug that could escape `base` is refused. `base` itself must be (or will be created as) a directory owned
    /// by this user and not a symbolic link; its ancestors are not checked.
    pub fn in_base(base: impl AsRef<Path>, slug: &str) -> Result<ScratchDir> {
        validate_slug(slug)?;
        let base = base.as_ref().to_path_buf();
        Ok(ScratchDir {
            root: base.join(slug),
            base,
        })
    }

    /// The directory itself: what panes get as `XCTX_SCRATCH_DIR`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether the directory exists, refusing it when it or its base is not a directory owned by this user.
    fn check_dirs(&self) -> Result<bool> {
        let base_exists = check_private_dir(&self.base)?;
        let root_exists = check_private_dir(&self.root)?;
        Ok(base_exists && root_exists)
    }

    /// `rel` resolved inside the directory, refusing paths that leave it and paths through symbolic links.
    fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let clean = validate_artifact_path(rel)?;
        self.check_dirs()?;
        let mut path = self.root.clone();
        for component in clean.components() {
            path.push(component);
            if is_symlink(&path)? {
                return Err(invalid(rel, "it goes through a symbolic link"));
            }
        }
        Ok(path)
    }

    /// Write `bytes` to the artifact at `rel` (e.g. `api/payload.json`), creating directories as needed and
    /// replacing any artifact already there atomically. Returns the artifact's full path.
    pub fn write_artifact(&self, rel: &str, bytes: &[u8]) -> Result<PathBuf> {
        let path = self.resolve(rel)?;
        ensure_private_dir(&self.base, true)?;
        ensure_private_dir(&self.root, false)?;
        if let Some(parent) = path.parent() {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(PRIVATE_DIR_MODE)
                .create(parent)
                .map_err(|e| io_error(parent, e))?;
        }
        write_atomic(&path, bytes)?;
        Ok(path)
    }

    /// The contents of the artifact at `rel`.
    pub fn read_artifact(&self, rel: &str) -> Result<Vec<u8>> {
        let path = self.resolve(rel)?;
        fs::read(&path).map_err(|e| io_error(&path, e))
    }

    /// Every artifact (regular file) in the directory, as paths relative to it, sorted. Symbolic links are not
    /// followed or listed. Empty when the directory does not exist yet.
    pub fn list_artifacts(&self) -> Result<Vec<PathBuf>> {
        if !self.check_dirs()? {
            return Ok(Vec::new());
        }
        let mut found = Vec::new();
        let mut pending = vec![PathBuf::new()];
        while let Some(rel_dir) = pending.pop() {
            let dir = self.root.join(&rel_dir);
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == io::ErrorKind::NotFound && rel_dir.as_os_str().is_empty() => {
                    return Ok(Vec::new());
                }
                Err(e) => return Err(io_error(&dir, e)),
            };
            for entry in entries {
                let entry = entry.map_err(|e| io_error(&dir, e))?;
                let kind = entry.file_type().map_err(|e| io_error(&entry.path(), e))?;
                let rel = rel_dir.join(entry.file_name());
                if kind.is_dir() {
                    pending.push(rel);
                } else if kind.is_file() {
                    found.push(rel);
                }
            }
        }
        found.sort();
        Ok(found)
    }

    /// Remove the directory and everything in it, on Ship or Discard. Returns whether there was anything to
    /// remove. A symbolic link in its place is removed without touching what it points to. An unsafe base (a
    /// symbolic link, not a directory, or another user's) is refused and nothing is removed.
    pub fn clean(&self) -> Result<bool> {
        if !check_private_dir(&self.base)? {
            return Ok(false);
        }
        if is_symlink(&self.root)? {
            fs::remove_file(&self.root).map_err(|e| io_error(&self.root, e))?;
            return Ok(true);
        }
        if !check_private_dir(&self.root)? {
            return Ok(false);
        }
        match fs::remove_dir_all(&self.root) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(io_error(&self.root, e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_relative_paths_are_accepted_and_normalised() -> Result<()> {
        assert_eq!(
            validate_artifact_path("api/./payload.json")?,
            PathBuf::from("api/payload.json")
        );
        assert_eq!(validate_artifact_path("a/")?, PathBuf::from("a"));
        Ok(())
    }

    #[test]
    fn escaping_absolute_and_nul_paths_are_refused() {
        for bad in [
            "",
            ".",
            "./",
            "..",
            "../x",
            "a/../../x",
            "a/..",
            "/etc/passwd",
            "//x",
            "a\0b",
        ] {
            assert!(
                matches!(
                    validate_artifact_path(bad),
                    Err(ChannelError::InvalidArtifactPath { .. })
                ),
                "{bad:?} should be refused"
            );
        }
    }

    #[test]
    fn the_default_base_is_tmp_xctx_scratch() -> Result<()> {
        let dir = ScratchDir::for_space("rate-limit")?;
        assert_eq!(dir.root(), Path::new("/tmp/xctx-scratch/rate-limit"));
        assert!(matches!(
            ScratchDir::for_space("../etc"),
            Err(ChannelError::InvalidSlug(_))
        ));
        Ok(())
    }
}
