pub mod piped;
#[cfg(feature = "pty")]
pub mod pty;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitStatus;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::Result;

/// Configuration for spawning a child process.
pub struct SpawnConfig {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: HashMap<OsString, OsString>,
    pub cwd: Option<PathBuf>,
}

/// Uniform I/O handles for a child process.
pub struct ChildIo {
    pub stdin: Box<dyn AsyncWrite + Unpin + Send>,
    pub stdout: Box<dyn AsyncRead + Unpin + Send>,
    /// `None` when using PTY (stderr merged with stdout).
    pub stderr: Option<Box<dyn AsyncRead + Unpin + Send>>,
}

/// A running child process that can be waited on.
pub enum RunningChild {
    Piped(tokio::process::Child),
    #[cfg(feature = "pty")]
    Pty(pty::PtyRunningChild),
}

impl RunningChild {
    /// Wait for the child to exit and return its exit status.
    pub async fn wait(&mut self) -> Result<ExitStatus> {
        match self {
            RunningChild::Piped(child) => child.wait().await.map_err(Into::into),
            #[cfg(feature = "pty")]
            RunningChild::Pty(pty_child) => pty_child.wait().await,
        }
    }
}

/// Result of spawning a child process.
pub struct SpawnResult {
    pub io: ChildIo,
    pub child: RunningChild,
}
