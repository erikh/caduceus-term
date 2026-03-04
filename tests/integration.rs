use caduceus::child::SpawnConfig;
use caduceus::queue::{IoQueue, IoQueueHandles, QueueHandle};
use caduceus::wasm::WasmTransform;
use std::collections::HashMap;
use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt};

/// WAT module that uppercases ASCII input
const UPPERCASE_WAT: &str = r#"
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
            (local $i i32)
            (local $out_ptr i32)
            (local $ch i32)

            ;; Allocate output buffer
            (local.set $out_ptr (global.get $bump))
            (global.set $bump (i32.add (global.get $bump) (local.get $len)))

            ;; Copy and uppercase
            (local.set $i (i32.const 0))
            (block $done
                (loop $loop
                    (br_if $done (i32.ge_u (local.get $i) (local.get $len)))
                    (local.set $ch
                        (i32.load8_u (i32.add (local.get $ptr) (local.get $i)))
                    )
                    ;; If 'a' <= ch <= 'z', subtract 32
                    (if (i32.and
                            (i32.ge_u (local.get $ch) (i32.const 97))
                            (i32.le_u (local.get $ch) (i32.const 122))
                        )
                        (then
                            (local.set $ch (i32.sub (local.get $ch) (i32.const 32)))
                        )
                    )
                    (i32.store8
                        (i32.add (local.get $out_ptr) (local.get $i))
                        (local.get $ch)
                    )
                    (local.set $i (i32.add (local.get $i) (i32.const 1)))
                    (br $loop)
                )
            )

            ;; Return packed (out_ptr << 32) | len
            (i64.or
                (i64.shl (i64.extend_i32_u (local.get $out_ptr)) (i64.const 32))
                (i64.extend_i32_u (local.get $len))
            )
        )
    )
"#;

fn make_test_queue() -> (
    tokio::task::JoinHandle<caduceus::error::Result<()>>,
    QueueHandle,
    // Readable ends for verification
    tokio::io::DuplexStream, // child_stdin readable end
    tokio::io::DuplexStream, // parent_stdout readable end
    tokio::io::DuplexStream, // parent_stderr readable end
    tokio::io::DuplexStream, // parent_stdin writable end
) {
    let (cs_w, cs_r) = duplex(4096);
    let (po_w, po_r) = duplex(4096);
    let (pe_w, pe_r) = duplex(4096);
    let (pi_w, pi_r) = duplex(4096);

    let handles = IoQueueHandles {
        child_stdin: Box::new(cs_w),
        parent_stdout: Box::new(po_w),
        parent_stderr: Box::new(pe_w),
        parent_stdin: Box::new(pi_r),
    };

    let (queue, handle) = IoQueue::new(64, handles);
    let task = tokio::spawn(queue.run());
    (task, handle, cs_r, po_r, pe_r, pi_w)
}

#[tokio::test]
async fn test_uppercase_transform() {
    let wasm_bytes = wat::parse_str(UPPERCASE_WAT).unwrap();
    let transform = WasmTransform::new(&wasm_bytes).unwrap();

    let (queue_task, handle, _cs_r, _po_r, _pe_r, _pi_w) = make_test_queue();

    let mut instance = transform.instantiate(handle.clone()).await.unwrap();

    let output = instance.transform(b"hello").await.unwrap();
    assert_eq!(output, b"HELLO");

    let output = instance.transform(b"Hello World 123").await.unwrap();
    assert_eq!(output, b"HELLO WORLD 123");

    handle.shutdown().await.unwrap();
    queue_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn test_queue_write_and_read_round_trip() {
    let (queue_task, handle, mut cs_r, mut po_r, mut pe_r, mut pi_w) = make_test_queue();

    // Write to child stdin and verify
    handle.write_child_stdin(b"to child".to_vec()).await.unwrap();

    let mut buf = vec![0u8; 1024];
    let n = cs_r.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"to child");

    // Write to parent stdout and verify
    handle.write_parent_stdout(b"stdout data".to_vec()).await.unwrap();

    let n = po_r.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"stdout data");

    // Write to parent stderr and verify
    handle.write_parent_stderr(b"stderr data".to_vec()).await.unwrap();

    let n = pe_r.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"stderr data");

    // Read from parent stdin
    pi_w.write_all(b"from parent").await.unwrap();
    let data = handle.read_parent_stdin(1024).await.unwrap();
    assert_eq!(&data, b"from parent");

    handle.shutdown().await.unwrap();
    queue_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn test_piped_child_echo() {
    use caduceus::child::piped::spawn_piped;

    let config = SpawnConfig {
        program: "echo".into(),
        args: vec!["hello".into()],
        env: HashMap::new(),
        cwd: None,
    };

    let mut result = spawn_piped(&config).unwrap();
    let mut buf = vec![0u8; 1024];
    let n = result.io.stdout.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"hello\n");

    let status = result.child.wait().await.unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn test_wasm_host_write_stdout() {
    // WAT module that calls host_write_stdout from within transform
    let wat = r#"
        (module
            (import "env" "host_write_stdout" (func $host_write_stdout (param i32 i32)))
            (memory (export "memory") 1)
            (global $bump (mut i32) (i32.const 1024))

            (func (export "alloc") (param $size i32) (result i32)
                (local $ptr i32)
                (local.set $ptr (global.get $bump))
                (global.set $bump (i32.add (global.get $bump) (local.get $size)))
                (local.get $ptr)
            )

            (func (export "transform") (param $ptr i32) (param $len i32) (result i64)
                ;; Write input to host stdout
                (call $host_write_stdout (local.get $ptr) (local.get $len))
                ;; Return the same data
                (i64.or
                    (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
                    (i64.extend_i32_u (local.get $len))
                )
            )
        )
    "#;

    let wasm_bytes = wat::parse_str(wat).unwrap();
    let transform = WasmTransform::new(&wasm_bytes).unwrap();

    let (queue_task, handle, _cs_r, mut po_r, _pe_r, _pi_w) = make_test_queue();

    let mut instance = transform.instantiate(handle.clone()).await.unwrap();

    let output = instance.transform(b"side effect").await.unwrap();
    assert_eq!(output, b"side effect");

    // The host_write_stdout call should have written to parent stdout
    let mut buf = vec![0u8; 1024];
    let n = po_r.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"side effect");

    handle.shutdown().await.unwrap();
    queue_task.await.unwrap().unwrap();
}
