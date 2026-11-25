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

/// The symlink Chrome keeps in its profile while it runs, pointing at `<hostname>-<pid>`.
const SINGLETON_LOCK: &str = "SingletonLock";

/// How long asking a possibly orphaned Chrome's DevTools port for `/json/version` may take.
const ORPHAN_PROBE_TIMEOUT: Duration = Duration::from_secs(1);

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
    /// A Chrome this client started earlier on the same profile and never quit (left running when xctx died)
    /// is stopped first: when the profile's [`ACTIVE_PORT_FILE`] names a port that still answers as that same
    /// browser, it is asked to close over DevTools, and its process group (the pid in the profile's
    /// `SingletonLock`) is killed if it does not. Nothing is done when the port does not answer, or answers as
    /// a different browser. Only `options.profile_dir` is looked at.
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
        stop_orphan(&options.profile_dir);
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
        let pid = child.id();
        let deadline = Instant::now() + QUIT_GRACE;
        while Instant::now() < deadline && !exited_unreaped(pid) {
            std::thread::sleep(Duration::from_millis(25));
        }
        // Take the helpers (renderers, GPU) with it: they share Chrome's process group. Chrome is not reaped
        // yet, so its pid (the group id) cannot have been reused by an unrelated process.
        kill_group(pid);
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

/// Whether our child `pid` has exited, without reaping it (so its pid, and its group id, stay reserved).
fn exited_unreaped(pid: u32) -> bool {
    let id: libc::id_t = pid;
    // SAFETY: siginfo_t is plain data, and waitid only writes into it; WNOWAIT leaves the child waitable.
    unsafe {
        let mut info: libc::siginfo_t = std::mem::zeroed();
        let rc = libc::waitid(
            libc::P_PID,
            id,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        );
        // An error means there is no such child left to wait for.
        rc != 0 || info.si_pid() != 0
    }
}

/// Stops a Chrome this client left running on `profile_dir` (see [`Browser::launch`]). Best effort: whatever
/// fails here, the launch goes on and reports its own error.
fn stop_orphan(profile_dir: &Path) {
    let Ok(text) = std::fs::read_to_string(profile_dir.join(ACTIVE_PORT_FILE)) else {
        return;
    };
    let Ok(active) = parse_active_port(&text) else {
        return;
    };
    if !answers_as(&active) {
        return;
    }
    let ws_url = format!("ws://127.0.0.1:{}{}", active.port, active.browser_path);
    if let Ok(mut conn) = Connection::connect(&ws_url) {
        let _ = conn.call(
            "Browser.close",
            json!({}),
            ORPHAN_PROBE_TIMEOUT,
            &mut |_| {},
        );
    }
    let pid = singleton_pid(profile_dir);
    let gone = |pid: Option<u32>| !answers_as(&active) && pid.is_none_or(|p| !is_running(p));
    if wait(QUIT_GRACE, || gone(pid)) {
        return;
    }
    // It did not quit. Its SingletonLock names its pid, which is its process group (it was started with
    // process_group(0)); only kill a group led by that pid.
    if let Some(pid) = pid
        && is_group_leader(pid)
    {
        kill_group(pid);
        wait(QUIT_GRACE, || gone(Some(pid)));
    }
}

/// Whether the DevTools port in `active` answers as the very browser that wrote it: its `/json/version`
/// websocket has the same (random, per-launch) browser path.
fn answers_as(active: &ActivePort) -> bool {
    http::get(active.port, "/json/version", ORPHAN_PROBE_TIMEOUT)
        .ok()
        .and_then(|body| serde_json::from_str::<VersionInfo>(&body).ok())
        .is_some_and(|v| v.web_socket_debugger_url.ends_with(&active.browser_path))
}

/// The pid in `profile_dir`'s `SingletonLock` (`<hostname>-<pid>`), when it names this machine.
fn singleton_pid(profile_dir: &Path) -> Option<u32> {
    let target = std::fs::read_link(profile_dir.join(SINGLETON_LOCK)).ok()?;
    let target = target.to_string_lossy();
    let (host, pid) = target.rsplit_once('-')?;
    let pid = pid.parse::<u32>().ok().filter(|p| *p > 1)?;
    (host == hostname()?).then_some(pid)
}

fn hostname() -> Option<String> {
    let mut buf = [0u8; 256];
    // SAFETY: gethostname writes at most `buf.len()` bytes into `buf`.
    let rc = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    Some(String::from_utf8_lossy(&buf[..end]).into_owned())
}

/// Whether `pid` is a live process (not a zombie waiting to be reaped).
fn is_running(pid: u32) -> bool {
    let Ok(pid) = libc::c_int::try_from(pid) else {
        return false;
    };
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = std::mem::size_of::<libc::proc_bsdinfo>();
    let Ok(size_c) = libc::c_int::try_from(size) else {
        return false;
    };
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`, which is that big.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size_c,
        )
    };
    if written != size_c {
        return false;
    }
    // SAFETY: proc_pidinfo filled the whole struct.
    let info = unsafe { info.assume_init() };
    info.pbi_status != libc::SZOMB
}

fn is_group_leader(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: getpgid only reads the process table.
    unsafe { libc::getpgid(pid) == pid }
}

/// Polls `done` until it is true or `timeout` passes; returns its last answer.
fn wait(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    done()
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
    fn a_stale_port_file_where_nothing_listens_is_left_to_the_launch() -> std::io::Result<()> {
        let profile = tempfile::tempdir()?;
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let port_file = profile.path().join(ACTIVE_PORT_FILE);
        std::fs::write(&port_file, format!("{port}\n/devtools/browser/gone\n"))?;
        // A lock naming this very process: it must never be signalled, since nothing answers on the port.
        let lock = format!("{}-{}", hostname().unwrap_or_default(), std::process::id());
        std::os::unix::fs::symlink(lock, profile.path().join(SINGLETON_LOCK))?;
        let started = Instant::now();
        stop_orphan(profile.path());
        assert!(started.elapsed() < Duration::from_secs(2));

        // The launch removes the stale file and goes on to start "Chrome" (here a program that exits).
        let options = LaunchOptions::new("/usr/bin/false", profile.path());
        assert!(matches!(
            Browser::launch(options),
            Err(BrowserError::ExitedEarly { .. })
        ));
        assert!(!port_file.exists());
        Ok(())
    }

    #[test]
    fn a_port_that_answers_as_another_browser_is_left_alone() -> std::io::Result<()> {
        // Something else now listens on the old port: it is not the profile's browser.
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let port = listener.local_addr()?.port();
        std::thread::spawn(move || {
            for mut stream in listener.incoming().flatten() {
                use std::io::{Read, Write};
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let body = r#"{"webSocketDebuggerUrl":"ws://127.0.0.1/devtools/browser/other"}"#;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        let active = ActivePort {
            port,
            browser_path: "/devtools/browser/mine".to_owned(),
        };
        assert!(!answers_as(&active));
        let profile = tempfile::tempdir()?;
        std::fs::write(
            profile.path().join(ACTIVE_PORT_FILE),
            format!("{port}\n/devtools/browser/mine\n"),
        )?;
        let started = Instant::now();
        stop_orphan(profile.path());
        assert!(started.elapsed() < Duration::from_secs(2));
        Ok(())
    }

    #[test]
    fn the_singleton_lock_names_a_pid_on_this_machine_only() -> std::io::Result<()> {
        let profile = tempfile::tempdir()?;
        assert_eq!(singleton_pid(profile.path()), None);
        let host = hostname().unwrap_or_default();
        std::os::unix::fs::symlink(format!("{host}-4242"), profile.path().join(SINGLETON_LOCK))?;
        assert_eq!(singleton_pid(profile.path()), Some(4242));
        let other = tempfile::tempdir()?;
        std::os::unix::fs::symlink("elsewhere.local-4242", other.path().join(SINGLETON_LOCK))?;
        assert_eq!(singleton_pid(other.path()), None);
        assert!(is_running(std::process::id()));
        Ok(())
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
