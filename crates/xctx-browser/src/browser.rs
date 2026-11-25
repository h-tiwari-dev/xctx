//! Starting Chrome with a dedicated profile, or attaching to one already running, and its targets.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::cdp::Connection;
use crate::error::{BrowserError, Result};
use crate::http;
use crate::page::Page;
use crate::policy::UrlPolicy;

/// The file Chrome writes in its profile directory once its DevTools port is open.
pub const ACTIVE_PORT_FILE: &str = "DevToolsActivePort";

/// How long [`Browser::launch`] waits for Chrome to open its DevTools port, unless the options say otherwise.
pub const DEFAULT_LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);

/// How long a browser-level call (create or close a target, the HTTP endpoints) may take.
const BROWSER_CALL_TIMEOUT: Duration = Duration::from_secs(10);

/// How long closing waits for Chrome to quit on its own before killing it.
const QUIT_GRACE: Duration = Duration::from_secs(3);

/// How Chrome is started by [`Browser::launch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchOptions {
    /// The executable (see [`crate::find_chrome`]).
    pub chrome: PathBuf,
    /// The profile directory (`--user-data-dir`). Always a directory dedicated to xctx (for example under the
    /// space's data folder), never the user's own Chrome profile: Chrome refuses to share a profile between
    /// two running instances, and the pages must not see the user's cookies. Created if missing.
    pub profile_dir: PathBuf,
    /// Run without a window (`--headless=new`).
    pub headless: bool,
    /// The DevTools port; `None` or `Some(0)` lets Chrome pick a free one (read back from
    /// [`ACTIVE_PORT_FILE`]).
    pub port: Option<u16>,
    /// The window size in pixels, as `(width, height)`.
    pub window_size: Option<(u32, u32)>,
    /// Extra command-line flags, appended after the ones xctx passes.
    pub extra_args: Vec<String>,
    /// How long to wait for Chrome to open its DevTools port.
    pub timeout: Duration,
}

impl LaunchOptions {
    /// Options for `chrome` with the profile in `profile_dir`: with a window, any free port, the default
    /// size and [`DEFAULT_LAUNCH_TIMEOUT`].
    pub fn new(chrome: impl Into<PathBuf>, profile_dir: impl Into<PathBuf>) -> Self {
        Self {
            chrome: chrome.into(),
            profile_dir: profile_dir.into(),
            headless: false,
            port: None,
            window_size: None,
            extra_args: Vec::new(),
            timeout: DEFAULT_LAUNCH_TIMEOUT,
        }
    }

    /// The command-line arguments Chrome is started with.
    pub fn args(&self) -> Vec<String> {
        let mut args = vec![
            format!("--remote-debugging-port={}", self.port.unwrap_or(0)),
            format!("--user-data-dir={}", self.profile_dir.display()),
            "--no-first-run".to_owned(),
            "--no-default-browser-check".to_owned(),
            // Keep Chrome away from the user's login keychain (no "allow access" prompts).
            "--use-mock-keychain".to_owned(),
            "--password-store=basic".to_owned(),
            "--disable-background-networking".to_owned(),
        ];
        if self.headless {
            args.push("--headless=new".to_owned());
        }
        if let Some((w, h)) = self.window_size {
            args.push(format!("--window-size={w},{h}"));
        }
        args.extend(self.extra_args.iter().cloned());
        args.push("about:blank".to_owned());
        args
    }
}

/// What [`ACTIVE_PORT_FILE`] says: the DevTools port and the browser's websocket path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivePort {
    /// The port Chrome listens on (on the loopback address).
    pub port: u16,
    /// The path of the browser's websocket, for example `/devtools/browser/<id>`.
    pub browser_path: String,
}

/// Parses the contents of [`ACTIVE_PORT_FILE`]: the port on the first line, the browser websocket path on
/// the second. A file still being written (no full first line yet) is an error too.
pub fn parse_active_port(text: &str) -> std::result::Result<ActivePort, String> {
    let mut lines = text.lines().map(str::trim);
    let port_line = lines.next().unwrap_or_default();
    let port = port_line
        .parse::<u16>()
        .ok()
        .filter(|p| *p != 0)
        .ok_or_else(|| format!("{port_line:?} is not a port"))?;
    let browser_path = lines.next().unwrap_or_default().to_owned();
    if !browser_path.starts_with("/devtools/browser/") {
        return Err(format!("{browser_path:?} is not a browser websocket path"));
    }
    Ok(ActivePort { port, browser_path })
}

/// What `/json/version` says about the browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionInfo {
    /// For example `Chrome/126.0.6478.127`.
    #[serde(rename = "Browser", default)]
    pub browser: String,
    /// The DevTools protocol version, for example `1.3`.
    #[serde(rename = "Protocol-Version", default)]
    pub protocol_version: String,
    /// The browser's user agent.
    #[serde(rename = "User-Agent", default)]
    pub user_agent: String,
    /// The browser-level websocket.
    #[serde(rename = "webSocketDebuggerUrl", default)]
    pub web_socket_debugger_url: String,
}

/// One DevTools target (a tab, a worker, an extension page), as `/json/list` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetInfo {
    /// The target id.
    pub id: String,
    /// `page`, `iframe`, `service_worker`, `background_page`, ...
    #[serde(rename = "type", default)]
    pub kind: String,
    /// The page title.
    #[serde(default)]
    pub title: String,
    /// The page URL.
    #[serde(default)]
    pub url: String,
    /// The target's websocket; absent when another client is already attached to it.
    #[serde(rename = "webSocketDebuggerUrl", default)]
    pub web_socket_debugger_url: Option<String>,
}

/// A running Chrome reachable over DevTools: one this client started ([`Browser::launch`]), which it quits
/// on [`Browser::close`] or drop, or one already running ([`Browser::attach`]), which it leaves alone.
///
/// All calls block; the app makes them from a background thread.
pub struct Browser {
    port: u16,
    child: Option<Child>,
    conn: Option<Connection>,
}

impl std::fmt::Debug for Browser {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Browser")
            .field("port", &self.port)
            .field("pid", &self.pid())
            .finish_non_exhaustive()
    }
}

impl Browser {
    /// Starts Chrome as `options` say and connects to it, waiting up to `options.timeout` for it to open its
    /// DevTools port. Chrome runs in its own process group, so closing takes its helper processes too.
    ///
    /// Fails if Chrome cannot be started, exits early (for example because another Chrome already uses the
    /// profile directory), or does not open its port in time; Chrome is killed in every failure case.
    pub fn launch(options: LaunchOptions) -> Result<Self> {
        std::fs::create_dir_all(&options.profile_dir).map_err(|e| {
            BrowserError::io(
                format!("could not create {}", options.profile_dir.display()),
                e,
            )
        })?;
        let port_file = options.profile_dir.join(ACTIVE_PORT_FILE);
        match std::fs::remove_file(&port_file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(BrowserError::io(
                    format!("could not remove the stale {}", port_file.display()),
                    e,
                ));
            }
        }
        let child = {
            use std::os::unix::process::CommandExt;
            Command::new(&options.chrome)
                .args(options.args())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .map_err(|source| BrowserError::Spawn {
                    chrome: options.chrome.clone(),
                    source,
                })?
        };
        // From here on, dropping `browser` kills Chrome, so every early return cleans up.
        let mut browser = Self {
            port: 0,
            child: Some(child),
            conn: None,
        };
        let active = browser.wait_for_port(&port_file, options.timeout)?;
        browser.port = active.port;
        let ws_url = format!("ws://127.0.0.1:{}{}", active.port, active.browser_path);
        browser.conn = Some(Connection::connect(&ws_url)?);
        Ok(browser)
    }

    /// Connects to a Chrome already listening for DevTools on `port` (started by someone else with
    /// `--remote-debugging-port`). Closing or dropping the result leaves that Chrome running.
    pub fn attach(port: u16) -> Result<Self> {
        let mut browser = Self {
            port,
            child: None,
            conn: None,
        };
        let version = browser.version()?;
        browser.conn = Some(Connection::connect(&version.web_socket_debugger_url)?);
        Ok(browser)
    }

    /// The DevTools port.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The process id of the Chrome this client started, or `None` when attached.
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(Child::id)
    }

    /// What `/json/version` says.
    pub fn version(&self) -> Result<VersionInfo> {
        let body = http::get(self.port, "/json/version", BROWSER_CALL_TIMEOUT)?;
        serde_json::from_str(&body).map_err(|e| BrowserError::Http {
            path: "/json/version".to_owned(),
            reason: format!("not the expected JSON: {e}"),
        })
    }

    /// Every target, as `/json/list` reports it.
    pub fn targets(&self) -> Result<Vec<TargetInfo>> {
        let body = http::get(self.port, "/json/list", BROWSER_CALL_TIMEOUT)?;
        serde_json::from_str(&body).map_err(|e| BrowserError::Http {
            path: "/json/list".to_owned(),
            reason: format!("not the expected JSON: {e}"),
        })
    }

    /// Opens a new blank tab and connects to it; [`Page::open`] then checks URLs against `policy`.
    pub fn new_page(&mut self, policy: UrlPolicy) -> Result<Page> {
        let result = self.call("Target.createTarget", json!({ "url": "about:blank" }))?;
        let target_id = result
            .get("targetId")
            .and_then(Value::as_str)
            .ok_or_else(|| BrowserError::BadMessage("createTarget gave no targetId".to_owned()))?
            .to_owned();
        let ws_url = format!("ws://127.0.0.1:{}/devtools/page/{target_id}", self.port);
        match Page::connect(self.port, target_id.clone(), &ws_url, policy) {
            Ok(page) => Ok(page),
            Err(e) => {
                let _ = self.close_target(&target_id);
                Err(e)
            }
        }
    }

    /// Connects to an existing page target (from [`Browser::targets`]).
    pub fn attach_page(&self, target: &TargetInfo, policy: UrlPolicy) -> Result<Page> {
        let ws_url = target
            .web_socket_debugger_url
            .clone()
            .unwrap_or_else(|| format!("ws://127.0.0.1:{}/devtools/page/{}", self.port, target.id));
        Page::connect(self.port, target.id.clone(), &ws_url, policy)
    }

    /// Closes the target `target_id` (a tab).
    pub fn close_target(&mut self, target_id: &str) -> Result<()> {
        self.call("Target.closeTarget", json!({ "targetId": target_id }))
            .map(drop)
    }

    /// Quits a Chrome this client started (asking first, then killing its process group) and reaps it, or
    /// just disconnects from an attached one.
    pub fn close(mut self) -> Result<()> {
        self.shutdown()
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let conn = self.conn.as_mut().ok_or(BrowserError::Closed)?;
        conn.call(method, params, BROWSER_CALL_TIMEOUT, &mut |_| {})
    }

    fn wait_for_port(&mut self, port_file: &Path, timeout: Duration) -> Result<ActivePort> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(child) = self.child.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                self.child = None;
                return Err(BrowserError::ExitedEarly {
                    status: status.to_string(),
                });
            }
            if let Ok(text) = std::fs::read_to_string(port_file)
                && let Ok(active) = parse_active_port(&text)
            {
                return Ok(active);
            }
            if Instant::now() >= deadline {
                return Err(BrowserError::LaunchTimeout(timeout));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn shutdown(&mut self) -> Result<()> {
        let mut conn = self.conn.take();
        let Some(mut child) = self.child.take() else {
            if let Some(conn) = conn.as_mut() {
                conn.close();
            }
            return Ok(());
        };
        if let Some(conn) = conn.as_mut() {
            let _ = conn.call(
                "Browser.close",
                json!({}),
                Duration::from_secs(1),
                &mut |_| {},
            );
        }
        drop(conn);
        let deadline = Instant::now() + QUIT_GRACE;
        let mut exited = false;
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(_)) => {
                    exited = true;
                    break;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(25)),
                Err(_) => break,
            }
        }
        // Take the helpers (renderers, GPU) with it: they share Chrome's process group.
        kill_group(child.id());
        if exited {
            return Ok(());
        }
        let _ = child.kill();
        child
            .wait()
            .map(drop)
            .map_err(|e| BrowserError::io("could not reap Chrome", e))
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn kill_group(pid: u32) {
    let Ok(pgid) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: killpg only sends a signal; the group is Chrome's own (process_group(0) at spawn).
    unsafe {
        libc::killpg(pgid, libc::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_active_port_file_gives_the_port_and_path() {
        let parsed = parse_active_port("53817\n/devtools/browser/3f1c-aa\n");
        assert_eq!(
            parsed,
            Ok(ActivePort {
                port: 53817,
                browser_path: "/devtools/browser/3f1c-aa".to_owned()
            })
        );
    }

    #[test]
    fn half_written_or_bad_files_are_refused() {
        assert!(parse_active_port("").is_err());
        assert!(parse_active_port("538").is_err());
        assert!(parse_active_port("0\n/devtools/browser/x").is_err());
        assert!(parse_active_port("99999\n/devtools/browser/x").is_err());
        assert!(parse_active_port("9222\n/somewhere/else").is_err());
    }

    #[test]
    fn launch_flags_use_the_dedicated_profile() {
        let mut options = LaunchOptions::new("/bin/chrome", "/tmp/xctx-profile");
        let args = options.args();
        assert!(args.contains(&"--remote-debugging-port=0".to_owned()));
        assert!(args.contains(&"--user-data-dir=/tmp/xctx-profile".to_owned()));
        assert!(args.contains(&"--no-first-run".to_owned()));
        assert!(args.contains(&"--no-default-browser-check".to_owned()));
        assert!(!args.iter().any(|a| a.starts_with("--headless")));
        options.headless = true;
        options.port = Some(9333);
        options.window_size = Some((1280, 800));
        let args = options.args();
        assert!(args.contains(&"--headless=new".to_owned()));
        assert!(args.contains(&"--remote-debugging-port=9333".to_owned()));
        assert!(args.contains(&"--window-size=1280,800".to_owned()));
        assert_eq!(args.last().map(String::as_str), Some("about:blank"));
    }
}
