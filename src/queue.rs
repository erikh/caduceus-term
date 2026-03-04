use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::error::{CaduceusError, Result};

/// An I/O operation to be executed by the queue.
pub enum IoOperation {
    /// Write data to the child process's stdin.
    WriteChildStdin(Vec<u8>),
    /// Write data to the parent's stdout.
    WriteParentStdout(Vec<u8>),
    /// Write data to the parent's stderr.
    WriteParentStderr(Vec<u8>),
    /// Read up to `max_bytes` from the parent's stdin, returning data via `response`.
    ReadParentStdin {
        max_bytes: usize,
        response: oneshot::Sender<Vec<u8>>,
    },
    /// Shut down the queue loop.
    Shutdown,
}

/// Cloneable sender handle for submitting I/O operations.
#[derive(Clone)]
pub struct QueueHandle {
    tx: mpsc::Sender<IoOperation>,
}

impl QueueHandle {
    /// Send data to the child process's stdin.
    pub async fn write_child_stdin(&self, data: Vec<u8>) -> Result<()> {
        self.tx
            .send(IoOperation::WriteChildStdin(data))
            .await
            .map_err(|e| CaduceusError::QueueSendFailed(e.to_string()))
    }

    /// Send data to the parent's stdout.
    pub async fn write_parent_stdout(&self, data: Vec<u8>) -> Result<()> {
        self.tx
            .send(IoOperation::WriteParentStdout(data))
            .await
            .map_err(|e| CaduceusError::QueueSendFailed(e.to_string()))
    }

    /// Send data to the parent's stderr.
    pub async fn write_parent_stderr(&self, data: Vec<u8>) -> Result<()> {
        self.tx
            .send(IoOperation::WriteParentStderr(data))
            .await
            .map_err(|e| CaduceusError::QueueSendFailed(e.to_string()))
    }

    /// Read up to `max_bytes` from the parent's stdin.
    pub async fn read_parent_stdin(&self, max_bytes: usize) -> Result<Vec<u8>> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(IoOperation::ReadParentStdin {
                max_bytes,
                response: response_tx,
            })
            .await
            .map_err(|e| CaduceusError::QueueSendFailed(e.to_string()))?;
        response_rx.await.map_err(|_| CaduceusError::QueueShutdown)
    }

    /// Signal the queue to shut down.
    pub async fn shutdown(&self) -> Result<()> {
        self.tx
            .send(IoOperation::Shutdown)
            .await
            .map_err(|e| CaduceusError::QueueSendFailed(e.to_string()))
    }
}

/// Physical I/O handles owned by the queue.
pub struct IoQueueHandles {
    pub child_stdin: Box<dyn AsyncWrite + Unpin + Send>,
    pub parent_stdout: Box<dyn AsyncWrite + Unpin + Send>,
    pub parent_stderr: Box<dyn AsyncWrite + Unpin + Send>,
    pub parent_stdin: Box<dyn AsyncRead + Unpin + Send>,
}

/// The I/O queue that serializes all physical I/O operations.
pub struct IoQueue {
    rx: mpsc::Receiver<IoOperation>,
    handles: IoQueueHandles,
}

impl IoQueue {
    /// Create a new IoQueue and its associated QueueHandle.
    pub fn new(capacity: usize, handles: IoQueueHandles) -> (Self, QueueHandle) {
        let (tx, rx) = mpsc::channel(capacity);
        let queue = IoQueue { rx, handles };
        let handle = QueueHandle { tx };
        (queue, handle)
    }

    /// Run the queue loop, processing operations until shutdown or sender drop.
    pub async fn run(mut self) -> Result<()> {
        let mut buf = vec![0u8; 8192];

        while let Some(op) = self.rx.recv().await {
            match op {
                IoOperation::WriteChildStdin(data) => {
                    if let Err(e) = self.handles.child_stdin.write_all(&data).await {
                        warn!("write to child stdin failed: {e}");
                    }
                    let _ = self.handles.child_stdin.flush().await;
                }
                IoOperation::WriteParentStdout(data) => {
                    if let Err(e) = self.handles.parent_stdout.write_all(&data).await {
                        warn!("write to parent stdout failed: {e}");
                    }
                    let _ = self.handles.parent_stdout.flush().await;
                }
                IoOperation::WriteParentStderr(data) => {
                    if let Err(e) = self.handles.parent_stderr.write_all(&data).await {
                        warn!("write to parent stderr failed: {e}");
                    }
                    let _ = self.handles.parent_stderr.flush().await;
                }
                IoOperation::ReadParentStdin { max_bytes, response } => {
                    // Drain any pending writes first
                    self.drain_pending_writes().await;

                    let read_buf = if max_bytes < buf.len() {
                        &mut buf[..max_bytes]
                    } else {
                        &mut buf[..]
                    };

                    let data = match self.handles.parent_stdin.read(read_buf).await {
                        Ok(0) => Vec::new(),
                        Ok(n) => read_buf[..n].to_vec(),
                        Err(e) => {
                            warn!("read from parent stdin failed: {e}");
                            Vec::new()
                        }
                    };
                    let _ = response.send(data);
                }
                IoOperation::Shutdown => {
                    debug!("I/O queue shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Drain pending write operations from the channel without blocking.
    async fn drain_pending_writes(&mut self) {
        loop {
            match self.rx.try_recv() {
                Ok(IoOperation::WriteChildStdin(data)) => {
                    let _ = self.handles.child_stdin.write_all(&data).await;
                    let _ = self.handles.child_stdin.flush().await;
                }
                Ok(IoOperation::WriteParentStdout(data)) => {
                    let _ = self.handles.parent_stdout.write_all(&data).await;
                    let _ = self.handles.parent_stdout.flush().await;
                }
                Ok(IoOperation::WriteParentStderr(data)) => {
                    let _ = self.handles.parent_stderr.write_all(&data).await;
                    let _ = self.handles.parent_stderr.flush().await;
                }
                Ok(IoOperation::ReadParentStdin { .. }) => {
                    // Shouldn't happen during drain, but handle gracefully
                    break;
                }
                Ok(IoOperation::Shutdown) => {
                    break;
                }
                Err(_) => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn test_write_child_stdin() {
        let (child_stdin_write, mut child_stdin_read) = duplex(1024);
        let (parent_stdout_write, _parent_stdout_read) = duplex(1024);
        let (parent_stderr_write, _parent_stderr_read) = duplex(1024);
        let (_parent_stdin_write, parent_stdin_read) = duplex(1024);

        let handles = IoQueueHandles {
            child_stdin: Box::new(child_stdin_write),
            parent_stdout: Box::new(parent_stdout_write),
            parent_stderr: Box::new(parent_stderr_write),
            parent_stdin: Box::new(parent_stdin_read),
        };

        let (queue, handle) = IoQueue::new(16, handles);
        let queue_task = tokio::spawn(queue.run());

        handle
            .write_child_stdin(b"hello".to_vec())
            .await
            .unwrap();
        handle.shutdown().await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = child_stdin_read.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"hello");

        queue_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_write_parent_stdout() {
        let (child_stdin_write, _child_stdin_read) = duplex(1024);
        let (parent_stdout_write, mut parent_stdout_read) = duplex(1024);
        let (parent_stderr_write, _parent_stderr_read) = duplex(1024);
        let (_parent_stdin_write, parent_stdin_read) = duplex(1024);

        let handles = IoQueueHandles {
            child_stdin: Box::new(child_stdin_write),
            parent_stdout: Box::new(parent_stdout_write),
            parent_stderr: Box::new(parent_stderr_write),
            parent_stdin: Box::new(parent_stdin_read),
        };

        let (queue, handle) = IoQueue::new(16, handles);
        let queue_task = tokio::spawn(queue.run());

        handle
            .write_parent_stdout(b"output".to_vec())
            .await
            .unwrap();
        handle.shutdown().await.unwrap();

        let mut buf = vec![0u8; 1024];
        let n = parent_stdout_read.read(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"output");

        queue_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_read_parent_stdin() {
        let (child_stdin_write, _child_stdin_read) = duplex(1024);
        let (parent_stdout_write, _parent_stdout_read) = duplex(1024);
        let (parent_stderr_write, _parent_stderr_read) = duplex(1024);
        let (mut parent_stdin_write, parent_stdin_read) = duplex(1024);

        let handles = IoQueueHandles {
            child_stdin: Box::new(child_stdin_write),
            parent_stdout: Box::new(parent_stdout_write),
            parent_stderr: Box::new(parent_stderr_write),
            parent_stdin: Box::new(parent_stdin_read),
        };

        let (queue, handle) = IoQueue::new(16, handles);
        let queue_task = tokio::spawn(queue.run());

        // Write some data to parent stdin
        parent_stdin_write.write_all(b"input data").await.unwrap();

        let data = handle.read_parent_stdin(1024).await.unwrap();
        assert_eq!(&data, b"input data");

        handle.shutdown().await.unwrap();
        queue_task.await.unwrap().unwrap();
    }
}
