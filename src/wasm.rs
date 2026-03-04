use std::future::Future;

use wasmtime::{Caller, Config, Engine, Instance, Linker, Memory, Module, Store, TypedFunc};

use crate::error::{CaduceusError, Result};
use crate::queue::QueueHandle;

/// Host state stored in the WASM Store.
pub struct WasmState {
    pub queue: QueueHandle,
}

/// A compiled WASM engine + module, reusable across instances.
pub struct WasmTransform {
    engine: Engine,
    module: Module,
}

impl WasmTransform {
    /// Compile a WASM module from bytes.
    pub fn new(wasm_bytes: &[u8]) -> Result<Self> {
        let config = Config::new();
        let engine = Engine::new(&config)?;
        let module = Module::new(&engine, wasm_bytes)?;
        Ok(WasmTransform { engine, module })
    }

    /// Create a live instance bound to a queue handle.
    pub async fn instantiate(&self, queue: QueueHandle) -> Result<WasmInstance> {
        let mut store = Store::new(&self.engine, WasmState { queue });
        let mut linker = Linker::new(&self.engine);

        // Register host functions — params arrive as a tuple
        linker.func_wrap_async(
            "env",
            "host_read_stdin",
            |mut caller: Caller<'_, WasmState>, (buf_ptr, max_len): (i32, i32)| -> Box<dyn Future<Output = i32> + Send + '_> {
                Box::new(async move {
                    let queue = caller.data().queue.clone();
                    let data: Vec<u8> = match queue.read_parent_stdin(max_len as usize).await {
                        Ok(d) => d,
                        Err(_) => return 0,
                    };
                    let memory = match caller.get_export("memory") {
                        Some(wasmtime::Extern::Memory(m)) => m,
                        _ => return 0,
                    };
                    let len = data.len().min(max_len as usize);
                    if memory.write(&mut caller, buf_ptr as usize, &data[..len]).is_err() {
                        return 0;
                    }
                    len as i32
                })
            },
        )?;

        linker.func_wrap_async(
            "env",
            "host_write_stdout",
            |mut caller: Caller<'_, WasmState>, (ptr, len): (i32, i32)| -> Box<dyn Future<Output = ()> + Send + '_> {
                Box::new(async move {
                    let memory = match caller.get_export("memory") {
                        Some(wasmtime::Extern::Memory(m)) => m,
                        _ => return,
                    };
                    let mut buf = vec![0u8; len as usize];
                    if memory.read(&caller, ptr as usize, &mut buf).is_err() {
                        return;
                    }
                    let queue = caller.data().queue.clone();
                    let _ = queue.write_parent_stdout(buf).await;
                })
            },
        )?;

        linker.func_wrap_async(
            "env",
            "host_write_stderr",
            |mut caller: Caller<'_, WasmState>, (ptr, len): (i32, i32)| -> Box<dyn Future<Output = ()> + Send + '_> {
                Box::new(async move {
                    let memory = match caller.get_export("memory") {
                        Some(wasmtime::Extern::Memory(m)) => m,
                        _ => return,
                    };
                    let mut buf = vec![0u8; len as usize];
                    if memory.read(&caller, ptr as usize, &mut buf).is_err() {
                        return;
                    }
                    let queue = caller.data().queue.clone();
                    let _ = queue.write_parent_stderr(buf).await;
                })
            },
        )?;

        let instance = linker.instantiate_async(&mut store, &self.module).await?;

        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or_else(|| CaduceusError::MissingExport {
                export: "memory".into(),
            })?;

        let alloc_fn: TypedFunc<i32, i32> = instance
            .get_typed_func(&mut store, "alloc")
            .map_err(|_| CaduceusError::MissingExport {
                export: "alloc".into(),
            })?;

        let transform_fn: TypedFunc<(i32, i32), i64> = instance
            .get_typed_func(&mut store, "transform")
            .map_err(|_| CaduceusError::MissingExport {
                export: "transform".into(),
            })?;

        Ok(WasmInstance {
            store,
            _instance: instance,
            memory,
            alloc_fn,
            transform_fn,
        })
    }
}

/// A live WASM instance ready to transform data.
pub struct WasmInstance {
    store: Store<WasmState>,
    _instance: Instance,
    memory: Memory,
    alloc_fn: TypedFunc<i32, i32>,
    transform_fn: TypedFunc<(i32, i32), i64>,
}

impl WasmInstance {
    /// Transform input bytes through the WASM module.
    ///
    /// Protocol:
    /// 1. Call `alloc(len)` to get a pointer in guest memory
    /// 2. Write input data to that pointer
    /// 3. Call `transform(ptr, len)` which returns a packed i64: `(out_ptr << 32) | out_len`
    /// 4. Read output data from the returned pointer
    pub async fn transform(&mut self, input: &[u8]) -> Result<Vec<u8>> {
        let len = input.len() as i32;

        // Allocate space in guest memory
        let ptr = self.alloc_fn.call_async(&mut self.store, len).await?;
        if ptr == 0 {
            return Err(CaduceusError::AllocFailed);
        }

        // Write input data
        self.memory
            .write(&mut self.store, ptr as usize, input)
            .map_err(|_| CaduceusError::MemoryAccess {
                offset: ptr as usize,
                size: input.len(),
            })?;

        // Call transform
        let result = self
            .transform_fn
            .call_async(&mut self.store, (ptr, len))
            .await?;

        // Unpack result: high 32 bits = out_ptr, low 32 bits = out_len
        let out_ptr = (result >> 32) as u32 as usize;
        let out_len = (result & 0xFFFF_FFFF) as u32 as usize;

        if out_len == 0 {
            return Ok(Vec::new());
        }

        // Read output data
        let mut output = vec![0u8; out_len];
        self.memory
            .read(&self.store, out_ptr, &mut output)
            .map_err(|_| CaduceusError::MemoryAccess {
                offset: out_ptr,
                size: out_len,
            })?;

        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::{IoQueue, IoQueueHandles};
    use tokio::io::duplex;

    // A simple WAT identity transform: returns input unchanged
    const IDENTITY_WAT: &str = r#"
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
                ;; Identity: return the same ptr and len packed as i64
                (i64.or
                    (i64.shl (i64.extend_i32_u (local.get $ptr)) (i64.const 32))
                    (i64.extend_i32_u (local.get $len))
                )
            )
        )
    "#;

    fn make_queue() -> (tokio::task::JoinHandle<Result<()>>, QueueHandle) {
        let (cs_w, _cs_r) = duplex(1024);
        let (po_w, _po_r) = duplex(1024);
        let (pe_w, _pe_r) = duplex(1024);
        let (_pi_w, pi_r) = duplex(1024);

        let handles = IoQueueHandles {
            child_stdin: Box::new(cs_w),
            parent_stdout: Box::new(po_w),
            parent_stderr: Box::new(pe_w),
            parent_stdin: Box::new(pi_r),
        };

        let (queue, handle) = IoQueue::new(16, handles);
        let task = tokio::spawn(queue.run());
        (task, handle)
    }

    #[tokio::test]
    async fn test_identity_transform() {
        let wasm_bytes = wat::parse_str(IDENTITY_WAT).unwrap();
        let transform = WasmTransform::new(&wasm_bytes).unwrap();

        let (queue_task, handle) = make_queue();

        let mut instance = transform.instantiate(handle.clone()).await.unwrap();

        let output = instance.transform(b"hello world").await.unwrap();
        assert_eq!(output, b"hello world");

        handle.shutdown().await.unwrap();
        queue_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_empty_transform() {
        let wasm_bytes = wat::parse_str(IDENTITY_WAT).unwrap();
        let transform = WasmTransform::new(&wasm_bytes).unwrap();

        let (queue_task, handle) = make_queue();
        let mut instance = transform.instantiate(handle.clone()).await.unwrap();

        let output = instance.transform(b"").await.unwrap();
        assert_eq!(output, b"");

        handle.shutdown().await.unwrap();
        queue_task.await.unwrap().unwrap();
    }
}
