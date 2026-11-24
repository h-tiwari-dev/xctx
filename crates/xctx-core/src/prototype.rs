//! Importing the spaces of the Swift prototype (PixelWorkspace), which kept its own store in
//! `~/Library/Application Support/PixelWorkspace/spaces.json` in a different format.
//!
//! [`import_prototype`] reads that file without changing it, converts each space whose worktrees still exist,
//! and adds it to the xctx [`Store`] unless a space with the same slug is already there, so running it again
//! imports nothing twice (a space imported and later discarded stays discarded). The prototype's indexed folders
//! are added too. Missing details (branches, base branch) are read from git.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result, io_error};
use crate::git;
use crate::mise;
use crate::space::{Space, SpaceId, SpaceRepo, SpaceShape, SpaceStatus, space_branch};
use crate::store::Store;

/// The reason given to repositories of imported spaces.
pub const IMPORTED_REASON: &str = "imported from the PixelWorkspace prototype";

/// The prototype's store: `~/Library/Application Support/PixelWorkspace/spaces.json`.
pub fn prototype_store_path(home: Option<OsString>) -> Option<PathBuf> {
    home.filter(|h| !h.is_empty()).map(|h| {
        PathBuf::from(h)
            .join("Library")
            .join("Application Support")
            .join("PixelWorkspace")
            .join("spaces.json")
    })
}

#[derive(Deserialize)]
struct ProtoStore {
    #[serde(default)]
    roots: Vec<PathBuf>,
    #[serde(default)]
    spaces: Vec<ProtoSpace>,
}

#[derive(Deserialize)]
struct ProtoSpace {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    task: String,
    #[serde(default)]
    port: u16,
    #[serde(default)]
    shape: String,
    #[serde(default)]
    created: Option<String>,
    #[serde(default)]
    repos: Vec<ProtoRepo>,
}

#[derive(Deserialize)]
struct ProtoRepo {
    repo: PathBuf,
    worktree: PathBuf,
    #[serde(default)]
    base: String,
}

/// What [`import_prototype`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// The spaces added to the store.
    pub imported: Vec<SpaceId>,
    /// Spaces not imported, with why (already in the store, worktrees gone, bad slug).
    pub skipped: Vec<(String, String)>,
    /// Indexed folders added.
    pub roots_added: usize,
}

/// Seconds since the Unix epoch for an ISO 8601 UTC time like `2026-11-23T18:42:33Z`; `None` when it is not one.
pub fn parse_iso_utc(text: &str) -> Option<u64> {
    let text = text.trim().strip_suffix('Z')?;
    let (date, time) = text.split_once('T')?;
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = time.split('.').next()?;
    let mut t = time.split(':').map(|p| p.parse::<i64>().ok());
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    // Days from civil (Howard Hinnant's algorithm).
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + hh * 3_600 + mm * 60 + ss).ok()
}

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn folder_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Convert one prototype space, or say why not.
fn convert(proto: &ProtoSpace, used_ports: &BTreeSet<u16>) -> std::result::Result<Space, String> {
    if !valid_slug(&proto.id) {
        return Err("its id is not a valid slug".into());
    }
    let mut repos = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for repo in &proto.repos {
        if !repo.worktree.is_dir() {
            continue;
        }
        let branch = git::current_branch(&repo.worktree)
            .ok()
            .flatten()
            .unwrap_or_else(|| space_branch(&proto.id));
        let base_branch = git::current_branch(&repo.repo)
            .ok()
            .flatten()
            .or_else(|| git::default_branch(&repo.repo).ok())
            .unwrap_or_else(|| "main".to_owned());
        let base_commit = if repo.base.is_empty() {
            git::head_commit(&repo.repo).unwrap_or_default()
        } else {
            repo.base.clone()
        };
        let mut name = folder_name(&repo.repo);
        let stem = name.clone();
        let mut n = 2;
        while names.contains(&name) {
            name = format!("{stem}-{n}");
            n += 1;
        }
        names.push(name.clone());
        repos.push(SpaceRepo {
            name,
            repo: repo.repo.clone(),
            worktree: repo.worktree.clone(),
            branch,
            base_branch,
            base_commit,
            branch_created: false,
            reason: IMPORTED_REASON.to_owned(),
        });
    }
    if repos.is_empty() {
        return Err("none of its worktrees exist any more".into());
    }
    let title = if proto.name.trim().is_empty() {
        proto.id.replace('-', " ")
    } else {
        proto.name.trim().to_owned()
    };
    let shape = match proto.shape.as_str() {
        "debug" => SpaceShape::Debug,
        "review" => SpaceShape::Review,
        _ => SpaceShape::Dev,
    };
    let port =
        if crate::space::PORT_RANGE.contains(&proto.port) && !used_ports.contains(&proto.port) {
            proto.port
        } else {
            crate::space::allocate_port(used_ports.iter().copied()).map_err(|e| e.to_string())?
        };
    Ok(Space {
        id: SpaceId::new(proto.id.clone()),
        title,
        task: if proto.task.trim().is_empty() {
            proto.name.clone()
        } else {
            proto.task.clone()
        },
        shape,
        status: SpaceStatus::Active,
        port,
        created_at: proto
            .created
            .as_deref()
            .and_then(parse_iso_utc)
            .unwrap_or(0),
        repos,
        production: false,
    })
}

/// Import the prototype's spaces and folders from `path` into `store`. A missing file imports nothing. The file is
/// only read. Each imported space's worktrees get xctx's mise file (with the space's `PORT`, which is kept unless
/// an active space already holds it).
pub fn import_prototype(store: &mut Store, path: &Path) -> Result<ImportReport> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(ImportReport::default()),
        Err(e) => return Err(io_error(path)(e)),
    };
    let proto: ProtoStore = serde_json::from_slice(&bytes).map_err(|source| Error::Json {
        path: path.to_path_buf(),
        source,
    })?;
    let mut report = ImportReport::default();
    for root in &proto.roots {
        if root.is_dir() && store.add_root(root.clone())? {
            report.roots_added += 1;
        }
    }
    for space in &proto.spaces {
        if store.spaces().iter().any(|s| s.id.as_str() == space.id) {
            report
                .skipped
                .push((space.id.clone(), "already in xctx".into()));
            continue;
        }
        let used: BTreeSet<u16> = store.active_spaces().map(|s| s.port).collect();
        match convert(space, &used) {
            Ok(converted) => {
                for repo in &converted.repos {
                    mise::write_space_mise_file(
                        &repo.worktree,
                        converted.id.as_str(),
                        &converted.title,
                        converted.port,
                    )?;
                }
                report.imported.push(converted.id.clone());
                store.upsert_space(converted)?;
            }
            Err(why) => report.skipped.push((space.id.clone(), why)),
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_times_parse_to_epoch_seconds() {
        assert_eq!(parse_iso_utc("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_iso_utc("2026-11-23T18:42:33Z"), Some(1_795_459_353));
        assert_eq!(parse_iso_utc("2000-02-29T12:00:00.5Z"), Some(951_825_600));
        assert_eq!(parse_iso_utc("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_iso_utc("yesterday"), None);
    }

    #[test]
    fn the_prototype_store_is_under_application_support() {
        assert_eq!(
            prototype_store_path(Some("/Users/me".into())),
            Some(PathBuf::from(
                "/Users/me/Library/Application Support/PixelWorkspace/spaces.json"
            ))
        );
        assert_eq!(prototype_store_path(None), None);
    }
}
