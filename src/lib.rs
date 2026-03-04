//! Terminal proxy library with WASM-based I/O stream transformation.
//!
//! Caduceus wraps a child process and proxies its stdin, stdout, and stderr
//! through optional WebAssembly transform modules. Each stream can have its own
//! independent WASM transform, allowing you to filter, modify, or inspect
//! terminal I/O in real time.
//!
//! # Quick start
//!
//! ```no_run
//! use caduceus::proxy::run_proxy;
//! use caduceus::{ChildMode, ProxyBuilder, WasmModuleSource};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let wasm_bytes = std::fs::read("my_transform.wasm")?;
//!
//!     let config = ProxyBuilder::new("bash")
//!         .arg("-i")
//!         .child_mode(ChildMode::Piped)
//!         .stdout_transform(WasmModuleSource::Bytes(wasm_bytes))
//!         .build();
//!
//!     let status = run_proxy(config).await?;
//!     std::process::exit(status.code().unwrap_or(1));
//! }
//! ```

/// Child process spawning (piped and PTY modes).
pub mod child;
/// Error types and result alias.
pub mod error;
/// Proxy configuration, builder, and main run loop.
pub mod proxy;
/// Serialized I/O queue for child and parent streams.
pub mod queue;
/// WASM transform compilation and instantiation.
pub mod wasm;

pub use error::{CaduceusError, Result};
pub use proxy::{ChildMode, ProxyBuilder, ProxyConfig, TransformConfig, WasmModuleSource};
pub use queue::QueueHandle;
pub use wasm::{WasmInstance, WasmTransform};
