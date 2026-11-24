//! The agent graph: lanes, the one-writer-per-repo invariant, edge wiring and agent state changes.

use xctx_graph::{
    AgentGraph, AgentNodeId, AgentRole, AgentState, EdgeKind, GraphEdge, GraphError, HelperKind,
    ORCHESTRATOR_ID,
};

fn repos(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| (*n).to_owned()).collect()
}

fn id(s: &str) -> AgentNodeId {
    AgentNodeId::new(s)
}

fn has_edge(graph: &AgentGraph, from: &str, to: &str, kind: EdgeKind) -> bool {
    graph.edges().contains(&GraphEdge {
        from: id(from),
        to: id(to),
        kind,
    })
}

fn writer(repo: &str) -> AgentRole {
    AgentRole::Writer {
        repo: repo.to_owned(),
    }
}

fn helper(kind: HelperKind) -> AgentRole {
    AgentRole::Helper { kind }
}

#[test]
fn a_space_starts_with_an_orchestrator_and_one_writer_per_unique_repo() {
    let graph = AgentGraph::for_space(&repos(&["api", "web", "api", ""]));
    let ids: Vec<&str> = graph.nodes().iter().map(|n| n.id.as_str()).collect();
    assert_eq!(ids, ["orchestrator", "writer-api", "writer-web"]);
    let orchestrator = graph.orchestrator().map(|n| n.id.as_str());
    assert_eq!(orchestrator, Some(ORCHESTRATOR_ID));
    let writer_repos: Vec<Option<&str>> = graph.writers().map(|n| n.role.repo()).collect();
    assert_eq!(writer_repos, [Some("api"), Some("web")]);
    assert_eq!(graph.helpers().count(), 0);
    assert!(graph.nodes().iter().all(|n| n.state == AgentState::Idle));
    assert!(graph.validate().is_ok());
}

#[test]
fn a_space_wires_coordinates_and_contract_edges() {
    let graph = AgentGraph::for_space(&repos(&["api", "web", "worker"]));
    for w in ["writer-api", "writer-web", "writer-worker"] {
        assert!(has_edge(&graph, "orchestrator", w, EdgeKind::Coordinates));
    }
    assert!(has_edge(
        &graph,
        "writer-api",
        "writer-web",
        EdgeKind::Contract
    ));
    assert!(has_edge(
        &graph,
        "writer-api",
        "writer-worker",
        EdgeKind::Contract
    ));
    assert!(has_edge(
        &graph,
        "writer-web",
        "writer-worker",
        EdgeKind::Contract
    ));
    assert_eq!(graph.edges().len(), 6);
}

#[test]
fn a_single_repo_space_has_no_contract_edges() {
    let graph = AgentGraph::for_space(&repos(&["api"]));
    assert_eq!(graph.edges().len(), 1);
    assert!(has_edge(
        &graph,
        "orchestrator",
        "writer-api",
        EdgeKind::Coordinates
    ));
    assert!(graph.edges().iter().all(|e| e.kind != EdgeKind::Contract));
}

#[test]
fn a_repo_cannot_get_a_second_writer() {
    let mut graph = AgentGraph::for_space(&repos(&["api"]));
    let result = graph.add_lane(writer("api"), Some("another api writer".into()));
    assert!(
        matches!(&result, Err(GraphError::DuplicateWriter(repo)) if repo == "api"),
        "{result:?}"
    );
    assert_eq!(graph.writers().count(), 1);
    assert_eq!(graph.nodes().len(), 2);
}

#[test]
fn a_writer_needs_a_repo() {
    let mut graph = AgentGraph::for_space(&repos(&["api"]));
    let result = graph.add_lane(writer(""), None);
    assert!(matches!(result, Err(GraphError::EmptyRepo)), "{result:?}");
}

#[test]
fn a_space_cannot_get_a_second_orchestrator() {
    let mut graph = AgentGraph::for_space(&repos(&["api"]));
    let result = graph.add_lane(AgentRole::Orchestrator, None);
    assert!(
        matches!(result, Err(GraphError::DuplicateOrchestrator)),
        "{result:?}"
    );
}

#[test]
fn a_new_writer_gets_coordinates_contract_and_verifies_edges() -> Result<(), GraphError> {
    let mut graph = AgentGraph::for_space(&repos(&["api"]));
    let tests = graph.add_lane(helper(HelperKind::Tests), None)?;
    let web = graph.add_lane(writer("web"), Some("Frontend".into()))?;
    assert_eq!(web.as_str(), "writer-web");
    assert_eq!(graph.node(&web).map(|n| n.label.as_str()), Some("Frontend"));
    assert!(has_edge(
        &graph,
        "orchestrator",
        "writer-web",
        EdgeKind::Coordinates
    ));
    assert!(has_edge(
        &graph,
        "writer-api",
        "writer-web",
        EdgeKind::Contract
    ));
    assert!(has_edge(
        &graph,
        tests.as_str(),
        "writer-web",
        EdgeKind::Verifies
    ));
    graph.validate()
}

#[test]
fn a_new_helper_is_coordinated_and_verifies_every_writer() -> Result<(), GraphError> {
    let mut graph = AgentGraph::for_space(&repos(&["api", "web"]));
    let reviewer = graph.add_lane(helper(HelperKind::Reviewer), None)?;
    assert_eq!(reviewer.as_str(), "helper-reviewer-1");
    let node = graph.node(&reviewer);
    assert_eq!(node.map(|n| n.label.as_str()), Some("Reviewer"));
    assert_eq!(node.map(|n| n.role.is_read_only()), Some(true));
    assert!(has_edge(
        &graph,
        "orchestrator",
        "helper-reviewer-1",
        EdgeKind::Coordinates
    ));
    assert!(has_edge(
        &graph,
        "helper-reviewer-1",
        "writer-api",
        EdgeKind::Verifies
    ));
    assert!(has_edge(
        &graph,
        "helper-reviewer-1",
        "writer-web",
        EdgeKind::Verifies
    ));
    assert_eq!(graph.helpers().count(), 1);
    graph.validate()
}

#[test]
fn helpers_of_one_kind_get_numbered_ids_and_labels() -> Result<(), GraphError> {
    let mut graph = AgentGraph::for_space(&repos(&["api"]));
    let first = graph.add_lane(helper(HelperKind::Tests), None)?;
    let second = graph.add_lane(helper(HelperKind::Tests), None)?;
    let probe = graph.add_lane(helper(HelperKind::Probe), None)?;
    assert_eq!(first.as_str(), "helper-tests-1");
    assert_eq!(second.as_str(), "helper-tests-2");
    assert_eq!(probe.as_str(), "helper-probe-1");
    assert_eq!(
        graph.node(&second).map(|n| n.label.as_str()),
        Some("Tests 2")
    );
    graph.validate()
}

#[test]
fn roles_answer_what_they_are() {
    let w = writer("api");
    assert!(w.is_writer());
    assert!(!w.is_read_only());
    assert_eq!(w.repo(), Some("api"));
    for role in [AgentRole::Orchestrator, helper(HelperKind::Probe)] {
        assert!(!role.is_writer());
        assert!(role.is_read_only());
        assert_eq!(role.repo(), None);
    }
}

#[test]
fn the_orchestrator_cannot_be_removed() {
    let mut graph = AgentGraph::for_space(&repos(&["api"]));
    let result = graph.remove_lane(&id(ORCHESTRATOR_ID));
    assert!(
        matches!(result, Err(GraphError::CannotRemoveOrchestrator)),
        "{result:?}"
    );
    assert!(graph.orchestrator().is_some());
    assert_eq!(graph.edges().len(), 1);
}

#[test]
fn removing_a_lane_removes_its_edges() -> Result<(), GraphError> {
    let mut graph = AgentGraph::for_space(&repos(&["api", "web"]));
    let tests = graph.add_lane(helper(HelperKind::Tests), None)?;
    let removed = graph.remove_lane(&id("writer-web"))?;
    assert_eq!(removed.role.repo(), Some("web"));
    assert!(graph.node(&id("writer-web")).is_none());
    assert!(
        graph
            .edges()
            .iter()
            .all(|e| e.from.as_str() != "writer-web" && e.to.as_str() != "writer-web")
    );
    assert!(has_edge(
        &graph,
        tests.as_str(),
        "writer-api",
        EdgeKind::Verifies
    ));
    graph.validate()?;

    graph.remove_lane(&tests)?;
    assert_eq!(graph.helpers().count(), 0);
    assert_eq!(graph.edges().len(), 1);
    graph.validate()
}

#[test]
fn a_removed_repo_can_get_a_writer_again() -> Result<(), GraphError> {
    let mut graph = AgentGraph::for_space(&repos(&["api"]));
    graph.remove_lane(&id("writer-api"))?;
    let again = graph.add_lane(writer("api"), None)?;
    assert_eq!(again.as_str(), "writer-api");
    graph.validate()
}

#[test]
fn removing_an_unknown_lane_is_not_found() {
    let mut graph = AgentGraph::for_space(&repos(&["api"]));
    let result = graph.remove_lane(&id("writer-nope"));
    assert!(
        matches!(&result, Err(GraphError::NotFound(missing)) if missing.as_str() == "writer-nope"),
        "{result:?}"
    );
}

#[test]
fn pause_and_resume_move_between_running_and_paused() -> Result<(), GraphError> {
    let mut graph = AgentGraph::for_space(&repos(&["api"]));
    let api = id("writer-api");
    graph.set_state(&api, AgentState::Running)?;
    graph.pause(&api)?;
    assert_eq!(
        graph.node(&api).map(|n| &n.state),
        Some(&AgentState::Paused)
    );
    graph.pause(&api)?;
    assert_eq!(
        graph.node(&api).map(|n| &n.state),
        Some(&AgentState::Paused)
    );
    graph.resume(&api)?;
    assert_eq!(
        graph.node(&api).map(|n| &n.state),
        Some(&AgentState::Running)
    );
    graph.resume(&api)?;
    assert_eq!(
        graph.node(&api).map(|n| &n.state),
        Some(&AgentState::Running)
    );
    Ok(())
}

#[test]
fn idle_and_blocked_agents_can_be_paused_and_blocked_ones_resumed() -> Result<(), GraphError> {
    let mut graph = AgentGraph::for_space(&repos(&["api"]));
    let api = id("writer-api");
    graph.pause(&api)?;
    assert_eq!(
        graph.node(&api).map(|n| &n.state),
        Some(&AgentState::Paused)
    );

    let blocked = AgentState::Blocked {
        reason: "waiting on the contract".into(),
    };
    graph.set_state(&api, blocked.clone())?;
    graph.resume(&api)?;
    assert_eq!(
        graph.node(&api).map(|n| &n.state),
        Some(&AgentState::Running)
    );

    graph.set_state(&api, blocked)?;
    graph.pause(&api)?;
    assert_eq!(
        graph.node(&api).map(|n| &n.state),
        Some(&AgentState::Paused)
    );
    Ok(())
}

#[test]
fn finished_or_idle_agents_refuse_invalid_transitions() -> Result<(), GraphError> {
    let mut graph = AgentGraph::for_space(&repos(&["api"]));
    let api = id("writer-api");

    let result = graph.resume(&api);
    assert!(
        matches!(
            &result,
            Err(GraphError::InvalidTransition {
                from: AgentState::Idle,
                action: "resume",
                ..
            })
        ),
        "{result:?}"
    );

    graph.set_state(&api, AgentState::Done)?;
    let result = graph.pause(&api);
    assert!(
        matches!(
            &result,
            Err(GraphError::InvalidTransition {
                from: AgentState::Done,
                action: "pause",
                ..
            })
        ),
        "{result:?}"
    );

    let failed = AgentState::Failed {
        reason: "crashed".into(),
    };
    graph.set_state(&api, failed.clone())?;
    let result = graph.resume(&api);
    assert!(
        matches!(&result, Err(GraphError::InvalidTransition { from, .. }) if *from == failed),
        "{result:?}"
    );
    assert_eq!(graph.node(&api).map(|n| &n.state), Some(&failed));
    Ok(())
}

#[test]
fn state_changes_on_unknown_agents_are_not_found() {
    let mut graph = AgentGraph::for_space(&repos(&["api"]));
    let ghost = id("ghost");
    assert!(matches!(graph.pause(&ghost), Err(GraphError::NotFound(_))));
    assert!(matches!(graph.resume(&ghost), Err(GraphError::NotFound(_))));
    assert!(matches!(
        graph.set_state(&ghost, AgentState::Running),
        Err(GraphError::NotFound(_))
    ));
    assert!(matches!(
        graph.bind_pane(&ghost, Some(1)),
        Err(GraphError::NotFound(_))
    ));
    assert!(matches!(
        graph.record_turn(&ghost),
        Err(GraphError::NotFound(_))
    ));
}

#[test]
fn panes_and_turns_are_recorded_per_agent() -> Result<(), GraphError> {
    let mut graph = AgentGraph::for_space(&repos(&["api"]));
    let api = id("writer-api");
    graph.bind_pane(&api, Some(42))?;
    assert_eq!(graph.node(&api).and_then(|n| n.pane_id), Some(42));
    assert_eq!(graph.record_turn(&api)?, 1);
    assert_eq!(graph.record_turn(&api)?, 2);
    assert_eq!(graph.node(&api).map(|n| n.turns_used), Some(2));
    graph.bind_pane(&api, None)?;
    assert_eq!(graph.node(&api).and_then(|n| n.pane_id), None);
    assert_eq!(
        graph.node(&id(ORCHESTRATOR_ID)).map(|n| n.turns_used),
        Some(0)
    );
    Ok(())
}

fn graph_from_json(json: serde_json::Value) -> Result<AgentGraph, serde_json::Error> {
    serde_json::from_value(json)
}

#[test]
fn validate_rejects_broken_graphs() -> Result<(), serde_json::Error> {
    let orchestrator =
        serde_json::json!({"id": "orchestrator", "role": {"role": "orchestrator"}, "label": "O"});
    let api = serde_json::json!({"id": "writer-api", "role": {"role": "writer", "repo": "api"}, "label": "api"});
    let api_again = serde_json::json!({"id": "writer-api-2", "role": {"role": "writer", "repo": "api"}, "label": "api"});

    let no_orchestrator = graph_from_json(serde_json::json!({"nodes": [api], "edges": []}))?;
    assert!(matches!(
        no_orchestrator.validate(),
        Err(GraphError::MissingOrchestrator)
    ));

    let two_orchestrators = graph_from_json(serde_json::json!({
        "nodes": [orchestrator, {"id": "o2", "role": {"role": "orchestrator"}, "label": "O2"}],
        "edges": []
    }))?;
    assert!(matches!(
        two_orchestrators.validate(),
        Err(GraphError::DuplicateOrchestrator)
    ));

    let two_writers =
        graph_from_json(serde_json::json!({"nodes": [orchestrator, api, api_again], "edges": []}))?;
    assert!(matches!(
        two_writers.validate(),
        Err(GraphError::DuplicateWriter(repo)) if repo == "api"
    ));

    let same_id = graph_from_json(serde_json::json!({
        "nodes": [orchestrator, api, {"id": "writer-api", "role": {"role": "helper", "kind": "tests"}, "label": "t"}],
        "edges": []
    }))?;
    assert!(matches!(
        same_id.validate(),
        Err(GraphError::DuplicateNodeId(dup)) if dup.as_str() == "writer-api"
    ));

    let dangling = graph_from_json(serde_json::json!({
        "nodes": [orchestrator, api],
        "edges": [{"from": "orchestrator", "to": "writer-gone", "kind": "coordinates"}]
    }))?;
    assert!(matches!(
        dangling.validate(),
        Err(GraphError::DanglingEdge { to, .. }) if to.as_str() == "writer-gone"
    ));

    let empty_repo = graph_from_json(serde_json::json!({
        "nodes": [orchestrator, {"id": "writer-x", "role": {"role": "writer", "repo": ""}, "label": "x"}],
        "edges": []
    }))?;
    assert!(matches!(empty_repo.validate(), Err(GraphError::EmptyRepo)));
    Ok(())
}
