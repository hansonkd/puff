//! WASM tool runtime via wasmtime.
//!
//! When the `wasm-tools` feature is enabled, WASM modules are executed via
//! wasmtime using WASIp1.  Input JSON is passed on stdin; the module writes its
//! JSON result to stdout.
//!
//! When the feature is **not** enabled, `execute_wasm_tool` returns a clear
//! error telling the caller how to opt in.

// ---------------------------------------------------------------------------
// Feature-gated implementation
// ---------------------------------------------------------------------------

#[cfg(feature = "wasm-tools")]
mod wasm_impl {
    use crate::agents::error::AgentError;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Mutex;
    use wasmtime::{Engine, Linker, Module, Store};
    use wasmtime_wasi::p1::{self, WasiP1Ctx};
    use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
    use wasmtime_wasi::WasiCtxBuilder;

    // -----------------------------------------------------------------------
    // Module cache
    // -----------------------------------------------------------------------

    /// Cache for compiled WASM modules.
    ///
    /// Compilation is expensive (~10–50 ms per module); execution of a cached
    /// module is fast.  The cache is keyed on the canonical path string.
    pub struct WasmModuleCache {
        engine: Engine,
        modules: Mutex<HashMap<String, Module>>,
    }

    impl WasmModuleCache {
        /// Create a new, empty cache with a default `Engine`.
        pub fn new() -> Result<Self, AgentError> {
            let engine = Engine::default();
            Ok(Self {
                engine,
                modules: Mutex::new(HashMap::new()),
            })
        }

        /// Return the underlying `Engine`.
        pub fn engine(&self) -> &Engine {
            &self.engine
        }

        /// Return (or compile and insert) the `Module` for `path`.
        fn get_or_compile(&self, path: &Path) -> Result<Module, AgentError> {
            let key = path.display().to_string();
            {
                let cache = self.modules.lock().unwrap();
                if let Some(module) = cache.get(&key) {
                    return Ok(module.clone());
                }
            }

            // Compile outside the lock so other threads are not blocked.
            let module = Module::from_file(&self.engine, path).map_err(|e| {
                AgentError::ToolExecutionError {
                    tool: key.clone(),
                    message: format!("Failed to compile WASM module: {e}"),
                }
            })?;

            self.modules.lock().unwrap().insert(key, module.clone());

            Ok(module)
        }
    }

    impl Default for WasmModuleCache {
        fn default() -> Self {
            Self::new().expect("wasmtime Engine::default() must succeed")
        }
    }

    // -----------------------------------------------------------------------
    // Executor
    // -----------------------------------------------------------------------

    /// Execute a WASM tool module.
    ///
    /// * `cache`        — compiled-module cache (shared across calls)
    /// * `module_path`  — path to the `.wasm` file
    /// * `input_json`   — JSON string delivered to the module via stdin
    /// * `timeout_ms`   — not yet enforced at the WASM level; reserved for
    ///                    future epoch-based interruption
    ///
    /// The module is expected to follow the WASIp1 "command" convention:
    /// export a `_start` function, read its input from stdin, and write its
    /// result to stdout.
    pub fn execute_wasm_tool_with_cache(
        cache: &WasmModuleCache,
        module_path: &Path,
        input_json: &str,
        _timeout_ms: u64,
    ) -> Result<String, AgentError> {
        let tool_name = module_path.display().to_string();

        let module = cache.get_or_compile(module_path)?;

        // Build an in-memory stdout pipe.  Because MemoryOutputPipe is backed
        // by an Arc<Mutex<…>>, cloning it before handing it to the builder lets
        // us read the contents after execution.
        let stdout_pipe = MemoryOutputPipe::new(1024 * 1024); // 1 MiB cap
        let stdout_for_read = stdout_pipe.clone();

        // WASIp1 context — stdin carries the JSON input.
        let wasi_ctx: WasiP1Ctx = WasiCtxBuilder::new()
            .stdin(MemoryInputPipe::new(input_json.as_bytes().to_vec()))
            .stdout(stdout_pipe)
            .build_p1();

        let mut store: Store<WasiP1Ctx> = Store::new(cache.engine(), wasi_ctx);

        // Link all WASIp1 host functions synchronously.
        let mut linker: Linker<WasiP1Ctx> = Linker::new(cache.engine());
        p1::add_to_linker_sync(&mut linker, |ctx| ctx).map_err(|e| {
            AgentError::ToolExecutionError {
                tool: tool_name.clone(),
                message: format!("Failed to link WASI: {e}"),
            }
        })?;

        let instance = linker.instantiate(&mut store, &module).map_err(|e| {
            AgentError::ToolExecutionError {
                tool: tool_name.clone(),
                message: format!("Failed to instantiate WASM module: {e}"),
            }
        })?;

        let start_fn = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(|e| AgentError::ToolExecutionError {
                tool: tool_name.clone(),
                message: format!("WASM module has no _start export: {e}"),
            })?;

        // Call the module.  Trap on non-zero WASI exit codes is expected; the
        // module may call `proc_exit(0)` on success which surfaces as a Trap.
        // We treat that as success when stdout has content.
        let call_result = start_fn.call(&mut store, ());

        // Collect stdout regardless of whether _start trapped.
        let output_bytes = stdout_for_read.contents();
        let output = String::from_utf8_lossy(&output_bytes).into_owned();

        match call_result {
            Ok(()) => Ok(output),
            Err(trap) => {
                // WASIp1 programs commonly exit via proc_exit(0), which
                // wasmtime surfaces as a Trap containing an I32Exit.  Check for
                // that specifically.
                if let Some(exit) = trap.downcast_ref::<wasmtime_wasi::I32Exit>() {
                    if exit.0 == 0 {
                        return Ok(output);
                    }
                    return Err(AgentError::ToolExecutionError {
                        tool: tool_name,
                        message: format!(
                            "WASM module exited with code {}: {}",
                            exit.0,
                            output.trim()
                        ),
                    });
                }
                // Any other trap — propagate, but include whatever stdout we got.
                Err(AgentError::ToolExecutionError {
                    tool: tool_name,
                    message: format!("WASM execution trap: {trap}"),
                })
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Public surface — feature enabled
// ---------------------------------------------------------------------------

#[cfg(feature = "wasm-tools")]
pub use wasm_impl::WasmModuleCache;

/// Execute a WASM tool module using a process-wide compiled-module cache.
#[cfg(feature = "wasm-tools")]
pub fn execute_wasm_tool(
    module_path: &std::path::Path,
    input_json: &str,
    timeout_ms: u64,
) -> Result<String, crate::agents::error::AgentError> {
    lazy_static::lazy_static! {
        static ref CACHE: WasmModuleCache = WasmModuleCache::default();
    }

    wasm_impl::execute_wasm_tool_with_cache(&CACHE, module_path, input_json, timeout_ms)
}

// ---------------------------------------------------------------------------
// Stub — feature not enabled
// ---------------------------------------------------------------------------

/// Always returns an error explaining how to enable WASM tool support.
#[cfg(not(feature = "wasm-tools"))]
pub fn execute_wasm_tool(
    _module_path: &std::path::Path,
    _input_json: &str,
    _timeout_ms: u64,
) -> Result<String, crate::agents::error::AgentError> {
    Err(crate::agents::error::AgentError::ToolExecutionError {
        tool: "wasm".into(),
        message: "WASM tools require the 'wasm-tools' feature. \
                  Rebuild with `--features wasm-tools`."
            .into(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #[cfg(not(feature = "wasm-tools"))]
    #[test]
    fn stub_returns_helpful_error() {
        use super::execute_wasm_tool;
        use crate::agents::error::AgentError;

        let result = execute_wasm_tool(std::path::Path::new("/nonexistent/tool.wasm"), "{}", 5000);

        match result {
            Err(AgentError::ToolExecutionError { message, .. }) => {
                assert!(
                    message.contains("wasm-tools"),
                    "error should mention the feature flag"
                );
            }
            other => panic!("expected ToolExecutionError, got {:?}", other),
        }
    }
}
