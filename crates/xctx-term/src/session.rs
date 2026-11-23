//! A running program on a PTY, with its emulator and the threads that connect them.

use std::fs::File;
use std::io::{ErrorKind, Read, Write};
use std::process::{Child, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::sync::FairMutex;
use polling::{Event, Events, PollMode, Poller};

use crate::config::TerminalConfig;
use crate::emulator::{Emulator, SelectionMode, Side, TerminalEvent};
use crate::error::TermError;
use crate::pty;
use crate::snapshot::{GridSnapshot, TerminalModes, ViewportPoint};

/// Poller key of the PTY master.
const PTY_KEY: usize = 0;

/// Bytes read from the PTY per parse.
const READ_BUFFER_SIZE: usize = 64 * 1024;

/// How long a program gets to exit after SIGHUP before the session kills its process group.
const KILL_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Default)]
struct ProcessState {
    exited: bool,
    status: Option<ExitStatus>,
}

type EventCallback = Box<dyn Fn(TerminalEvent) + Send + Sync + 'static>;

struct Shared {
    emulator: FairMutex<Emulator>,
    process: Mutex<ProcessState>,
    process_exited: Condvar,
    shutdown: AtomicBool,
    on_event: EventCallback,
    pid: u32,
}

impl Shared {
    fn process(&self) -> MutexGuard<'_, ProcessState> {
        self.process.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn emit(&self, event: TerminalEvent) {
        (self.on_event)(event);
    }
}

/// A program running on a pseudo-terminal, with an emulator tracking its screen.
///
/// Three background threads serve a session: a reader that parses program output into the
/// emulator, a writer that forwards input, and a waiter that collects the exit status. The
/// session reports changes through the callback given to [`TerminalSession::spawn`], which is
/// called on those threads; it should only schedule work (for example a redraw), and it may
/// call back into the session. Methods the owner calls do not fire the callback.
///
/// Dropping the session stops the reader and writer, sends SIGHUP to the program's process
/// group, and kills the group with SIGKILL if it has not exited within two seconds. The exit
/// status is always collected, so no zombie is left behind. Dropping never blocks.
pub struct TerminalSession {
    shared: Arc<Shared>,
    writer: Option<Sender<Vec<u8>>>,
    poller: Arc<Poller>,
    master: File,
}

impl TerminalSession {
    /// Starts `config.program` on a new PTY.
    ///
    /// `on_event` receives [`TerminalEvent`]s from background threads: `Wakeup` whenever a new
    /// snapshot would differ, and title, bell, clipboard and exit events.
    pub fn spawn(
        config: TerminalConfig,
        on_event: impl Fn(TerminalEvent) + Send + Sync + 'static,
    ) -> Result<Self, TermError> {
        let poller = Arc::new(Poller::new().map_err(TermError::Poll)?);
        let pty::SpawnedPty { master, child } = pty::spawn(&config)?;
        let pid = child.id();
        let shared = Arc::new(Shared {
            emulator: FairMutex::new(Emulator::new(&config)),
            process: Mutex::new(ProcessState::default()),
            process_exited: Condvar::new(),
            shutdown: AtomicBool::new(false),
            on_event: Box::new(on_event),
            pid,
        });

        spawn_waiter(Arc::clone(&shared), child)?;

        // From here on, dropping `session` on an error path hangs up and reaps the program.
        let mut session = TerminalSession {
            shared: Arc::clone(&shared),
            writer: None,
            poller: Arc::clone(&poller),
            master,
        };

        let (writer, writer_rx) = mpsc::channel::<Vec<u8>>();
        let writer_file = session
            .master
            .try_clone()
            .map_err(TermError::ConfigurePty)?;
        thread::Builder::new()
            .name(format!("xctx-term writer {pid}"))
            .spawn(move || run_writer(writer_file, writer_rx))
            .map_err(TermError::Thread)?;
        session.writer = Some(writer.clone());

        let reader_file = Arc::new(
            session
                .master
                .try_clone()
                .map_err(TermError::ConfigurePty)?,
        );
        // SAFETY: `reader_file` stays open until it is removed from the poller: by the reader
        // thread when it finishes, or below if the thread cannot start.
        unsafe { poller.add_with_mode(&*reader_file, Event::readable(PTY_KEY), PollMode::Level) }
            .map_err(TermError::Poll)?;
        let reader_poller = Arc::clone(&poller);
        let thread_file = Arc::clone(&reader_file);
        let spawned = thread::Builder::new()
            .name(format!("xctx-term reader {pid}"))
            .spawn(move || run_reader(shared, reader_poller, thread_file, writer));
        if let Err(err) = spawned {
            let _ = poller.delete(&*reader_file);
            return Err(TermError::Thread(err));
        }

        Ok(session)
    }

    /// The program's process id. It also leads the program's process group.
    pub fn pid(&self) -> u32 {
        self.shared.pid
    }

    /// Queues bytes for the program's input. Never blocks; fails with [`TermError::Closed`]
    /// once the program has gone and its input can no longer be written.
    pub fn write(&self, bytes: &[u8]) -> Result<(), TermError> {
        if bytes.is_empty() {
            return Ok(());
        }
        let writer = self.writer.as_ref().ok_or(TermError::Closed)?;
        writer.send(bytes.to_vec()).map_err(|_| TermError::Closed)
    }

    /// Resizes the grid and tells the program (SIGWINCH). Columns below 2 and rows below 1 are
    /// raised to those minimums; cell sizes are in pixels.
    pub fn resize(
        &self,
        cols: u16,
        rows: u16,
        cell_width: u16,
        cell_height: u16,
    ) -> Result<(), TermError> {
        let (cols, rows) = (cols.max(2), rows.max(1));
        let mut emulator = self.shared.emulator.lock();
        emulator.resize(cols, rows, cell_width, cell_height);
        pty::set_window_size(&self.master, cols, rows, cell_width, cell_height)
            .map_err(TermError::Resize)
    }

    /// Whether the program is still running.
    pub fn is_alive(&self) -> bool {
        !self.shared.process().exited
    }

    /// The program's exit status, once it has exited and the status was collected.
    pub fn exit_status(&self) -> Option<ExitStatus> {
        self.shared.process().status
    }

    /// Blocks up to `timeout` for the program to exit. Returns whether it has.
    pub fn wait_for_exit(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut process = self.shared.process();
        while !process.exited {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            process = self
                .shared
                .process_exited
                .wait_timeout(process, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    /// Copies the visible state for drawing.
    pub fn snapshot(&self) -> GridSnapshot {
        self.shared.emulator.lock().snapshot()
    }

    /// The current generation; compare with [`GridSnapshot::generation`] to skip redundant
    /// snapshots.
    pub fn generation(&self) -> u64 {
        self.shared.emulator.lock().generation()
    }

    /// Modes needed to encode input (see [`crate::input`]).
    pub fn modes(&self) -> TerminalModes {
        self.shared.emulator.lock().modes()
    }

    /// The window title the program set, if any.
    pub fn title(&self) -> Option<String> {
        self.shared.emulator.lock().title().map(str::to_owned)
    }

    /// Scrolls the viewport by `delta` lines: positive moves back into history.
    pub fn scroll_lines(&self, delta: i32) {
        self.shared.emulator.lock().scroll_lines(delta);
    }

    /// Returns the viewport to the live screen.
    pub fn scroll_to_bottom(&self) {
        self.shared.emulator.lock().scroll_to_bottom();
    }

    /// How many lines the viewport is scrolled back; 0 when following output.
    pub fn display_offset(&self) -> usize {
        self.shared.emulator.lock().display_offset()
    }

    /// Lines of scrollback stored above the screen.
    pub fn history_size(&self) -> usize {
        self.shared.emulator.lock().history_size()
    }

    /// Starts a selection at a viewport point; see [`Emulator::start_selection`].
    pub fn start_selection(&self, mode: SelectionMode, point: ViewportPoint, side: Side) {
        self.shared
            .emulator
            .lock()
            .start_selection(mode, point, side);
    }

    /// Moves the free end of the selection; see [`Emulator::update_selection`].
    pub fn update_selection(&self, point: ViewportPoint, side: Side) {
        self.shared.emulator.lock().update_selection(point, side);
    }

    /// Removes the selection.
    pub fn clear_selection(&self) {
        self.shared.emulator.lock().clear_selection();
    }

    /// The selected text; see [`Emulator::selected_text`].
    pub fn selected_text(&self) -> Option<String> {
        self.shared.emulator.lock().selected_text()
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::Release);
        let _ = self.poller.notify();
        self.writer = None;

        {
            let process = self.shared.process();
            if process.exited {
                return;
            }
            pty::signal_group(self.shared.pid, libc::SIGHUP);
            // A stopped job would not act on SIGHUP until continued.
            pty::signal_group(self.shared.pid, libc::SIGCONT);
        }

        let shared = Arc::clone(&self.shared);
        let escalate = move || kill_after_grace(&shared);
        let spawned = thread::Builder::new()
            .name(format!("xctx-term reaper {}", self.shared.pid))
            .spawn(escalate);
        if spawned.is_err() {
            let process = self.shared.process();
            if !process.exited {
                pty::signal_group(self.shared.pid, libc::SIGKILL);
            }
        }
    }
}

/// Waits for the program to exit after SIGHUP, and kills its process group if it does not.
fn kill_after_grace(shared: &Shared) {
    let deadline = Instant::now() + KILL_GRACE;
    let mut process = shared.process();
    while !process.exited {
        let now = Instant::now();
        if now >= deadline {
            // Holding the lock guarantees the waiter has not reaped the pid yet.
            pty::signal_group(shared.pid, libc::SIGKILL);
            return;
        }
        process = shared
            .process_exited
            .wait_timeout(process, deadline - now)
            .unwrap_or_else(PoisonError::into_inner)
            .0;
    }
}

/// Starts the thread that collects the exit status.
fn spawn_waiter(shared: Arc<Shared>, child: Child) -> Result<(), TermError> {
    let pid = shared.pid;
    // The child is shared with the error path so it can still be reaped if the thread cannot
    // start.
    let child = Arc::new(Mutex::new(Some(child)));
    let thread_child = Arc::clone(&child);
    let spawned = thread::Builder::new()
        .name(format!("xctx-term waiter {pid}"))
        .spawn(move || {
            let child = thread_child
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            if let Some(child) = child {
                run_waiter(&shared, child);
            }
        });
    match spawned {
        Ok(_) => Ok(()),
        Err(err) => {
            let child = child.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(mut child) = child {
                pty::signal_group(pid, libc::SIGKILL);
                let _ = child.wait();
            }
            Err(TermError::Thread(err))
        }
    }
}

fn run_waiter(shared: &Shared, mut child: Child) {
    let status = if pty::wait_for_exit_without_reaping(shared.pid) {
        // The child has exited but is not reaped; reap it under the lock so signal senders,
        // which check `exited` under the same lock, never signal a recycled pid.
        let mut process = shared.process();
        let status = child.wait().ok();
        process.exited = true;
        process.status = status;
        status
    } else {
        let status = child.wait().ok();
        let mut process = shared.process();
        process.exited = true;
        process.status = status;
        status
    };
    shared.process_exited.notify_all();
    shared.emit(TerminalEvent::Exited(status));
}

fn run_writer(mut master: File, input: Receiver<Vec<u8>>) {
    for bytes in input {
        if master.write_all(&bytes).is_err() {
            break;
        }
    }
}

fn run_reader(
    shared: Arc<Shared>,
    poller: Arc<Poller>,
    master: Arc<File>,
    writer: Sender<Vec<u8>>,
) {
    let mut buffer = vec![0u8; READ_BUFFER_SIZE];
    let mut events = Events::new();

    while !shared.shutdown.load(Ordering::Acquire) {
        let timeout = shared
            .emulator
            .lock()
            .sync_deadline()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
        events.clear();
        match poller.wait(&mut events, timeout) {
            Ok(_) => {}
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
        if shared.shutdown.load(Ordering::Acquire) {
            break;
        }

        if !events.iter().any(|event| event.key == PTY_KEY) {
            // Woken by a timeout: a synchronized update may have expired.
            let flushed = {
                let mut emulator = shared.emulator.lock();
                emulator
                    .flush_expired_sync(Instant::now())
                    .then(|| (emulator.take_replies(), emulator.take_events()))
            };
            if let Some((replies, events)) = flushed {
                dispatch(&shared, &writer, replies, events, true);
            }
            continue;
        }

        let read = match (&*master).read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if matches!(err.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {
                continue;
            }
            // EIO once every process holding the PTY slave has gone.
            Err(_) => break,
        };

        let (changed, replies, events) = {
            let mut emulator = shared.emulator.lock();
            let changed = emulator.advance(&buffer[..read]);
            (changed, emulator.take_replies(), emulator.take_events())
        };
        dispatch(&shared, &writer, replies, events, changed);
    }

    let _ = poller.delete(&*master);
}

fn dispatch(
    shared: &Shared,
    writer: &Sender<Vec<u8>>,
    replies: Vec<u8>,
    events: Vec<TerminalEvent>,
    changed: bool,
) {
    if !replies.is_empty() {
        let _ = writer.send(replies);
    }
    for event in events {
        shared.emit(event);
    }
    if changed {
        shared.emit(TerminalEvent::Wakeup);
    }
}
