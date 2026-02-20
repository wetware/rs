use anyhow::{Context, Result};
use futures::FutureExt;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{stderr, stdin, stdout};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::info;

use crate::cell::{proc::DataStreamHandles, Loader, ProcBuilder};
use crate::host::SwarmCommand;
use crate::rpc::membrane::GuestMembrane;
use crate::rpc::NetworkState;

/// Builder for constructing a [`Cell`].
///
/// A `Cell` represents an isolated execution environment for a WASM guest.
/// The builder requires an *image path* — either a local filesystem directory
/// or an IPFS path — that follows FHS conventions:
///
/// ```text
/// <image>/
///   bin/
///     main.wasm      # guest entrypoint (required)
///   boot/            # bootstrap peer hints (optional)
///     <peerID>       # file per peer; contents = multiaddrs, one per line
///   svc/             # background services (optional)
///     <name>/        # nested image, spawned automatically at boot
///       bin/main.wasm
///   etc/             # reserved for configuration
///   usr/lib/         # reserved for shared libraries
/// ```
///
/// # Required fields
///
/// - **path** (set via [`CellBuilder::new`]) — the image root
/// - **loader** — resolves `<path>/bin/main.wasm` to bytes
/// - **network_state** — shared libp2p network snapshot for the Host RPC capability
/// - **swarm_cmd_tx** — channel for sending swarm commands (connect, etc.)
///
/// # Example
///
/// ```ignore
/// let cell = CellBuilder::new("images/kernel".into())
///     .with_loader(Box::new(HostPathLoader))
///     .with_network_state(network_state)
///     .with_swarm_cmd_tx(swarm_cmd_tx)
///     .build();
/// let exit_code = cell.spawn().await?;
/// ```
pub struct CellBuilder {
    loader: Option<Box<dyn Loader>>,
    path: String,
    args: Vec<String>,
    env: Vec<String>,
    wasm_debug: bool,
    wasmtime_engine: Option<Arc<wasmtime::Engine>>,
    network_state: Option<NetworkState>,
    swarm_cmd_tx: Option<mpsc::Sender<SwarmCommand>>,
    image_root: Option<PathBuf>,
}

impl CellBuilder {
    /// Create a new builder for the given image path.
    ///
    /// The path should point to an image directory (local or IPFS) that contains
    /// `bin/main.wasm` as its entrypoint.
    pub fn new(path: String) -> Self {
        Self {
            loader: None,
            path,
            args: Vec::new(),
            env: Vec::new(),
            wasm_debug: false,
            wasmtime_engine: None,
            network_state: None,
            swarm_cmd_tx: None,
            image_root: None,
        }
    }

    /// Set the loader used to resolve `<image>/bin/main.wasm` to bytes.
    pub fn with_loader(mut self, loader: Box<dyn Loader>) -> Self {
        self.loader = Some(loader);
        self
    }

    /// Set command line arguments passed to the guest.
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }

    /// Set environment variables passed to the guest.
    pub fn with_env(mut self, env: Vec<String>) -> Self {
        self.env = env;
        self
    }

    /// Enable or disable WASM debug info for the guest.
    pub fn with_wasm_debug(mut self, wasm_debug: bool) -> Self {
        self.wasm_debug = wasm_debug;
        self
    }

    /// Provide a shared Wasmtime engine for the host runtime.
    pub fn with_wasmtime_engine(mut self, engine: Arc<wasmtime::Engine>) -> Self {
        self.wasmtime_engine = Some(engine);
        self
    }

    /// Set the network state for the Host RPC capability.
    pub fn with_network_state(mut self, network_state: NetworkState) -> Self {
        self.network_state = Some(network_state);
        self
    }

    /// Set the swarm command sender for the Host RPC capability.
    pub fn with_swarm_cmd_tx(mut self, tx: mpsc::Sender<SwarmCommand>) -> Self {
        self.swarm_cmd_tx = Some(tx);
        self
    }

    /// Set the FHS image root for WASI preopen.
    ///
    /// When set, the merged image directory is mounted read-only at `/`
    /// in the guest's WASI filesystem, giving guests access to `boot/`,
    /// `svc/`, `etc/`, and other FHS paths.
    pub fn with_image_root(mut self, root: PathBuf) -> Self {
        self.image_root = Some(root);
        self
    }

    /// Build the Cell.
    ///
    /// # Panics
    ///
    /// Panics if `loader`, `network_state`, or `swarm_cmd_tx` have not been set.
    pub fn build(self) -> Cell {
        Cell {
            path: self.path,
            args: self.args,
            loader: self.loader.expect("loader must be set"),
            env: Some(self.env),
            wasm_debug: self.wasm_debug,
            wasmtime_engine: self.wasmtime_engine,
            network_state: self.network_state.expect("network_state must be set"),
            swarm_cmd_tx: self.swarm_cmd_tx.expect("swarm_cmd_tx must be set"),
            image_root: self.image_root,
        }
    }
}

/// An isolated execution environment for a WASM guest.
///
/// A `Cell` loads a guest binary from an image path, spawns it with
/// WASI stdio bound to the host's stdin/stdout/stderr, and serves the
/// Host RPC capability over in-memory data streams (Cap'n Proto over
/// duplex pipes — no TCP listener needed).
///
/// Use [`CellBuilder`] to construct a `Cell`.
pub struct Cell {
    pub path: String,
    pub args: Vec<String>,
    pub loader: Box<dyn Loader>,
    pub env: Option<Vec<String>>,
    pub wasm_debug: bool,
    pub wasmtime_engine: Option<Arc<wasmtime::Engine>>,
    pub network_state: NetworkState,
    pub swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    pub image_root: Option<PathBuf>,
}

impl Cell {
    /// Execute the cell command using wetware streams for RPC transport.
    ///
    /// Returns the guest's exit code and its exported [`GuestMembrane`] capability.
    /// The membrane is valid while the guest is running; callers that need to serve
    /// it to external peers (see issue #42) should act on it before awaiting the
    /// exit code.  If the guest called `wetware_guest::run()` rather than `serve()`,
    /// the returned membrane is broken and attempts to use it will fail gracefully.
    pub async fn spawn(self) -> Result<(i32, GuestMembrane)> {
        self.spawn_with_streams_rpc().await
    }

    /// Execute the cell command and return the join handle plus data stream handles.
    ///
    /// This enables bidirectional data streams so the host can speak Cap'n Proto
    /// RPC to the guest over in-memory duplex pipes, while the guest's WASI stdio
    /// is bound to the host process's stdin/stdout/stderr.
    pub async fn spawn_with_streams(self) -> Result<(JoinHandle<Result<()>>, DataStreamHandles)> {
        let Cell {
            path,
            args,
            loader,
            env,
            wasm_debug,
            wasmtime_engine,
            network_state: _,
            swarm_cmd_tx: _,
            image_root,
        } = self;

        crate::config::init_tracing();

        info!(binary = %path, "Starting cell execution");

        // FHS convention: <image>/bin/main.wasm
        let wasm_path = format!("{}/bin/main.wasm", path.trim_end_matches('/'));
        let bytecode = loader.load(&wasm_path).await.with_context(|| {
            format!("Failed to load bin/main.wasm from image: {path} (resolved to: {wasm_path})")
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

        let builder = builder
            .with_wasm_debug(wasm_debug)
            .with_env(env.unwrap_or_default())
            .with_args(args)
            .with_bytecode(bytecode)
            .with_loader(Some(loader))
            .with_stdio(stdin_handle, stdout_handle, stderr_handle)
            .with_image_root(image_root);
        let (builder, handles) = builder.with_data_streams();

        let proc = builder.build().await?;
        info!(binary = %path, "Built guest process");
        let join = tokio::spawn(async move { proc.run().await });

        Ok((join, handles))
    }

    /// Execute the cell command and serve Cap'n Proto RPC over wetware streams.
    pub async fn spawn_with_streams_rpc(self) -> Result<(i32, GuestMembrane)> {
        let wasm_debug = self.wasm_debug;
        let network_state = self.network_state.clone();
        let swarm_cmd_tx = self.swarm_cmd_tx.clone();
        let (join, handles) = self.spawn_with_streams().await?;
        let mut handles = handles;
        let (reader, writer) = handles
            .take_host_split()
            .ok_or_else(|| anyhow::anyhow!("host stream missing; RPC streams already consumed"))?;
        // Static epoch (never advances) — real epoch wiring is a future concern.
        let initial_epoch = membrane::Epoch {
            seq: 0,
            head: vec![],
            adopted_block: 0,
        };
        let (_epoch_tx, epoch_rx) = tokio::sync::watch::channel(initial_epoch);
        let (rpc_system, guest_membrane) = crate::rpc::membrane::build_membrane_rpc(
            reader,
            writer,
            network_state,
            swarm_cmd_tx,
            wasm_debug,
            epoch_rx,
        );

        info!("Starting streams RPC server for guest");
        let local = tokio::task::LocalSet::new();
        local.spawn_local(rpc_system.map(|_| ()));
        let exit_code = local
            .run_until(async move {
                let exit_code = match join.await {
                    Ok(Ok(())) => 0,
                    Ok(Err(_)) | Err(_) => 1,
                };
                info!(code = exit_code, "Guest exited (streams RPC)");
                Ok::<i32, anyhow::Error>(exit_code)
            })
            .await?;

        Ok((exit_code, guest_membrane))
    }

}
