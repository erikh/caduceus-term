use std::process::Stdio;
use tokio::process::Command;

use super::{ChildIo, RunningChild, SpawnConfig, SpawnResult};
use crate::error::{CaduceusError, Result};

/// Spawn a child process with piped stdin/stdout/stderr.
pub fn spawn_piped(config: &SpawnConfig) -> Result<SpawnResult> {
    let mut cmd = Command::new(&config.program);
    cmd.args(&config.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    for (key, val) in &config.env {
        cmd.env(key, val);
    }

    if let Some(cwd) = &config.cwd {
        cmd.current_dir(cwd);
    }

    let mut child = cmd.spawn().map_err(CaduceusError::SpawnFailed)?;

    let stdin = child
        .stdin
        .take()
        .expect("stdin was set to piped but is None");
    let stdout = child
        .stdout
        .take()
        .expect("stdout was set to piped but is None");
    let stderr = child
        .stderr
        .take()
        .expect("stderr was set to piped but is None");

    let io = ChildIo {
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
        stderr: Some(Box::new(stderr)),
    };

    Ok(SpawnResult {
        io,
        child: RunningChild::Piped(child),
    })
}
