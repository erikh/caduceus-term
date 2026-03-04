//! caduceus-build: Compile Rust and JavaScript source files into WASM modules
//! that conform to the caduceus guest transform contract.

use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand};

mod js_build;
mod rust_build;

/// Errors that can occur during the build process.
#[derive(Debug)]
pub enum BuildError {
    /// I/O or filesystem error.
    Io(String),
    /// A required tool is missing or not configured.
    ToolMissing(String),
    /// Compilation failed.
    CompileFailed(String),
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildError::Io(msg) => write!(f, "I/O error: {msg}"),
            BuildError::ToolMissing(msg) => write!(f, "missing tool: {msg}"),
            BuildError::CompileFailed(msg) => write!(f, "compilation failed: {msg}"),
        }
    }
}

impl std::error::Error for BuildError {}

/// Compile source files into caduceus-compatible WASM transform modules.
#[derive(Parser)]
#[command(name = "caduceus-build", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Compile a Rust source file or project into a WASM transform module.
    ///
    /// If <INPUT> is a `.rs` file, a temporary Cargo project is generated with
    /// the caduceus boilerplate and compiled. The user's file must define:
    ///   `pub fn transform(input: &[u8]) -> Vec<u8>`
    ///
    /// If <INPUT> is a directory containing a `Cargo.toml`, it is built directly
    /// with `--target wasm32-unknown-unknown`.
    Rust {
        /// Input `.rs` file or Cargo project directory.
        input: PathBuf,

        /// Output `.wasm` file path.
        #[arg(short, long)]
        output: PathBuf,
    },

    /// Compile a JavaScript file into a WASM transform module.
    ///
    /// The JS file is embedded in a QuickJS runtime compiled to `wasm32-wasip1`.
    /// Requires wasi-sdk to be installed. The user's file must define:
    ///   `function transform(data) { ... }`
    /// taking and returning a `Uint8Array`.
    Js {
        /// Input `.js` file.
        input: PathBuf,

        /// Output `.wasm` file path.
        #[arg(short, long)]
        output: PathBuf,
    },
}

fn main() {
    let cli = Cli::parse();

    let result = match cli.command {
        Commands::Rust { input, output } => rust_build::build_rust(&input, &output),
        Commands::Js { input, output } => js_build::build_js(&input, &output),
    };

    if let Err(e) = result {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
