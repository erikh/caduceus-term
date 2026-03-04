use std::process::ExitStatus;
use thiserror::Error;

/// Errors that can occur during proxy operation.
#[derive(Error, Debug)]
pub enum CaduceusError {
    /// Standard I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Error from the Wasmtime runtime.
    #[error("WASM error: {0}")]
    Wasm(#[from] wasmtime::Error),

    /// A required WASM export was not found in the module.
    #[error("missing WASM export: {export}")]
    MissingExport { export: String },

    /// Attempted to access WASM linear memory out of bounds.
    #[error("memory access out of bounds: offset={offset}, size={size}")]
    MemoryAccess { offset: usize, size: usize },

    /// The guest `alloc` function returned a null pointer.
    #[error("WASM allocation failed")]
    AllocFailed,

    /// The child process exited (possibly unexpectedly).
    #[error("child process exited: {0}")]
    ChildExited(ExitStatus),

    /// Failed to spawn the child process.
    #[error("failed to spawn child process: {0}")]
    SpawnFailed(std::io::Error),

    /// The I/O queue receiver has been dropped.
    #[error("I/O queue has shut down")]
    QueueShutdown,

    /// Failed to send an operation to the I/O queue.
    #[error("I/O queue send failed: {0}")]
    QueueSendFailed(String),

    /// PTY-specific error.
    #[cfg(feature = "pty")]
    #[error("PTY error: {0}")]
    Pty(String),

    /// Invalid or missing configuration.
    #[error("configuration error: {0}")]
    Config(String),
}

/// Convenience alias for `Result<T, CaduceusError>`.
pub type Result<T> = std::result::Result<T, CaduceusError>;
