//! The graph document on disk, in temporary data directories.

use std::fs;
use std::path::Path;

use xctx_graph::{
    AgentGraph, AgentNodeId, AgentRole, AgentState, AutonomyLevel, GRAPH_DOCUMENT_VERSION,
    GraphError, HelperKind, LoopRun, SpaceGraphDocument, graph_path, load, save,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn sample_document() -> Result<SpaceGraphDocument, Box<dyn std::error::Error>> {
    let mut graph = AgentGraph::for_space(&["api".to_owned(), "web".to_owned()]);
    let tests = graph.add_lane(
        AgentRole::Helper {
            kind: HelperKind::Tests,
        },
        None,
    )?;
    let api = AgentNodeId::new("writer-api");
    graph.set_state(&api, AgentState::Running)?;
    graph.bind_pane(&api, Some(7))?;
    graph.record_turn(&api)?;
    graph.set_state(
        &tests,
        AgentState::Blocked {
            reason: "waiting for a build".into(),
        },
    )?;
    let mut loop_run = LoopRun::new(AutonomyLevel::WithinBudget {
        max_iterations: 3,
        max_turns: 40,
    });
    loop_run.start()?;
    loop_run.record_turn(2);
    loop_run.advance(true)?;
    Ok(SpaceGraphDocument::new(graph, loop_run))
}

fn file_count(dir: &Path) -> std::io::Result<usize> {
    Ok(fs::read_dir(dir)?.count())
}

#[test]
fn graph_path_is_under_the_space_directory() -> TestResult {
    let data = Path::new("/data");
    assert_eq!(
        graph_path(data, "rate-limit-api")?,
        Path::new("/data/spaces/rate-limit-api/graph.json")
    );
    for bad in ["", "..", "a/b", "../escape", "dot.dot", "space slug"] {
        let result = graph_path(data, bad);
        assert!(
            matches!(&result, Err(GraphError::InvalidSlug(s)) if s == bad),
            "{bad:?}: {result:?}"
        );
    }
    Ok(())
}

#[test]
fn a_document_round_trips_through_json_and_leaves_no_temp_files() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = graph_path(dir.path(), "rate-limit-api")?;
    let doc = sample_document()?;
    save(&path, &doc)?;
    let loaded = load(&path)?;
    assert_eq!(loaded, doc);
    assert_eq!(loaded.version, GRAPH_DOCUMENT_VERSION);

    let mut changed = loaded;
    changed.graph.remove_lane(&AgentNodeId::new("writer-web"))?;
    save(&path, &changed)?;
    assert_eq!(load(&path)?, changed);

    let space_dir = path.parent().ok_or("graph.json has no parent")?;
    assert_eq!(file_count(space_dir)?, 1);
    Ok(())
}

#[test]
fn the_json_uses_readable_tags() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = graph_path(dir.path(), "s")?;
    save(&path, &sample_document()?)?;
    let json: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
    assert_eq!(json["version"], 1);
    assert_eq!(json["graph"]["nodes"][0]["id"], "orchestrator");
    assert_eq!(json["graph"]["nodes"][1]["role"]["role"], "writer");
    assert_eq!(json["graph"]["nodes"][1]["role"]["repo"], "api");
    assert_eq!(json["graph"]["nodes"][1]["state"]["state"], "running");
    assert_eq!(json["graph"]["nodes"][3]["role"]["kind"], "tests");
    assert_eq!(json["graph"]["edges"][0]["kind"], "coordinates");
    assert_eq!(json["loop_run"]["autonomy"]["level"], "within_budget");
    assert_eq!(json["loop_run"]["stage"], "test");
    assert_eq!(json["loop_run"]["status"]["status"], "running");
    Ok(())
}

#[test]
fn an_invalid_graph_is_not_saved() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = graph_path(dir.path(), "s")?;
    let graph: AgentGraph = serde_json::from_value(serde_json::json!({"nodes": [], "edges": []}))?;
    let result = save(&path, &SpaceGraphDocument::new(graph, LoopRun::default()));
    assert!(
        matches!(result, Err(GraphError::MissingOrchestrator)),
        "{result:?}"
    );
    assert!(!path.exists());
    Ok(())
}

fn write_raw(path: &Path, value: &serde_json::Value) -> TestResult {
    let parent = path.parent().ok_or("no parent")?;
    fs::create_dir_all(parent)?;
    fs::write(path, serde_json::to_vec(value)?)?;
    Ok(())
}

#[test]
fn a_graph_with_two_writers_for_one_repo_is_rejected_on_load() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = graph_path(dir.path(), "s")?;
    let mut json = serde_json::to_value(sample_document()?)?;
    let nodes = json["graph"]["nodes"]
        .as_array_mut()
        .ok_or("nodes is not an array")?;
    nodes.push(serde_json::json!({
        "id": "writer-api-rogue",
        "role": {"role": "writer", "repo": "api"},
        "label": "rogue"
    }));
    write_raw(&path, &json)?;
    let result = load(&path);
    assert!(
        matches!(&result, Err(GraphError::DuplicateWriter(repo)) if repo == "api"),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_graph_with_a_dangling_edge_is_rejected_on_load() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = graph_path(dir.path(), "s")?;
    let mut json = serde_json::to_value(sample_document()?)?;
    let edges = json["graph"]["edges"]
        .as_array_mut()
        .ok_or("edges is not an array")?;
    edges.push(
        serde_json::json!({"from": "helper-tests-1", "to": "writer-ghost", "kind": "verifies"}),
    );
    write_raw(&path, &json)?;
    let result = load(&path);
    assert!(
        matches!(result, Err(GraphError::DanglingEdge { .. })),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_graph_without_an_orchestrator_is_rejected_on_load() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = graph_path(dir.path(), "s")?;
    let mut json = serde_json::to_value(sample_document()?)?;
    json["graph"]["nodes"]
        .as_array_mut()
        .ok_or("nodes is not an array")?
        .remove(0);
    json["graph"]["edges"] = serde_json::json!([]);
    write_raw(&path, &json)?;
    let result = load(&path);
    assert!(
        matches!(result, Err(GraphError::MissingOrchestrator)),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_damaged_file_is_a_json_error_and_is_left_alone() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = graph_path(dir.path(), "s")?;
    let parent = path.parent().ok_or("no parent")?;
    fs::create_dir_all(parent)?;
    fs::write(&path, b"{\"version\": 1, \"graph\": {\"nodes\": [")?;
    let result = load(&path);
    assert!(matches!(result, Err(GraphError::Json { .. })), "{result:?}");
    assert_eq!(
        fs::read(&path)?,
        b"{\"version\": 1, \"graph\": {\"nodes\": ["
    );
    Ok(())
}

#[test]
fn a_newer_version_is_refused_even_if_its_shape_changed() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = graph_path(dir.path(), "s")?;
    write_raw(&path, &serde_json::json!({"version": 99, "agents": {}}))?;
    let result = load(&path);
    assert!(
        matches!(
            result,
            Err(GraphError::UnsupportedVersion {
                found: 99,
                supported: GRAPH_DOCUMENT_VERSION,
                ..
            })
        ),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn version_zero_is_refused() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = graph_path(dir.path(), "s")?;
    let mut json = serde_json::to_value(sample_document()?)?;
    json["version"] = serde_json::json!(0);
    write_raw(&path, &json)?;
    let result = load(&path);
    assert!(
        matches!(result, Err(GraphError::UnsupportedVersion { found: 0, .. })),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_missing_file_is_an_io_error() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = graph_path(dir.path(), "never-saved")?;
    let result = load(&path);
    assert!(
        matches!(&result, Err(GraphError::Io { source, .. }) if source.kind() == std::io::ErrorKind::NotFound),
        "{result:?}"
    );
    Ok(())
}

#[test]
fn a_save_that_cannot_be_written_leaves_the_old_document() -> TestResult {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir()?;
    let path = graph_path(dir.path(), "s")?;
    let doc = sample_document()?;
    save(&path, &doc)?;
    let space_dir = path.parent().ok_or("no parent")?.to_path_buf();
    fs::set_permissions(&space_dir, fs::Permissions::from_mode(0o555))?;
    let mut changed = doc.clone();
    changed.loop_run.reset();
    let result = save(&path, &changed);
    fs::set_permissions(&space_dir, fs::Permissions::from_mode(0o755))?;
    assert!(matches!(result, Err(GraphError::Io { .. })), "{result:?}");
    assert_eq!(load(&path)?, doc);
    Ok(())
}
