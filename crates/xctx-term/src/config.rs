//! What to run in a terminal session and how big it starts.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::snapshot::Rgb;

/// Everything needed to start a [`crate::TerminalSession`].
///
/// The child inherits the parent's environment with `env` applied on top. `env` starts with
/// `TERM=xterm-256color` and `COLORTERM=truecolor`, plus `LANG=en_US.UTF-8` when the parent has
/// neither `LANG` nor `LC_CTYPE` set (as when the app is launched from Finder or the Dock), so
/// shells and pagers handle UTF-8; overwrite or remove those entries to change them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalConfig {
    /// Program to run, as a path or a name looked up in `PATH`.
    pub program: String,
    /// Arguments passed to the program (not including `argv[0]`).
    pub args: Vec<String>,
    /// Working directory; `None` inherits the parent's. A directory that does not exist makes
    /// spawning fail.
    pub cwd: Option<PathBuf>,
    /// Environment variables set for the child on top of the inherited environment.
    pub env: BTreeMap<String, String>,
    /// Initial width in cells. Values below 2 are raised to 2.
    pub cols: u16,
    /// Initial height in cells. Values below 1 are raised to 1.
    pub rows: u16,
    /// Initial cell width in pixels, reported to programs that ask for the window size.
    pub cell_width: u16,
    /// Initial cell height in pixels, reported to programs that ask for the window size.
    pub cell_height: u16,
    /// Lines of scrollback kept above the screen.
    pub scrollback_lines: usize,
    /// Default foreground, reported to programs that query it (OSC 10).
    pub default_foreground: Rgb,
    /// Default background, reported to programs that query it (OSC 11); `nvim` uses this to pick
    /// a light or dark scheme.
    pub default_background: Rgb,
}

impl TerminalConfig {
    /// Default scrollback length.
    pub const DEFAULT_SCROLLBACK_LINES: usize = 10_000;

    /// A config that runs `program` with no arguments at 80x24 with the default environment.
    pub fn new(program: impl Into<String>) -> Self {
        let mut env = BTreeMap::new();
        env.insert("TERM".to_owned(), "xterm-256color".to_owned());
        env.insert("COLORTERM".to_owned(), "truecolor".to_owned());
        if let Some(lang) = default_lang(|key| std::env::var_os(key).is_some()) {
            env.insert("LANG".to_owned(), lang.to_owned());
        }
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env,
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
            scrollback_lines: Self::DEFAULT_SCROLLBACK_LINES,
            default_foreground: Rgb::new(0xd8, 0xd8, 0xd8),
            default_background: Rgb::new(0x18, 0x18, 0x18),
        }
    }

    /// A config that runs the user's login shell (`$SHELL`, else `/bin/zsh`) with `-l`.
    pub fn login_shell() -> Self {
        let shell = std::env::var("SHELL")
            .ok()
            .filter(|shell| !shell.is_empty())
            .unwrap_or_else(|| "/bin/zsh".to_owned());
        Self::new(shell).args(["-l"])
    }

    /// Replaces the arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the working directory.
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Sets one environment variable for the child.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Sets the initial size in cells.
    pub fn size(mut self, cols: u16, rows: u16) -> Self {
        self.cols = cols;
        self.rows = rows;
        self
    }

    /// Sets the scrollback length.
    pub fn scrollback_lines(mut self, lines: usize) -> Self {
        self.scrollback_lines = lines;
        self
    }
}

/// The `LANG` to add to the child's environment, or `None` when the parent already chose a
/// locale. `is_set` reports whether a variable is present in the parent environment.
fn default_lang(is_set: impl Fn(&str) -> bool) -> Option<&'static str> {
    if is_set("LANG") || is_set("LC_CTYPE") {
        None
    } else {
        Some("en_US.UTF-8")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_term_and_colorterm() {
        let config = TerminalConfig::new("/bin/sh");
        assert_eq!(
            config.env.get("TERM").map(String::as_str),
            Some("xterm-256color")
        );
        assert_eq!(
            config.env.get("COLORTERM").map(String::as_str),
            Some("truecolor")
        );
        assert_eq!((config.cols, config.rows), (80, 24));
    }

    #[test]
    fn default_lang_is_utf8_only_when_no_locale_is_inherited() {
        assert_eq!(default_lang(|_| false), Some("en_US.UTF-8"));
        assert_eq!(default_lang(|key| key == "LANG"), None);
        assert_eq!(default_lang(|key| key == "LC_CTYPE"), None);
    }

    #[test]
    fn builder_methods_override_fields() {
        let config = TerminalConfig::new("/bin/sh")
            .args(["-c", "true"])
            .cwd("/tmp")
            .env("TERM", "dumb")
            .size(100, 30)
            .scrollback_lines(5);
        assert_eq!(config.args, vec!["-c", "true"]);
        assert_eq!(config.cwd, Some(PathBuf::from("/tmp")));
        assert_eq!(config.env.get("TERM").map(String::as_str), Some("dumb"));
        assert_eq!((config.cols, config.rows), (100, 30));
        assert_eq!(config.scrollback_lines, 5);
    }
}
