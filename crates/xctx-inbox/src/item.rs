//! Inbox items: what they are about, who posted them, and where they are in their life.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use xctx_core::{DraftRepo, Redactor, RequiredVar, SpaceDraft, SpaceId, SpaceShape};

/// An item's id, unique in its Inbox and never reused: `inbox-<n>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(String);

impl ItemId {
    /// Wrap an id, for example one a client sent.
    pub fn new(id: impl Into<String>) -> ItemId {
        ItemId(id.into())
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who posted an item.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// xctx itself, for example after checking a repo's mise config.
    App,
    /// The user.
    User,
    /// The Parent Agent on Home.
    ParentAgent,
    /// An agent in a space's pane, over the control socket.
    Agent,
}

/// How much a notice matters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Neutral.
    #[default]
    Info,
    /// Something finished well.
    Success,
    /// Something needs a look.
    Warning,
    /// Something failed.
    Error,
}

/// A proposed space waiting for the user. Approving it is the only way a proposal launches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanGate {
    /// The proposed space: task, title, shape and repos with their reasons. The user may edit it while pending.
    pub draft: SpaceDraft,
    /// Why it was proposed this way, from whoever drafted it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// A request to give an active space more reach.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpaceWiden {
    /// The space.
    pub space: SpaceId,
    /// Repos to add a worktree of.
    #[serde(default)]
    pub add_repos: Vec<DraftRepo>,
    /// Switch the space's terminals to `MISE_ENV=production`.
    #[serde(default)]
    pub production: bool,
    /// Why, from whoever asked.
    pub reason: String,
}

/// A request to fork competing attempts of one writer. Approving it is the only way an agent's request forks:
/// it adds agents, worktrees and ports to the space.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptProposal {
    /// The space.
    pub space: SpaceId,
    /// The graph node of the writer to fork.
    pub agent: String,
    /// How many attempts, 1 to 4.
    pub count: u32,
    /// What each attempt should try differently; may be shorter than `count`.
    #[serde(default)]
    pub instructions: Vec<String>,
    /// Why, from whoever asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// What a [`PluginGate`] asks the user to allow.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PluginGateKind {
    /// Run one call of a plugin's write tool, for example `sql.write` or `issue.comment`.
    WriteCall {
        /// The tool's name.
        tool: String,
        /// The call's arguments as the user should see them, already masked by whoever posted the gate.
        args: String,
        /// The plugin pane the call goes through (its layout id), so the call runs against the same mount after a
        /// restart. Absent in gates posted before it was recorded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pane: Option<u64>,
        /// What the call reaches, masked: the pane's database or base URL. Absent when the pane has none, and
        /// in gates posted before it was recorded.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
    /// Mount a plugin that needs approval to mount, for example a connector with write tools.
    Mount {
        /// Why it needs approval, for example the write tools it brings.
        reason: String,
    },
    /// Trust a repo's own plugin (in its `.xctx/plugins/`) so it can be mounted and run, the way mise needs trust.
    TrustRepoPlugin {
        /// The repo the plugin lives in.
        repo: String,
        /// The SHA-256 (hex) of the exact `plugin.toml` bytes the user is asked about. Approving trusts only that
        /// manifest; empty in gates posted before it was recorded, which cannot be approved.
        #[serde(default, skip_serializing_if = "String::is_empty")]
        hash: String,
    },
}

/// A plugin action waiting for the user: a write tool call, a mount, or trusting a repo plugin. Approving it is
/// the only way an agent's request does any of these; carrying it out is the app's job.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginGate {
    /// The space.
    pub space: SpaceId,
    /// The plugin's manifest name.
    pub plugin: String,
    /// What is asked. Stored as `request`, because the item's own `kind` tag already uses that name on disk.
    #[serde(rename = "request")]
    pub kind: PluginGateKind,
    /// Why, from whoever asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rationale: Option<String>,
}

/// A repo whose mise config requires a variable that is not set.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingEnvVar {
    /// The repo's name in the space (or its folder name).
    pub repo: String,
    /// The directory mise was run in: the worktree or checkout.
    pub dir: PathBuf,
    /// The `MISE_ENV` it was run with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mise_env: Option<String>,
    /// The variable, with mise's help text.
    pub var: RequiredVar,
}

/// Something to tell the user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Notice {
    /// One line.
    pub title: String,
    /// More detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// How much it matters.
    #[serde(default)]
    pub level: Level,
}

/// What an item is about.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemBody {
    /// See [`PlanGate`].
    PlanGate(PlanGate),
    /// See [`SpaceWiden`].
    SpaceWiden(SpaceWiden),
    /// See [`AttemptProposal`].
    AttemptProposal(AttemptProposal),
    /// See [`PluginGate`].
    PluginGate(PluginGate),
    /// See [`MissingEnvVar`].
    MissingEnvVar(MissingEnvVar),
    /// See [`Notice`].
    Notice(Notice),
}

/// An item's kind, without its content.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    /// [`ItemBody::PlanGate`].
    PlanGate,
    /// [`ItemBody::SpaceWiden`].
    SpaceWiden,
    /// [`ItemBody::AttemptProposal`].
    AttemptProposal,
    /// [`ItemBody::PluginGate`].
    PluginGate,
    /// [`ItemBody::MissingEnvVar`].
    MissingEnvVar,
    /// [`ItemBody::Notice`].
    Notice,
}

impl ItemKind {
    /// The name used on the control socket: `plan_gate`, `space_widen`, `attempt_proposal`, `plugin_gate`,
    /// `missing_env_var` or `notice`.
    pub fn name(self) -> &'static str {
        match self {
            ItemKind::PlanGate => "plan_gate",
            ItemKind::SpaceWiden => "space_widen",
            ItemKind::AttemptProposal => "attempt_proposal",
            ItemKind::PluginGate => "plugin_gate",
            ItemKind::MissingEnvVar => "missing_env_var",
            ItemKind::Notice => "notice",
        }
    }

    /// Whether items of this kind are decided by approve or reject (a gate), rather than resolved or dismissed.
    pub fn is_gate(self) -> bool {
        matches!(
            self,
            ItemKind::PlanGate
                | ItemKind::SpaceWiden
                | ItemKind::AttemptProposal
                | ItemKind::PluginGate
        )
    }
}

/// Where an item is in its life. Every item starts `Pending` and moves exactly once:
///
/// | kind | to |
/// | --- | --- |
/// | Plan Gate, space widening, attempt proposal, plugin gate | `Approved` or `Rejected` |
/// | missing variable | `Resolved` (it is set now) or `Dismissed` |
/// | notice | `Dismissed` |
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemState {
    /// Waiting for the user.
    Pending,
    /// The user approved the gate.
    Approved,
    /// The user turned the gate down.
    Rejected,
    /// The missing variable is set now.
    Resolved,
    /// Put away without acting on it.
    Dismissed,
}

impl ItemState {
    /// The name used on the control socket.
    pub fn name(self) -> &'static str {
        match self {
            ItemState::Pending => "pending",
            ItemState::Approved => "approved",
            ItemState::Rejected => "rejected",
            ItemState::Resolved => "resolved",
            ItemState::Dismissed => "dismissed",
        }
    }
}

/// One item of the Inbox.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxItem {
    /// Its id.
    pub id: ItemId,
    /// The space it is about, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space: Option<SpaceId>,
    /// Who posted it.
    pub source: Source,
    /// Where it is in its life.
    pub state: ItemState,
    /// When it was posted, in seconds since the Unix epoch.
    pub created_at: u64,
    /// When it last changed, in seconds since the Unix epoch.
    pub updated_at: u64,
    /// The user's note when deciding it, e.g. why a plan was rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// What it is about.
    pub body: ItemBody,
}

/// What the Inbox shows for an item, with secrets masked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    /// The item's id.
    pub id: ItemId,
    /// Its kind.
    pub kind: ItemKind,
    /// Its state.
    pub state: ItemState,
    /// One line.
    pub title: String,
    /// The rest, possibly several lines; empty when there is nothing more.
    pub detail: String,
    /// The space it is about.
    pub space: Option<SpaceId>,
}

fn shape_name(shape: SpaceShape) -> &'static str {
    match shape {
        SpaceShape::Dev => "dev",
        SpaceShape::Debug => "debug",
        SpaceShape::Review => "review",
    }
}

fn repo_list(repos: &[DraftRepo]) -> String {
    repos
        .iter()
        .map(|r| r.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

impl InboxItem {
    /// Its kind.
    pub fn kind(&self) -> ItemKind {
        match self.body {
            ItemBody::PlanGate(_) => ItemKind::PlanGate,
            ItemBody::SpaceWiden(_) => ItemKind::SpaceWiden,
            ItemBody::AttemptProposal(_) => ItemKind::AttemptProposal,
            ItemBody::PluginGate(_) => ItemKind::PluginGate,
            ItemBody::MissingEnvVar(_) => ItemKind::MissingEnvVar,
            ItemBody::Notice(_) => ItemKind::Notice,
        }
    }

    /// Whether it still waits for the user.
    pub fn is_pending(&self) -> bool {
        self.state == ItemState::Pending
    }

    /// One line for the Inbox list. Not redacted; use [`InboxItem::preview`] for display.
    pub fn title(&self) -> String {
        match &self.body {
            ItemBody::PlanGate(p) => {
                let n = p.draft.repos.len();
                let repos = if n == 1 { "repo" } else { "repos" };
                format!("Launch \"{}\" ({n} {repos})", p.draft.title)
            }
            ItemBody::SpaceWiden(w) => {
                let mut parts = Vec::new();
                if !w.add_repos.is_empty() {
                    parts.push(format!("add {}", repo_list(&w.add_repos)));
                }
                if w.production {
                    parts.push("switch to MISE_ENV=production".to_owned());
                }
                if parts.is_empty() {
                    parts.push("no change".to_owned());
                }
                format!("Widen {}: {}", w.space, parts.join(" and "))
            }
            ItemBody::AttemptProposal(a) => {
                let attempts = if a.count == 1 { "attempt" } else { "attempts" };
                format!("Fork {} {attempts} of {} in {}", a.count, a.agent, a.space)
            }
            ItemBody::PluginGate(g) => match &g.kind {
                PluginGateKind::WriteCall { tool, target, .. } => match target {
                    Some(target) if !target.trim().is_empty() => {
                        format!("Run {tool} ({}) on {target} in {}", g.plugin, g.space)
                    }
                    _ => format!("Run {tool} ({}) in {}", g.plugin, g.space),
                },
                PluginGateKind::Mount { .. } => format!("Mount {} in {}", g.plugin, g.space),
                PluginGateKind::TrustRepoPlugin { repo, .. } => {
                    format!("Trust {repo}'s plugin {} in {}", g.plugin, g.space)
                }
            },
            ItemBody::MissingEnvVar(m) => format!("{} needs {}", m.repo, m.var.name),
            ItemBody::Notice(n) => n.title.clone(),
        }
    }

    /// The rest of what the Inbox shows when the item is opened. Not redacted; use [`InboxItem::preview`].
    pub fn detail(&self) -> String {
        let mut lines: Vec<String> = Vec::new();
        match &self.body {
            ItemBody::PlanGate(p) => {
                lines.push(format!("Task: {}", p.draft.task));
                lines.push(format!("Shape: {}", shape_name(p.draft.shape)));
                if p.draft.repos.is_empty() {
                    lines.push("No repositories yet: add one before approving.".to_owned());
                }
                for repo in &p.draft.repos {
                    lines.push(format!("- {}: {}", repo.name, repo.reason));
                }
                if let Some(r) = &p.rationale {
                    lines.push(String::new());
                    lines.push(r.clone());
                }
            }
            ItemBody::SpaceWiden(w) => {
                lines.push(w.reason.clone());
                for repo in &w.add_repos {
                    lines.push(format!("- {}: {}", repo.name, repo.reason));
                }
                if w.production {
                    lines.push(
                        "Terminals restart with MISE_ENV=production and production credentials."
                            .to_owned(),
                    );
                }
            }
            ItemBody::AttemptProposal(a) => {
                if let Some(r) = &a.rationale {
                    lines.push(r.clone());
                }
                for (i, instruction) in a.instructions.iter().enumerate() {
                    lines.push(format!("- attempt {}: {instruction}", i + 1));
                }
                lines.push(
                    "Each attempt gets its own worktree, PORT and a fork of the writer's session."
                        .to_owned(),
                );
            }
            ItemBody::PluginGate(g) => {
                if let Some(r) = &g.rationale {
                    lines.push(r.clone());
                }
                match &g.kind {
                    PluginGateKind::WriteCall {
                        tool,
                        args,
                        pane,
                        target,
                    } => {
                        if !args.trim().is_empty() {
                            lines.push(format!("Arguments: {args}"));
                        }
                        match (target, pane) {
                            (Some(target), Some(pane)) => {
                                lines.push(format!("Target: {target} (pane {pane})"))
                            }
                            (Some(target), None) => lines.push(format!("Target: {target}")),
                            (None, Some(pane)) => lines.push(format!("Pane: {pane}")),
                            (None, None) => {}
                        }
                        lines.push(format!(
                            "{tool} writes; it runs once, in this space's environment, only if you approve."
                        ));
                    }
                    PluginGateKind::Mount { reason } => {
                        lines.push(reason.clone());
                        lines.push(
                            "Its pane joins the space; its write tools still ask here each time."
                                .to_owned(),
                        );
                    }
                    PluginGateKind::TrustRepoPlugin { repo, hash } => {
                        lines.push(format!(
                            "The plugin comes from {repo}'s .xctx/plugins/ and runs its own commands."
                        ));
                        if hash.is_empty() {
                            lines.push(
                                "This request does not record which manifest it is about: reject it and mount the plugin again."
                                    .to_owned(),
                            );
                        } else {
                            lines.push(format!(
                                "Trust covers this plugin.toml (SHA-256 {}…) only, not the scripts or programs it runs: review those too. An edited manifest asks again.",
                                hash.chars().take(12).collect::<String>()
                            ));
                        }
                        lines.push(
                            "Approving trusts it for this repo, the way mise trust does."
                                .to_owned(),
                        );
                    }
                }
            }
            ItemBody::MissingEnvVar(m) => {
                if let Some(help) = &m.var.help {
                    lines.push(help.clone());
                }
                match &m.var.required_in {
                    Some(file) => lines.push(format!("Required in {}", file.display())),
                    None => lines.push(format!("Required by mise in {}", m.dir.display())),
                }
                if let Some(env) = &m.mise_env {
                    lines.push(format!("With MISE_ENV={env}"));
                }
            }
            ItemBody::Notice(n) => {
                if let Some(body) = &n.body {
                    lines.push(body.clone());
                }
            }
        }
        if let Some(note) = &self.note {
            lines.push(String::new());
            lines.push(format!("Note: {note}"));
        }
        lines.join("\n").trim().to_owned()
    }

    /// What to show for it, with `redactor`'s secrets masked in the title and detail.
    pub fn preview(&self, redactor: &Redactor) -> Preview {
        Preview {
            id: self.id.clone(),
            kind: self.kind(),
            state: self.state,
            title: redactor.redact(&self.title()).into_owned(),
            detail: redactor.redact(&self.detail()).into_owned(),
            space: self.space.clone(),
        }
    }
}

/// The notice a new Inbox starts with, so the first run explains the flow: describe a task on Home, the Parent
/// Agent drafts a space, and it waits here as a Plan Gate until approved.
pub fn welcome_notice() -> Notice {
    Notice {
        title: "Welcome to the Inbox".to_owned(),
        body: Some(
            "Describe a task on Home and the Parent Agent drafts a space for it: the repos, why each one, and a \
             layout. The draft waits here as a Plan Gate; nothing launches until you approve it, and you can \
             edit it first.\n\
             Agents in your spaces post here too: requests to add repos or switch to production, mise variables \
             a repo needs, and anything they want you to see. Secret values from mise are masked."
                .to_owned(),
        ),
        level: Level::Info,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(body: ItemBody) -> InboxItem {
        InboxItem {
            id: ItemId::new("inbox-1"),
            space: None,
            source: Source::App,
            state: ItemState::Pending,
            created_at: 1,
            updated_at: 1,
            note: None,
            body,
        }
    }

    #[test]
    fn titles_say_what_is_asked() {
        let mut draft = SpaceDraft::new("add rate limiting to the api");
        draft.add_repo(DraftRepo::new("/code/api", "name matches \"api\""));
        let gate = item(ItemBody::PlanGate(PlanGate {
            draft,
            rationale: None,
        }));
        assert_eq!(gate.title(), "Launch \"add rate limiting to\" (1 repo)");
        assert!(gate.detail().contains("- api: name matches \"api\""));

        let widen = item(ItemBody::SpaceWiden(SpaceWiden {
            space: SpaceId::new("rate-limit"),
            add_repos: vec![DraftRepo::new("/code/web", "calls the api")],
            production: true,
            reason: "the web client needs the new header".into(),
        }));
        assert_eq!(
            widen.title(),
            "Widen rate-limit: add web and switch to MISE_ENV=production"
        );
    }

    #[test]
    fn previews_mask_secrets() {
        let notice = item(ItemBody::Notice(Notice {
            title: "used tok-12345".into(),
            body: Some("curl -H 'Authorization: tok-12345'".into()),
            level: Level::Warning,
        }));
        let redactor = Redactor::new([("API_TOKEN".to_owned(), "tok-12345".to_owned())]);
        let preview = notice.preview(&redactor);
        assert_eq!(preview.title, "used [REDACTED:API_TOKEN]");
        assert!(!preview.detail.contains("tok-12345"));
        assert_eq!(preview.kind, ItemKind::Notice);
    }

    #[test]
    fn missing_vars_show_mises_help() {
        let missing = item(ItemBody::MissingEnvVar(MissingEnvVar {
            repo: "api".into(),
            dir: "/code/api".into(),
            mise_env: Some("rate-limit".into()),
            var: RequiredVar {
                name: "STRIPE_KEY".into(),
                help: Some("Get it from 1Password".into()),
                required_in: Some("/code/api/mise.toml".into()),
            },
        }));
        assert_eq!(missing.title(), "api needs STRIPE_KEY");
        assert_eq!(
            missing.detail(),
            "Get it from 1Password\nRequired in /code/api/mise.toml\nWith MISE_ENV=rate-limit"
        );
    }

    #[test]
    fn bodies_are_tagged_by_kind_on_disk() -> Result<(), serde_json::Error> {
        let body = ItemBody::Notice(welcome_notice());
        let value = serde_json::to_value(&body)?;
        assert_eq!(value["kind"], "notice");
        assert_eq!(serde_json::from_value::<ItemBody>(value)?, body);
        Ok(())
    }
}
