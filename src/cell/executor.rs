use anyhow::{Context, Result};
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use futures::FutureExt;
use k256::ecdsa::SigningKey;
use libp2p::StreamProtocol;
use membrane::Epoch;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{stderr, stdout, AsyncWriteExt};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::info;

use crate::cell::{proc::DataStreamHandles, Loader, ProcBuilder};
use crate::host::SwarmCommand;
use crate::rpc::membrane::GuestMembrane;
use crate::rpc::NetworkState;

const CAPNP_PROTOCOL: StreamProtocol = StreamProtocol::new("/ww/0.1.0");

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
    initial_epoch: Option<Epoch>,
    ipfs_client: Option<crate::ipfs::HttpClient>,
    epoch_rx: Option<watch::Receiver<Epoch>>,
    signing_key: Option<Arc<SigningKey>>,
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
            initial_epoch: None,
            ipfs_client: None,
            epoch_rx: None,
            signing_key: None,
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

    /// Set the initial epoch from on-chain state.
    ///
    /// When set, this epoch seeds the watch channel instead of the default
    /// zero epoch. The epoch pipeline can later advance it via the returned
    /// `watch::Sender<Epoch>`.
    pub fn with_initial_epoch(mut self, epoch: Epoch) -> Self {
        self.initial_epoch = Some(epoch);
        self
    }

    /// Provide a pre-created epoch receiver.
    ///
    /// When set, `spawn_rpc_inner` uses this receiver instead of creating
    /// a new channel. The caller retains the corresponding `watch::Sender`
    /// and is responsible for advancing epochs (e.g. via the epoch pipeline).
    pub fn with_epoch_rx(mut self, rx: watch::Receiver<Epoch>) -> Self {
        self.epoch_rx = Some(rx);
        self
    }

    /// Set the IPFS HTTP client for the CoreAPI capability.
    pub fn with_ipfs_client(mut self, client: crate::ipfs::HttpClient) -> Self {
        self.ipfs_client = Some(client);
        self
    }

    /// Set the secp256k1 signing key for the node identity.
    ///
    /// When set:
    /// - The `VerifyingKey` is threaded to `MembraneServer` for challenge-response
    ///   authentication in `graft()` (issue #57).
    /// - An [`EpochGuardedIdentity`] hub backed by this key is injected into every
    ///   `Session` so the kernel can request domain-scoped signers without holding
    ///   the private key.
    pub fn with_signing_key(mut self, sk: Arc<SigningKey>) -> Self {
        self.signing_key = Some(sk);
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
            initial_epoch: self.initial_epoch,
            ipfs_client: self
                .ipfs_client
                .unwrap_or_else(|| crate::ipfs::HttpClient::new("http://localhost:5001".into())),
            epoch_rx: self.epoch_rx,
            signing_key: self.signing_key,
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
    pub initial_epoch: Option<Epoch>,
    pub ipfs_client: crate::ipfs::HttpClient,
    pub epoch_rx: Option<watch::Receiver<Epoch>>,
    pub signing_key: Option<Arc<SigningKey>>,
}

/// Result of spawning a cell with RPC: exit code, guest membrane, and optional epoch sender.
///
/// `epoch_tx` is `Some` when the cell created its own epoch channel (no external
/// receiver was provided). It is `None` when the caller supplied a pre-created
/// receiver via [`CellBuilder::with_epoch_rx`].
pub struct SpawnResult {
    pub exit_code: i32,
    pub guest_membrane: GuestMembrane,
    pub epoch_tx: Option<watch::Sender<Epoch>>,
}

impl Cell {
    /// Execute the cell command using wetware streams for RPC transport.
    ///
    /// Returns a [`SpawnResult`] containing the guest's exit code, its exported
    /// [`GuestMembrane`], and the epoch sender for advancing epochs.
    pub async fn spawn(self) -> Result<SpawnResult> {
        self.spawn_rpc_inner(None).await
    }

    /// Like [`spawn`], but also accepts incoming libp2p streams for
    /// `/wetware/capnp/1.0.0` and bootstraps each one with the guest's
    /// exported [`GuestMembrane`].
    ///
    /// This is the production entry point: Ganglion (and other peers) connect
    /// to the host's libp2p swarm and open a stream on this protocol to obtain
    /// the kernel's capability surface.
    pub async fn spawn_serving(self, control: libp2p_stream::Control) -> Result<SpawnResult> {
        self.spawn_rpc_inner(Some(control)).await
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
            initial_epoch: _,
            ipfs_client: _,
            epoch_rx: _,
            signing_key: _,
        } = self;

        crate::config::init_tracing();

        info!(binary = %path, "Starting cell execution");

        // FHS convention: <image>/bin/main.wasm
        let wasm_path = format!("{}/bin/main.wasm", path.trim_end_matches('/'));
        let bytecode = loader.load(&wasm_path).await.with_context(|| {
            format!("Failed to load bin/main.wasm from image: {path} (resolved to: {wasm_path})")
        })?;
        tracing::debug!(binary = %path, "Loaded guest bytecode");

        let interactive = std::io::stdin().is_terminal() || std::env::var("WW_TTY").is_ok();

        // Bridge host stdin → guest regardless of interactive mode.
        //
        // tokio::io::stdin() is unsuitable here: tokio sets O_NONBLOCK on the fd,
        // and macOS tty reads in non-blocking mode can return 0 bytes unexpectedly,
        // which wasmtime-wasi treats as EOF (causing the kernel to exit instantly).
        //
        // Fix: a plain OS thread (no tokio context) blocks on std::io::stdin() in
        // cooked mode and forwards bytes via mpsc. A tokio task drains the channel
        // into the duplex writer. Both shell and daemon modes need a live stdin pipe
        // so the guest can block on it until the host signals shutdown (closes stdin).
        let stdin_handle: Box<dyn tokio::io::AsyncRead + Send + Sync + Unpin> = {
            let (reader, mut writer) = tokio::io::duplex(4096);
            let (tx, mut rx) = mpsc::channel::<Vec<u8>>(4);

            std::thread::spawn(move || {
                use std::io::Read;
                let stdin = std::io::stdin();
                let mut handle = stdin.lock();
                let mut buf = [0u8; 4096];
                loop {
                    match handle.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if tx.blocking_send(buf[..n].to_vec()).is_err() {
                                break;
                            }
                        }
                    }
                }
            });

            tokio::spawn(async move {
                while let Some(data) = rx.recv().await {
                    if writer.write_all(&data).await.is_err() {
                        break;
                    }
                }
            });

            Box::new(reader)
        };
        let stdout_handle = stdout();
        let stderr_handle = stderr();

        let builder = if let Some(engine) = wasmtime_engine {
            ProcBuilder::new().with_engine(engine)
        } else {
            ProcBuilder::new()
        };

        // Inject host-side environment signals for the guest.
        let mut guest_env = env.unwrap_or_default();
        if interactive {
            guest_env.push("WW_TTY=1".to_string());
        }
        if !guest_env.iter().any(|v| v.starts_with("PATH=")) {
            guest_env.push("PATH=/bin".to_string());
        }

        let builder = builder
            .with_wasm_debug(wasm_debug)
            .with_env(guest_env)
            .with_args(args)
            .with_bytecode(bytecode)
            .with_loader(Some(loader))
            .with_stdio(stdin_handle, stdout_handle, stderr_handle)
            .with_image_root(image_root);
        let (builder, handles) = builder.with_data_streams();

        let proc = builder.build().await?;
        tracing::debug!(binary = %path, "Guest process ready");
        let join = tokio::spawn(async move { proc.run().await });

        Ok((join, handles))
    }

    /// Execute the cell command and serve Cap'n Proto RPC over wetware streams.
    pub async fn spawn_with_streams_rpc(self) -> Result<SpawnResult> {
        self.spawn_rpc_inner(None).await
    }

    async fn spawn_rpc_inner(
        mut self,
        stream_control: Option<libp2p_stream::Control>,
    ) -> Result<SpawnResult> {
        let wasm_debug = self.wasm_debug;
        let network_state = self.network_state.clone();
        let swarm_cmd_tx = self.swarm_cmd_tx.clone();
        let ipfs_client = self.ipfs_client.clone();
        let signing_key = self.signing_key.take();
        let pre_epoch_rx = self.epoch_rx.take();
        let initial_epoch = self.initial_epoch.clone().unwrap_or(Epoch {
            seq: 0,
            head: vec![],
            adopted_block: 0,
        });
        let (join, handles) = self.spawn_with_streams().await?;
        let mut handles = handles;
        let (reader, writer) = handles
            .take_host_split()
            .ok_or_else(|| anyhow::anyhow!("host stream missing; RPC streams already consumed"))?;

        // Use the externally-provided epoch receiver if available,
        // otherwise create a new channel.
        let (epoch_tx, epoch_rx) = if let Some(rx) = pre_epoch_rx {
            (None, rx)
        } else {
            let (tx, rx) = watch::channel(initial_epoch);
            (Some(tx), rx)
        };

        // Clone the stream control for the membrane RPC layer (Server capability).
        // If no stream_control is provided (non-serving mode), create a dummy one.
        let membrane_stream_control = stream_control.clone().unwrap_or_else(|| {
            // Non-serving mode: Server.serve() will fail at accept() time,
            // which is acceptable — guests that don't have a real swarm
            // shouldn't be registering subprotocol handlers.
            libp2p_stream::Behaviour::new().new_control()
        });

        let (rpc_system, guest_membrane) = crate::rpc::membrane::build_membrane_rpc(
            reader,
            writer,
            network_state,
            swarm_cmd_tx,
            wasm_debug,
            epoch_rx,
            ipfs_client,
            signing_key,
            membrane_stream_control,
        );

        tracing::debug!("Starting streams RPC server for guest");
        let local = tokio::task::LocalSet::new();
        local.spawn_local(rpc_system.map(|_| ()));

        if let Some(control) = stream_control {
            let membrane = guest_membrane.clone();
            local.spawn_local(accept_capnp_streams(control, membrane));
        }

        let exit_code = local
            .run_until(async move {
                let exit_code = match join.await {
                    Ok(Ok(())) => 0,
                    Ok(Err(_)) | Err(_) => 1,
                };
                tracing::debug!(code = exit_code, "Guest exited (streams RPC)");
                Ok::<i32, anyhow::Error>(exit_code)
            })
            .await?;

        Ok(SpawnResult {
            exit_code,
            guest_membrane,
            epoch_tx,
        })
    }
}

/// Accept incoming libp2p streams for the capnp protocol and serve each with
/// the guest's exported membrane.  Runs inside the cell's `LocalSet` so that
/// `spawn_local` is available for per-connection tasks.
async fn accept_capnp_streams(mut control: libp2p_stream::Control, membrane: GuestMembrane) {
    let mut incoming = match control.accept(CAPNP_PROTOCOL) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("failed to register capnp stream handler: {}", e);
            return;
        }
    };
    tracing::info!(protocol = %CAPNP_PROTOCOL, "Accepting capnp streams");
    use futures::StreamExt;
    while let Some((_peer_id, stream)) = incoming.next().await {
        let m = membrane.clone();
        tokio::task::spawn_local(serve_one_capnp_stream(stream, m));
    }
}

/// Serve a single libp2p stream as a Cap'n Proto RPC connection, bootstrapping
/// the remote peer with the guest's exported membrane.
async fn serve_one_capnp_stream(stream: libp2p::Stream, membrane: GuestMembrane) {
    // Box::pin(stream) → Pin<Box<Stream>>: AsyncRead + AsyncWrite + Unpin,
    // which allows .split() even though Stream itself is !Unpin.
    use futures::AsyncReadExt;
    let (reader, writer) = Box::pin(stream).split();
    let network = VatNetwork::new(reader, writer, Side::Server, Default::default());
    let rpc_system = RpcSystem::new(Box::new(network), Some(membrane.client));
    let _ = rpc_system.await;
}
