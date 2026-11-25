//! The first-party plugins' manifests, embedded as TOML and parsed by the same code as user plugins, so they are
//! also examples of the format.

use crate::error::Result;
use crate::manifest::Manifest;

/// The built-in manifests as `(name, TOML text)`, in the order the Plugins menu lists them.
pub const BUILTIN_MANIFESTS: &[(&str, &str)] = &[
    ("terminal", include_str!("builtin/terminal.toml")),
    ("api-client", include_str!("builtin/api-client.toml")),
    ("logs", include_str!("builtin/logs.toml")),
    ("database", include_str!("builtin/database.toml")),
    ("browser", include_str!("builtin/browser.toml")),
    ("files", include_str!("builtin/files.toml")),
    ("jira", include_str!("builtin/jira.toml")),
    ("figma", include_str!("builtin/figma.toml")),
];

/// The origin a built-in manifest is named by in errors.
pub fn builtin_origin(name: &str) -> String {
    format!("built-in {name}")
}

/// Parse every built-in manifest. An error here is a bug in xctx, caught by this crate's tests.
pub fn builtin_manifests() -> Vec<Result<Manifest>> {
    BUILTIN_MANIFESTS
        .iter()
        .map(|(name, text)| Manifest::parse(text, &builtin_origin(name)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Effect, PaneSpec, ToolAction};

    fn builtin(name: &str) -> Manifest {
        let text = BUILTIN_MANIFESTS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, t)| *t)
            .unwrap_or_default();
        match Manifest::parse(text, &builtin_origin(name)) {
            Ok(m) => m,
            Err(e) => panic!("{e}"),
        }
    }

    #[test]
    fn every_builtin_parses_under_its_own_name() {
        for (name, result) in BUILTIN_MANIFESTS
            .iter()
            .map(|(n, _)| n)
            .zip(builtin_manifests())
        {
            match result {
                Ok(m) => assert_eq!(&m.name, name),
                Err(e) => panic!("{e}"),
            }
        }
    }

    #[test]
    fn builtins_have_the_planned_tools_panes_and_effects() {
        let effect = |plugin: &str, tool: &str| builtin(plugin).tool(tool).map(|t| t.effect);
        assert_eq!(effect("terminal", "shell.run"), Some(Effect::Write));
        assert!(matches!(
            builtin("terminal").tool("shell.run").map(|t| &t.action),
            Some(ToolAction::Run(_))
        ));
        for tool in ["http.send", "http.replay", "http.diff"] {
            assert_eq!(effect("api-client", tool), Some(Effect::Read), "{tool}");
        }
        let replay = builtin("api-client");
        let times = replay
            .tool("http.replay")
            .and_then(|t| t.param("times"))
            .map(|p| (p.kind, p.required, p.min, p.max));
        assert_eq!(
            times,
            Some((
                crate::manifest::ParamKind::Integer,
                false,
                Some(1),
                Some(100)
            ))
        );
        assert!(builtin("api-client").event("response.status").is_some());
        assert!(builtin("api-client").event("response.slow").is_some());
        assert_eq!(effect("logs", "logs.query"), Some(Effect::Read));
        assert!(builtin("logs").event("logs.error").is_some());
        assert_eq!(effect("database", "sql.read"), Some(Effect::Read));
        assert_eq!(effect("database", "sql.write"), Some(Effect::Write));
        for tool in [
            "page.open",
            "page.screenshot",
            "page.console",
            "page.click",
            "page.type",
            "page.text",
        ] {
            assert_eq!(effect("browser", tool), Some(Effect::Read), "{tool}");
        }
        assert_eq!(effect("jira", "issue.read"), Some(Effect::Read));
        assert_eq!(effect("jira", "issue.comment"), Some(Effect::Write));
        assert_eq!(effect("figma", "frame.read"), Some(Effect::Read));
        assert_eq!(effect("figma", "frame.export"), Some(Effect::Read));

        assert!(matches!(builtin("api-client").pane, PaneSpec::Http { .. }));
        assert!(matches!(builtin("logs").pane, PaneSpec::Logs { .. }));
        assert!(matches!(builtin("database").pane, PaneSpec::Sql { .. }));
        assert!(matches!(builtin("browser").pane, PaneSpec::Web { .. }));
        assert_eq!(builtin("files").pane, PaneSpec::Files);
        assert!(
            builtin("files").tools.is_empty(),
            "the files pane reads on its own"
        );
        for m in builtin_manifests().into_iter().flatten() {
            assert!(
                !m.scope.allows("production"),
                "{} must not reach production by default",
                m.name
            );
        }
    }
}
