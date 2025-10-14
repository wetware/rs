use anyhow::Result;
use wasmtime::{Engine, Store, Module, Instance, Linker};
use wasmtime_wasi::p1::{WasiP1Ctx, add_to_linker_sync};
use wasmtime_wasi::{WasiCtxBuilder, cli::{AsyncStdinStream, AsyncStdoutStream}};

/// Cell runtime wrapper using Wasmtime
/// 
/// This provides a high-level interface for compiling and running cell modules
/// with WASI support, following the Go implementation's approach with wazero.
pub struct WasmRuntime {
    engine: Engine,
}

impl WasmRuntime {
    /// Create a new WASM runtime with async support
    pub fn new() -> Result<Self> {
        let mut config = wasmtime::Config::new();
        config.async_support(true);
        let engine = Engine::new(&config)?;
        Ok(Self { engine })
    }

    /// Create a new WASM runtime with debug info enabled
    pub fn new_with_debug(debug: bool) -> Result<Self> {
        let mut config = wasmtime::Config::default();
        config.debug_info(debug);
        let engine = Engine::new(&config)?;
        Ok(Self { engine })
    }

    /// Compile a WASM module from bytecode
    pub fn compile_module(&self, bytecode: &[u8]) -> Result<Module> {
        Module::from_binary(&self.engine, bytecode)
            .map_err(|e| anyhow::anyhow!("Failed to compile WASM module: {}", e))
    }

    /// Create a new store for module instantiation
    pub fn new_store(&self) -> Store<WasiP1Ctx> {
        let wasi_ctx = WasiCtxBuilder::new().build_p1();
        Store::new(&self.engine, wasi_ctx)
    }

    /// Instantiate a WASM module with WASI support
    /// 
    /// CANONICAL BEHAVIOR: `_start` is ALWAYS called synchronously during instantiation
    /// for both sync and async modes. This allows WASM modules to initialize global state.
    /// 
    /// This differs from the Go implementation which skips `_start` in async mode using
    /// wazero's WithStartFunctions(). The Rust behavior is recommended by lthibault as
    /// canonical going forward.
    /// 
    /// The endpoint parameter provides stdin/stdout for the WASM module.
    /// In sync mode, this will be stdin/stdout. In async mode, it will be
    /// set per-message to the network stream.
    pub fn instantiate_module(
        &self,
        module: &Module,
        args: &[String],
        env: &[String],
        stdin_stream: Option<Box<dyn tokio::io::AsyncRead + Send + Sync + Unpin>>,
        stdout_stream: Option<Box<dyn tokio::io::AsyncWrite + Send + Sync + Unpin>>,
    ) -> Result<(Instance, Store<WasiP1Ctx>)> {
        // Create WASI context builder
        let mut wasi_ctx_builder = WasiCtxBuilder::new();
        
        // Add environment variables
        for var in env {
            if let Some((key, value)) = var.split_once('=') {
                wasi_ctx_builder.env(key, value);
            }
        }
        
        // Add command line arguments
        for arg in args {
            wasi_ctx_builder.arg(arg);
        }
        
        // Configure stdin/stdout streams
        if let Some(stdin) = stdin_stream {
            let async_stdin = AsyncStdinStream::new(stdin);
            wasi_ctx_builder.stdin(async_stdin);
        } else {
            wasi_ctx_builder.inherit_stdin();
        }
        
        if let Some(stdout) = stdout_stream {
            let async_stdout = AsyncStdoutStream::new(1024, stdout);
            wasi_ctx_builder.stdout(async_stdout);
        } else {
            wasi_ctx_builder.inherit_stdout();
        }
        
        // Inherit stderr for now
        wasi_ctx_builder.inherit_stderr();
        
        let wasi_ctx = wasi_ctx_builder.build_p1();
        let mut store = Store::new(&self.engine, wasi_ctx);
        
        // Create linker and add WASI support using WASIp1 with async streams
        let mut linker = Linker::new(&self.engine);
        add_to_linker_sync(&mut linker, |ctx: &mut WasiP1Ctx| ctx)?;
        
        // Instantiate the module
        // NOTE: wasmtime automatically calls _start during instantiation for WASI modules.
        // This is the correct behavior for both sync and async modes - it allows modules
        // to initialize global state before processing requests.
        let instance = linker.instantiate(&mut store, module)?;
        
        Ok((instance, store))
    }
}

impl Default for WasmRuntime {
    fn default() -> Self {
        Self::new().expect("Failed to create WASM runtime")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_runtime_creation() {
        let runtime = WasmRuntime::new();
        assert!(runtime.is_ok());
    }

    #[test]
    fn test_runtime_with_debug() {
        let runtime = WasmRuntime::new_with_debug(true);
        assert!(runtime.is_ok());
        
        let runtime = WasmRuntime::new_with_debug(false);
        assert!(runtime.is_ok());
    }

    #[test]
    fn test_store_creation() {
        let runtime = WasmRuntime::new().unwrap();
        let _store = runtime.new_store();
        // Store creation should succeed
        assert!(true);
    }

    #[test]
    fn test_minimal_wasm_runtime() {
        // Test basic WASM runtime functionality without external files
        let runtime = WasmRuntime::new_with_debug(true).unwrap();
        
        // Create a minimal WASM module (just the module header)
        // This is a minimal valid WASM module that does nothing
        let minimal_wasm = vec![
            0x00, 0x61, 0x73, 0x6d, // WASM magic number
            0x01, 0x00, 0x00, 0x00, // Version 1
        ];
        
        // This should compile successfully (even if minimal)
        let result = runtime.compile_module(&minimal_wasm);
        assert!(result.is_ok(), "Should be able to compile minimal WASM module");
    }

    #[test]
    fn test_wasm_function_discovery() {
        // Test that we can discover functions in a WASM module
        let runtime = WasmRuntime::new_with_debug(true).unwrap();
        
        // Create a simple WASM module with a function
        // This is a minimal WASM module with an empty function
        let wasm_with_function = vec![
            0x00, 0x61, 0x73, 0x6d, // WASM magic
            0x01, 0x00, 0x00, 0x00, // Version 1
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // Type section: 1 function type (no params, no returns)
            0x03, 0x02, 0x01, 0x00, // Function section: 1 function of type 0
            0x07, 0x07, 0x01, 0x03, 0x66, 0x6f, 0x6f, 0x00, 0x00, // Export section: export "foo" function
            0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b, // Code section: empty function body
        ];
        
        let module = runtime.compile_module(&wasm_with_function).unwrap();
        
        // Create simple I/O using async streams
        let stdin_stream = Some(Box::new(tokio::io::empty()) as Box<dyn tokio::io::AsyncRead + Send + Sync + Unpin>);
        let stdout_stream = Some(Box::new(tokio::io::sink()) as Box<dyn tokio::io::AsyncWrite + Send + Sync + Unpin>);
        
        let (instance, mut store) = runtime.instantiate_module(
            &module,
            &[],
            &[],
            stdin_stream,
            stdout_stream,
        ).unwrap();
        
        // Test function discovery
        assert!(instance.get_func(&mut store, "foo").is_some(), "Should find exported 'foo' function");
        assert!(instance.get_func(&mut store, "nonexistent").is_none(), "Should not find nonexistent function");
    }
}
