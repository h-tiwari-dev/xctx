//! What keeps a mounted plugin pane going without a window: the tail behind a logs pane (a command or a file,
//! into a bounded [`LogBuffer`]) and the Chrome behind a browser pane.
//!
//! Both run on their own threads or are driven from the background executor, and hand what the UI needs to
//! the workspace through shared state: the buffer itself, a dirty flag the workspace polls to redraw, and a
//! channel of the plugin events matched in the tailed lines. Nothing here is run inside a shell.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::Duration;

use xctx_browser::{
    Browser, ConsoleEntry, LaunchOptions, NetworkError, Page, UrlPolicy, find_chrome,
};
use xctx_connectors::LogBuffer;
use xctx_core::{Redactor, SpaceId};
use xctx_layout::PaneId;
use xctx_plugin::{Manifest, match_events};

/// How often a tailed file is read for new lines.
pub const FILE_POLL: Duration = Duration::from_millis(300);

/// How much of a tailed file's end is read when the tail starts.
pub const FILE_TAIL_START_BYTES: u64 = 64 * 1024;

/// The longest line kept from a tailed command or file; the rest of the line is dropped.
pub const MAX_LINE_BYTES: usize = 16 * 1024;

/// A plugin event matched in a line a logs pane tailed, sent to the workspace to fire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailEvent {
    /// The space.
    pub space: SpaceId,
    /// The plugin whose manifest declares the event.
    pub plugin: String,
    /// The pane whose tail saw it.
    pub pane: PaneId,
    /// The event's name.
    pub event: String,
    /// The line that fired it, masked.
    pub detail: String,
}

/// Where a tail is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TailStatus {
    /// Reading.
    Running,
    /// The command exited (its code, `None` when killed by a signal), or the tail was stopped.
    Exited(Option<i32>),
    /// It could not start or read, with why (masked).
    Failed(String),
}

/// What a tail is attached to, for the events it matches.
#[derive(Clone, Debug)]
pub struct TailTarget {
    /// The space.
    pub space: SpaceId,
    /// The pane.
    pub pane: PaneId,
    /// The plugin's manifest, whose events are matched in every line.
    pub manifest: Manifest,
    /// Masks the space's secrets in every line kept.
    pub redactor: Redactor,
    /// Where matched events go.
    pub events: Sender<TailEvent>,
    /// Set whenever a line is added, so the workspace knows to redraw.
    pub dirty: Arc<AtomicBool>,
}

/// The tail behind one logs pane. Dropping it stops it: the command's whole process group is killed, a file
/// tail stops at its next poll.
#[derive(Debug)]
pub struct LogTail {
    /// The lines, shared with `logs.query` and the pane.
    pub buffer: LogBuffer,
    /// What it tails, for the pane's header: the command or the file.
    pub source: String,
    status: Arc<Mutex<TailStatus>>,
    stop: Arc<AtomicBool>,
    pgid: Option<libc::pid_t>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while holding the lock leaves the data usable; keep going with it.
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl LogTail {
    /// A tail that is not reading anything, with why: the pane shows the reason.
    pub fn failed(buffer: LogBuffer, source: String, why: String) -> LogTail {
        LogTail {
            buffer,
            source,
            status: Arc::new(Mutex::new(TailStatus::Failed(why))),
            stop: Arc::new(AtomicBool::new(true)),
            pgid: None,
        }
    }

    /// Run `argv` (no shell) in `cwd` with `env` on top of xctx's own environment, its stdout and stderr tailed
    /// into `buffer`. Fails when it cannot be started.
    pub fn command(
        argv: &[String],
        cwd: &Path,
        env: &BTreeMap<String, String>,
        buffer: LogBuffer,
        target: TailTarget,
    ) -> Result<LogTail, String> {
        let Some((program, args)) = argv.split_first() else {
            return Err("the logs command is empty".to_owned());
        };
        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .spawn()
            .map_err(|e| format!("could not start {program}: {e}"))?;
        let pgid = libc::pid_t::try_from(child.id()).ok();
        let status = Arc::new(Mutex::new(TailStatus::Running));
        let stop = Arc::new(AtomicBool::new(false));
        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            readers.push(spawn_line_reader(out, buffer.clone(), target.clone()));
        }
        if let Some(err) = child.stderr.take() {
            readers.push(spawn_line_reader(err, buffer.clone(), target.clone()));
        }
        let waiter_status = Arc::clone(&status);
        thread::spawn(move || {
            let code = child.wait().ok().and_then(|s| s.code());
            for reader in readers {
                let _ = reader.join();
            }
            *lock(&waiter_status) = TailStatus::Exited(code);
            target.dirty.store(true, Ordering::Relaxed);
        });
        Ok(LogTail {
            buffer,
            source: argv.join(" "),
            status,
            stop,
            pgid,
        })
    }

    /// Tail the file at `path` into `buffer`: its last [`FILE_TAIL_START_BYTES`] first, then what is added, read
    /// every [`FILE_POLL`]. A file that does not exist yet is waited for; one that shrinks (rotated) is read again
    /// from its start.
    pub fn file(path: PathBuf, buffer: LogBuffer, target: TailTarget) -> LogTail {
        let status = Arc::new(Mutex::new(TailStatus::Running));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread_status = Arc::clone(&status);
        let thread_buffer = buffer.clone();
        let source = path.display().to_string();
        thread::spawn(move || {
            follow_file(&path, &thread_buffer, &target, &thread_stop);
            let mut status = lock(&thread_status);
            if *status == TailStatus::Running {
                *status = TailStatus::Exited(None);
            }
        });
        LogTail {
            buffer,
            source,
            status,
            stop,
            pgid: None,
        }
    }

    /// Where the tail is.
    pub fn status(&self) -> TailStatus {
        lock(&self.status).clone()
    }

    /// Stop it: kill the command's process group, or end the file tail at its next poll.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(pgid) = self.pgid.take() {
            // SAFETY: killpg only sends a signal; the group is the tailed command's own (process_group(0)).
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }
}

impl Drop for LogTail {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Add one raw line to the buffer, masked, and send the events it fires.
fn take_line(raw: &[u8], buffer: &LogBuffer, target: &TailTarget) {
    let cut = &raw[..raw.len().min(MAX_LINE_BYTES)];
    let text = String::from_utf8_lossy(cut);
    let text = text.trim_end_matches(['\n', '\r']);
    let masked = target.redactor.redact(text).into_owned();
    for fired in match_events(&target.manifest, [masked.as_str()]) {
        let _ = target.events.send(TailEvent {
            space: target.space.clone(),
            plugin: target.manifest.name.clone(),
            pane: target.pane,
            event: fired.name,
            detail: masked.clone(),
        });
    }
    buffer.push(masked);
    target.dirty.store(true, Ordering::Relaxed);
}

fn spawn_line_reader(
    pipe: impl Read + Send + 'static,
    buffer: LogBuffer,
    target: TailTarget,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut reader = BufReader::new(pipe);
        let mut line = Vec::new();
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => take_line(&line, &buffer, &target),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
    })
}

fn follow_file(path: &Path, buffer: &LogBuffer, target: &TailTarget, stop: &AtomicBool) {
    let mut offset: Option<u64> = None;
    let mut partial: Vec<u8> = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        if let Ok(mut file) = File::open(path) {
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            let start = match offset {
                None => len.saturating_sub(FILE_TAIL_START_BYTES),
                Some(at) if at > len => {
                    partial.clear();
                    0
                }
                Some(at) => at,
            };
            if file.seek(SeekFrom::Start(start)).is_ok() {
                let mut bytes = Vec::new();
                if file.read_to_end(&mut bytes).is_ok() {
                    offset = Some(start + bytes.len() as u64);
                    partial.extend_from_slice(&bytes);
                    while let Some(end) = partial.iter().position(|&b| b == b'\n') {
                        let line: Vec<u8> = partial.drain(..=end).collect();
                        take_line(&line, buffer, target);
                    }
                    if partial.len() > MAX_LINE_BYTES {
                        let line: Vec<u8> = std::mem::take(&mut partial);
                        take_line(&line, buffer, target);
                    }
                }
            }
        }
        thread::sleep(FILE_POLL);
    }
}

// -------------------------------------------------------------------------------------------------------------
// Browser

/// Where a space's browser is, for its pane.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum BrowserPhase {
    /// Chrome has not been started yet; it starts on the first open.
    #[default]
    Idle,
    /// No Chrome or Chromium was found (set `XCTX_CHROME` to one).
    ChromeNotFound,
    /// Chrome is starting or a page is loading.
    Connecting,
    /// A page is open.
    Ready,
    /// Chrome failed or the connection dropped, with why (masked).
    Failed(String),
}

/// What a browser pane shows, updated after each page action. Cheap to read from the UI thread.
#[derive(Clone, Debug, Default)]
pub struct BrowserSnapshot {
    /// Where the browser is.
    pub phase: BrowserPhase,
    /// The page open, masked.
    pub url: Option<String>,
    /// The latest screenshot as PNG bytes.
    pub screenshot: Option<Arc<Vec<u8>>>,
    /// Counts up with every new screenshot, so the pane rebuilds its image only when it changed.
    pub screenshot_generation: u64,
    /// The page's console, masked, oldest first.
    pub console: Vec<ConsoleEntry>,
    /// The page's failed requests, masked, oldest first.
    pub network_errors: Vec<NetworkError>,
    /// Why the last action failed, masked.
    pub error: Option<String>,
}

/// The Chrome of one space, started on first use with its own profile under the data directory. Not headless:
/// the page opens in a normal Chrome window the user can use, and the pane shows its screenshot, console and
/// failed requests. Closed (Chrome quit) when the browser plugin is unmounted, the space closes, or xctx quits.
pub struct BrowserSession {
    profile_dir: PathBuf,
    browser: Option<Browser>,
    page: Option<Page>,
}

impl std::fmt::Debug for BrowserSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BrowserSession")
            .field("profile_dir", &self.profile_dir)
            .field("running", &self.browser.is_some())
            .finish()
    }
}

/// A space's browser: the session (held by one background action at a time) and its snapshot (read by the UI).
#[derive(Clone, Debug)]
pub struct BrowserHandle {
    session: Arc<Mutex<BrowserSession>>,
    snapshot: Arc<Mutex<BrowserSnapshot>>,
}

impl BrowserHandle {
    /// A browser whose Chrome will use the profile in `profile_dir`; nothing is started yet.
    pub fn new(profile_dir: PathBuf) -> BrowserHandle {
        BrowserHandle {
            session: Arc::new(Mutex::new(BrowserSession {
                profile_dir,
                browser: None,
                page: None,
            })),
            snapshot: Arc::new(Mutex::new(BrowserSnapshot::default())),
        }
    }

    /// What the pane shows now.
    pub fn snapshot(&self) -> BrowserSnapshot {
        lock(&self.snapshot).clone()
    }

    fn update(&self, change: impl FnOnce(&mut BrowserSnapshot)) {
        change(&mut lock(&self.snapshot));
    }

    /// Run `action` on the space's page, starting Chrome and opening a tab first when needed, then refresh the
    /// snapshot (screenshot, console, failed requests). Blocks: call it from the background. Errors are masked.
    pub fn with_page<T>(
        &self,
        policy: UrlPolicy,
        redactor: &Redactor,
        action: impl FnOnce(&mut Page) -> xctx_browser::Result<T>,
    ) -> Result<T, String> {
        let mut session = lock(&self.session);
        self.update(|s| {
            s.phase = BrowserPhase::Connecting;
            s.error = None;
        });
        let result = (|| {
            if session.browser.is_none() {
                let chrome = find_chrome().map_err(|e| (true, e.to_string()))?;
                let options = LaunchOptions::new(chrome, session.profile_dir.clone());
                session.browser =
                    Some(Browser::launch(options).map_err(|e| (false, e.to_string()))?);
                session.page = None;
            }
            if session.page.is_none() {
                let Some(browser) = session.browser.as_mut() else {
                    return Err((false, "Chrome is not running".to_owned()));
                };
                session.page = Some(
                    browser
                        .new_page(policy.clone())
                        .map_err(|e| (false, e.to_string()))?,
                );
            }
            let Some(page) = session.page.as_mut() else {
                return Err((false, "no page is open".to_owned()));
            };
            page.set_policy(policy);
            let value = action(page).map_err(|e| (false, e.to_string()))?;
            let url = page.url().ok();
            let png = page.screenshot().ok();
            let console = page.console().unwrap_or_default();
            let network = page.network_errors().unwrap_or_default();
            Ok((value, url, png, console, network))
        })();
        match result {
            Ok((value, url, png, console, network)) => {
                self.update(|s| {
                    s.phase = BrowserPhase::Ready;
                    s.url = url.map(|u| redactor.redact(&u).into_owned());
                    if let Some(png) = png {
                        s.screenshot = Some(Arc::new(png));
                        s.screenshot_generation += 1;
                    }
                    s.console = console
                        .into_iter()
                        .map(|c| mask_console(c, redactor))
                        .collect();
                    s.network_errors = network
                        .into_iter()
                        .map(|n| mask_network(n, redactor))
                        .collect();
                });
                Ok(value)
            }
            Err((not_found, message)) => {
                let message = redactor.redact(&message).into_owned();
                if !not_found {
                    // A broken connection is not reused: the next action starts over.
                    session.page = None;
                    session.browser = None;
                }
                self.update(|s| {
                    s.phase = if not_found {
                        BrowserPhase::ChromeNotFound
                    } else {
                        BrowserPhase::Failed(message.clone())
                    };
                    s.error = Some(message.clone());
                });
                Err(message)
            }
        }
    }

    /// Quit Chrome now unless a page action holds the session (as xctx quits, it must not wait for one).
    pub fn close_if_idle(&self) {
        if let Ok(mut session) = self.session.try_lock() {
            session.page = None;
            if let Some(browser) = session.browser.take() {
                let _ = browser.close();
            }
        }
    }

    /// Quit Chrome, if it was started. Blocks for as long as a running action holds the session.
    pub fn close(&self) {
        let mut session = lock(&self.session);
        session.page = None;
        if let Some(browser) = session.browser.take() {
            let _ = browser.close();
        }
        self.update(|s| s.phase = BrowserPhase::Idle);
    }
}

/// A console entry with its text masked.
pub fn mask_console(mut entry: ConsoleEntry, redactor: &Redactor) -> ConsoleEntry {
    entry.text = redactor.redact(&entry.text).into_owned();
    entry.location = entry.location.map(|l| redactor.redact(&l).into_owned());
    entry
}

/// A failed request with its URL and reason masked.
pub fn mask_network(mut error: NetworkError, redactor: &Redactor) -> NetworkError {
    error.url = redactor.redact(&error.url).into_owned();
    error.reason = redactor.redact(&error.reason).into_owned();
    error
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::time::Instant;

    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn manifest() -> Result<Manifest, xctx_plugin::PluginError> {
        Manifest::parse(
            r#"
name = "app-logs"
version = "1.0.0"
description = "logs"
[pane]
kind = "logs"
[[events]]
name = "app.crash"
match = { regex = "panicked" }
action = "fail"
"#,
            "test",
        )
    }

    fn target(
        dirty: Arc<AtomicBool>,
    ) -> Result<(TailTarget, mpsc::Receiver<TailEvent>), Box<dyn std::error::Error>> {
        let (tx, rx) = mpsc::channel();
        Ok((
            TailTarget {
                space: SpaceId::new("s"),
                pane: PaneId(3),
                manifest: manifest()?,
                redactor: Redactor::new([("TOKEN".to_owned(), "hunter22".to_owned())]),
                events: tx,
                dirty,
            },
            rx,
        ))
    }

    fn wait_for(mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if done() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn a_command_tail_masks_lines_and_matches_events() -> TestResult {
        let dir = tempfile::tempdir()?;
        let dirty = Arc::new(AtomicBool::new(false));
        let (target, events) = target(Arc::clone(&dirty))?;
        let buffer = LogBuffer::new(100);
        let argv: Vec<String> = [
            "/bin/sh",
            "-c",
            "echo token hunter22; echo thread panicked >&2",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let tail = LogTail::command(&argv, dir.path(), &BTreeMap::new(), buffer.clone(), target)?;
        assert!(wait_for(|| matches!(tail.status(), TailStatus::Exited(_))));
        let lines: Vec<String> = buffer.snapshot().into_iter().map(|l| l.text).collect();
        assert!(
            lines.contains(&"token [REDACTED:TOKEN]".to_owned()),
            "{lines:?}"
        );
        assert!(lines.contains(&"thread panicked".to_owned()), "{lines:?}");
        let event = events.recv_timeout(Duration::from_secs(5))?;
        assert_eq!(event.event, "app.crash");
        assert_eq!(event.pane, PaneId(3));
        assert!(dirty.load(Ordering::Relaxed));
        Ok(())
    }

    #[test]
    fn a_file_tail_follows_appends() -> TestResult {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("app.log");
        std::fs::write(&path, "first\n")?;
        let (target, _events) = target(Arc::new(AtomicBool::new(false)))?;
        let buffer = LogBuffer::new(100);
        let mut tail = LogTail::file(path.clone(), buffer.clone(), target);
        assert!(wait_for(|| buffer.len() == 1));
        let mut file = std::fs::OpenOptions::new().append(true).open(&path)?;
        std::io::Write::write_all(&mut file, b"second\nthird")?;
        assert!(wait_for(|| buffer.len() == 2));
        std::io::Write::write_all(&mut file, b"\n")?;
        assert!(wait_for(|| buffer.len() == 3));
        tail.stop();
        let lines: Vec<String> = buffer.snapshot().into_iter().map(|l| l.text).collect();
        assert_eq!(lines, ["first", "second", "third"]);
        Ok(())
    }

    #[test]
    fn a_command_that_cannot_start_is_an_error() -> TestResult {
        let dir = tempfile::tempdir()?;
        let (target, _events) = target(Arc::new(AtomicBool::new(false)))?;
        let argv = vec![dir.path().join("missing").display().to_string()];
        let result = LogTail::command(
            &argv,
            dir.path(),
            &BTreeMap::new(),
            LogBuffer::new(1),
            target,
        );
        assert!(result.is_err());
        Ok(())
    }
}
