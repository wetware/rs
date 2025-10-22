use anyhow::Result;
use std::io::Write;
use wasmtime::component::{Component, Instance, Linker, ResourceTable};
use wasmtime::{Config as WasmConfig, Engine, Store};
use wasmtime_wasi::cli::{AsyncStdinStream, AsyncStdoutStream};
use wasmtime_wasi::p2::add_to_linker_async;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

pub const BUFFER_SIZE: usize = 1024;

// Required for WASI IO to work.
pub struct ComponentRunStates {
    pub wasi_ctx: WasiCtx,
    pub resource_table: ResourceTable,
}

// Required for WASI IO to work.
impl WasiView for ComponentRunStates {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.resource_table,
        }
    }
}

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

/// Cell process that encapsulates a WASM instance and its configuration.
///
/// Designed for per-stream instantiation - each incoming stream gets its own Proc instance.
/// This enables concurrent execution of multiple services.
pub struct Proc {
    /// Process configuration
    pub config: Config,
    /// Service metadata
    pub service_info: ServiceInfo,
    // /// Compiled cell module
    // pub module: Module,
    /// Cell runtime instance
    pub instance: Instance,
    /// Cell runtime store
    pub store: Store<ComponentRunStates>,
    /// Host-side stdin handle for communication
    pub host_stdin: tokio::io::DuplexStream,
    /// Host-side stdout handle for communication
    pub host_stdout: tokio::io::DuplexStream,
}

/// Service metadata for tracking per-stream instances
#[derive(Debug, Clone)]
pub struct ServiceInfo {
    /// Service path (e.g., "/ww/0.1.0/echo")
    pub service_path: String,
    /// Version (e.g., "0.1.0")
    pub version: String,
    /// Service name (e.g., "echo")
    pub service_name: String,
    /// Protocol used for this service
    pub protocol: String,
}

impl Proc {
    /// Create a new WASM process with duplex pipes for bidirectional communication
    ///
    /// Returns the Proc instance with service metadata and host-side handles.
    /// The WASM instance gets the other end of the pipes.
    pub async fn new_with_duplex_pipes(
        config: Config,
        bytecode: &[u8],
        service_info: ServiceInfo,
    ) -> Result<Self> {
        // Create duplex pipes for bidirectional communication
        let (host_stdin, wasm_stdin) = tokio::io::duplex(BUFFER_SIZE);
        let (wasm_stdout, host_stdout) = tokio::io::duplex(BUFFER_SIZE);

        let wasm_stdin_async = AsyncStdinStream::new(wasm_stdin);
        let wasm_stdout_async = AsyncStdoutStream::new(BUFFER_SIZE, wasm_stdout);

        let mut wasm_config = WasmConfig::new();
        wasm_config.async_support(true);
        let engine = Engine::new(&wasm_config)?;
        let mut linker = Linker::new(&engine);
        add_to_linker_async(&mut linker)?;

        // Prepare environment variables as key-value pairs
        let envs: Vec<(&str, &str)> = config
            .env
            .iter()
            .filter_map(|var| var.split_once('='))
            .collect();

        // Wire the async stdio streams into WASI and inherit host args.
        let wasi = WasiCtx::builder()
            .stdin(wasm_stdin_async)
            .stdout(wasm_stdout_async)
            .envs(&envs)
            .args(&config.args)
            .build();

        let state = ComponentRunStates {
            wasi_ctx: wasi,
            resource_table: ResourceTable::new(),
        };
        let mut store = Store::new(&engine, state);

        // Instantiate it as a normal component
        let component = Component::from_binary(&engine, bytecode)?;
        let instance = linker.instantiate_async(&mut store, &component).await?;

        Ok(Self {
            config,
            service_info,
            // module,
            instance,
            store,
            host_stdin,
            host_stdout,
        })
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
    fn test_service_info_creation() {
        let service_info = ServiceInfo {
            service_path: "/ww/0.1.0/echo".to_string(),
            version: "0.1.0".to_string(),
            service_name: "echo".to_string(),
            protocol: "/ww/0.1.0/".to_string(),
        };

        assert_eq!(service_info.service_path, "/ww/0.1.0/echo");
        assert_eq!(service_info.version, "0.1.0");
        assert_eq!(service_info.service_name, "echo");
        assert_eq!(service_info.protocol, "/ww/0.1.0/");
    }
}
