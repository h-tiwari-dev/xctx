//! The first message an agent lane starts with: what its role in the space is and which `xctx-ctl` calls it works
//! through. Writers launched with a space get the space's task as is; lanes added later (the orchestrator, helpers,
//! the writer of a repo added to the space) get a brief built here, so a new pane never starts with no idea what it
//! is for.

use xctx_core::Space;
use xctx_graph::{AgentNode, AgentRole, HelperKind};

/// The brief `node` of `space` starts with when its pane is added after the space launched.
pub fn lane_brief(space: &Space, node: &AgentNode) -> String {
    let slug = space.id.as_str();
    let id = node.id.as_str();
    let repos: Vec<&str> = space.repos.iter().map(|r| r.name.as_str()).collect();
    let header = format!(
        "You are {label} ({id}) in the xctx space {slug}. The space's task: {task}\nIts repos: {repos}. \
         Coordinate through xctx-ctl (run `xctx-ctl help`); every call takes space={slug}.",
        label = node.label,
        task = space.task,
        repos = repos.join(", "),
    );
    let report = format!(
        "Report what you find with `xctx-ctl channel.post space={slug} from={id} kind=finding body=\"...\"` \
         (kind=status for progress, kind=question to ask)."
    );
    let role = match &node.role {
        AgentRole::Orchestrator => format!(
            "You split the task across the space's agents and keep them in step; you do not edit files. See \
             them with `xctx-ctl graph.get space={slug}` and `xctx-ctl agent.list space={slug}`, read one with \
             `xctx-ctl agent.read space={slug} agent=<id>`, and brief one with `xctx-ctl agent.send \
             space={slug} agent=<id> text=\"...\"`. Agree what crosses repos with `xctx-ctl contract.set`, and \
             add helpers with `xctx-ctl graph.add_lane space={slug} role=helper kind=tests|reviewer|probe|browser`. \
             Start by reading each writer and sending it its part of the task."
        ),
        AgentRole::Writer { repo } => format!(
            "You are the only agent that edits the {repo} repo; your working directory is its worktree. Do your \
             part of the task there, and post what other repos need from you on the channel."
        ),
        AgentRole::Helper { kind } => match kind {
            HelperKind::Tests => "You are read-only: never edit files. Run the repos' tests (their mise tasks or \
                 test commands), find what fails and why, and check that the contract holds on both sides."
                .to_owned(),
            HelperKind::Reviewer => "You are read-only: never edit files. Review the writers' changes \
                 (`git diff` against each worktree's base branch) for bugs, missing tests and anything the task \
                 did not ask for."
                .to_owned(),
            HelperKind::Probe => format!(
                "You are read-only: never edit files. Exercise the running app on its PORT: send requests with \
                 the api-client plugin and read the logs plugin (`xctx-ctl plugin.list space={slug}` shows what \
                 is mounted; `xctx-ctl plugin.mount space={slug} plugin=<name>` mounts one)."
            ),
            HelperKind::Browser => format!(
                "You are read-only: never edit files. Test the app as a user sees it, in xctx's own browser and \
                 never in any other browser. Mount it if needed (`xctx-ctl plugin.mount space={slug} \
                 plugin=browser`), then drive it with `xctx-ctl plugin.call space={slug} plugin=browser \
                 tool=<tool> args:='{{...}}'`: page.open {{\"path\":\"/\"}}, page.click {{\"selector\":\"...\"}}, \
                 page.type {{\"selector\":\"...\",\"text\":\"...\",\"submit\":true}}, page.text, page.screenshot \
                 and page.console {{\"errors_only\":true}}. Walk the flows the task touches and report broken \
                 pages, console errors and anything that looks wrong."
            ),
        },
    };
    format!("{header}\n{role}\n{report}")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use xctx_core::{SpaceId, SpaceRepo, SpaceShape, SpaceStatus};
    use xctx_graph::{AgentNodeId, AgentState};

    use super::*;

    fn space() -> Space {
        Space {
            id: SpaceId::new("rate-limit"),
            title: "rate limit".to_owned(),
            task: "add rate limiting to checkout".to_owned(),
            shape: SpaceShape::Dev,
            status: SpaceStatus::Active,
            port: 4003,
            created_at: 0,
            repos: vec![SpaceRepo {
                name: "api".to_owned(),
                repo: PathBuf::from("/code/api"),
                worktree: PathBuf::from("/code/api-worktrees/rate-limit"),
                branch: "space/rate-limit".to_owned(),
                base_branch: "main".to_owned(),
                base_commit: "abc".to_owned(),
                branch_created: true,
                reason: String::new(),
            }],
            production: false,
        }
    }

    fn node(id: &str, role: AgentRole) -> AgentNode {
        AgentNode {
            id: AgentNodeId::new(id),
            label: "Browser".to_owned(),
            role,
            state: AgentState::Idle,
            turns_used: 0,
            pane_id: None,
            session: None,
            runner: None,
        }
    }

    #[test]
    fn a_browser_helper_is_told_to_use_xctxs_browser_with_its_own_space_and_id() {
        let brief = lane_brief(
            &space(),
            &node(
                "helper-browser-1",
                AgentRole::Helper {
                    kind: HelperKind::Browser,
                },
            ),
        );
        assert!(brief.contains("add rate limiting to checkout"));
        assert!(brief.contains("plugin=browser"));
        assert!(brief.contains("page.click"));
        assert!(brief.contains("never in any other browser"));
        assert!(brief.contains("from=helper-browser-1"));
        assert!(brief.contains("space=rate-limit"));
    }

    #[test]
    fn the_orchestrator_is_told_to_brief_the_writers_and_not_edit() {
        let brief = lane_brief(&space(), &node("orchestrator", AgentRole::Orchestrator));
        assert!(brief.contains("agent.send space=rate-limit"));
        assert!(brief.contains("you do not edit files"));
    }
}
