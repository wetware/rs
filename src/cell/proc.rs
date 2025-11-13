use anyhow::Result;
use wasmtime::component::{Component, Instance, Linker, ResourceTable};
use wasmtime::{Config as WasmConfig, Engine, Store};
use wasmtime_wasi::cli::{AsyncStdinStream, AsyncStdoutStream};
use wasmtime_wasi::p2::add_to_linker_async;
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

#[cfg(not(target_arch = "wasm32"))]
use capnp::capability::Client;
#[cfg(not(target_arch = "wasm32"))]
use capnp_rpc::{rpc_twoparty_capnp, twoparty, RpcSystem};

use super::Loader;

pub const BUFFER_SIZE: usize = 1024;

// Required for WASI IO to work.
pub struct ComponentRunStates {
    pub wasi_ctx: WasiCtx,
    pub resource_table: ResourceTable,
    pub loader: Option<Box<dyn Loader>>,
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

/// Builder for constructing a Proc configuration
pub struct Builder {
    env: Vec<String>,
    args: Vec<String>,
    wasm_debug: bool,
}

impl Builder {
    /// Create a new Proc builder
    pub fn new() -> Self {
        Self {
            env: Vec::new(),
            args: Vec::new(),
            wasm_debug: false,
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

    /// Build the Proc configuration
    pub fn build(self) -> ProcConfig {
        ProcConfig {
            env: self.env,
            args: self.args,
            wasm_debug: self.wasm_debug,
        }
    }
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for cell process creation
pub struct ProcConfig {
    /// Environment variables for the cell process
    pub env: Vec<String>,
    /// Command line arguments for the cell process
    pub args: Vec<String>,
    /// Whether to enable cell debug info
    pub wasm_debug: bool,
}

/// Cell process that encapsulates a WASM instance and its configuration.
///
/// Designed for per-stream instantiation - each incoming stream gets its own Proc instance.
/// This enables concurrent execution of multiple services.
pub struct Proc {
    /// Service metadata
    #[allow(dead_code)]
    pub service_info: ServiceInfo,
    // /// Compiled cell module
    // pub module: Module,
    /// Cell runtime instance
    #[allow(dead_code)]
    pub instance: Instance,
    /// Cell runtime store
    #[allow(dead_code)]
    pub store: Store<ComponentRunStates>,
    /// Host-side stdin handle for communication
    #[allow(dead_code)]
    pub host_stdin: tokio::io::DuplexStream,
    /// Host-side stdout handle for communication
    #[allow(dead_code)]
    pub host_stdout: tokio::io::DuplexStream,
}

/// Service metadata for tracking per-stream instances
#[derive(Debug, Clone)]
pub struct ServiceInfo {
    /// Service path (e.g., "/ww/0.1.0/echo")
    #[allow(dead_code)]
    pub service_path: String,
    /// Version (e.g., "0.1.0")
    #[allow(dead_code)]
    pub version: String,
    /// Service name (e.g., "echo")
    #[allow(dead_code)]
    pub service_name: String,
    /// Protocol used for this service
    #[allow(dead_code)]
    pub protocol: String,
}

impl Proc {
    /// Create a new WASM process with duplex pipes for bidirectional communication
    ///
    /// Returns the Proc instance with service metadata and host-side handles.
    /// The WASM instance gets the other end of the pipes.
    pub async fn new_with_duplex_pipes(
        config: ProcConfig,
        bytecode: &[u8],
        service_info: ServiceInfo,
        loader: Option<Box<dyn Loader>>,
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
        
        // Add loader host function if loader is provided
        if loader.is_some() {
            add_loader_to_linker(&mut linker)?;
        }

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
            loader,
        };
        let mut store = Store::new(&engine, state);

        // Instantiate it as a normal component
        let component = Component::from_binary(&engine, bytecode)?;
        let instance = linker.instantiate_async(&mut store, &component).await?;

        Ok(Self {
            service_info,
            // module,
            instance,
            store,
            host_stdin,
            host_stdout,
        })
    }

    /// Create a host-side RPC system using the duplex streams
    ///
    /// This sets up a Cap'n Proto RPC system on the host side that communicates
    /// with the guest over the duplex streams. The bootstrap capability is provided
    /// to the guest when it calls `bootstrap()`.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn create_host_rpc_system(
        host_stdin: tokio::io::DuplexStream,
        host_stdout: tokio::io::DuplexStream,
        bootstrap: impl Into<Client>,
    ) -> RpcSystem<rpc_twoparty_capnp::Side> {
        use tokio_util::compat::TokioAsyncReadCompatExt;
        use tokio_util::compat::TokioAsyncWriteCompatExt;

        // Convert tokio streams to futures-compatible streams
        let read = host_stdin.compat();
        let write = host_stdout.compat_write();

        let network = twoparty::VatNetwork::new(
            read,
            write,
            rpc_twoparty_capnp::Side::Server,
            Default::default(),
        );

        RpcSystem::new(Box::new(network), Some(bootstrap.into()))
    }
}

/// Add the loader host function to the Wasmtime linker
///
/// This exports a host function that allows WASM guests to call back into
/// the host to load bytecode from various sources (IPFS, filesystem, etc.).
///
/// Note: This requires a WIT interface definition. For now, this is a
/// placeholder that can be implemented once the WIT interface is defined.
fn add_loader_to_linker<T>(_linker: &mut Linker<T>) -> Result<()> {
    // TODO: Implement using WIT interface
    // The WIT interface would look something like:
    // 
    // package wetware:loader;
    // 
    // interface loader {
    //   load: func(path: string) -> result<list<u8>, string>;
    // }
    //
    // world wetware {
    //   import loader: self.loader;
    // }
    //
    // Then we'd use wit-bindgen to generate bindings and implement:
    // linker.root().func_wrap_async("wetware:loader/loader", "load", |mut store, (path,): (String,)| async move {
    //     let state = store.data_mut();
    //     if let Some(ref loader) = state.loader {
    //         match loader.load(&path).await {
    //             Ok(data) => Ok((data,)),
    //             Err(e) => Err(e.to_string()),
    //         }
    //     } else {
    //         Err("Loader not available".to_string())
    //     }
    // })?;
    
    // For now, this is a no-op placeholder
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proc_builder_creation() {
        let config = Builder::new().build();
        assert!(!config.wasm_debug);
        assert!(config.env.is_empty());
        assert!(config.args.is_empty());
    }

    #[test]
    fn test_proc_builder() {
        let config = Builder::new()
            .with_wasm_debug(true)
            .with_env(vec!["TEST=1".to_string()])
            .with_args(vec!["arg1".to_string()])
            .build();

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
