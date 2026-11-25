//! What a newly launched space sets up around its agents, from what each repo is. A repo with a web app
//! (`package.json` naming a web framework and a `dev` or `start` script) gets a terminal pane running its dev
//! server on the space's `PORT` and xctx's browser on that page; a space whose repos include a web app and other
//! repos (a frontend and its backend) also gets the API client for the backend. Only one repo serves on the
//! space's `PORT`, so only the first web app gets a dev server.

use std::path::Path;

use serde_json::Value;

/// The dependencies that make a `package.json` a web app served on a port, with whether its dev server takes
/// `--port` (the rest read `PORT` from the environment, as `react-scripts` does).
const WEB_FRAMEWORKS: &[(&str, bool)] = &[
    ("next", true),
    ("vite", true),
    ("astro", true),
    ("nuxt", true),
    ("@sveltejs/kit", true),
    ("@remix-run/dev", true),
    ("@angular/core", true),
    ("gatsby", true),
    ("react-scripts", false),
];

/// How a repo's web app is served.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebApp {
    /// The terminal pane's command: `/bin/sh -c` installing the dependencies when `node_modules` is missing, then
    /// running the dev script on `$PORT`.
    pub command: Vec<String>,
}

/// What launch mounts in one repo of a space.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RepoSetup {
    /// Serve this web app (a terminal pane) and open xctx's browser on it.
    pub web_app: Option<WebApp>,
    /// Mount the API client: a backend beside a web app.
    pub api_client: bool,
}

/// The web app in the worktree at `dir`, if its `package.json` names a web framework and a `dev` (else `start`)
/// script. `None` when there is no readable `package.json`, no such script, or no web framework.
pub fn web_app(dir: &Path) -> Option<WebApp> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let package: Value = serde_json::from_str(&text).ok()?;
    let scripts = package.get("scripts")?.as_object()?;
    let script = ["dev", "start"]
        .into_iter()
        .find(|name| scripts.get(*name).is_some_and(Value::is_string))?;
    let depends_on = |name: &str| {
        ["dependencies", "devDependencies"]
            .iter()
            .any(|key| package.get(*key).and_then(|d| d.get(name)).is_some())
    };
    let takes_port = WEB_FRAMEWORKS
        .iter()
        .find(|(name, _)| depends_on(name))
        .map(|(_, port)| *port)?;
    let manager = package_manager(dir);
    let run = match manager {
        "npm" => format!("npm run {script}{}", if takes_port { " --" } else { "" }),
        "bun" => format!("bun run {script}"),
        other => format!("{other} {script}"),
    };
    let port = if takes_port { " --port \"$PORT\"" } else { "" };
    Some(WebApp {
        command: vec![
            "/bin/sh".to_owned(),
            "-c".to_owned(),
            format!("[ -d node_modules ] || {manager} install; exec {run}{port}"),
        ],
    })
}

/// The package manager a JavaScript project at `dir` uses, from its lock file: `bun`, `pnpm`, `yarn`, else `npm`.
fn package_manager(dir: &Path) -> &'static str {
    let has = |file: &str| dir.join(file).is_file();
    if has("bun.lock") || has("bun.lockb") {
        "bun"
    } else if has("pnpm-lock.yaml") {
        "pnpm"
    } else if has("yarn.lock") {
        "yarn"
    } else {
        "npm"
    }
}

/// What launch mounts in each of `worktrees` (one per repo, in the space's order): the first web app gets its dev
/// server and the browser; when there is one, every repo that is not a web app gets the API client.
pub fn space_setup(worktrees: &[&Path]) -> Vec<RepoSetup> {
    let apps: Vec<Option<WebApp>> = worktrees.iter().map(|dir| web_app(dir)).collect();
    let first = apps.iter().position(Option::is_some);
    apps.into_iter()
        .enumerate()
        .map(|(index, app)| RepoSetup {
            api_client: first.is_some() && app.is_none(),
            web_app: app.filter(|_| Some(index) == first),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn repo(root: &Path, name: &str, package: Option<&str>, lock: Option<&str>) -> TestResult {
        let dir = root.join(name);
        fs::create_dir_all(&dir)?;
        if let Some(package) = package {
            fs::write(dir.join("package.json"), package)?;
        }
        if let Some(lock) = lock {
            fs::write(dir.join(lock), "")?;
        }
        Ok(())
    }

    #[test]
    fn a_next_app_with_bun_installs_then_serves_on_the_spaces_port() -> TestResult {
        let root = tempfile::tempdir()?;
        repo(
            root.path(),
            "site",
            Some(r#"{"scripts":{"dev":"next dev"},"dependencies":{"next":"15"}}"#),
            Some("bun.lock"),
        )?;
        let app = web_app(&root.path().join("site")).ok_or("no web app")?;
        assert_eq!(
            app.command,
            [
                "/bin/sh",
                "-c",
                "[ -d node_modules ] || bun install; exec bun run dev --port \"$PORT\""
            ]
        );
        Ok(())
    }

    #[test]
    fn npm_passes_the_port_after_a_double_dash_and_cra_reads_port_itself() -> TestResult {
        let root = tempfile::tempdir()?;
        repo(
            root.path(),
            "vite",
            Some(r#"{"scripts":{"dev":"vite"},"devDependencies":{"vite":"5"}}"#),
            None,
        )?;
        repo(
            root.path(),
            "cra",
            Some(
                r#"{"scripts":{"start":"react-scripts start"},"dependencies":{"react-scripts":"5"}}"#,
            ),
            Some("yarn.lock"),
        )?;
        let vite = web_app(&root.path().join("vite")).ok_or("no vite app")?;
        assert!(vite.command[2].ends_with("exec npm run dev -- --port \"$PORT\""));
        let cra = web_app(&root.path().join("cra")).ok_or("no cra app")?;
        assert!(cra.command[2].ends_with("exec yarn start"));
        Ok(())
    }

    #[test]
    fn a_frontend_gets_the_dev_server_and_browser_and_the_backend_the_api_client() -> TestResult {
        let root = tempfile::tempdir()?;
        let web = r#"{"scripts":{"dev":"vite"},"devDependencies":{"vite":"5"}}"#;
        repo(root.path(), "web", Some(web), None)?;
        repo(root.path(), "admin", Some(web), None)?;
        repo(
            root.path(),
            "api",
            Some(r#"{"scripts":{"dev":"node server.js"},"dependencies":{"express":"4"}}"#),
            None,
        )?;
        repo(root.path(), "infra", None, None)?;
        let dirs = ["api", "web", "admin", "infra"].map(|name| root.path().join(name));
        let setup = space_setup(&dirs.iter().map(|d| d.as_path()).collect::<Vec<_>>());
        let served: Vec<bool> = setup.iter().map(|s| s.web_app.is_some()).collect();
        let api: Vec<bool> = setup.iter().map(|s| s.api_client).collect();
        // Only the first web app takes the space's PORT; the second one gets neither.
        assert_eq!(served, [false, true, false, false]);
        assert_eq!(api, [true, false, false, true]);
        Ok(())
    }

    #[test]
    fn repos_without_a_web_app_set_nothing_up() -> TestResult {
        let root = tempfile::tempdir()?;
        repo(
            root.path(),
            "cli",
            Some(r#"{"scripts":{"build":"tsc"},"dependencies":{"commander":"1"}}"#),
            None,
        )?;
        repo(root.path(), "rust", None, None)?;
        let dirs = ["cli", "rust"].map(|name| root.path().join(name));
        let setup = space_setup(&dirs.iter().map(|d| d.as_path()).collect::<Vec<_>>());
        assert_eq!(setup, [RepoSetup::default(), RepoSetup::default()]);
        Ok(())
    }
}
