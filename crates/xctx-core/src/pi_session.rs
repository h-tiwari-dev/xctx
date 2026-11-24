//! `pi` sessions with ids xctx chooses, so an agent's session can be reopened and forked exactly.
//!
//! `pi --session-id <id>` opens the current directory's session with that id, or creates it. `pi --fork <id>
//! --session-id <new>` copies session `<id>`, from any directory, into the current one as session `<new>`, and
//! records the source in the copy's header (`parentSession`). xctx names sessions with [`agent_session_id`] and
//! [`attempt_session_id`]; [`find_session`] and [`read_header`] look at what pi wrote, for the attempt lineage.
//!
//! Checked against pi 1.6 (`docs/cli.md`, `docs/sessions.md`, `docs/session-format.md`).

use std::ffi::OsString;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result, io_error};

/// The environment variable pi reads for its session folder.
pub const SESSION_DIR_ENV: &str = "PI_CODING_AGENT_SESSION_DIR";

/// A session id from `parts`: `xctx-` and the parts joined by `-`, keeping letters, digits, `.`, `_` and `-` (pi's
/// allowed characters) and turning anything else into `-`. Always starts and ends with a letter or digit.
pub fn session_id(parts: &[&str]) -> String {
    let raw = std::iter::once("xctx")
        .chain(parts.iter().copied())
        .collect::<Vec<_>>()
        .join("-");
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    cleaned
        .trim_end_matches(|c: char| !c.is_ascii_alphanumeric())
        .to_owned()
}

/// The session of agent `agent` in space `slug`.
pub fn agent_session_id(slug: &str, agent: &str) -> String {
    session_id(&[slug, agent])
}

/// The session of attempt `number` of agent `agent` in space `slug`.
pub fn attempt_session_id(slug: &str, agent: &str, number: u32) -> String {
    session_id(&[slug, agent, "attempt", &number.to_string()])
}

/// `pi` arguments that open (or create) session `id` in the current directory.
pub fn open_args(id: &str) -> Vec<String> {
    vec!["--session-id".to_owned(), id.to_owned()]
}

/// `pi` arguments that fork session `source` into the current directory as session `new`.
pub fn fork_args(source: &str, new: &str) -> Vec<String> {
    vec![
        "--fork".to_owned(),
        source.to_owned(),
        "--session-id".to_owned(),
        new.to_owned(),
    ]
}

/// pi's session folder: `$PI_CODING_AGENT_SESSION_DIR` when set and non-empty, else `~/.pi/agent/sessions`.
pub fn sessions_root(session_dir: Option<OsString>, home: Option<OsString>) -> Option<PathBuf> {
    if let Some(dir) = session_dir.filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    home.filter(|h| !h.is_empty())
        .map(|h| PathBuf::from(h).join(".pi").join("agent").join("sessions"))
}

/// The newest file of session `id` under `root`: pi names session files `<timestamp>_<id>.jsonl`, in `root` itself
/// (with a session folder set) or in one folder per working directory. `None` when there is none.
pub fn find_session(root: &Path, id: &str) -> Option<PathBuf> {
    let suffix = format!("_{id}.jsonl");
    let mut dirs = vec![root.to_path_buf()];
    if let Ok(entries) = fs::read_dir(root) {
        dirs.extend(entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    dirs.iter()
        .filter_map(|dir| fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.ends_with(&suffix))
        })
        .max_by(|a, b| a.file_name().cmp(&b.file_name()))
}

/// What the first line of a session file says about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionHeader {
    /// The session's id.
    pub id: String,
    /// The directory it works in.
    pub cwd: Option<PathBuf>,
    /// The session file it was forked from, if it was.
    pub parent_session: Option<PathBuf>,
}

/// Read a session file's header line.
pub fn read_header(path: &Path) -> Result<SessionHeader> {
    let file = fs::File::open(path).map_err(io_error(path))?;
    let mut line = String::new();
    BufReader::new(file)
        .read_line(&mut line)
        .map_err(io_error(path))?;
    let value: serde_json::Value = serde_json::from_str(&line).map_err(|source| Error::Json {
        path: path.to_path_buf(),
        source,
    })?;
    let text = |key: &str| value.get(key).and_then(|v| v.as_str()).map(str::to_owned);
    Ok(SessionHeader {
        id: text("id").unwrap_or_default(),
        cwd: text("cwd").map(PathBuf::from),
        parent_session: text("parentSession").map(PathBuf::from),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_keep_pi_characters_and_end_on_alphanumerics() {
        assert_eq!(
            agent_session_id("rate-limit", "writer:api"),
            "xctx-rate-limit-writer-api"
        );
        assert_eq!(
            attempt_session_id("s", "writer api", 2),
            "xctx-s-writer-api-attempt-2"
        );
        assert_eq!(session_id(&["a/b", "c!"]), "xctx-a-b-c");
        assert_eq!(fork_args("x", "y"), ["--fork", "x", "--session-id", "y"]);
        assert_eq!(open_args("x"), ["--session-id", "x"]);
    }

    #[test]
    fn the_session_folder_comes_from_pi_s_variable_then_home() {
        assert_eq!(
            sessions_root(Some("/s".into()), Some("/h".into())),
            Some(PathBuf::from("/s"))
        );
        assert_eq!(
            sessions_root(Some("".into()), Some("/h".into())),
            Some(PathBuf::from("/h/.pi/agent/sessions"))
        );
        assert_eq!(sessions_root(None, None), None);
    }

    #[test]
    fn sessions_are_found_flat_or_per_directory_newest_first_with_their_lineage() -> Result<()> {
        let root = tempfile::tempdir().map_err(io_error("tempdir"))?;
        let per_dir = root.path().join("--tmp-a--");
        fs::create_dir_all(&per_dir).map_err(io_error(&per_dir))?;
        let old = per_dir.join("2026-01-01T00-00-00-000Z_xctx-s-w.jsonl");
        let new = root.path().join("2026-02-01T00-00-00-000Z_xctx-s-w.jsonl");
        let fork = root
            .path()
            .join("2026-03-01T00-00-00-000Z_xctx-s-w-attempt-1.jsonl");
        fs::write(&old, "{\"type\":\"session\",\"id\":\"xctx-s-w\"}\n").map_err(io_error(&old))?;
        fs::write(
            &new,
            "{\"type\":\"session\",\"id\":\"xctx-s-w\",\"cwd\":\"/tmp/a\"}\n",
        )
        .map_err(io_error(&new))?;
        let header = format!(
            "{{\"type\":\"session\",\"id\":\"xctx-s-w-attempt-1\",\"cwd\":\"/tmp/b\",\"parentSession\":\"{}\"}}\n",
            new.display()
        );
        fs::write(&fork, header).map_err(io_error(&fork))?;

        assert_eq!(find_session(root.path(), "xctx-s-w"), Some(new.clone()));
        assert_eq!(find_session(root.path(), "xctx-s"), None, "ids match whole");
        let lineage = read_header(&fork)?;
        assert_eq!(lineage.id, "xctx-s-w-attempt-1");
        assert_eq!(lineage.cwd, Some(PathBuf::from("/tmp/b")));
        assert_eq!(lineage.parent_session, Some(new));
        Ok(())
    }
}
