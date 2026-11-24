//! The per-space mise file: `mise.<slug>.toml` in each of a space's worktrees, holding the space's own `PORT`.
//! mise loads it when `MISE_ENV=<slug>`, layered over the repository's `mise.toml` (and under `mise.local.toml`).
//! The file is ignored through the repository's `info/exclude`, so no tracked file changes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::{Result, io_error};
use crate::git;
use crate::store::write_atomic;

/// The name of the mise file for the space `slug`: `mise.<slug>.toml`.
pub fn mise_file_name(slug: &str) -> String {
    format!("mise.{slug}.toml")
}

/// The environment variables a space sets, in the order they are written: `PORT` and `XCTX_SPACE`.
pub fn space_env_vars(slug: &str, port: u16) -> Vec<(String, String)> {
    vec![
        ("PORT".to_owned(), port.to_string()),
        ("XCTX_SPACE".to_owned(), slug.to_owned()),
    ]
}

/// The text of a space's mise file.
pub fn mise_file_contents(slug: &str, title: &str, port: u16) -> String {
    let title: String = title.chars().filter(|c| !c.is_control()).collect();
    let mut text = format!(
        "# Written by xctx for the space \"{title}\". Used when MISE_ENV={slug}; mise.local.toml still wins.\n\
         # Removed when the space is shipped or discarded.\n[env]\n"
    );
    for (key, value) in space_env_vars(slug, port) {
        text.push_str(&format!("{key} = {}\n", toml_string(&value)));
    }
    text
}

/// A TOML basic string.
fn toml_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Write `mise.<slug>.toml` into `worktree` and make git ignore it. Returns the file's path.
pub fn write_space_mise_file(
    worktree: &Path,
    slug: &str,
    title: &str,
    port: u16,
) -> Result<PathBuf> {
    let name = mise_file_name(slug);
    let path = worktree.join(&name);
    write_atomic(&path, mise_file_contents(slug, title, port).as_bytes())?;
    ensure_git_excluded(worktree, &name)?;
    Ok(path)
}

/// Whether `name` can be an environment variable name: a letter or `_`, then letters, digits or `_`.
pub fn is_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Set `name = value` in the `[env]` of the space's mise file in `worktree`, replacing an earlier value of
/// `name`. This is how a missing `required` variable is given for one space: the file is git-ignored and removed
/// with the space. The file must exist (the space has launched). Returns its path.
pub fn set_space_var(worktree: &Path, slug: &str, name: &str, value: &str) -> Result<PathBuf> {
    if !is_env_var_name(name) {
        return Err(crate::Error::Io {
            path: PathBuf::from(name),
            source: io::Error::new(
                io::ErrorKind::InvalidInput,
                "not an environment variable name",
            ),
        });
    }
    let path = worktree.join(mise_file_name(slug));
    let old = fs::read_to_string(&path).map_err(io_error(&path))?;
    let prefix = format!("{name} =");
    let mut text: String = old
        .lines()
        .filter(|line| !line.trim_start().starts_with(&prefix))
        .map(|line| format!("{line}\n"))
        .collect();
    if !text.lines().any(|l| l.trim() == "[env]") {
        text.push_str("[env]\n");
    }
    text.push_str(&format!("{name} = {}\n", toml_string(value)));
    write_atomic(&path, text.as_bytes())?;
    Ok(path)
}

/// Delete the space's mise file from `worktree` (if it is there) and drop its line from the repository's exclude
/// file. `repo` is any checkout of the repository; it is used for the exclude file when `worktree` is gone.
pub fn remove_space_mise_file(worktree: &Path, repo: &Path, slug: &str) -> Result<()> {
    let name = mise_file_name(slug);
    let path = worktree.join(&name);
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_error(path)(e)),
    }
    let git_dir = if worktree.is_dir() { worktree } else { repo };
    remove_git_exclude(git_dir, &name)
}

/// The repository's `info/exclude` file, shared by every worktree of the repository containing `dir`.
fn exclude_file(dir: &Path) -> Result<PathBuf> {
    let out = git::run_git(dir, ["rev-parse", "--git-path", "info/exclude"])?;
    let path = PathBuf::from(out.trim());
    Ok(if path.is_absolute() {
        path
    } else {
        dir.join(path)
    })
}

fn exclude_line(file_name: &str) -> String {
    format!("/{file_name}")
}

/// Make git ignore `file_name` at the top of every worktree of the repository containing `dir`, by adding it to
/// the repository's `info/exclude`. Does nothing when the line is already there. No tracked file changes.
pub fn ensure_git_excluded(dir: &Path, file_name: &str) -> Result<()> {
    let path = exclude_file(dir)?;
    let line = exclude_line(file_name);
    let old = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(io_error(path)(e)),
    };
    if old.lines().any(|l| l == line) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io_error(parent))?;
    }
    let mut new = old;
    if !new.is_empty() && !new.ends_with('\n') {
        new.push('\n');
    }
    new.push_str(&line);
    new.push('\n');
    write_atomic(&path, new.as_bytes())
}

/// Drop the line [`ensure_git_excluded`] added for `file_name`. Does nothing when it is not there.
pub fn remove_git_exclude(dir: &Path, file_name: &str) -> Result<()> {
    let path = exclude_file(dir)?;
    let old = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_error(path)(e)),
    };
    let line = exclude_line(file_name);
    if !old.lines().any(|l| l == line) {
        return Ok(());
    }
    let mut new: String = old
        .lines()
        .filter(|l| *l != line)
        .collect::<Vec<_>>()
        .join("\n");
    if !new.is_empty() {
        new.push('\n');
    }
    write_atomic(&path, new.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contents_hold_the_port_and_space_and_escape_the_title() {
        let text = mise_file_contents("rate-limit", "say \"hi\"\nnow", 4001);
        assert!(
            text.contains("[env]\nPORT = \"4001\"\nXCTX_SPACE = \"rate-limit\"\n"),
            "{text}"
        );
        assert!(text.contains("MISE_ENV=rate-limit"), "{text}");
        assert_eq!(
            text.lines().filter(|l| l.starts_with('#')).count(),
            2,
            "{text}"
        );
    }

    #[test]
    fn set_space_var_adds_then_replaces_and_keeps_the_port() -> Result<()> {
        let dir = tempfile::tempdir().map_err(io_error("tempdir"))?;
        let path = dir.path().join(mise_file_name("s"));
        write_atomic(&path, mise_file_contents("s", "S", 4001).as_bytes())?;
        set_space_var(dir.path(), "s", "STRIPE_KEY", "sk \"one\"")?;
        set_space_var(dir.path(), "s", "STRIPE_KEY", "sk_two")?;
        let text = fs::read_to_string(&path).map_err(io_error(&path))?;
        assert!(text.contains("PORT = \"4001\""), "{text}");
        assert_eq!(text.matches("STRIPE_KEY").count(), 1, "{text}");
        assert!(text.ends_with("STRIPE_KEY = \"sk_two\"\n"), "{text}");
        assert!(set_space_var(dir.path(), "s", "BAD NAME", "x").is_err());
        assert!(set_space_var(dir.path(), "missing", "A", "x").is_err());
        Ok(())
    }

    #[test]
    fn env_var_names() {
        assert!(is_env_var_name("_A1"));
        assert!(!is_env_var_name("1A"));
        assert!(!is_env_var_name("A-B"));
        assert!(!is_env_var_name(""));
    }

    #[test]
    fn toml_strings_escape_quotes_and_backslashes() {
        assert_eq!(toml_string(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
