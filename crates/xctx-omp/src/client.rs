//! The `omp` child process: spawn it, shake hands, write commands and read frames, all on plain threads.
//!
//! One thread reads omp's output and decodes it (so the pane never blocks omp: it spills to disk under
//! backpressure, but a reader that stops reading stalls its exit), one thread writes commands. Frames reach the
//! host over a channel as [`Incoming`].

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command as Process, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::codec::{Decoder, encode_line};
use crate::command::Command;
use crate::error::{OmpError, Result};
use crate::frame::{Frame, Ready};
use crate::model::SessionModel;

/// How long omp gets to exit after its input closes before it is killed.
const EXIT_GRACE: Duration = Duration::from_secs(5);

/// How much of omp's error output is kept for the message when it exits.
const STDERR_KEEP: usize = 4096;

/// How to start `omp`.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    /// The program, a name looked up on the child's `PATH` or an absolute path.
    pub program: PathBuf,
    /// Its arguments, without `--mode rpc-ui` (added by [`OmpClient::start`]).
    pub args: Vec<OsString>,
    /// The folder it works in.
    pub cwd: PathBuf,
    /// Environment variables to set.
    pub env: Vec<(OsString, OsString)>,
    /// Environment variables to remove (an outer xctx's agent identity, for one).
    pub env_remove: Vec<OsString>,
    /// How long each handshake step may take.
    pub handshake_timeout: Duration,
}

impl LaunchSpec {
    /// A spec that runs `program` in `cwd` with the default 30 second handshake timeout.
    pub fn new(program: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        LaunchSpec {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            env: Vec::new(),
            env_remove: Vec::new(),
            handshake_timeout: Duration::from_secs(30),
        }
    }
}

/// Something that happened on the child.
#[derive(Debug)]
pub enum Incoming {
    /// A decoded frame.
    Frame(Frame),
    /// A line or chunk run that could not be decoded; reading goes on.
    Malformed(String),
    /// The child exited (its output closed).
    Exited {
        /// The exit code, `None` when a signal ended it.
        code: Option<i32>,
        /// The end of its error output.
        stderr: String,
    },
}

/// A running `omp --mode rpc-ui` child.
#[derive(Debug)]
pub struct OmpClient {
    commands: Option<Sender<String>>,
    child: Arc<Mutex<Child>>,
}

/// A started child: the client, the channel its frames arrive on, and the frames the handshake already read
/// (apply them to the model first, in order).
#[derive(Debug)]
pub struct Started {
    /// The client.
    pub client: OmpClient,
    /// Everything after the handshake.
    pub frames: Receiver<Incoming>,
    /// The frames read during the handshake, `ready` first.
    pub startup: Vec<Frame>,
}

impl OmpClient {
    /// Start omp, wait for `ready`, negotiate protocol v2 when omp offers it, ask for message deltas, and send
    /// the commands that give the model its first picture ([`SessionModel::startup_commands`]).
    ///
    /// Blocks up to the spec's handshake timeout per step: call it off the UI thread.
    pub fn start(spec: &LaunchSpec) -> Result<Started> {
        let mut process = Process::new(&spec.program);
        process
            .arg("--mode")
            .arg("rpc-ui")
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &spec.env {
            process.env(key, value);
        }
        for key in &spec.env_remove {
            process.env_remove(key);
        }
        let mut child = process.spawn().map_err(|source| OmpError::Spawn {
            program: spec.program.display().to_string(),
            source,
        })?;
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(OmpError::Closed);
        };
        let child = Arc::new(Mutex::new(child));
        let tail = Arc::new(Mutex::new(String::new()));
        spawn_stderr(stderr, Arc::clone(&tail));

        let (command_tx, command_rx) = mpsc::channel::<String>();
        thread::spawn(move || write_commands(stdin, &command_rx));

        let (frame_tx, frame_rx) = mpsc::channel::<Incoming>();
        let max = Arc::new(Mutex::new(crate::codec::DEFAULT_MAX_REASSEMBLED));
        spawn_reader(stdout, frame_tx, Arc::clone(&child), tail, Arc::clone(&max));

        let client = OmpClient {
            commands: Some(command_tx),
            child,
        };
        let mut startup = Vec::new();
        let timeout = spec.handshake_timeout;

        let ready = wait_for(&frame_rx, timeout, "ready", &mut startup, |f| {
            matches!(f, Frame::Ready(_))
        })?;
        let Frame::Ready(ready) = ready else {
            return Err(OmpError::ExitedEarly);
        };
        set_limit(&max, &ready);
        if ready.supported.contains(&2) {
            client.send(&Command::negotiate_protocol().with_id("handshake-protocol"))?;
            wait_for(
                &frame_rx,
                timeout,
                "negotiate_protocol",
                &mut startup,
                |f| matches!(f, Frame::Response(r) if r.command == "negotiate_protocol"),
            )?;
        }
        client.send(&Command::set_event_filter_delta().with_id("handshake-filter"))?;
        for command in SessionModel::startup_commands() {
            client.send(&command)?;
        }
        Ok(Started {
            client,
            frames: frame_rx,
            startup,
        })
    }

    /// Write one command. Returns before omp answers; the answer arrives as a frame.
    pub fn send(&self, command: &Command) -> Result<()> {
        let line = encode_line(command.as_value());
        self.commands
            .as_ref()
            .ok_or(OmpError::Closed)?
            .send(line)
            .map_err(|_| OmpError::Closed)
    }

    /// Close omp's input so it shuts down, as omp does on end of input. Frames keep arriving until it exits.
    pub fn close_input(&mut self) {
        self.commands = None;
    }

    /// Stop the child now.
    pub fn kill(&self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
        }
    }
}

impl Drop for OmpClient {
    fn drop(&mut self) {
        self.commands = None;
        let child = Arc::clone(&self.child);
        thread::spawn(move || {
            let deadline = Instant::now() + EXIT_GRACE;
            while Instant::now() < deadline {
                match child.lock() {
                    Ok(mut c) => match c.try_wait() {
                        Ok(Some(_)) | Err(_) => return,
                        Ok(None) => {}
                    },
                    Err(_) => return,
                }
                thread::sleep(Duration::from_millis(50));
            }
            if let Ok(mut c) = child.lock() {
                let _ = c.kill();
                let _ = c.wait();
            }
        });
    }
}

fn set_limit(max: &Arc<Mutex<usize>>, ready: &Ready) {
    if let Ok(mut m) = max.lock() {
        *m = ready.max_reassembled_bytes;
    }
}

fn wait_for(
    rx: &Receiver<Incoming>,
    timeout: Duration,
    step: &'static str,
    seen: &mut Vec<Frame>,
    done: impl Fn(&Frame) -> bool,
) -> Result<Frame> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(Incoming::Frame(frame)) => {
                let hit = done(&frame);
                seen.push(frame.clone());
                if hit {
                    return Ok(frame);
                }
            }
            Ok(Incoming::Malformed(_)) => {}
            Ok(Incoming::Exited { .. }) | Err(RecvTimeoutError::Disconnected) => {
                return Err(OmpError::ExitedEarly);
            }
            Err(RecvTimeoutError::Timeout) => return Err(OmpError::Timeout(step)),
        }
    }
}

fn spawn_stderr(mut stderr: impl Read + Send + 'static, tail: Arc<Mutex<String>>) {
    thread::spawn(move || {
        let mut buf = [0u8; 1024];
        while let Ok(n) = stderr.read(&mut buf) {
            if n == 0 {
                break;
            }
            if let Ok(mut t) = tail.lock() {
                t.push_str(&String::from_utf8_lossy(&buf[..n]));
                if t.len() > STDERR_KEEP {
                    let cut = t.len() - STDERR_KEEP;
                    let at = (cut..t.len()).find(|i| t.is_char_boundary(*i)).unwrap_or(0);
                    t.drain(..at);
                }
            }
        }
    });
}

fn write_commands(mut stdin: impl Write, rx: &Receiver<String>) {
    while let Ok(line) = rx.recv() {
        if stdin.write_all(line.as_bytes()).is_err() || stdin.flush().is_err() {
            break;
        }
    }
}

fn spawn_reader(
    stdout: impl Read + Send + 'static,
    tx: Sender<Incoming>,
    child: Arc<Mutex<Child>>,
    tail: Arc<Mutex<String>>,
    max: Arc<Mutex<usize>>,
) {
    thread::spawn(move || {
        let mut decoder = Decoder::default();
        let mut limit = crate::codec::DEFAULT_MAX_REASSEMBLED;
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            if let Ok(m) = max.lock()
                && *m != limit
            {
                limit = *m;
                decoder = Decoder::new(limit);
            }
            let incoming = match decoder.push_line(&line) {
                Ok(Some(value)) => Incoming::Frame(Frame::parse(value)),
                Ok(None) => continue,
                Err(e) => Incoming::Malformed(e.to_string()),
            };
            if tx.send(incoming).is_err() {
                return;
            }
        }
        let code = reap(&child);
        let stderr = tail.lock().map(|t| t.clone()).unwrap_or_default();
        let _ = tx.send(Incoming::Exited { code, stderr });
    });
}

fn reap(child: &Arc<Mutex<Child>>) -> Option<i32> {
    let deadline = Instant::now() + EXIT_GRACE;
    loop {
        if let Ok(mut c) = child.lock() {
            match c.try_wait() {
                Ok(Some(status)) => return status.code(),
                Err(_) => return None,
                Ok(None) => {
                    if Instant::now() >= deadline {
                        let _ = c.kill();
                        return c.wait().ok().and_then(|s| s.code());
                    }
                }
            }
        } else {
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}
