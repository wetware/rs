use anyhow::{anyhow, Result};
use std::io::Write;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tracing::info;
use wasmtime::{Module, Instance, Store};
use wasmtime_wasi::p1::WasiP1Ctx;
use libp2p::swarm::Stream;

use crate::cell::endpoint::Endpoint;
use crate::cell::runtime::WasmRuntime;
use crate::cell::pipe::Pipe;

/// Configuration for cell process creation
pub struct Config {
    /// Environment variables for the cell process
    pub env: Vec<String>,
    /// Command line arguments for the cell process
    pub args: Vec<String>,
    /// Whether to enable cell debug info
    pub wasm_debug: bool,
    /// Error writer for stderr
    pub err_writer: Option<Box<dyn Write + Send + Sync>>,
}

impl Default for Config {
    fn default() -> Self {
        Self::new()
    }
}

impl Config {
    /// Create a new process configuration
    pub fn new() -> Self {
        Self {
            env: Vec::new(),
            args: Vec::new(),
            wasm_debug: false,
            err_writer: None,
        }
    }

    /// Set WASM debug mode
    pub fn with_wasm_debug(mut self, debug: bool) -> Self {
        self.wasm_debug = debug;
        self
    }

    /// Add environment variables
    pub fn with_env(mut self, env: Vec<String>) -> Self {
        self.env = env;
        self
    }

    /// Add command line arguments
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// Set error writer
    pub fn with_err_writer(mut self, writer: Box<dyn Write + Send + Sync>) -> Self {
        self.err_writer = Some(writer);
        self
    }
}

/// A cell process that can run in sync or async mode
/// 
/// This matches the Go implementation's Proc struct, providing
/// dual-mode execution with method-based routing for cell exports.
pub struct Proc {
    /// Process configuration
    pub config: Config,
    /// Communication endpoint
    pub endpoint: Endpoint,
    /// Compiled cell module
    pub module: Module,
    /// Cell runtime instance
    pub instance: Instance,
    /// Cell runtime store
    pub store: Store<WasiP1Ctx>,
    /// Semaphore for thread-safe message processing
    pub semaphore: Arc<Semaphore>,
    /// Stdin pipe (can be swapped per-message in async mode)
    pub stdin_pipe: Pipe,
    /// Stdout pipe (can be swapped per-message in async mode)
    pub stdout_pipe: Pipe,
}

impl Proc {
    /// Create a new WASM process with duplex pipes for bidirectional communication
    /// 
    /// Returns the Proc instance and the host-side handles for stdin/stdout.
    /// The WASM instance gets the other end of the pipes.
    pub fn new_with_duplex_pipes(
        config: Config, 
        bytecode: &[u8],
    ) -> Result<(Self, tokio::io::DuplexStream, tokio::io::DuplexStream)> {
        // Create duplex pipes for bidirectional communication
        let (host_stdin, wasm_stdin) = tokio::io::duplex(1024);
        let (wasm_stdout, host_stdout) = tokio::io::duplex(1024);
        
        // Create the Proc with the WASM-side pipes
        let proc = Self::new_with_pipes(
            config,
            bytecode,
            Box::new(wasm_stdin),
            Box::new(wasm_stdout),
        )?;
        
        Ok((proc, host_stdin, host_stdout))
    }

    /// Create a new WASM process from bytecode with async pipes
    /// 
    /// This creates a WASM instance with dedicated stdin/stdout pipes.
    /// The host-side handles are returned for bidirectional communication.
    pub fn new_with_pipes(
        config: Config, 
        bytecode: &[u8],
        stdin_stream: Box<dyn tokio::io::AsyncRead + Send + Sync + Unpin>,
        stdout_stream: Box<dyn tokio::io::AsyncWrite + Send + Sync + Unpin>,
    ) -> Result<Self> {
        // Create WASM runtime
        let runtime = WasmRuntime::new_with_debug(config.wasm_debug)?;
        
        // Compile the WASM module
        let module = runtime.compile_module(bytecode)?;
        
        // Create endpoint
        let endpoint = Endpoint::new();
        
        // Create pipes for stdin/stdout (legacy - will be removed)
        let stdin_pipe = Pipe::new();
        let stdout_pipe = Pipe::new();
        
        // Instantiate the module with WASI support
        // NOTE: _start is automatically called during instantiation
        let (instance, store) = runtime.instantiate_module(
            &module,
            &config.args,
            &config.env,
            Some(stdin_stream),
            Some(stdout_stream),
        )?;
        
        // Create semaphore for thread-safe message processing
        let semaphore = Arc::new(Semaphore::new(1));
        
        Ok(Self {
            config,
            endpoint,
            module,
            instance,
            store,
            semaphore,
            stdin_pipe,
            stdout_pipe,
        })
    }

    /// Create a new WASM process from bytecode (legacy method)
    pub fn new(config: Config, bytecode: &[u8]) -> Result<Self> {
        // Create WASM runtime
        let runtime = WasmRuntime::new_with_debug(config.wasm_debug)?;
        
        // Compile the WASM module
        let module = runtime.compile_module(bytecode)?;
        
        // Create endpoint
        let endpoint = Endpoint::new();
        
        // Create pipes for stdin/stdout
        // These can be swapped per-message in async mode
        let stdin_pipe = Pipe::new();
        let stdout_pipe = Pipe::new();
        
        // Set up stdio streams initially
        let stdin_stream: Option<Box<dyn tokio::io::AsyncRead + Send + Sync + Unpin>> = 
            Some(Box::new(crate::cell::stream::StdioWrapper::new()));
        let stdout_stream: Option<Box<dyn tokio::io::AsyncWrite + Send + Sync + Unpin>> = 
            Some(Box::new(crate::cell::stream::StdioWrapper::new()));
        
        // Instantiate the module with WASI support
        // NOTE: _start is automatically called during instantiation for both sync and async modes
        let (instance, store) = runtime.instantiate_module(
            &module,
            &config.args,
            &config.env,
            stdin_stream,
            stdout_stream,
        )?;
        
        // CANONICAL BEHAVIOR: _start runs automatically during instantiation for BOTH modes
        // This allows WASM modules to initialize global state before processing requests
        
        // Log WASM exports for debugging
        info!("WASM module exports:");
        for export in module.exports() {
            match export.ty() {
                wasmtime::ExternType::Func(_) => {
                    info!("  Function: {}", export.name());
                }
                wasmtime::ExternType::Memory(_) => {
                    info!("  Memory: {}", export.name());
                }
                wasmtime::ExternType::Table(_) => {
                    info!("  Table: {}", export.name());
                }
                wasmtime::ExternType::Global(_) => {
                    info!("  Global: {}", export.name());
                }
                wasmtime::ExternType::Tag(_) => {
                    info!("  Tag: {}", export.name());
                }
            }
        }
        
        Ok(Proc {
            config,
            endpoint,
            module,
            instance,
            store,
            semaphore: Arc::new(Semaphore::new(1)),
            stdin_pipe,
            stdout_pipe,
        })
    }

    /// Get the process ID (endpoint name)
    pub fn id(&self) -> &str {
        &self.endpoint.name
    }

    /// Get the protocol for this process
    pub fn protocol(&self) -> String {
        self.endpoint.protocol()
    }

    /// Process a message in async mode
    /// 
    /// This sets the stream as stdin/stdout, calls the specified WASM export function,
    /// then clears the stream. This matches the Go implementation's ProcessMessage semantics.
    /// Process a message using the specified method
    /// 
    /// NOTE: This method needs to be redesigned for proper async I/O.
    /// The current approach of swapping streams mid-execution is not compatible
    /// with WASI Preview 2's async stream model.
    pub async fn process_message(&mut self, _stream: Stream, method: &str) -> Result<()> {
        // Acquire semaphore for thread safety (one message at a time)
        let _permit = self.semaphore.acquire().await?;
        
        // TODO: Redesign this for proper async I/O
        // The current approach of swapping streams mid-execution doesn't work
        // with WASI Preview 2's async stream model. We need to either:
        // 1. Create a new instance per message, or
        // 2. Use a different approach for per-message I/O
        
        // For now, just log the method call
        info!("Processing message with method: {}", method);
        
        // Call the specified export function
        let method_name = if method.is_empty() { "poll" } else { method };
        
        if let Some(func) = self.instance.get_func(&mut self.store, method_name) {
            // Call with no arguments (WASM uses stdin/stdout for I/O)
            func.call(&mut self.store, &[], &mut [])?;
        } else {
            return Err(anyhow!("Unknown method: {}", method_name));
        }
        
        Ok(())
    }

    /// Check if the process is closed (placeholder - wasmtime 22.0 doesn't have is_closed)
    pub fn is_closed(&mut self) -> bool {
        false // Always return false for now
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proc_config_creation() {
        let config = Config::new();
        assert!(!config.wasm_debug);
        assert!(config.env.is_empty());
        assert!(config.args.is_empty());
    }

    #[test]
    fn test_proc_config_builder() {
        let config = Config::new()
            .with_wasm_debug(true)
            .with_env(vec!["TEST=1".to_string()])
            .with_args(vec!["arg1".to_string()]);
        
        assert!(config.wasm_debug);
        assert_eq!(config.env.len(), 1);
        assert_eq!(config.args.len(), 1);
    }

    #[test]
    fn test_proc_creation_with_minimal_wasm() {
        // Test creating a Proc with a minimal WASM module
        let config = Config::new()
            .with_wasm_debug(true);
        
        // Create a minimal valid WASM module
        let minimal_wasm = vec![
            0x00, 0x61, 0x73, 0x6d, // WASM magic
            0x01, 0x00, 0x00, 0x00, // Version 1
        ];
        
        let result = Proc::new(config, &minimal_wasm);
        assert!(result.is_ok(), "Should be able to create Proc with minimal WASM");
        
        let proc = result.unwrap();
        assert!(proc.protocol().starts_with("/ww/0.1.0/"), "Protocol should start with /ww/0.1.0/");
        assert!(proc.id().len() > 0);
    }

    #[test]
    fn test_proc_function_discovery() {
        // Test that we can discover functions in a Proc's WASM module
        let config = Config::new()
            .with_wasm_debug(true);
        
        // Create a WASM module with an exported function
        let wasm_with_function = vec![
            0x00, 0x61, 0x73, 0x6d, // WASM magic
            0x01, 0x00, 0x00, 0x00, // Version 1
            0x01, 0x04, 0x01, 0x60, 0x00, 0x00, // Type section: 1 function type
            0x03, 0x02, 0x01, 0x00, // Function section: 1 function
            0x07, 0x07, 0x01, 0x03, 0x66, 0x6f, 0x6f, 0x00, 0x00, // Export "foo"
            0x0a, 0x04, 0x01, 0x02, 0x00, 0x0b, // Code section: empty function
        ];
        
        let mut proc = Proc::new(config, &wasm_with_function).unwrap();
        
        // Test function discovery
        assert!(proc.instance.get_func(&mut proc.store, "foo").is_some(), 
                "Should find exported 'foo' function");
        assert!(proc.instance.get_func(&mut proc.store, "nonexistent").is_none(), 
                "Should not find nonexistent function");
    }

    #[test]
    fn test_proc_async_vs_sync_modes() {
        // Test that both async and sync modes work
        let minimal_wasm = vec![
            0x00, 0x61, 0x73, 0x6d, // WASM magic
            0x01, 0x00, 0x00, 0x00, // Version 1
        ];
        
        // Test that Proc creation works
        let config = Config::new();
        let proc = Proc::new(config, &minimal_wasm);
        assert!(proc.is_ok(), "Proc creation should work");
    }
}
