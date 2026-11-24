//! A space's graph and loop on disk: `<data dir>/spaces/<slug>/graph.json`, written atomically and validated when
//! read.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::GraphError;
use crate::graph::AgentGraph;
use crate::loop_run::LoopRun;

/// The directory under the data directory that holds one directory per space.
pub const SPACES_DIR: &str = "spaces";

/// The graph document's file in a space's directory.
pub const GRAPH_FILE: &str = "graph.json";

/// The newest graph document version this build reads, and the one it writes.
pub const GRAPH_DOCUMENT_VERSION: u32 = 1;

/// What `graph.json` holds: a space's agent graph and its loop.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceGraphDocument {
    /// The format version; [`GRAPH_DOCUMENT_VERSION`] for documents made by [`SpaceGraphDocument::new`].
    pub version: u32,
    /// The space's agents.
    pub graph: AgentGraph,
    /// The space's loop.
    pub loop_run: LoopRun,
}

impl SpaceGraphDocument {
    /// A document of the current version.
    pub fn new(graph: AgentGraph, loop_run: LoopRun) -> SpaceGraphDocument {
        SpaceGraphDocument {
            version: GRAPH_DOCUMENT_VERSION,
            graph,
            loop_run,
        }
    }
}

/// Where the space `slug` keeps its graph: `<data_dir>/spaces/<slug>/graph.json`. Fails with
/// [`GraphError::InvalidSlug`] unless `slug` is non-empty ASCII letters, digits, `-` or `_`, so it cannot leave
/// the spaces directory.
pub fn graph_path(data_dir: &Path, slug: &str) -> Result<PathBuf, GraphError> {
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err(GraphError::InvalidSlug(slug.to_owned()));
    }
    Ok(data_dir.join(SPACES_DIR).join(slug).join(GRAPH_FILE))
}

/// Write `doc` to `path`, creating its directory if needed. The file is written to a temporary file beside it and
/// renamed into place, so readers see the old document or the new one, never half of one. A graph that fails
/// [`AgentGraph::validate`] is refused and nothing is written.
pub fn save(path: &Path, doc: &SpaceGraphDocument) -> Result<(), GraphError> {
    doc.graph.validate()?;
    let parent = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent).map_err(io_error(parent))?;
    let bytes = serde_json::to_vec_pretty(doc).map_err(|source| GraphError::Json {
        path: path.to_path_buf(),
        source,
    })?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(io_error(parent))?;
    file.write_all(&bytes).map_err(io_error(file.path()))?;
    file.as_file().sync_all().map_err(io_error(file.path()))?;
    file.persist(path).map_err(|e| io_error(path)(e.error))?;
    Ok(())
}

/// Read the document at `path`. Fails when the file cannot be read, is not a graph document, was written by a
/// newer xctx or has version 0, or holds a graph that fails [`AgentGraph::validate`] (with that error), so a damaged graph is never
/// used.
pub fn load(path: &Path) -> Result<SpaceGraphDocument, GraphError> {
    let bytes = fs::read(path).map_err(io_error(path))?;
    let json_error = |source| GraphError::Json {
        path: path.to_path_buf(),
        source,
    };
    // Read the version first: a newer document may not parse as this version's shape.
    let Version { version } = serde_json::from_slice(&bytes).map_err(json_error)?;
    if version == 0 || version > GRAPH_DOCUMENT_VERSION {
        return Err(GraphError::UnsupportedVersion {
            path: path.to_path_buf(),
            found: version,
            supported: GRAPH_DOCUMENT_VERSION,
        });
    }
    let doc: SpaceGraphDocument = serde_json::from_slice(&bytes).map_err(json_error)?;
    doc.graph.validate()?;
    Ok(doc)
}

#[derive(Deserialize)]
struct Version {
    version: u32,
}

fn io_error(path: &Path) -> impl FnOnce(std::io::Error) -> GraphError + '_ {
    move |source| GraphError::Io {
        path: path.to_path_buf(),
        source,
    }
}
