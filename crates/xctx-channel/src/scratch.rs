//! A space's scratch directory for intermediate artifacts (generated payloads, probe output, logs) that agents
//! share but that do not belong in any repo: `/tmp/xctx-scratch/<slug>/`, removed on Ship or Discard.
//!
//! Artifact paths come from agents, so every path is checked to be a plain relative path that stays inside the
//! directory, and no artifact is read or written through a symbolic link.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use xctx_core::store::write_atomic;

use crate::error::{ChannelError, Result, validate_slug};

/// Where scratch directories live unless a test or caller picks another base.
pub const DEFAULT_SCRATCH_BASE: &str = "/tmp/xctx-scratch";

/// One space's scratch directory. Nothing is created until the first artifact is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScratchDir {
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

impl ScratchDir {
    /// The scratch directory of the space `slug` under [`DEFAULT_SCRATCH_BASE`].
    pub fn for_space(slug: &str) -> Result<ScratchDir> {
        ScratchDir::in_base(DEFAULT_SCRATCH_BASE, slug)
    }

    /// The scratch directory of the space `slug` under `base`, for tests and callers that keep scratch elsewhere.
    /// A slug that could escape `base` is refused.
    pub fn in_base(base: impl AsRef<Path>, slug: &str) -> Result<ScratchDir> {
        validate_slug(slug)?;
        Ok(ScratchDir {
            root: base.as_ref().join(slug),
        })
    }

    /// The directory itself: what panes get as `XCTX_SCRATCH_DIR`.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `rel` resolved inside the directory, refusing paths that leave it and paths through symbolic links.
    fn resolve(&self, rel: &str) -> Result<PathBuf> {
        let clean = validate_artifact_path(rel)?;
        let mut path = self.root.clone();
        if is_symlink(&path)? {
            return Err(invalid(rel, "the scratch directory is a symbolic link"));
        }
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
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| io_error(parent, e))?;
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
        if is_symlink(&self.root)? {
            return Err(invalid(
                &self.root.display().to_string(),
                "the scratch directory is a symbolic link",
            ));
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
    /// remove. A symbolic link in its place is removed without touching what it points to.
    pub fn clean(&self) -> Result<bool> {
        if is_symlink(&self.root)? {
            fs::remove_file(&self.root).map_err(|e| io_error(&self.root, e))?;
            return Ok(true);
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
