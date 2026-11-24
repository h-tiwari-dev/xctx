//! The socket server: binds the control socket, reads requests, checks them, and hands each one to the app.

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::error::{ControlError, Result, io_error};
use crate::path::check_socket_path;
use crate::protocol::{
    Call, Capability, ErrorCode, MAX_LINE_BYTES, PingResult, RequestEnvelope, Response, RpcError,
};

/// What the server calls with each authorised request other than `ping`. It runs on the connection's thread, so
/// it should hand the request to the app (for example over a channel to the UI loop) and return; the
/// connection waits for the [`Responder`] up to [`ServerConfig::reply_timeout`].
pub type Handler = Arc<dyn Fn(Incoming) + Send + Sync + 'static>;

/// How a [`Server`] treats its connections.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// What connections may do. Calls to a method outside this set get [`ErrorCode::Forbidden`].
    pub capabilities: BTreeSet<Capability>,
    /// How long a connection waits for the app to answer a request before replying [`ErrorCode::Timeout`].
    pub reply_timeout: Duration,
}

impl Default for ServerConfig {
    /// Every capability and a 30 second reply timeout: what the app gives the agents in its panes.
    fn default() -> Self {
        ServerConfig {
            capabilities: Capability::ALL.into_iter().collect(),
            reply_timeout: Duration::from_secs(30),
        }
    }
}

/// A request for the app to carry out, with the way to answer it.
#[derive(Debug)]
pub struct Incoming {
    call: Call,
    responder: Responder,
}

impl Incoming {
    /// What is asked for.
    pub fn call(&self) -> &Call {
        &self.call
    }

    /// Answer the request.
    pub fn respond(self, result: Result<Value, RpcError>) {
        self.responder.respond(result);
    }

    /// The call and its responder, to answer later from somewhere else.
    pub fn into_parts(self) -> (Call, Responder) {
        (self.call, self.responder)
    }
}

/// Answers one request. Dropping it without answering replies with an internal error, so a request never hangs
/// past the app's lifetime.
#[derive(Debug)]
pub struct Responder {
    tx: mpsc::SyncSender<Result<Value, RpcError>>,
}

impl Responder {
    /// Answer with `result`. Does nothing when the client has gone away or the request timed out.
    pub fn respond(self, result: Result<Value, RpcError>) {
        // The receiver is gone only when the connection stopped waiting; there is no one left to tell.
        let _ = self.tx.send(result);
    }

    /// Answer with `value` as the result.
    pub fn ok<T: Serialize>(self, value: &T) {
        let result = serde_json::to_value(value)
            .map_err(|e| RpcError::internal(format!("could not encode the result: {e}")));
        self.respond(result);
    }

    /// Answer with `error`.
    pub fn err(self, error: RpcError) {
        self.respond(Err(error));
    }
}

struct Shared {
    stop: AtomicBool,
    config: ServerConfig,
    handler: Handler,
    connections: Mutex<HashMap<u64, UnixStream>>,
    next_connection: AtomicU64,
}

/// A bound control socket. Requests are served on background threads until the server is dropped, which stops
/// accepting, closes open connections and removes the socket file (unless something else has replaced it).
pub struct Server {
    path: PathBuf,
    identity: Option<(u64, u64)>,
    shared: Arc<Shared>,
    accept: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").field("path", &self.path).finish()
    }
}

impl Server {
    /// Bind `path` and start serving, calling `handler` for every authorised request but `ping`, which the
    /// server answers itself.
    ///
    /// Creates the parent directory if needed and makes the socket readable and writable by its owner only;
    /// connections from other users are also refused by peer id. A stale socket file left by a crashed window is
    /// replaced. Fails with [`ControlError::AlreadyRunning`] when a live server answers on `path`, and with
    /// [`ControlError::SocketPathTooLong`] when the path does not fit in a socket address.
    pub fn bind(
        path: impl Into<PathBuf>,
        config: ServerConfig,
        handler: impl Fn(Incoming) + Send + Sync + 'static,
    ) -> Result<Server> {
        let path = path.into();
        check_socket_path(&path)?;
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(io_error(parent))?;
        }
        clear_stale_socket(&path)?;
        let listener = UnixListener::bind(&path).map_err(io_error(&path))?;
        listener.set_nonblocking(true).map_err(io_error(&path))?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(io_error(&path))?;
        let identity = fs::metadata(&path).ok().map(|m| (m.dev(), m.ino()));

        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            config,
            handler: Arc::new(handler),
            connections: Mutex::new(HashMap::new()),
            next_connection: AtomicU64::new(0),
        });
        let accept = thread::Builder::new()
            .name("xctx-control-accept".into())
            .spawn({
                let shared = Arc::clone(&shared);
                move || accept_loop(&listener, &shared)
            })
            .map_err(io_error(&path))?;
        Ok(Server {
            path,
            identity,
            shared,
            accept: Some(accept),
        })
    }

    /// The socket's path, as clients should be given it (for example in `XCTX_CONTROL_SOCKET`).
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Stop serving and remove the socket. The same as dropping the server.
    pub fn shutdown(self) {}
}

impl Drop for Server {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // The accept loop sees the flag within one ACCEPT_POLL.
        if let Some(accept) = self.accept.take() {
            let _ = accept.join();
        }
        if let Ok(mut connections) = self.shared.connections.lock() {
            for (_, stream) in connections.drain() {
                let _ = stream.shutdown(std::net::Shutdown::Both);
            }
        }
        let ours = fs::metadata(&self.path)
            .ok()
            .map(|m| (m.dev(), m.ino()))
            .is_some_and(|id| Some(id) == self.identity);
        if ours {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Remove a socket file nothing answers on. Leave a live one alone and refuse to touch anything that is not a
/// socket.
fn clear_stale_socket(path: &Path) -> Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_error(path)(e)),
    };
    if !meta.file_type().is_socket() {
        return Err(io_error(path)(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "exists and is not a socket",
        )));
    }
    if UnixStream::connect(path).is_ok() {
        return Err(ControlError::AlreadyRunning(path.to_path_buf()));
    }
    fs::remove_file(path).map_err(io_error(path))
}

/// How long the accept loop waits for a connection before checking whether the server is stopping.
const ACCEPT_POLL: Duration = Duration::from_millis(100);

/// Wait up to `timeout` for `listener` to have a connection waiting.
fn wait_readable(listener: &UnixListener, timeout: Duration) {
    use std::os::fd::AsRawFd;
    let mut fds = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let millis = libc::c_int::try_from(timeout.as_millis()).unwrap_or(libc::c_int::MAX);
    // SAFETY: `fds` is one live pollfd for a descriptor `listener` keeps open during the call.
    unsafe { libc::poll(&mut fds, 1, millis) };
}

fn accept_loop(listener: &UnixListener, shared: &Arc<Shared>) {
    // The listener is non-blocking and polled, so stopping never depends on connecting to the socket path,
    // which another window may have taken over by then.
    while !shared.stop.load(Ordering::SeqCst) {
        wait_readable(listener, ACCEPT_POLL);
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let stream = match listener.accept() {
            Ok((stream, _)) => stream,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
            Err(_) => {
                // Usually out of file descriptors; back off rather than spin.
                thread::sleep(Duration::from_millis(50));
                continue;
            }
        };
        // Accepted sockets inherit the listener's O_NONBLOCK on macOS; connections are served blocking.
        if stream.set_nonblocking(false).is_err() || !matches!(peer_is_same_user(&stream), Ok(true))
        {
            let _ = stream.shutdown(std::net::Shutdown::Both);
            continue;
        }
        let id = shared.next_connection.fetch_add(1, Ordering::SeqCst);
        if let (Ok(clone), Ok(mut connections)) = (stream.try_clone(), shared.connections.lock()) {
            connections.insert(id, clone);
        }
        let shared = Arc::clone(shared);
        let spawned = thread::Builder::new()
            .name(format!("xctx-control-{id}"))
            .spawn(move || {
                serve_connection(stream, &shared);
                if let Ok(mut connections) = shared.connections.lock() {
                    connections.remove(&id);
                }
            });
        if spawned.is_err() {
            thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Whether the process on the other end runs as the same user as this one.
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
))]
fn peer_is_same_user(stream: &UnixStream) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let mut uid: libc::uid_t = 0;
    let mut gid: libc::gid_t = 0;
    // SAFETY: the fd is a connected Unix socket owned by `stream` for the duration of the call, and both out
    // pointers point at live locals of the right type.
    let rc = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    Ok(uid == me)
}

/// Whether the process on the other end runs as the same user as this one. Without `getpeereid` the socket's
/// owner-only mode is the only check.
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
)))]
fn peer_is_same_user(_stream: &UnixStream) -> io::Result<bool> {
    Ok(true)
}

/// Read one line of at most [`MAX_LINE_BYTES`]. `Ok(None)` at end of stream; an error for a longer line.
fn read_line(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    let limit = MAX_LINE_BYTES as u64 + 1;
    let n = reader.by_ref().take(limit).read_until(b'\n', &mut line)?;
    if n == 0 {
        return Ok(None);
    }
    if line.last() != Some(&b'\n') && n as u64 == limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("request line longer than {MAX_LINE_BYTES} bytes"),
        ));
    }
    Ok(Some(line))
}

fn serve_connection(stream: UnixStream, shared: &Shared) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut writer = stream;
    loop {
        if shared.stop.load(Ordering::SeqCst) {
            return;
        }
        let line = match read_line(&mut reader) {
            Ok(Some(line)) => line,
            Ok(None) => return,
            Err(e) => {
                let response = Response::new(
                    Value::Null,
                    Err(RpcError::new(ErrorCode::ParseError, e.to_string())),
                );
                let _ = writeln!(writer, "{}", response.to_line());
                return;
            }
        };
        let Ok(text) = std::str::from_utf8(&line) else {
            let response = Response::new(
                Value::Null,
                Err(RpcError::new(ErrorCode::ParseError, "request is not UTF-8")),
            );
            if writeln!(writer, "{}", response.to_line()).is_err() {
                return;
            }
            continue;
        };
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if let Some(response) = handle_line(text, shared)
            && (writeln!(writer, "{}", response.to_line()).is_err() || writer.flush().is_err())
        {
            return;
        }
    }
}

/// The response to one request line, or `None` for a notification.
fn handle_line(line: &str, shared: &Shared) -> Option<Response> {
    let envelope = match RequestEnvelope::parse_line(line) {
        Ok(envelope) => envelope,
        Err(response) => return Some(*response),
    };
    let result = envelope.call().and_then(|call| dispatch(call, shared));
    envelope.id.map(|id| Response::new(id, result))
}

fn dispatch(call: Call, shared: &Shared) -> Result<Value, RpcError> {
    let method = call.method();
    let needed = method.capability();
    if !shared.config.capabilities.contains(&needed) {
        return Err(RpcError::new(
            ErrorCode::Forbidden,
            format!("{method} needs the {needed:?} capability, which this socket does not grant"),
        ));
    }
    if call == Call::Ping {
        return serde_json::to_value(PingResult::current())
            .map_err(|e| RpcError::internal(e.to_string()));
    }
    let (tx, rx) = mpsc::sync_channel(1);
    (shared.handler)(Incoming {
        call,
        responder: Responder { tx },
    });
    let timeout = shared.config.reply_timeout;
    match rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(RpcError::new(
            ErrorCode::Timeout,
            format!(
                "xctx did not answer {method} within {}s",
                timeout.as_secs_f32()
            ),
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(RpcError::internal(format!(
            "xctx dropped the {method} request without answering"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_lines_are_refused_and_short_ones_read() -> io::Result<()> {
        let mut ok = io::Cursor::new(b"abc\ndef".to_vec());
        assert_eq!(read_line(&mut ok)?, Some(b"abc\n".to_vec()));
        assert_eq!(read_line(&mut ok)?, Some(b"def".to_vec()));
        assert_eq!(read_line(&mut ok)?, None);
        let mut long = io::Cursor::new(vec![b'a'; MAX_LINE_BYTES + 10]);
        assert!(read_line(&mut long).is_err());
        Ok(())
    }
}
