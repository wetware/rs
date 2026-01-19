use anyhow::{Context, Result};
use futures::FutureExt;
use std::sync::Arc;
use tokio::io::{stderr, stdin, stdout};
use tokio::task::JoinHandle;
use tracing::info;

use crate::cell::{proc::DataStreamHandles, streams, Loader, ProcBuilder};

/// Builder for constructing a cell Command
pub struct CommandBuilder {
    loader: Option<Box<dyn Loader>>,
    path: String,
    args: Vec<String>,
    env: Vec<String>,
    wasm_debug: bool,
    ipfs: Option<crate::ipfs::HttpClient>,
    port: Option<u16>,
    wasmtime_engine: Option<Arc<wasmtime::Engine>>,
}

impl CommandBuilder {
    /// Create a new Builder with a path
    pub fn new(path: String) -> Self {
        Self {
            loader: None,
            path,
            args: Vec::new(),
            env: Vec::new(),
            wasm_debug: false,
            ipfs: None,
            port: None,
            wasmtime_engine: None,
        }
    }

    /// Set the loader
    pub fn with_loader(mut self, loader: Box<dyn Loader>) -> Self {
        self.loader = Some(loader);
        self
    }

    /// Set command line arguments
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// Set keyword arguments (environment variables)
    pub fn with_env(mut self, env: Vec<String>) -> Self {
        self.env = env;
        self
    }

    /// Set WASM debug mode
    pub fn with_wasm_debug(mut self, wasm_debug: bool) -> Self {
        self.wasm_debug = wasm_debug;
        self
    }

    /// Set the IPFS client
    pub fn with_ipfs(mut self, ipfs: crate::ipfs::HttpClient) -> Self {
        self.ipfs = Some(ipfs);
        self
    }

    /// Set the port
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// Provide a shared Wasmtime engine for the host runtime.
    pub fn with_wasmtime_engine(mut self, engine: Arc<wasmtime::Engine>) -> Self {
        self.wasmtime_engine = Some(engine);
        self
    }

    /// Build the Command
    pub fn build(self) -> Command {
        Command {
            path: self.path,
            args: self.args,
            loader: self.loader.expect("loader must be set"),
            ipfs: self.ipfs.expect("ipfs must be set"),
            env: Some(self.env),
            wasm_debug: self.wasm_debug,
            port: self.port.unwrap_or(2020),
            wasmtime_engine: self.wasmtime_engine,
        }
    }
}

/// Configuration for running a cell
pub struct Command {
    pub path: String,
    pub args: Vec<String>,
    pub loader: Box<dyn Loader>,
    pub ipfs: crate::ipfs::HttpClient,
    pub env: Option<Vec<String>>,
    pub wasm_debug: bool,
    pub port: u16,
    pub wasmtime_engine: Option<Arc<wasmtime::Engine>>,
}

impl Command {
    /// Execute the cell command
    pub async fn spawn(self) -> Result<i32> {
        self.spawn_with_rpc().await
    }

    /// Execute the cell command and return the join handle plus data stream handles.
    ///
    /// This enables bidirectional streams by default so the host can speak async
    /// protocols (Capnp/Protobuf/etc.) while the guest boots.
    pub async fn spawn_with_streams(self) -> Result<(JoinHandle<Result<()>>, DataStreamHandles)> {
        let Command {
            path,
            args,
            loader,
            ipfs: _,
            env,
            wasm_debug,
            port: _,
            wasmtime_engine,
        } = self;

        crate::config::init_tracing();

        info!(binary = %path, "Starting cell execution");

        // Construct the path to main.wasm: <path>/main.wasm
        let wasm_path = format!("{}/main.wasm", path.trim_end_matches('/'));
        let bytecode = loader.load(&wasm_path).await.with_context(|| {
            format!("Failed to load main.wasm from path: {path} (resolved to: {wasm_path})")
        })?;
        info!(binary = %path, "Loaded guest bytecode");

        let stdin_handle = stdin();
        let stdout_handle = stdout();
        let stderr_handle = stderr();

        let builder = if let Some(engine) = wasmtime_engine {
            ProcBuilder::new().with_engine(engine)
        } else {
            ProcBuilder::new()
        };

        let (builder, handles) = builder
            .with_wasm_debug(wasm_debug)
            .with_env(env.unwrap_or_default())
            .with_args(args)
            .with_bytecode(bytecode)
            .with_loader(Some(loader))
            .with_stdio(stdin_handle, stdout_handle, stderr_handle)
            .with_data_streams();

        let proc = builder.build().await?;
        info!(binary = %path, "Built guest process");
        let join = tokio::spawn(async move { proc.run().await });

        Ok((join, handles))
    }

    /// Execute the cell command and serve Cap'n Proto RPC over wetware streams.
    pub async fn spawn_with_streams_rpc(self) -> Result<i32> {
        let wasm_debug = self.wasm_debug;
        let (join, handles) = self.spawn_with_streams().await?;
        let mut handles = handles;
        let guest_output_rx = handles.take_guest_output_receiver().ok_or_else(|| {
            anyhow::anyhow!("guest output receiver missing; RPC streams already consumed")
        })?;
        let reader = streams::Reader::new(guest_output_rx);
        let writer = streams::Writer::new(handles.host_to_guest_tx);
        let rpc_system = crate::rpc::build_peer_rpc(reader, writer, wasm_debug);

        info!("Starting streams RPC server for guest");
        let local = tokio::task::LocalSet::new();
        local.spawn_local(rpc_system.map(|_| ()));
        local
            .run_until(async move {
                let exit_code = match join.await {
                    Ok(Ok(())) => 0,
                    Ok(Err(_)) | Err(_) => 1,
                };
                info!(code = exit_code, "Guest exited (streams RPC)");
                Ok::<i32, anyhow::Error>(exit_code)
            })
            .await
    }

    async fn spawn_with_rpc(self) -> Result<i32> {
        let Command {
            path,
            args,
            loader,
            ipfs: _,
            env,
            wasm_debug,
            port: _,
            wasmtime_engine,
        } = self;

        crate::config::init_tracing();

        info!(binary = %path, "Starting cell execution with RPC over stdio");

        let wasm_path = format!("{}/main.wasm", path.trim_end_matches('/'));
        let bytecode = loader.load(&wasm_path).await.with_context(|| {
            format!("Failed to load main.wasm from path: {path} (resolved to: {wasm_path})")
        })?;

        let (host_in, guest_in) = tokio::io::duplex(64 * 1024);
        let (host_out, guest_out) = tokio::io::duplex(64 * 1024);

        let rpc_system = crate::rpc::build_peer_rpc(host_out, host_in, wasm_debug);

        let stderr_handle = stderr();
        let builder = if let Some(engine) = wasmtime_engine {
            ProcBuilder::new().with_engine(engine)
        } else {
            ProcBuilder::new()
        };
        let builder = builder
            .with_wasm_debug(wasm_debug)
            .with_env(env.unwrap_or_default())
            .with_args(args)
            .with_bytecode(bytecode)
            .with_loader(Some(loader))
            .with_stdio(guest_in, guest_out, stderr_handle);

        let proc = builder.build().await?;
        let join = tokio::spawn(async move { proc.run().await });

        let local = tokio::task::LocalSet::new();
        local.spawn_local(rpc_system.map(|_| ()));
        local
            .run_until(async move {
                join.await
                    .context("cell task panicked")?
                    .context("cell execution failed")?;
                Ok::<(), anyhow::Error>(())
            })
            .await?;
        info!(binary = %path, "Guest exited");
        Ok(0)
    }
}
