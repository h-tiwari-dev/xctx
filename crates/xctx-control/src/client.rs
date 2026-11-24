//! The client side: connect to a running window and make calls.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::error::{ControlError, Result, io_error};
use crate::path::{check_socket_path, socket_path};
use crate::protocol::{Call, JSONRPC_VERSION, PingResult, RequestEnvelope, Response};

/// How long a call waits for its answer by default: a little longer than the server's own reply timeout, so the
/// server's timeout error arrives first.
pub const DEFAULT_CALL_TIMEOUT: Duration = Duration::from_secs(35);

/// A connection to a running xctx window. Calls are made one at a time, in order.
#[derive(Debug)]
pub struct Client {
    path: PathBuf,
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl Client {
    /// Connect to the socket at `path`. Fails with [`ControlError::NotRunning`] when nothing listens there.
    pub fn connect(path: impl Into<PathBuf>) -> Result<Client> {
        let path = path.into();
        check_socket_path(&path)?;
        let stream = UnixStream::connect(&path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => {
                ControlError::NotRunning(path.clone())
            }
            _ => io_error(&path)(e),
        })?;
        stream
            .set_read_timeout(Some(DEFAULT_CALL_TIMEOUT))
            .map_err(io_error(&path))?;
        stream
            .set_write_timeout(Some(DEFAULT_CALL_TIMEOUT))
            .map_err(io_error(&path))?;
        let reader = BufReader::new(stream.try_clone().map_err(io_error(&path))?);
        Ok(Client {
            path,
            reader,
            writer: stream,
            next_id: 1,
        })
    }

    /// Connect to [`socket_path`]: `$XCTX_CONTROL_SOCKET`, else the data directory's `control.sock`.
    pub fn connect_default() -> Result<Client> {
        Client::connect(socket_path()?)
    }

    /// The socket this client is connected to.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// How long a call waits for its answer; `None` waits forever.
    pub fn set_timeout(&mut self, timeout: Option<Duration>) -> Result<()> {
        self.writer
            .set_read_timeout(timeout)
            .map_err(io_error(&self.path))
    }

    /// Make `call` and return its result. A failed call is [`ControlError::Rpc`].
    pub fn call(&mut self, call: &Call) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&RequestEnvelope::new(json!(id), call))?;
        let mut line = String::new();
        let n = self
            .reader
            .read_line(&mut line)
            .map_err(|e| match e.kind() {
                io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut => {
                    ControlError::Protocol(format!(
                        "no answer to {} from {}",
                        call.method(),
                        self.path.display()
                    ))
                }
                _ => io_error(&self.path)(e),
            })?;
        if n == 0 {
            return Err(ControlError::Protocol(
                "the window closed the connection".into(),
            ));
        }
        let response: Response = serde_json::from_str(line.trim())
            .map_err(|e| ControlError::Protocol(format!("unreadable response: {e}")))?;
        if response.jsonrpc != JSONRPC_VERSION {
            return Err(ControlError::Protocol(format!(
                "response is jsonrpc {:?}",
                response.jsonrpc
            )));
        }
        if response.id != json!(id) {
            // A null id means the server could not read the request; its error says why.
            if !response.id.is_null() {
                return Err(ControlError::Protocol(format!(
                    "response for id {} to request {id}",
                    response.id
                )));
            }
        }
        Ok(response.into_result()?)
    }

    /// Make `call` and decode its result as `T`.
    pub fn call_as<T: DeserializeOwned>(&mut self, call: &Call) -> Result<T> {
        let value = self.call(call)?;
        serde_json::from_value(value).map_err(|e| {
            ControlError::Protocol(format!("unexpected result for {}: {e}", call.method()))
        })
    }

    /// `ping` the window.
    pub fn ping(&mut self) -> Result<PingResult> {
        self.call_as(&Call::Ping)
    }

    /// Send `call` as a notification: it runs, and nothing comes back, not even an error.
    pub fn notify(&mut self, call: &Call) -> Result<()> {
        self.send(&RequestEnvelope {
            jsonrpc: JSONRPC_VERSION.to_owned(),
            id: None,
            method: call.method().name().to_owned(),
            params: Some(call.params()),
        })
    }

    fn send(&mut self, request: &RequestEnvelope) -> Result<()> {
        let mut line = serde_json::to_string(request)
            .map_err(|e| ControlError::Protocol(format!("could not encode the request: {e}")))?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .and_then(|()| self.writer.flush())
            .map_err(io_error(&self.path))
    }
}
