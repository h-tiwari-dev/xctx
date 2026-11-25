//! What keeps a mounted logs pane going without a window: the tail behind it (a command or a file, into a
//! bounded [`LogBuffer`]).
//!
//! A tail runs on its own thread and hands what the UI needs to the workspace through shared state: the buffer
//! itself, a dirty flag the workspace polls to redraw, and a channel of the plugin events matched in the tailed
//! lines. Nothing here is run inside a shell. (The browser pane's live page lives in the window: see
//! `plugin_browser.rs`.)

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

/// The most bytes of a tailed file read in one poll.
pub const FILE_READ_BYTES: u64 = 1024 * 1024;

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

    /// Run `argv` (no shell) in `cwd` with exactly `env` as its environment (nothing of xctx's own is inherited;
    /// see `plugin_host::isolated_env`), its stdout and stderr tailed into `buffer`. Fails when it cannot be
    /// started.
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
            .env_clear()
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
        read_lines(pipe, |line| take_line(line, &buffer, &target));
    })
}

/// Read `pipe` line by line until it ends, giving each line (without its newline) to `take`. At most
/// [`MAX_LINE_BYTES`] of a line are kept: the rest of it is read and dropped as it arrives, so a stream that never
/// sends a newline cannot grow memory without bound. A last line without a newline is given at the end.
pub fn read_lines(pipe: impl Read, mut take: impl FnMut(&[u8])) {
    let mut reader = BufReader::new(pipe);
    let mut line: Vec<u8> = Vec::new();
    // True while the rest of a line that was already cut is skipped.
    let mut skipping = false;
    loop {
        let available = match reader.fill_buf() {
            Ok([]) => break,
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let newline = available.iter().position(|&b| b == b'\n');
        let chunk = &available[..newline.unwrap_or(available.len())];
        if !skipping {
            let room = MAX_LINE_BYTES.saturating_sub(line.len());
            line.extend_from_slice(&chunk[..chunk.len().min(room)]);
        }
        let used = newline.map_or(available.len(), |at| at + 1);
        reader.consume(used);
        if newline.is_some() {
            if !skipping {
                take(&line);
            }
            line.clear();
            skipping = false;
        } else if !skipping && line.len() >= MAX_LINE_BYTES {
            take(&line);
            line.clear();
            skipping = true;
        }
    }
    if !skipping && !line.is_empty() {
        take(&line);
    }
}

fn follow_file(path: &Path, buffer: &LogBuffer, target: &TailTarget, stop: &AtomicBool) {
    let mut offset: Option<u64> = None;
    let mut partial: Vec<u8> = Vec::new();
    let mut skipping = false;
    while !stop.load(Ordering::Relaxed) {
        if let Ok(mut file) = File::open(path) {
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            let start = match offset {
                None => len.saturating_sub(FILE_TAIL_START_BYTES),
                Some(at) if at > len => {
                    partial.clear();
                    skipping = false;
                    0
                }
                Some(at) => at,
            };
            if file.seek(SeekFrom::Start(start)).is_ok() {
                // At most FILE_READ_BYTES a poll, so a file that grows fast is read in steps.
                let mut bytes = Vec::new();
                if (&mut file)
                    .take(FILE_READ_BYTES)
                    .read_to_end(&mut bytes)
                    .is_ok()
                {
                    offset = Some(start + bytes.len() as u64);
                    for &b in &bytes {
                        if b == b'\n' {
                            if !skipping {
                                take_line(&partial, buffer, target);
                            }
                            partial.clear();
                            skipping = false;
                        } else if !skipping {
                            partial.push(b);
                            if partial.len() >= MAX_LINE_BYTES {
                                take_line(&partial, buffer, target);
                                partial.clear();
                                skipping = true;
                            }
                        }
                    }
                }
            }
        }
        thread::sleep(FILE_POLL);
    }
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
    fn a_line_without_a_newline_is_cut_and_the_rest_dropped() {
        let mut long = vec![b'x'; MAX_LINE_BYTES * 5];
        long.extend_from_slice(b"\nnext\nlast");
        let mut lines: Vec<Vec<u8>> = Vec::new();
        read_lines(long.as_slice(), |l| lines.push(l.to_vec()));
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].len(), MAX_LINE_BYTES);
        assert_eq!(lines[1], b"next");
        assert_eq!(lines[2], b"last");
        // A stream that never sends a newline keeps one capped line and nothing else.
        let endless = std::io::repeat(b'y').take(MAX_LINE_BYTES as u64 * 64);
        let mut count = 0;
        let mut longest = 0;
        read_lines(endless, |l| {
            count += 1;
            longest = longest.max(l.len());
        });
        assert_eq!((count, longest), (1, MAX_LINE_BYTES));
    }

    #[test]
    fn a_tailed_command_does_not_inherit_xctxs_environment() -> TestResult {
        let Ok(inherited) = std::env::var("CARGO_MANIFEST_DIR") else {
            eprintln!("skipping: CARGO_MANIFEST_DIR is not set");
            return Ok(());
        };
        let dir = tempfile::tempdir()?;
        let (target, _events) = target(Arc::new(AtomicBool::new(false)))?;
        let buffer = LogBuffer::new(10);
        let env: BTreeMap<String, String> = [("PATH", "/usr/bin:/bin"), ("PORT", "4011")]
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .into();
        let argv: Vec<String> = ["/bin/sh", "-c", "echo \"[$CARGO_MANIFEST_DIR] $PORT\""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let tail = LogTail::command(&argv, dir.path(), &env, buffer.clone(), target)?;
        assert!(wait_for(|| matches!(tail.status(), TailStatus::Exited(_))));
        let lines: Vec<String> = buffer.snapshot().into_iter().map(|l| l.text).collect();
        assert_eq!(lines, ["[] 4011"], "{inherited}");
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
