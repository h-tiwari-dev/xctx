//! Running a planned tool call as a child process: no shell, its own process group (killed whole on timeout),
//! capped output, and secrets masked.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::call::{CallPlan, Invocation};
use crate::error::{PluginError, Result};

/// Bytes read past the cap before masking, so a secret straddling the cap is still masked whole.
const MASK_MARGIN: usize = 64 * 1024;

/// How often the child is polled.
const POLL: Duration = Duration::from_millis(10);

/// How long to wait for the output pipes to close after the child was killed.
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
    let pgid = child.id() as libc::pid_t;
    let kill_group = || {
        // SAFETY: killpg only sends a signal; the group is the child's own (process_group(0) above).
        unsafe {
            libc::killpg(pgid, libc::SIGKILL);
        }
    };

    let keep = max_output_bytes.saturating_add(MASK_MARGIN);
    let (tx, rx) = mpsc::channel::<(bool, Captured)>();
    let mut readers = 0;
    if let Some(out) = child.stdout.take() {
        readers += 1;
        spawn_reader(out, keep, true, tx.clone());
    }
    if let Some(err) = child.stderr.take() {
        readers += 1;
        spawn_reader(err, keep, false, tx.clone());
    }
    drop(tx);

    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => {
                kill_group();
                let _ = child.wait();
                return Err(run_err(format!("waiting for it failed: {e}")));
            }
        }
        if Instant::now() >= deadline {
            timed_out = true;
            kill_group();
            match child.wait() {
                Ok(status) => break status,
                Err(e) => {
                    return Err(run_err(format!(
                        "waiting for it after the timeout failed: {e}"
                    )));
                }
            }
        }
        thread::sleep(POLL);
    };

    // The pipes stay open while anything the tool started still holds them: wait for that until the deadline,
    // then kill the group.
    let mut stdout = Captured::default();
    let mut stderr = Captured::default();
    let mut killed_leftovers = timed_out;
    while readers > 0 {
        let wait = if killed_leftovers {
            DRAIN_GRACE
        } else {
            deadline.saturating_duration_since(Instant::now())
        };
        match rx.recv_timeout(wait) {
            Ok((is_stdout, captured)) => {
                readers -= 1;
                if is_stdout {
                    stdout = captured;
                } else {
                    stderr = captured;
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) if !killed_leftovers => {
                timed_out = true;
                killed_leftovers = true;
                kill_group();
            }
            Err(_) => break,
        }
    }

    let (stdout, cut_out) = stdout.finish(plan, max_output_bytes);
    let (stderr, cut_err) = stderr.finish(plan, max_output_bytes);
    Ok(ToolOutput {
        status: status.code(),
        stdout,
        stderr,
        truncated: cut_out || cut_err,
        timed_out,
    })
}

/// What a reader kept of one stream.
#[derive(Default)]
struct Captured {
    bytes: Vec<u8>,
    total: usize,
}

impl Captured {
    /// The masked text, cut to `cap` bytes on a character boundary, and whether anything was cut.
    fn finish(self, plan: &CallPlan, cap: usize) -> (String, bool) {
        let text = String::from_utf8_lossy(&self.bytes);
        let mut masked = plan.redactor.redact(&text).into_owned();
        let mut cut = self.total > cap;
        if masked.len() > cap {
            let mut end = cap;
            while !masked.is_char_boundary(end) {
                end -= 1;
            }
            masked.truncate(end);
            cut = true;
        }
        (masked, cut)
    }
}

fn spawn_reader(
    mut pipe: impl Read + Send + 'static,
    keep: usize,
    is_stdout: bool,
    tx: mpsc::Sender<(bool, Captured)>,
) {
    thread::spawn(move || {
        let mut captured = Captured::default();
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    captured.total = captured.total.saturating_add(n);
                    let room = keep.saturating_sub(captured.bytes.len());
                    captured.bytes.extend_from_slice(&chunk[..n.min(room)]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = tx.send((is_stdout, captured));
    });
}
