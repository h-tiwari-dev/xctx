//! Lightweight scanner for persisted `omp` JSONL session files inside an agent's session folder.

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde_json::Value;

/// Maximum characters kept for a session preview snippet.
const PREVIEW_MAX_CHARS: usize = 96;

/// Metadata extracted from one persisted `omp` `.jsonl` session file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    /// Session id from the file's `session` header (or derived from the filename).
    pub id: String,
    /// Full path to the `.jsonl` file on disk (passed to `switch_session`).
    pub path: PathBuf,
    /// Explicit session title/name if recorded (`title` or `session_info` entry).
    pub title: Option<String>,
    /// First non-empty user message snippet, trimmed to a single line.
    pub preview: Option<String>,
    /// Number of user and assistant turns in the file.
    pub message_count: usize,
    /// Last modification time in Unix epoch milliseconds (0 if unavailable).
    pub updated_ms: u64,
    /// File size in bytes.
    pub size_bytes: u64,
}

impl SessionSummary {
    /// Short display label: explicit title, else first user prompt preview, else short session id.
    pub fn display_title(&self) -> String {
        if let Some(title) = self.title.as_deref().filter(|t| !t.trim().is_empty()) {
            return title.trim().to_owned();
        }
        if let Some(preview) = self.preview.as_deref().filter(|p| !p.trim().is_empty()) {
            return preview.trim().to_owned();
        }
        short_id(&self.id)
    }
}

/// Scan `dir` for persisted `omp` `.jsonl` session files and return them newest-first.
///
/// Missing or unreadable directories return an empty list; malformed lines inside a file are skipped.
pub fn list_sessions_in_dir(dir: &Path) -> Vec<SessionSummary> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut sessions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        if let Some(summary) = read_session_summary(&path) {
            sessions.push(summary);
        }
    }
    sessions.sort_by(|a, b| {
        b.updated_ms
            .cmp(&a.updated_ms)
            .then_with(|| b.path.file_name().cmp(&a.path.file_name()))
    });
    sessions
}

fn read_session_summary(path: &Path) -> Option<SessionSummary> {
    let meta = fs::metadata(path).ok()?;
    let updated_ms = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let size_bytes = meta.len();

    let file = File::open(path).ok()?;
    let reader = BufReader::new(file);

    let mut id: Option<String> = None;
    let mut title: Option<String> = None;
    let mut preview: Option<String> = None;
    let mut message_count: usize = 0;

    for line in reader.lines() {
        let Ok(line) = line else {
            break;
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(val) = serde_json::from_str::<Value>(trimmed) else {
            continue;
        };
        match val.get("type").and_then(Value::as_str) {
            Some("session") => {
                if let Some(s) = val
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                {
                    id = Some(s.to_owned());
                }
            }
            Some("title") => {
                if let Some(t) = val
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                {
                    title = Some(t.to_owned());
                }
            }
            Some("session_info") => {
                if let Some(name) = val
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                {
                    title = Some(name.to_owned());
                }
            }
            Some("message") => {
                let Some(msg) = val.get("message") else {
                    continue;
                };
                match msg.get("role").and_then(Value::as_str) {
                    Some("user") => {
                        message_count += 1;
                        if preview.is_none() {
                            preview = extract_preview(&msg["content"]);
                        }
                    }
                    Some("assistant") => {
                        message_count += 1;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let id = id.unwrap_or_else(|| fallback_id(path));
    Some(SessionSummary {
        id,
        path: path.to_path_buf(),
        title,
        preview,
        message_count,
        updated_ms,
        size_bytes,
    })
}

fn extract_preview(content: &Value) -> Option<String> {
    let raw = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| {
                (b.get("type").and_then(Value::as_str) == Some("text"))
                    .then(|| b.get("text").and_then(Value::as_str))
                    .flatten()
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => return None,
    };
    let line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if line.is_empty() {
        return None;
    }
    let truncated: String = line.chars().take(PREVIEW_MAX_CHARS).collect();
    if line.chars().count() > PREVIEW_MAX_CHARS {
        Some(format!("{truncated}…"))
    } else {
        Some(truncated)
    }
}

fn fallback_id(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("session");
    stem.rsplit_once('_')
        .map(|(_, id)| id)
        .unwrap_or(stem)
        .to_owned()
}

fn short_id(id: &str) -> String {
    let short: String = id.chars().take(8).collect();
    if short.is_empty() {
        "Session".to_owned()
    } else {
        short
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_sessions_reads_titles_previews_counts_and_sorts_newest_first() {
        let dir = tempfile::tempdir().expect("tempdir");
        let older = dir.path().join("2026-11-26T10-00-00-000Z_sess-old.jsonl");
        let newer = dir.path().join("2026-11-28T10-00-00-000Z_sess-new.jsonl");
        let ignored = dir.path().join("notes.txt");

        fs::write(
            &older,
            concat!(
                r#"{"type":"title","v":1,"title":"Auth refactor"}"#,
                "\n",
                r#"{"type":"session","version":3,"id":"sess-old","cwd":"/tmp"}"#,
                "\n",
                r#"{"type":"message","message":{"role":"user","content":[{"type":"text","text":"Refactor token refresh"}]}}"#,
                "\n",
                r#"{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"Done."}]}}"#,
                "\n",
                "not-json\n",
            ),
        )
        .expect("write older");
        // Ensure distinct mtimes or filename fallback
        std::thread::sleep(std::time::Duration::from_millis(15));
        fs::write(
            &newer,
            concat!(
                r#"{"type":"session","version":3,"id":"sess-new","cwd":"/tmp"}"#,
                "\n",
                r#"{"type":"message","message":{"role":"user","content":"Fix   login\nsecond line"}}"#,
                "\n",
            ),
        )
        .expect("write newer");
        fs::write(&ignored, "ignore me").expect("write txt");

        let list = list_sessions_in_dir(dir.path());
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].id, "sess-new");
        assert_eq!(list[0].title, None);
        assert_eq!(list[0].preview.as_deref(), Some("Fix login"));
        assert_eq!(list[0].display_title(), "Fix login");
        assert_eq!(list[0].message_count, 1);

        assert_eq!(list[1].id, "sess-old");
        assert_eq!(list[1].title.as_deref(), Some("Auth refactor"));
        assert_eq!(list[1].preview.as_deref(), Some("Refactor token refresh"));
        assert_eq!(list[1].display_title(), "Auth refactor");
        assert_eq!(list[1].message_count, 2);
    }
}
