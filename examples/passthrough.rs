use caduceus::proxy::{run_proxy, ChildMode, WasmModuleSource};
use caduceus::ProxyBuilder;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    // Identity WASM transform: returns input unchanged
    let identity_wat = r#"
        (module
            (memory (export "memory") 1)
            (global $bump (mut i32) (i32.const 1024))

            (func (export "alloc") (param $size i32) (result i32)
                (local $ptr i32)
                (local.set $ptr (global.get $bump))
                (global.set $bump (i32.add (global.get $bump) (local.get $size)))
                (local.get $ptr)
            )

            (func (export "transform") (param $ptr i32) (param $len i32) (result i64)
                (i64.or
                    (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
                    (i64.extend_i32_u (local.get $len))
                )
            )
        )
    "#;

    let wasm_bytes = wat::parse_str(identity_wat)?;
    let source = WasmModuleSource::Bytes(wasm_bytes);

    let config = ProxyBuilder::new("cat")
        .child_mode(ChildMode::Piped)
        .stdout_transform(source)
        .build();

    let status = run_proxy(config).await?;
    std::process::exit(status.code().unwrap_or(1));
}
