use pty_process::{Command as PtyCommand, Size};
use std::process::ExitStatus;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::process::Child;

use super::{ChildIo, RunningChild, SpawnConfig, SpawnResult};
use crate::error::{CaduceusError, Result};

/// A running child process attached to a PTY.
pub struct PtyRunningChild {
    child: Child,
}

impl PtyRunningChild {
    pub async fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().await.map_err(Into::into)
    }
}

/// Spawn a child process attached to a PTY.
pub fn spawn_pty(config: &SpawnConfig, rows: u16, cols: u16) -> Result<SpawnResult> {
    let (pty, pts) = pty_process::open().map_err(|e| CaduceusError::Pty(e.to_string()))?;

    pty.resize(Size::new(rows, cols))
        .map_err(|e| CaduceusError::Pty(e.to_string()))?;

    let mut cmd = PtyCommand::new(&config.program);
    cmd = cmd.args(&config.args);

    for (key, val) in &config.env {
        cmd = cmd.env(key, val);
    }

    if let Some(cwd) = &config.cwd {
        cmd = cmd.current_dir(cwd);
    }

    let child = cmd
        .spawn(pts)
        .map_err(|e| CaduceusError::Pty(e.to_string()))?;

    let (read_pty, write_pty) = pty.into_split();

    let io = ChildIo {
        stdin: Box::new(write_pty) as Box<dyn AsyncWrite + Unpin + Send>,
        stdout: Box::new(read_pty) as Box<dyn AsyncRead + Unpin + Send>,
        stderr: None, // PTY merges stderr with stdout
    };

    Ok(SpawnResult {
        io,
        child: RunningChild::Pty(PtyRunningChild { child }),
    })
}
