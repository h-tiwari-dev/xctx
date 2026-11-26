use std::io;

/// What can go wrong talking to an `omp` child process.
#[derive(Debug, thiserror::Error)]
pub enum OmpError {
    /// The program could not be started.
    #[error("could not start {program}: {source}")]
    Spawn {
        /// The program that was run.
        program: String,
        /// Why it did not start.
        #[source]
        source: io::Error,
    },
    /// Reading from or writing to the child failed.
    #[error("omp pipe: {0}")]
    Io(#[from] io::Error),
    /// A line from omp was not valid JSON.
    #[error("omp sent a line that is not JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A `rpc_chunk` sequence broke one of the protocol's rules.
    #[error("omp sent a broken chunked frame: {0}")]
    Chunk(String),
    /// omp closed its output before the handshake finished.
    #[error("omp exited before it was ready")]
    ExitedEarly,
    /// omp did not answer a handshake step in time.
    #[error("omp did not answer {0} in time")]
    Timeout(&'static str),
    /// The child's input is closed, so no command can be sent.
    #[error("omp is not running")]
    Closed,
}

/// A result whose error is an [`OmpError`] unless another is given.
pub type Result<T, E = OmpError> = std::result::Result<T, E>;
