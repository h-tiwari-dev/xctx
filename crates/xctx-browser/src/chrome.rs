//! Finding a Chrome-family browser on this Mac.

use std::path::{Path, PathBuf};

use crate::error::{BrowserError, Result};

/// The environment variable that names the browser executable to use, ahead of the usual places.
pub const CHROME_ENV: &str = "XCTX_CHROME";

/// The app bundles that are looked for, in order, as `(bundle, executable inside Contents/MacOS)`.
pub const CHROME_APPS: &[(&str, &str)] = &[
    ("Google Chrome.app", "Google Chrome"),
    ("Chromium.app", "Chromium"),
    ("Brave Browser.app", "Brave Browser"),
    ("Microsoft Edge.app", "Microsoft Edge"),
    ("Google Chrome Canary.app", "Google Chrome Canary"),
];

/// The browser executable to drive.
///
/// `$XCTX_CHROME` wins when it is set (and it is an error if it names nothing executable). Otherwise Chrome,
/// Chromium, Brave, Edge and Chrome Canary are looked for under `/Applications`, then under `~/Applications`.
/// The error says where it looked and how to install Chrome.
pub fn find_chrome() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let over = std::env::var_os(CHROME_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    find_chrome_in(over.as_deref(), home.as_deref(), is_executable)
}

/// [`find_chrome`] with its inputs passed in, so it can be tested without touching the machine.
fn find_chrome_in(
    over: Option<&Path>,
    home: Option<&Path>,
    exists: impl Fn(&Path) -> bool,
) -> Result<PathBuf> {
    if let Some(path) = over {
        return if exists(path) {
            Ok(path.to_path_buf())
        } else {
            Err(BrowserError::BadChromeOverride(path.to_path_buf()))
        };
    }
    let candidates = candidates(home);
    if let Some(found) = candidates.iter().find(|p| exists(p)) {
        return Ok(found.clone());
    }
    let searched = candidates
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(BrowserError::ChromeNotFound { searched })
}

/// Every path [`find_chrome`] checks, in order (not counting `$XCTX_CHROME`).
fn candidates(home: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = home {
        roots.push(home.join("Applications"));
    }
    roots
        .iter()
        .flat_map(|root| {
            CHROME_APPS
                .iter()
                .map(move |(app, exe)| root.join(app).join("Contents/MacOS").join(exe))
        })
        .collect()
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_override_wins_and_must_exist() {
        let over = Path::new("/opt/chrome");
        let found = find_chrome_in(Some(over), None, |_| true);
        assert_eq!(found.ok(), Some(over.to_path_buf()));
        let err = find_chrome_in(Some(over), None, |_| false);
        assert!(matches!(err, Err(BrowserError::BadChromeOverride(_))));
    }

    #[test]
    fn chrome_comes_before_the_others_and_home_is_checked_last() {
        let home = Path::new("/Users/someone");
        let all = candidates(Some(home));
        assert_eq!(
            all[0],
            Path::new("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome")
        );
        assert!(
            all.last()
                .is_some_and(|p| p.starts_with("/Users/someone/Applications"))
        );

        let only_brave = |p: &Path| p.ends_with("Brave Browser") && p.starts_with(home);
        let found = find_chrome_in(None, Some(home), only_brave);
        assert_eq!(
            found.ok(),
            Some(home.join("Applications/Brave Browser.app/Contents/MacOS/Brave Browser"))
        );
    }

    #[test]
    fn not_found_names_where_it_looked_and_how_to_fix_it() {
        let err = find_chrome_in(None, None, |_| false)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("/Applications/Chromium.app"), "{err}");
        assert!(err.contains("XCTX_CHROME"), "{err}");
        assert!(err.contains("install"), "{err}");
    }
}
