use std::path::Path;
use std::process::Command;

use crate::BuildError;

/// Locate wasi-sdk: check `WASI_SDK_PATH` env var, then fall back to `/opt/wasi-sdk`.
fn find_wasi_sdk() -> Result<std::path::PathBuf, BuildError> {
    if let Ok(path) = std::env::var("WASI_SDK_PATH") {
        let p = std::path::PathBuf::from(&path);
        if p.is_dir() {
            return Ok(p);
        }
        return Err(BuildError::ToolMissing(format!(
            "WASI_SDK_PATH is set to '{path}' but that directory does not exist"
        )));
    }

    let default = std::path::PathBuf::from("/opt/wasi-sdk");
    if default.is_dir() {
        return Ok(default);
    }

    Err(BuildError::ToolMissing(
        "wasi-sdk not found. Set WASI_SDK_PATH or install to /opt/wasi-sdk.\n\
         Download from: https://github.com/WebAssembly/wasi-sdk/releases"
            .into(),
    ))
}

/// Check that the `wasm32-wasip1` target is installed.
fn check_wasi_target() -> Result<(), BuildError> {
    let output = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .map_err(|e| BuildError::ToolMissing(format!("rustup: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if !stdout.lines().any(|l| l.trim() == "wasm32-wasip1") {
        return Err(BuildError::ToolMissing(
            "wasm32-wasip1 target not installed. Run: rustup target add wasm32-wasip1".into(),
        ));
    }
    Ok(())
}

/// Build a JavaScript file into a WASM module using rquickjs.
pub fn build_js(input: &Path, output: &Path) -> Result<(), BuildError> {
    let wasi_sdk = find_wasi_sdk()?;
    check_wasi_target()?;

    let user_js = std::fs::read_to_string(input)
        .map_err(|e| BuildError::Io(format!("reading {}: {e}", input.display())))?;

    let tmp_dir = tempfile::TempDir::new()
        .map_err(|e| BuildError::Io(format!("creating temp dir: {e}")))?;
    let project_dir = tmp_dir.path();

    // Write Cargo.toml
    std::fs::write(
        project_dir.join("Cargo.toml"),
        r#"[package]
name = "caduceus_js_transform"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib"]

[dependencies]
rquickjs = { version = "0.9", features = ["bindgen", "classes", "properties"] }
"#,
    )
    .map_err(|e| BuildError::Io(format!("writing Cargo.toml: {e}")))?;

    // Write src/lib.rs embedding the user's JS
    let src_dir = project_dir.join("src");
    std::fs::create_dir_all(&src_dir)
        .map_err(|e| BuildError::Io(format!("creating src dir: {e}")))?;

    let escaped_js = user_js.replace('\\', "\\\\").replace('"', "\\\"");

    let lib_rs = format!(
        r#"//! Auto-generated caduceus JS transform wrapper.

use rquickjs::{{Context, Runtime, Function}};
use std::slice;

const USER_JS: &str = "{escaped_js}";

static mut HEAP_PTR: usize = 0x10000;

#[no_mangle]
pub extern "C" fn alloc(size: i32) -> i32 {{
    unsafe {{
        let ptr = HEAP_PTR;
        HEAP_PTR += size as usize;
        HEAP_PTR = (HEAP_PTR + 7) & !7;
        ptr as i32
    }}
}}

#[no_mangle]
pub extern "C" fn transform(ptr: i32, len: i32) -> i64 {{
    let input = unsafe {{ slice::from_raw_parts(ptr as *const u8, len as usize) }};

    let result = run_js_transform(input);

    if result.is_empty() {{
        return 0;
    }}

    let out_len = result.len();
    let out_ptr = alloc(out_len as i32);

    unsafe {{
        core::ptr::copy_nonoverlapping(result.as_ptr(), out_ptr as *mut u8, out_len);
    }}

    ((out_ptr as i64) << 32) | (out_len as i64)
}}

fn run_js_transform(input: &[u8]) -> Vec<u8> {{
    let rt = Runtime::new().expect("failed to create QuickJS runtime");
    let ctx = Context::full(&rt).expect("failed to create QuickJS context");

    ctx.with(|ctx| {{
        // Evaluate user script to define the transform function
        ctx.eval::<(), _>(USER_JS).expect("failed to evaluate user JS");

        // Get the transform function
        let globals = ctx.globals();
        let transform_fn: Function = globals
            .get("transform")
            .expect("user JS must define a global 'transform' function");

        // Convert input to a JS array
        let js_input: Vec<u8> = input.to_vec();

        // Call transform(input) -> output
        let js_output: Vec<u8> = transform_fn
            .call((js_input,))
            .expect("JS transform function failed");

        js_output
    }})
}}
"#,
    );

    std::fs::write(src_dir.join("lib.rs"), &lib_rs)
        .map_err(|e| BuildError::Io(format!("writing lib.rs: {e}")))?;

    // Set up wasi-sdk CC/AR environment
    let cc = wasi_sdk.join("bin/clang");
    let ar = wasi_sdk.join("bin/llvm-ar");

    // Also set the sysroot for C compilation
    let sysroot = wasi_sdk.join("share/wasi-sysroot");

    let cc_flags = if sysroot.exists() {
        format!("{} --sysroot={}", cc.display(), sysroot.display())
    } else {
        cc.display().to_string()
    };

    // Build
    let status = Command::new("cargo")
        .args(["build", "--target", "wasm32-wasip1", "--release"])
        .env("CC_wasm32_wasip1", &cc_flags)
        .env("AR_wasm32_wasip1", ar.display().to_string())
        .env("CFLAGS_wasm32_wasip1", format!("--sysroot={}", sysroot.display()))
        .current_dir(project_dir)
        .status()
        .map_err(|e| BuildError::Io(format!("running cargo build: {e}")))?;

    if !status.success() {
        return Err(BuildError::CompileFailed(
            "cargo build (JS/wasi) failed (see output above)".into(),
        ));
    }

    // Copy output wasm
    let wasm_path = project_dir
        .join("target/wasm32-wasip1/release/caduceus_js_transform.wasm");
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
