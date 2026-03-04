use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::ExitStatus;

use tokio::io::{self, AsyncReadExt};
use tracing::{debug, error};

use crate::child::{self, ChildIo, SpawnConfig};
use crate::error::{CaduceusError, Result};
use crate::queue::{IoQueue, IoQueueHandles};
use crate::wasm::WasmTransform;

/// How to spawn the child process.
#[derive(Clone, Debug)]
pub enum ChildMode {
    Piped,
    #[cfg(feature = "pty")]
    Pty { rows: u16, cols: u16 },
}

/// Source for a WASM module.
#[derive(Clone, Debug)]
pub enum WasmModuleSource {
    Bytes(Vec<u8>),
    File(PathBuf),
}

/// Per-stream transform configuration.
#[derive(Clone, Debug, Default)]
pub struct TransformConfig {
    pub stdin: Option<WasmModuleSource>,
    pub stdout: Option<WasmModuleSource>,
    pub stderr: Option<WasmModuleSource>,
}

/// Full proxy configuration.
#[derive(Clone, Debug)]
pub struct ProxyConfig {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: HashMap<OsString, OsString>,
    pub cwd: Option<PathBuf>,
    pub child_mode: ChildMode,
    pub transforms: TransformConfig,
    pub queue_capacity: usize,
}

/// Fluent builder for ProxyConfig.
pub struct ProxyBuilder {
    config: ProxyConfig,
}

impl ProxyBuilder {
    pub fn new(program: impl Into<OsString>) -> Self {
        ProxyBuilder {
            config: ProxyConfig {
                program: program.into(),
                args: Vec::new(),
                env: HashMap::new(),
                cwd: None,
                child_mode: ChildMode::Piped,
                transforms: TransformConfig::default(),
                queue_capacity: 256,
            },
        }
    }

    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.config.args.push(arg.into());
        self
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.config.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn env(mut self, key: impl Into<OsString>, val: impl Into<OsString>) -> Self {
        self.config.env.insert(key.into(), val.into());
        self
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.config.cwd = Some(cwd.into());
        self
    }

    pub fn child_mode(mut self, mode: ChildMode) -> Self {
        self.config.child_mode = mode;
        self
    }

    pub fn stdin_transform(mut self, source: WasmModuleSource) -> Self {
        self.config.transforms.stdin = Some(source);
        self
    }

    pub fn stdout_transform(mut self, source: WasmModuleSource) -> Self {
        self.config.transforms.stdout = Some(source);
        self
    }

    pub fn stderr_transform(mut self, source: WasmModuleSource) -> Self {
        self.config.transforms.stderr = Some(source);
        self
    }

    pub fn queue_capacity(mut self, capacity: usize) -> Self {
        self.config.queue_capacity = capacity;
        self
    }

    pub fn build(self) -> ProxyConfig {
        self.config
    }
}

/// Load WASM bytes from a module source.
fn load_wasm_bytes(source: &WasmModuleSource) -> Result<Vec<u8>> {
    match source {
        WasmModuleSource::Bytes(bytes) => Ok(bytes.clone()),
        WasmModuleSource::File(path) => std::fs::read(path).map_err(CaduceusError::Io),
    }
}

/// Run the proxy with the given configuration.
///
/// Spawns the child process, starts the I/O queue, optionally creates WASM
/// transform instances, and runs three forwarding tasks (stdin, stdout, stderr).
/// Returns the child's exit status.
pub async fn run_proxy(config: ProxyConfig) -> Result<ExitStatus> {
    let spawn_config = SpawnConfig {
        program: config.program,
        args: config.args,
        env: config.env,
        cwd: config.cwd,
    };

    // Spawn child
    let spawn_result = match &config.child_mode {
        ChildMode::Piped => child::piped::spawn_piped(&spawn_config)?,
        #[cfg(feature = "pty")]
        ChildMode::Pty { rows, cols } => child::pty::spawn_pty(&spawn_config, *rows, *cols)?,
    };

    let ChildIo {
        stdin: child_stdin,
        stdout: child_stdout,
        stderr: child_stderr,
    } = spawn_result.io;
    let mut running_child = spawn_result.child;

    // Set up I/O queue with parent's stdin/stdout/stderr
    let parent_stdin = io::stdin();
    let parent_stdout = io::stdout();
    let parent_stderr = io::stderr();

    let handles = IoQueueHandles {
        child_stdin,
        parent_stdout: Box::new(parent_stdout),
        parent_stderr: Box::new(parent_stderr),
        parent_stdin: Box::new(parent_stdin),
    };

    let (queue, queue_handle) = IoQueue::new(config.queue_capacity, handles);
    let queue_task = tokio::spawn(queue.run());

    // Compile WASM transforms if configured
    let stdin_transform = config
        .transforms
        .stdin
        .as_ref()
        .map(|s| load_wasm_bytes(s).and_then(|b| WasmTransform::new(&b)))
        .transpose()?;

    let stdout_transform = config
        .transforms
        .stdout
        .as_ref()
        .map(|s| load_wasm_bytes(s).and_then(|b| WasmTransform::new(&b)))
        .transpose()?;

    let stderr_transform = config
        .transforms
        .stderr
        .as_ref()
        .map(|s| load_wasm_bytes(s).and_then(|b| WasmTransform::new(&b)))
        .transpose()?;

    // Spawn stdin forwarding task
    let stdin_handle = queue_handle.clone();
    let stdin_task = tokio::spawn(async move {
        let mut wasm = match stdin_transform {
            Some(t) => Some(t.instantiate(stdin_handle.clone()).await?),
            None => None,
        };

        loop {
            let data = stdin_handle.read_parent_stdin(8192).await?;
            if data.is_empty() {
                debug!("parent stdin EOF");
                break;
            }

            let output = if let Some(ref mut inst) = wasm {
                inst.transform(&data).await?
            } else {
                data
            };

            if !output.is_empty() {
                stdin_handle.write_child_stdin(output).await?;
            }
        }

        Ok::<(), CaduceusError>(())
    });

    // Spawn stdout forwarding task
    let stdout_handle = queue_handle.clone();
    let stdout_task = tokio::spawn(async move {
        let mut wasm = match stdout_transform {
            Some(t) => Some(t.instantiate(stdout_handle.clone()).await?),
            None => None,
        };

        let mut child_stdout = child_stdout;
        let mut buf = vec![0u8; 8192];

        loop {
            let n = child_stdout.read(&mut buf).await?;
            if n == 0 {
                debug!("child stdout EOF");
                break;
            }

            let data = &buf[..n];
            let output = if let Some(ref mut inst) = wasm {
                inst.transform(data).await?
            } else {
                data.to_vec()
            };

            if !output.is_empty() {
                stdout_handle.write_parent_stdout(output).await?;
            }
        }

        Ok::<(), CaduceusError>(())
    });

    // Spawn stderr forwarding task (only if stderr handle exists)
    let stderr_handle = queue_handle.clone();
    let stderr_task = if let Some(child_stderr) = child_stderr {
        Some(tokio::spawn(async move {
            let mut wasm = match stderr_transform {
                Some(t) => Some(t.instantiate(stderr_handle.clone()).await?),
                None => None,
            };

            let mut child_stderr = child_stderr;
            let mut buf = vec![0u8; 8192];

            loop {
                let n = child_stderr.read(&mut buf).await?;
                if n == 0 {
                    debug!("child stderr EOF");
                    break;
                }

                let data = &buf[..n];
                let output = if let Some(ref mut inst) = wasm {
                    inst.transform(data).await?
                } else {
                    data.to_vec()
                };

                if !output.is_empty() {
                    stderr_handle.write_parent_stderr(output).await?;
                }
            }

            Ok::<(), CaduceusError>(())
        }))
    } else {
        None
    };

    // Wait for child to exit
    let status = running_child.wait().await?;
    debug!("child exited with status: {status}");

    // Wait for stdout/stderr forwarding to finish (they'll hit EOF)
    if let Err(e) = stdout_task.await {
        error!("stdout task panicked: {e}");
    }
    if let Some(task) = stderr_task {
        if let Err(e) = task.await {
            error!("stderr task panicked: {e}");
        }
    }

    // Shut down the queue and abort stdin task
    let _ = queue_handle.shutdown().await;
    stdin_task.abort();

    if let Err(e) = queue_task.await {
        error!("queue task panicked: {e}");
    }

    Ok(status)
}
