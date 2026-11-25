//! The registry against plugin folders in temporary directories.

use std::fs;
use std::path::Path;

use xctx_plugin::{BUILTIN_MANIFESTS, PluginError, Registry, Source};

fn write_plugin(dir: &Path, folder: &str, text: &str) {
    let folder = dir.join(folder);
    fs::create_dir_all(&folder).expect("make plugin folder");
    fs::write(folder.join("plugin.toml"), text).expect("write manifest");
}

fn manifest(name: &str) -> String {
    format!(
        "name = \"{name}\"\nversion = \"0.1.0\"\ndescription = \"test\"\n[pane]\nkind = \"terminal\"\nrun = [\"echo\", \"hi\"]\n\n\
         [[tools]]\nname = \"demo.say\"\neffect = \"read\"\nrun = [\"echo\", \"{{text}}\"]\n\
         params = [{{ name = \"text\", type = \"string\", required = true }}]\n"
    )
}

#[test]
fn builtins_load_without_problems() {
    let registry = Registry::with_builtins();
    assert!(registry.problems().is_empty(), "{:?}", registry.problems());
    assert_eq!(registry.plugins().len(), BUILTIN_MANIFESTS.len());
    for plugin in registry.plugins() {
        assert_eq!(plugin.source, Source::Builtin);
        assert!(plugin.trusted);
    }
    assert!(registry.usable("api-client").is_some());
}

#[test]
fn user_plugins_load_and_bad_ones_are_reported_without_stopping_the_rest() {
    let dir = tempfile::tempdir().expect("tempdir");
    write_plugin(dir.path(), "alpha", &manifest("alpha"));
    write_plugin(dir.path(), "broken", "name = \"broken\"\nversion = ");
    write_plugin(
        dir.path(),
        "invalid",
        &manifest("invalid").replace("demo.say", "say"),
    );
    write_plugin(dir.path(), "misnamed", &manifest("other-name"));
    write_plugin(dir.path(), "zeta", &manifest("zeta"));
    fs::create_dir_all(dir.path().join("empty")).expect("folder without manifest");
    fs::create_dir_all(dir.path().join(".hidden")).expect("hidden folder");
    fs::write(dir.path().join("README.md"), "not a plugin").expect("stray file");

    let mut registry = Registry::with_builtins();
    registry.add_user_dir(dir.path());

    let alpha = registry.usable("alpha").expect("alpha loaded");
    assert!(matches!(&alpha.source, Source::User { path } if path.ends_with("alpha/plugin.toml")));
    assert!(
        registry.usable("zeta").is_some(),
        "a plugin after the bad ones still loads"
    );
    assert!(registry.get("broken").is_none() && registry.get("invalid").is_none());
    assert!(registry.get("other-name").is_none());

    let problems = registry.problems();
    assert_eq!(problems.len(), 4, "{problems:?}");
    let about = |folder: &str| {
        problems
            .iter()
            .find(|p| {
                p.path
                    .as_deref()
                    .is_some_and(|p| p.components().any(|c| c.as_os_str() == folder))
            })
            .unwrap_or_else(|| panic!("no problem for {folder}: {problems:?}"))
    };
    assert!(matches!(about("broken").error, PluginError::Syntax { .. }));
    assert!(
        matches!(&about("invalid").error, PluginError::Invalid { field, .. } if field == "tools[0].name"),
        "{}",
        about("invalid").error
    );
    assert!(
        matches!(&about("misnamed").error, PluginError::Invalid { field, .. } if field == "name")
    );
    assert!(matches!(about("empty").error, PluginError::Io { .. }));
}

#[test]
fn a_missing_user_dir_loads_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut registry = Registry::empty();
    registry.add_user_dir(&dir.path().join("nope"));
    assert!(registry.plugins().is_empty() && registry.problems().is_empty());
}

#[test]
fn clashes_are_refused_not_shadowed() {
    let user = tempfile::tempdir().expect("tempdir");
    let repo = tempfile::tempdir().expect("tempdir");
    write_plugin(user.path(), "logs", &manifest("logs"));
    write_plugin(user.path(), "mine", &manifest("mine"));
    let repo_plugins = repo.path().join(".xctx/plugins");
    write_plugin(&repo_plugins, "mine", &manifest("mine"));
    write_plugin(&repo_plugins, "api-client", &manifest("api-client"));

    let mut registry = Registry::with_builtins();
    registry.add_user_dir(user.path());
    registry.add_repo(repo.path(), |_| true);

    assert_eq!(
        registry.get("logs").map(|p| &p.source),
        Some(&Source::Builtin)
    );
    assert_eq!(
        registry.get("api-client").map(|p| &p.source),
        Some(&Source::Builtin)
    );
    assert!(matches!(
        registry.get("mine").map(|p| &p.source),
        Some(Source::User { .. })
    ));
    let clashes: Vec<&str> = registry
        .problems()
        .iter()
        .filter_map(|p| match &p.error {
            PluginError::Clash { name, .. } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(clashes.len(), 3, "{:?}", registry.problems());
    for name in ["logs", "mine", "api-client"] {
        assert!(clashes.contains(&name), "{name} clash reported");
    }
    let message = registry.problems()[0].error.to_string();
    assert!(message.contains("built-in"), "{message}");
}

#[test]
fn repo_plugins_need_trust_until_the_user_gives_it() {
    let repo = tempfile::tempdir().expect("tempdir");
    let plugins = repo.path().join(".xctx/plugins");
    write_plugin(&plugins, "known", &manifest("known"));
    write_plugin(&plugins, "fresh", &manifest("fresh"));

    let mut registry = Registry::with_builtins();
    registry.add_repo(repo.path(), |m| m.name == "known");

    let fresh = registry.get("fresh").expect("fresh loaded");
    assert!(matches!(&fresh.source, Source::Repo { repo: r, .. } if r == repo.path()));
    assert!(!fresh.trusted);
    assert!(registry.usable("fresh").is_none());
    assert!(registry.usable("known").is_some());
    let waiting: Vec<&str> = registry.needs_trust().iter().map(|p| p.name()).collect();
    assert_eq!(waiting, ["fresh"]);

    assert!(registry.trust("fresh"));
    assert!(registry.needs_trust().is_empty());
    assert!(registry.usable("fresh").is_some());
    assert!(!registry.trust("absent"));
}
