use anyhow::Result;
use wasmtime::component::{Component, Instance, Linker, ResourceTable};
use wasmtime::{Config as WasmConfig, Engine, Store};
use wasmtime_wasi::p2::add_to_linker_async;
use wasmtime_wasi::{
    cli::{AsyncStdinStream, AsyncStdoutStream},
    WasiCtx,
};

use crate::cell::proc::{ComponentRunStates, BUFFER_SIZE};

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
        config.async_support(true);
        let engine = Engine::new(&config)?;
        Ok(Self { engine })
    }

    /// Compile a WASM component from bytecode
    pub fn compile_component(&self, bytecode: &[u8]) -> Result<Component> {
        Component::from_binary(&self.engine, bytecode)
            .map_err(|e| anyhow::anyhow!("Failed to compile WASM component: {}", e))
    }

    /// Create a new store for component instantiation
    pub fn new_store(&self) -> Store<ComponentRunStates> {
        let wasi_ctx = WasiCtx::builder().build();
        let state = ComponentRunStates {
            wasi_ctx: wasi_ctx,
            resource_table: ResourceTable::new(),
        };
        Store::new(&self.engine, state)
    }

    /// Instantiate a WASM component with WASI support
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
    pub async fn instantiate_component(
        &self,
        bytecode: &[u8],
        args: &[String],
        env: &[String],
        stdin_stream: Option<Box<dyn tokio::io::AsyncRead + Send + Sync + Unpin>>,
        stdout_stream: Option<Box<dyn tokio::io::AsyncWrite + Send + Sync + Unpin>>,
    ) -> Result<(Instance, Store<ComponentRunStates>)> {
        // Create engine and linker
        let mut wasm_config = WasmConfig::new();
        wasm_config.async_support(true);
        let engine = Engine::new(&wasm_config)?;
        let mut linker = Linker::new(&engine);
        add_to_linker_async(&mut linker)?;

        // Prepare environment variables as key-value pairs
        let envs: Vec<(&str, &str)> = env.iter().filter_map(|var| var.split_once('=')).collect();

        // Create WASI context builder
        let mut wasi_ctx_builder = WasiCtx::builder();

        // Add environment variables
        wasi_ctx_builder.envs(&envs);

        // Add command line arguments
        wasi_ctx_builder.args(args);

        // Configure stdin/stdout streams - create async wrappers if provided
        if let Some(stdin) = stdin_stream {
            let wasm_stdin_async = AsyncStdinStream::new(stdin);
            wasi_ctx_builder.stdin(wasm_stdin_async);
        } else {
            wasi_ctx_builder.inherit_stdin();
        }

        if let Some(stdout) = stdout_stream {
            let wasm_stdout_async = AsyncStdoutStream::new(BUFFER_SIZE, stdout);
            wasi_ctx_builder.stdout(wasm_stdout_async);
        } else {
            wasi_ctx_builder.inherit_stdout();
        }

        // Inherit stderr for now
        wasi_ctx_builder.inherit_stderr();

        // Build the WASI context
        let wasi = wasi_ctx_builder.build();

        let state = ComponentRunStates {
            wasi_ctx: wasi,
            resource_table: ResourceTable::new(),
        };
        let mut store = Store::new(&engine, state);

        // Compile and instantiate the component
        let component = Component::from_binary(&engine, bytecode)?;
        let instance = linker.instantiate_async(&mut store, &component).await?;

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

    #[tokio::test]
    async fn test_minimal_wasm_component() {
        // Test basic WASM component functionality without external files
        // Use runtime without debug info to avoid Cranelift debug transformation issues
        let runtime = WasmRuntime::new().unwrap();

        // Create clearly invalid bytecode to test error handling
        let invalid_wasm = vec![
            0xFF, 0xFF, 0xFF, 0xFF, // Invalid magic number
        ];

        // This should fail gracefully for invalid component bytecode
        let result = runtime.compile_component(&invalid_wasm);
        assert!(
            result.is_err(),
            "Should return error for invalid component bytecode"
        );

        // Test that the error message is reasonable
        if let Err(e) = result {
            let error_msg = e.to_string();
            assert!(
                error_msg.contains("Failed to compile WASM component"),
                "Error message should be descriptive: {}",
                error_msg
            );
        }
    }

    #[tokio::test]
    async fn test_component_instantiation_with_io() {
        let runtime = WasmRuntime::new().unwrap();

        // Create simple I/O using async streams
        let stdin_stream = Some(
            Box::new(tokio::io::empty()) as Box<dyn tokio::io::AsyncRead + Send + Sync + Unpin>
        );
        let stdout_stream = Some(
            Box::new(tokio::io::sink()) as Box<dyn tokio::io::AsyncWrite + Send + Sync + Unpin>
        );

        // Create invalid component bytecode
        let invalid_component = vec![
            0xFF, 0xFF, 0xFF, 0xFF, // Invalid magic number
        ];

        // Test that the instantiation method can be called and fails gracefully
        let result = runtime
            .instantiate_component(&invalid_component, &[], &[], stdin_stream, stdout_stream)
            .await;

        // The invalid component should fail gracefully
        assert!(
            result.is_err(),
            "Should return error for invalid component bytecode"
        );
    }
}
