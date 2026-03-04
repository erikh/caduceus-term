use std::path::Path;
use std::process::Command;

use crate::BuildError;

/// Check that the `wasm32-unknown-unknown` target is installed.
fn check_wasm_target() -> Result<(), BuildError> {
    let output = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map_err(|e| BuildError::ToolMissing(format!("rustup: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.lines().any(|l| l.trim() == "wasm32-unknown-unknown") {
        return Err(BuildError::ToolMissing(
            "wasm32-unknown-unknown target not installed. Run: rustup target add wasm32-unknown-unknown".into(),
        ));
    }
    Ok(())
}

/// Build a single `.rs` file by generating a temporary Cargo project.
pub fn build_single_file(input: &Path, output: &Path) -> Result<(), BuildError> {
    check_wasm_target()?;

    let user_code = std::fs::read_to_string(input)
        .map_err(|e| BuildError::Io(format!("reading {}: {e}", input.display())))?;

    let tmp_dir = tempfile::TempDir::new()
        .map_err(|e| BuildError::Io(format!("creating temp dir: {e}")))?;
    let project_dir = tmp_dir.path();

    // Write Cargo.toml
    std::fs::write(
        project_dir.join("Cargo.toml"),
        r#"[package]
name = "caduceus_user_transform"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]
"#,
    )
    .map_err(|e| BuildError::Io(format!("writing Cargo.toml: {e}")))?;

    // Write src/lib.rs with boilerplate
    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(&src_dir)
        .map_err(|e| BuildError::Io(format!("creating src dir: {e}")))?;

    std::fs::write(
        src_dir.join("lib.rs"),
        BOILERPLATE_LIB,
    )
    .map_err(|e| BuildError::Io(format!("writing lib.rs: {e}")))?;

    // Write user code as src/user_transform.rs
    std::fs::write(src_dir.join("user_transform.rs"), &user_code)
        .map_err(|e| BuildError::Io(format!("writing user_transform.rs: {e}")))?;

    // Build
    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-unknown-unknown", "--release"])
        .current_dir(project_dir)
        .status()
        .map_err(|e| BuildError::Io(format!("running cargo build: {e}")))?;

    if !status.success() {
        return Err(BuildError::CompileFailed(
            "cargo build failed (see output above)".into(),
        ));
    }

    // Copy output wasm
    let wasm_path = project_dir
        .join("target/wasm32-unknown-unknown/release/caduceus_user_transform.wasm");
    if !wasm_path.exists() {
        return Err(BuildError::Io(format!(
            "expected wasm output not found at {}",
            wasm_path.display()
        )));
    }

    std::fs::copy(&wasm_path, output)
        .map_err(|e| BuildError::Io(format!("copying wasm to {}: {e}", output.display())))?;

    Ok(())
}

/// Build a full Cargo project directory.
pub fn build_project(input: &Path, output: &Path) -> Result<(), BuildError> {
    check_wasm_target()?;

    // Build
    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-unknown-unknown", "--release"])
        .current_dir(input)
        .status()
        .map_err(|e| BuildError::Io(format!("running cargo build: {e}")))?;

    if !status.success() {
        return Err(BuildError::CompileFailed(
            "cargo build failed (see output above)".into(),
        ));
    }

    // Get package name from cargo metadata
    let metadata_output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(input)
        .output()
        .map_err(|e| BuildError::Io(format!("running cargo metadata: {e}")))?;

    if !metadata_output.status.success() {
        return Err(BuildError::CompileFailed(
            "cargo metadata failed".into(),
        ));
    }

    let pkg_name = extract_package_name(&metadata_output.stdout)?;
    let wasm_name = pkg_name.replace('-', "_");

    let wasm_path = input
        .join("target/wasm32-unknown-unknown/release")
        .join(format!("{wasm_name}.wasm"));

    if !wasm_path.exists() {
        return Err(BuildError::Io(format!(
            "expected wasm output not found at {}",
            wasm_path.display()
        )));
    }

    std::fs::copy(&wasm_path, output)
        .map_err(|e| BuildError::Io(format!("copying wasm to {}: {e}", output.display())))?;

    Ok(())
}

/// Extract the package name from `cargo metadata` JSON output.
fn extract_package_name(json_bytes: &[u8]) -> Result<String, BuildError> {
    // Minimal JSON parsing — look for "packages":[{"name":"..."}]
    let json_str = std::str::from_utf8(json_bytes)
        .map_err(|_| BuildError::CompileFailed("cargo metadata output is not valid UTF-8".into()))?;

    // Find "packages":[ ... ] and extract the first "name":"..."
    if let Some(packages_start) = json_str.find("\"packages\"") {
        let rest = &json_str[packages_start..];
        if let Some(name_start) = rest.find("\"name\"") {
            let after_name = &rest[name_start + 6..]; // skip "name"
            // Find the value after the colon
            if let Some(colon) = after_name.find(':') {
                let after_colon = after_name[colon + 1..].trim_start();
                if after_colon.starts_with('"') {
                    let value_start = 1;
                    if let Some(value_end) = after_colon[value_start..].find('"') {
                        return Ok(after_colon[value_start..value_start + value_end].to_string());
                    }
                }
            }
        }
    }

    Err(BuildError::CompileFailed(
        "could not extract package name from cargo metadata".into(),
    ))
}

/// Determine input mode: single file (.rs) or project directory.
pub fn build_rust(input: &Path, output: &Path) -> Result<(), BuildError> {
    if input.is_file() {
        build_single_file(input, output)
    } else if input.is_dir() {
        let cargo_toml = input.join("Cargo.toml");
        if !cargo_toml.exists() {
            return Err(BuildError::Io(format!(
                "directory {} does not contain a Cargo.toml",
                input.display()
            )));
        }
        build_project(input, output)
    } else {
        Err(BuildError::Io(format!(
            "input {} is neither a file nor a directory",
            input.display()
        )))
    }
}

const BOILERPLATE_LIB: &str = r#"//! Auto-generated caduceus transform wrapper.
#![no_std]

extern crate alloc;
use alloc::vec::Vec;
use core::slice;

/// Bump allocator for guest memory.
static mut HEAP_PTR: usize = 0x10000;

/// Allocate `size` bytes in guest memory (bump allocator).
#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {
    unsafe {
        let ptr = HEAP_PTR;
        HEAP_PTR += size as usize;
        // Align to 8 bytes
        HEAP_PTR = (HEAP_PTR + 7) & !7;
        ptr as i32
    }
}

/// Optional host imports — user code can call these if the host provides them.
pub mod host {
    extern "C" {
        /// Read up to `max_len` bytes from host stdin into `buf`. Returns bytes read.
        pub fn host_read_stdin(buf: *mut u8, max_len: i32) -> i32;
        /// Write `len` bytes from `ptr` to host stdout.
        pub fn host_write_stdout(ptr: *const u8, len: i32);
        /// Write `len` bytes from `ptr` to host stderr.
        pub fn host_write_stderr(ptr: *const u8, len: i32);
    }
}

mod user_transform;

/// Transform entry point called by the caduceus runtime.
///
/// Reads `len` bytes from `ptr`, calls the user's `transform` function,
/// allocates space for the output, copies it, and returns a packed i64:
/// `(out_ptr << 32) | out_len`.
#[no_mangle]
pub extern "C" fn transform(ptr: i32, len: i32) -> i64 {
    let input = unsafe { slice::from_raw_parts(ptr as *const u8, len as usize) };
    let output: Vec<u8> = user_transform::transform(input);

    if output.is_empty() {
        return 0;
    }

    let out_len = output.len();
    let out_ptr = alloc(out_len as i32);

    unsafe {
        core::ptr::copy_nonoverlapping(output.as_ptr(), out_ptr as *mut u8, out_len);
    }

    ((out_ptr as i64) << 32) | (out_len as i64)
}
"#;
