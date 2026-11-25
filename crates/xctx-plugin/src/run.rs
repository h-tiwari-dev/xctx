//! Running a planned tool call as a child process: no shell, its own process group, capped output, and secrets
//! masked.
//!
//! The tool runs in a new process group. Once its output is drained (both pipes closed, or the deadline and a
//! short grace passed) the whole group is killed, so anything it left running (`cmd &`, `nohup cmd`) ends with
//! the call. The tool itself is reaped only after that kill: while it is an unreaped zombie its pid, and so
//! the group's id, cannot be reused, so the kill never reaches an unrelated process. Output is read on the
//! calling thread with `poll`, so no reader thread outlives the call.
//!
//! A process that leaves the group (`setsid`, `setpgid`) escapes the kill; if it holds the output open, the
//! read stops at the deadline plus the grace and its pipes are closed on our side.

use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde::Serialize;
use xctx_core::Redactor;

use crate::call::{CallPlan, Invocation};
use crate::error::{PluginError, Result};

/// Bytes read past the cap, so a secret of up to this length straddling the cap is still seen whole.
const MASK_MARGIN: usize = 64 * 1024;

/// The window first searched for a secret straddling the cut (secrets are rarely longer); a longer one is
/// still found by a second search over [`MASK_MARGIN`].
const SECRET_WINDOW: usize = 4 * 1024;

/// How often the child is polled.
const POLL: Duration = Duration::from_millis(10);

/// How long to wait for the output pipes to close after the group was killed.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// What a tool run produced.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ToolOutput {
    /// The exit code; None when the tool was killed by a signal (including on timeout).
    pub status: Option<i32>,
    /// Its standard output, masked, at most the cap in bytes (lossy UTF-8).
    pub stdout: String,
    /// Its standard error, masked, at most the cap in bytes (lossy UTF-8).
    pub stderr: String,
    /// True when either stream was cut at the cap.
    pub truncated: bool,
    /// True when it ran past the timeout and was killed, or exited but left a process holding its output open
    /// past the timeout, which was killed.
    pub timed_out: bool,
}

impl ToolOutput {
    /// True when it exited with status 0.
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }

    /// Every line of stdout then stderr, for event matching.
    pub fn lines(&self) -> impl Iterator<Item = &str> {
        self.stdout.lines().chain(self.stderr.lines())
    }
}

/// Run a [`Invocation::Run`] plan: `argv` without a shell, in `plan.cwd`, with `plan.env` on top of xctx's own
/// environment and stdin closed. The tool and everything it starts are killed after `timeout`. Each of stdout and
/// stderr is capped at `max_output_bytes` and masked with `plan.redactor`.
///
/// A built-in plan is refused with [`PluginError::Builtin`]: the app routes those to its own implementations.
/// A non-zero exit is not an error; it is in [`ToolOutput::status`].
pub fn run_plan(plan: &CallPlan, timeout: Duration, max_output_bytes: usize) -> Result<ToolOutput> {
    let argv = match &plan.invocation {
        Invocation::Run { argv } => argv,
        Invocation::Builtin { name, .. } => {
            return Err(PluginError::Builtin {
                tool: plan.tool.clone(),
                builtin: name.clone(),
            });
        }
    };
    let Some((program, args)) = argv.split_first() else {
        return Err(PluginError::Run {
            program: String::new(),
            message: "the argv is empty".to_owned(),
        });
    };
    let run_err = |message: String| PluginError::Run {
        program: program.clone(),
        message,
    };

    let mut child = Command::new(program)
        .args(args)
        .current_dir(&plan.cwd)
        .envs(&plan.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|source| PluginError::Spawn {
            program: program.clone(),
            source,
        })?;
    let pid = child.id() as libc::pid_t;

    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let keep = max_output_bytes.saturating_add(MASK_MARGIN);
    let mut stdout = Captured::default();
    let mut stderr = Captured::default();

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut exited = false;
    let mut killed_at: Option<Instant> = None;
    loop {
        if !exited {
            match exited_unreaped(pid) {
                Ok(done) => exited = done,
                Err(e) => {
                    kill_group(pid);
                    let _ = child.wait();
                    return Err(run_err(format!("waiting for it failed: {e}")));
                }
            }
        }
        if exited && stdout_pipe.is_none() && stderr_pipe.is_none() {
            break;
        }
        let now = Instant::now();
        match killed_at {
            // Still running, or something it started still holds its output: kill the group.
            None if now >= deadline => {
                timed_out = true;
                kill_group(pid);
                killed_at = Some(now);
            }
            // What still holds the output left the group; stop reading.
            Some(at) if now >= at + DRAIN_GRACE => break,
            _ => {}
        }
        read_ready(
            &mut stdout_pipe,
            &mut stdout,
            &mut stderr_pipe,
            &mut stderr,
            keep,
        );
    }

    // Whatever the tool left running goes with it; then reap it.
    kill_group(pid);
    drop(stdout_pipe);
    drop(stderr_pipe);
    let status = reap(&mut child).map_err(|e| run_err(format!("waiting for it failed: {e}")))?;

    let (stdout, cut_out) = stdout.finish(&plan.redactor, max_output_bytes);
    let (stderr, cut_err) = stderr.finish(&plan.redactor, max_output_bytes);
    Ok(ToolOutput {
        status: status.code(),
        stdout,
        stderr,
        truncated: cut_out || cut_err,
        timed_out,
    })
}

/// SIGKILL the tool's process group, and the tool itself in case it left the group. Only called while the
/// tool is not reaped yet, so `pid` (and the group id, which is the same) still belong to it.
fn kill_group(pid: libc::pid_t) {
    // SAFETY: kill and killpg only send a signal; `pid` is our unreaped child and the leader of its own group
    // (`process_group(0)`), so neither id can have been reused.
    unsafe {
        libc::killpg(pid, libc::SIGKILL);
        libc::kill(pid, libc::SIGKILL);
    }
}

/// True once the child `pid` has exited, without reaping it (`WNOWAIT`).
fn exited_unreaped(pid: libc::pid_t) -> std::io::Result<bool> {
    loop {
        // SAFETY: an all-zero siginfo_t is a valid value; waitid only writes into it.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid, writable siginfo_t, and `pid` is our own child.
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
            )
        };
        if rc == 0 {
            // SAFETY: waitid filled `info` (or left it zeroed when nothing exited).
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// Reap the child, which was killed or has exited.
fn reap(child: &mut Child) -> std::io::Result<std::process::ExitStatus> {
    child.wait()
}

/// Wait up to [`POLL`] for output on the open pipes and read what came; a pipe that reached its end (or
/// failed) is closed and set to None.
fn read_ready(
    out_pipe: &mut Option<ChildStdout>,
    out: &mut Captured,
    err_pipe: &mut Option<ChildStderr>,
    err: &mut Captured,
    keep: usize,
) {
    let mut fds: Vec<libc::pollfd> = Vec::with_capacity(2);
    for fd in [
        out_pipe.as_ref().map(|p| p.as_raw_fd()),
        err_pipe.as_ref().map(|p| p.as_raw_fd()),
    ]
    .into_iter()
    .flatten()
    {
        fds.push(libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        });
    }
    if fds.is_empty() {
        std::thread::sleep(POLL);
        return;
    }
    let timeout_ms = POLL.as_millis() as libc::c_int;
    // SAFETY: `fds` is a valid array of `fds.len()` pollfd structs for the duration of the call.
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
    if rc <= 0 {
        return;
    }
    let ready = |fd: libc::c_int| {
        fds.iter().any(|p| {
            p.fd == fd
                && p.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
        })
    };
    if out_pipe.as_ref().is_some_and(|p| ready(p.as_raw_fd()))
        && let Some(p) = out_pipe
        && !out.read_from(p, keep)
    {
        *out_pipe = None;
    }
    if err_pipe.as_ref().is_some_and(|p| ready(p.as_raw_fd()))
        && let Some(p) = err_pipe
        && !err.read_from(p, keep)
    {
        *err_pipe = None;
    }
}

/// What was kept of one stream.
#[derive(Default)]
struct Captured {
    bytes: Vec<u8>,
    total: usize,
}

impl Captured {
    /// Read once from a pipe that is ready. False when it reached its end or failed.
    fn read_from(&mut self, pipe: &mut impl Read, keep: usize) -> bool {
        let mut chunk = [0u8; 8192];
        match pipe.read(&mut chunk) {
            Ok(0) => false,
            Ok(n) => {
                self.total = self.total.saturating_add(n);
                let room = keep.saturating_sub(self.bytes.len());
                self.bytes.extend_from_slice(&chunk[..n.min(room)]);
                true
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) =>
            {
                true
            }
            Err(_) => false,
        }
    }

    /// The masked text, at most `cap` bytes, and whether anything was cut.
    ///
    /// When more was read than kept, the raw text is first cut where no secret straddles the cut (see
    /// [`safe_cut`]), so no prefix of a secret is left unmasked at the end; only then is it masked and cut to
    /// `cap` (a cut there can only fall in text that is already masked or in no secret).
    fn finish(self, redactor: &Redactor, cap: usize) -> (String, bool) {
        let text = String::from_utf8_lossy(&self.bytes);
        let raw_cut = self.total > self.bytes.len();
        let text = if raw_cut {
            &text[..safe_cut(&text, redactor)]
        } else {
            &text[..]
        };
        let mut masked = redactor.redact(text).into_owned();
        let mut cut = raw_cut;
        if masked.len() > cap {
            masked.truncate(floor_boundary(&masked, cap));
            cut = true;
        }
        (masked, cut)
    }
}

/// The largest char boundary of `s` at or below `i`.
fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The smallest char boundary of `s` at or above `i`.
fn ceil_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// True when a secret's whole occurrence in `text` within `window` bytes of `k` crosses `k`: masking the two
/// sides apart then differs from masking them together.
fn straddles(text: &str, k: usize, window: usize, redactor: &Redactor) -> bool {
    let from = floor_boundary(text, k.saturating_sub(window));
    let to = ceil_boundary(text, k.saturating_add(window));
    let whole = redactor.redact(&text[from..to]);
    let apart = format!(
        "{}{}",
        redactor.redact(&text[from..k]),
        redactor.redact(&text[k..to])
    );
    whole != apart
}

/// Where to cut `text`, which ends where reading stopped (maybe inside a secret): at least [`MASK_MARGIN`]
/// before its end, so every secret of up to that length near the cut is seen whole, and where no secret
/// straddles the cut. 0 when no such place is found.
fn safe_cut(text: &str, redactor: &Redactor) -> usize {
    let mut k = floor_boundary(text, text.len().saturating_sub(MASK_MARGIN));
    if redactor.is_empty() {
        return k;
    }
    for _ in 0..(2 * MASK_MARGIN / SECRET_WINDOW) {
        // Step back over a usual (short) secret one character at a time.
        let mut steps = 0;
        while k > 0 && steps < SECRET_WINDOW && straddles(text, k, SECRET_WINDOW, redactor) {
            k = floor_boundary(text, k - 1);
            steps += 1;
        }
        if k == 0 {
            return 0;
        }
        // Inside a long secret (or a run of them): jump back a window and look again.
        if steps == SECRET_WINDOW || straddles(text, k, MASK_MARGIN, redactor) {
            k = floor_boundary(text, k.saturating_sub(SECRET_WINDOW));
            continue;
        }
        return k;
    }
    0
}
