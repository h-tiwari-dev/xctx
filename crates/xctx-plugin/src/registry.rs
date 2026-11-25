//! The registry of loaded plugins: built-in, the user's (`~/.config/xctx/plugins/<name>/plugin.toml`) and a
//! repository's own (`<repo>/.xctx/plugins/<name>/plugin.toml`).
//!
//! Names are unique across all sources: a user or repo plugin with the name of one already loaded is refused and
//! reported, never silently shadowing it. A bad manifest is reported and the others still load. A repo plugin is
//! only usable once the user trusts it (an Inbox approval the app records); until then it is listed by
//! [`Registry::needs_trust`].
//!
//! A user or repo manifest is read once: the bytes that were parsed are the bytes hashed
//! ([`Plugin::manifest_hash`]), so trust is decided on exactly what runs.

use std::path::{Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::builtin::{BUILTIN_MANIFESTS, builtin_origin};
use crate::error::{PluginError, Result};
use crate::manifest::{MANIFEST_FILE, Manifest};

/// The folder inside a repository that holds its plugins.
pub const REPO_PLUGINS_DIR: &str = ".xctx/plugins";

/// Where a plugin came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Source {
    /// Shipped with xctx.
    Builtin,
    /// From the user's plugins folder.
    User {
        /// The manifest file.
        path: PathBuf,
    },
    /// From a repository's `.xctx/plugins`.
    Repo {
        /// The repository's root.
        repo: PathBuf,
        /// The manifest file.
        path: PathBuf,
    },
}

impl std::fmt::Display for Source {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Source::Builtin => f.write_str("built-in"),
            Source::User { path } => write!(f, "{}", path.display()),
            Source::Repo { path, .. } => write!(f, "{}", path.display()),
        }
    }
}

/// A loaded plugin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Plugin {
    /// Its manifest.
    pub manifest: Manifest,
    /// Where it came from.
    pub source: Source,
    /// Whether it may be mounted and its tools called. Always true for built-in and user plugins; for a repo
    /// plugin, what the caller decided when loading it, or [`Registry::trust`] later.
    pub trusted: bool,
    /// The [`manifest_hash`] of the exact bytes of `plugin.toml` that were parsed into
    /// [`Plugin::manifest`], for a user or repo plugin; None for a built-in. Trust is recorded against it.
    pub manifest_hash: Option<String>,
}

impl Plugin {
    /// The plugin's name.
    pub fn name(&self) -> &str {
        &self.manifest.name
    }
}

/// A manifest that was not loaded, and why.
#[derive(Debug)]
pub struct LoadProblem {
    /// The manifest file or folder, absent for a built-in.
    pub path: Option<PathBuf>,
    /// What went wrong.
    pub error: PluginError,
}

/// The loaded plugins, and the problems met loading them.
#[derive(Debug, Default)]
pub struct Registry {
    plugins: Vec<Plugin>,
    problems: Vec<LoadProblem>,
}

impl Registry {
    /// A registry with no plugins at all.
    pub fn empty() -> Registry {
        Registry::default()
    }

    /// A registry with the built-in plugins.
    pub fn with_builtins() -> Registry {
        let mut registry = Registry::default();
        for (name, text) in BUILTIN_MANIFESTS {
            match Manifest::parse(text, &builtin_origin(name)) {
                Ok(manifest) => registry.insert(manifest, Source::Builtin, true, None),
                Err(error) => registry.problems.push(LoadProblem { path: None, error }),
            }
        }
        registry
    }

    /// Load the user's plugins from `dir` (each `<dir>/<name>/plugin.toml`). A missing `dir` loads nothing.
    pub fn add_user_dir(&mut self, dir: &Path) {
        for path in self.manifest_paths(dir) {
            match load_named(&path) {
                Ok((manifest, hash)) => {
                    self.insert(manifest, Source::User { path }, true, Some(hash))
                }
                Err(error) => self.problems.push(LoadProblem {
                    path: Some(path),
                    error,
                }),
            }
        }
    }

    /// Load `repo`'s own plugins from `<repo>/.xctx/plugins`. `trusted` is given each manifest and the
    /// [`manifest_hash`] of the bytes it was parsed from, and says whether the user already trusted that; the
    /// rest are listed by [`Registry::needs_trust`].
    pub fn add_repo(&mut self, repo: &Path, trusted: impl Fn(&Manifest, &str) -> bool) {
        for path in self.manifest_paths(&repo.join(REPO_PLUGINS_DIR)) {
            match load_named(&path) {
                Ok((manifest, hash)) => {
                    let is_trusted = trusted(&manifest, &hash);
                    self.insert(
                        manifest,
                        Source::Repo {
                            repo: repo.to_owned(),
                            path,
                        },
                        is_trusted,
                        Some(hash),
                    );
                }
                Err(error) => self.problems.push(LoadProblem {
                    path: Some(path),
                    error,
                }),
            }
        }
    }

    /// Every loaded plugin, built-in first, then in loading order.
    pub fn plugins(&self) -> &[Plugin] {
        &self.plugins
    }

    /// The plugin named `name`, trusted or not.
    pub fn get(&self, name: &str) -> Option<&Plugin> {
        self.plugins.iter().find(|p| p.name() == name)
    }

    /// The plugin named `name` if it may be used (mounted, its tools called).
    pub fn usable(&self, name: &str) -> Option<&Plugin> {
        self.get(name).filter(|p| p.trusted)
    }

    /// The repo plugins not yet trusted, which need an Inbox approval before they can be used.
    pub fn needs_trust(&self) -> Vec<&Plugin> {
        self.plugins.iter().filter(|p| !p.trusted).collect()
    }

    /// Mark the plugin named `name` trusted, after the user approved it. Returns false when there is none.
    pub fn trust(&mut self, name: &str) -> bool {
        match self.plugins.iter_mut().find(|p| p.manifest.name == name) {
            Some(plugin) => {
                plugin.trusted = true;
                true
            }
            None => false,
        }
    }

    /// The manifests that could not be loaded.
    pub fn problems(&self) -> &[LoadProblem] {
        &self.problems
    }

    fn insert(
        &mut self,
        manifest: Manifest,
        source: Source,
        trusted: bool,
        manifest_hash: Option<String>,
    ) {
        if let Some(existing) = self.get(&manifest.name) {
            let error = PluginError::Clash {
                name: manifest.name.clone(),
                existing: existing.source.to_string(),
                new: source.to_string(),
            };
            let path = match source {
                Source::Builtin => None,
                Source::User { path } | Source::Repo { path, .. } => Some(path),
            };
            self.problems.push(LoadProblem { path, error });
            return;
        }
        self.plugins.push(Plugin {
            manifest,
            source,
            trusted,
            manifest_hash,
        });
    }

    /// The `plugin.toml` of every plugin folder in `dir`, sorted by folder name. A folder without one is reported.
    fn manifest_paths(&mut self, dir: &Path) -> Vec<PathBuf> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(source) => {
                self.problems.push(LoadProblem {
                    path: Some(dir.to_owned()),
                    error: PluginError::Io {
                        path: dir.to_owned(),
                        source,
                    },
                });
                return Vec::new();
            }
        };
        let mut folders: Vec<PathBuf> = entries
            .filter_map(|entry| entry.ok())
            .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        folders.sort();
        let mut paths = Vec::with_capacity(folders.len());
        for folder in folders {
            let path = folder.join(MANIFEST_FILE);
            if path.is_file() {
                paths.push(path);
            } else {
                self.problems.push(LoadProblem {
                    error: PluginError::Io {
                        path: path.clone(),
                        source: std::io::Error::new(
                            std::io::ErrorKind::NotFound,
                            "the plugin folder has no plugin.toml",
                        ),
                    },
                    path: Some(folder),
                });
            }
        }
        paths
    }
}

/// The SHA-256 of a manifest's bytes, in lowercase hex: what trusting a repo plugin records.
pub fn manifest_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Read a manifest once, parse it, and hash the bytes that were parsed. Its `name` must match its folder's.
fn load_named(path: &Path) -> Result<(Manifest, String)> {
    let bytes = std::fs::read(path).map_err(|source| PluginError::Io {
        path: path.to_owned(),
        source,
    })?;
    let hash = manifest_hash(&bytes);
    let text = String::from_utf8(bytes).map_err(|e| PluginError::Syntax {
        origin: path.display().to_string(),
        message: format!("plugin.toml is not UTF-8: {e}"),
    })?;
    let manifest = Manifest::parse(&text, &path.display().to_string())?;
    let folder = path
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned());
    if folder.as_deref() != Some(manifest.name.as_str()) {
        return Err(PluginError::Invalid {
            origin: path.display().to_string(),
            field: "name".to_owned(),
            message: format!(
                "{:?} must match its folder's name {:?}",
                manifest.name,
                folder.unwrap_or_default()
            ),
        });
    }
    Ok((manifest, hash))
}
