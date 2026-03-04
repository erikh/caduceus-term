use std::process::ExitStatus;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum CaduceusError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("WASM error: {0}")]
    Wasm(#[from] wasmtime::Error),

    #[error("missing WASM export: {export}")]
    MissingExport { export: String },

    #[error("memory access out of bounds: offset={offset}, size={size}")]
    MemoryAccess { offset: usize, size: usize },

    #[error("WASM allocation failed")]
    AllocFailed,

    #[error("child process exited: {0}")]
    ChildExited(ExitStatus),

    #[error("failed to spawn child process: {0}")]
    SpawnFailed(std::io::Error),

    #[error("I/O queue has shut down")]
    QueueShutdown,

    #[error("I/O queue send failed: {0}")]
    QueueSendFailed(String),

    #[cfg(feature = "pty")]
    #[error("PTY error: {0}")]
    Pty(String),

    #[error("configuration error: {0}")]
    Config(String),
}

pub type Result<T> = std::result::Result<T, CaduceusError>;
