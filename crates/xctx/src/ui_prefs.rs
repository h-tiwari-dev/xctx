//! The window's look-and-feel choices that outlive a run: whether the window is translucent and whether the
//! Roster is collapsed. Kept in `ui.json` in the data directory, apart from the spaces store, so a bad or missing
//! file only costs the defaults.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The file the preferences live in, inside the data directory.
const UI_PREFS_FILE: &str = "ui.json";

/// What the user chose about the window's look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct UiPrefs {
    /// The window lets the blurred desktop show through its chrome and panes.
    pub(crate) translucent: bool,
    /// The Roster is shown as a narrow rail of icons.
    pub(crate) roster_collapsed: bool,
    /// The Inspector is shown beside the space's panes.
    pub(crate) inspector_open: bool,
}

impl Default for UiPrefs {
    fn default() -> Self {
        Self {
            translucent: true,
            roster_collapsed: false,
            inspector_open: false,
        }
    }
}

/// Where the preferences are kept for data directory `dir`.
pub(crate) fn prefs_path(dir: &Path) -> PathBuf {
    dir.join(UI_PREFS_FILE)
}

impl UiPrefs {
    /// The preferences saved in data directory `dir`; the defaults when there is no file or it cannot be read.
    pub(crate) fn load(dir: Option<&Path>) -> Self {
        dir.map(prefs_path)
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Save these preferences to `path`. Run it off the main thread.
    pub(crate) fn save(self, path: &Path) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(&self)
            .map_err(|e| format!("could not encode the window preferences: {e}"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
        }
        xctx_core::store::write_atomic(path, &bytes)
            .map_err(|e| format!("could not save the window preferences: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_round_trip_and_fall_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(UiPrefs::load(Some(dir.path())), UiPrefs::default());
        let prefs = UiPrefs {
            translucent: false,
            roster_collapsed: true,
            inspector_open: true,
        };
        prefs.save(&prefs_path(dir.path())).unwrap();
        assert_eq!(UiPrefs::load(Some(dir.path())), prefs);

        std::fs::write(prefs_path(dir.path()), b"{ not json").unwrap();
        assert_eq!(UiPrefs::load(Some(dir.path())), UiPrefs::default());
        // A file from an older build without some keys keeps the defaults for them.
        std::fs::write(prefs_path(dir.path()), br#"{"roster_collapsed":true}"#).unwrap();
        assert_eq!(
            UiPrefs::load(Some(dir.path())),
            UiPrefs {
                translucent: true,
                roster_collapsed: true,
                inspector_open: false,
            }
        );
    }
}
