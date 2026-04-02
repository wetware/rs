//! Cap'n Proto RPC for host-provided capabilities.
//!
//! The Host capability is served to each WASM guest over in-memory duplex
//! streams (no TCP listener). See [`build_peer_rpc`] for the entry point.
#![cfg(not(target_arch = "wasm32"))]

pub mod http_client;
pub mod http_listener;
pub mod membrane;
pub mod routing;
pub mod stream_dialer;
pub mod stream_listener;
pub mod vat_client;
pub mod vat_listener;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use capnp::capability::Promise;
use capnp_rpc::pry;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use futures::FutureExt;
use tokio::io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use ::membrane::EpochGuard;

use libp2p::StreamProtocol;

use crate::cell::proc::Builder as ProcBuilder;
use crate::host::SwarmCommand;
use crate::system_capnp;

/// Derive a content-addressed protocol ID from canonical schema bytes.
///
/// The input is the canonical Cap'n Proto encoding of a `schema.Node`, which
/// includes the 64-bit unique type ID. Two interfaces with identical structure
/// but different type IDs produce different CIDs.
///
/// Returns the CID as a string: `CIDv1(raw, BLAKE3(schema_bytes))`.
pub(crate) fn schema_cid(schema_bytes: &[u8]) -> String {
    let digest = blake3::hash(schema_bytes);
    let mh = cid::multihash::Multihash::<64>::wrap(0x1e, digest.as_bytes())
        .expect("blake3 digest always fits in 64-byte multihash");
    cid::Cid::new_v1(0x55, mh).to_string()
}

/// Build a `StreamProtocol` from a schema CID string.
pub(crate) fn schema_protocol(cid: &str) -> Result<StreamProtocol, capnp::Error> {
    StreamProtocol::try_from_owned(format!("/ww/0.1.0/rpc/{cid}"))
        .map_err(|e| capnp::Error::failed(format!("invalid protocol from schema CID: {e}")))
}

/// Extract a custom section from a WASM binary (component or module).
///
/// Returns the section data if found, or `None` if the section doesn't exist.
/// Used by tooling (schema-inject, `ww inspect`). Listeners use explicit params.
#[allow(dead_code)]
pub(crate) fn extract_wasm_custom_section<'a>(
    wasm_bytes: &'a [u8],
    section_name: &str,
) -> Result<Option<&'a [u8]>, capnp::Error> {
    use wasmparser::{Parser, Payload};

    for payload in Parser::new(0).parse_all(wasm_bytes) {
        let payload = payload
            .map_err(|e| capnp::Error::failed(format!("failed to parse WASM binary: {e}")))?;
        match payload {
            Payload::CustomSection(reader) if reader.name() == section_name => {
                return Ok(Some(reader.data()));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// Decoded Cell type from a WASM custom section.
///
/// All variants are used at decode time; `Http` host-side handling
/// is not yet implemented (see TODOS.md: FastCGI / HttpListener).
#[derive(Debug)]
#[allow(dead_code)]
pub(crate) enum CellType {
    /// Raw libp2p stream with protocol ID.
    Raw(String),
    /// HTTP/WAGI cell with path prefix.
    Http(String),
    /// Cap'n Proto RPC cell with canonical schema bytes.
    Capnp(Vec<u8>),
}

/// Decode the Cell type tag from a WASM binary's "cell.capnp" custom section.
///
/// Returns `Ok(Some(CellType))` if the section is present and valid,
/// `Ok(None)` if the section is absent (pid0 mode), or `Err` if the
/// section data is malformed.
/// Used by tooling. Listeners use explicit params; custom sections are optional hints.
#[allow(dead_code)]
pub(crate) fn decode_cell_section(wasm_bytes: &[u8]) -> Result<Option<CellType>, capnp::Error> {
    let section_data = match extract_wasm_custom_section(wasm_bytes, "cell.capnp")? {
        Some(data) if !data.is_empty() => data,
        Some(_) => {
            return Err(capnp::Error::failed(
                "cell.capnp custom section is empty".into(),
            ));
        }
        None => return Ok(None),
    };

    // Copy section data to ensure 8-byte alignment (WASM custom sections
    // are not guaranteed to be aligned within the binary).
    let aligned_data = section_data.to_vec();
    let message = capnp::serialize::read_message_from_flat_slice(
        &mut aligned_data.as_slice(),
        capnp::message::ReaderOptions::default(),
    )
    .map_err(|e| capnp::Error::failed(format!("failed to decode cell.capnp section: {e}")))?;

    let cell: crate::cell_capnp::cell::Reader = message
        .get_root()
        .map_err(|e| capnp::Error::failed(format!("failed to read Cell root: {e}")))?;

    use crate::cell_capnp::cell::Which;
    match cell.which() {
        Ok(Which::Raw(text)) => {
            let protocol_id = text?.to_string()?;
            Ok(Some(CellType::Raw(protocol_id)))
        }
        Ok(Which::Http(text)) => {
            let path_prefix = text?.to_string()?;
            Ok(Some(CellType::Http(path_prefix)))
        }
        Ok(Which::Capnp(node)) => {
            let node = node?;
            // Re-canonicalize the schema node to get raw segment bytes for CID derivation.
            // Uses get_segments_for_output (no framing) to match the build-time path
            // in schema_id::canonicalize_node, ensuring CID stability.
            let mut canonical_msg = capnp::message::Builder::new_default();
            canonical_msg
                .set_root_canonical(node)
                .map_err(|e| capnp::Error::failed(format!("failed to canonicalize schema: {e}")))?;
            let segments = canonical_msg.get_segments_for_output();
            if segments.len() != 1 {
                return Err(capnp::Error::failed(format!(
                    "canonical message produced {} segments, expected 1",
                    segments.len()
                )));
            }
            Ok(Some(CellType::Capnp(segments[0].to_vec())))
        }
        Err(capnp::NotInSchema(n)) => Err(capnp::Error::failed(format!(
            "unknown Cell variant discriminant: {n}"
        ))),
    }
}

/// Maximum bytes a single ByteStream read may allocate.
///
/// Guards against OOM from callers requesting u32::MAX bytes.
/// 64 KiB matches the RPC pipe buffer and the listener pump size.
const MAX_READ_BYTES: usize = 64 * 1024;

/// Maximum WASM binary size accepted by the Executor.
///
/// Rejects oversized binaries before compilation to bound memory and
/// CPU spent on untrusted guest code.  2 MiB is generous for current
/// guests (chess ≈ 1.1 MiB) while preventing abuse.
const MAX_WASM_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub peer_id: Vec<u8>,
    pub addrs: Vec<Vec<u8>>,
}

#[derive(Clone, Debug)]
pub struct NetworkSnapshot {
    pub local_peer_id: Vec<u8>,
    pub listen_addrs: Vec<Vec<u8>>,
    pub known_peers: Vec<PeerInfo>,
}

#[derive(Clone, Debug)]
pub struct NetworkState {
    inner: Arc<RwLock<NetworkSnapshot>>,
}

impl Default for NetworkState {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkState {
    pub fn new() -> Self {
        use libp2p::identity::Keypair;
        use libp2p::PeerId;

        let keypair = Keypair::generate_ed25519();
        let peer_id = PeerId::from_public_key(&keypair.public());
        Self::from_peer_id(peer_id.to_bytes())
    }

    pub fn from_peer_id(peer_id: Vec<u8>) -> Self {
        let snapshot = NetworkSnapshot {
            local_peer_id: peer_id,
            listen_addrs: Vec::new(),
            known_peers: Vec::new(),
        };
        Self {
            inner: Arc::new(RwLock::new(snapshot)),
        }
    }

    pub async fn snapshot(&self) -> NetworkSnapshot {
        self.inner.read().await.clone()
    }

    pub async fn set_local_peer_id(&self, peer_id: Vec<u8>) {
        let mut guard = self.inner.write().await;
        guard.local_peer_id = peer_id;
    }

    pub async fn add_listen_addr(&self, addr: Vec<u8>) {
        let mut guard = self.inner.write().await;
        if !guard.listen_addrs.contains(&addr) {
            guard.listen_addrs.push(addr);
        }
    }

    pub async fn remove_listen_addr(&self, addr: &[u8]) {
        let mut guard = self.inner.write().await;
        guard.listen_addrs.retain(|a| a != addr);
    }

    pub async fn set_known_peers(&self, peers: Vec<PeerInfo>) {
        let mut guard = self.inner.write().await;
        guard.known_peers = peers;
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum StreamMode {
    ReadOnly,
    WriteOnly,
    Bidirectional,
}

pub struct ByteStreamImpl {
    stream: Arc<Mutex<io::DuplexStream>>,
    mode: StreamMode,
}

impl ByteStreamImpl {
    pub(crate) fn new(stream: io::DuplexStream, mode: StreamMode) -> Self {
        Self {
            stream: Arc::new(Mutex::new(stream)),
            mode,
        }
    }

    async fn with_stream<'a>(
        stream: &'a Arc<Mutex<io::DuplexStream>>,
    ) -> tokio::sync::MutexGuard<'a, io::DuplexStream> {
        stream.lock().await
    }
}

impl system_capnp::byte_stream::Server for ByteStreamImpl {
    fn read(
        self: capnp::capability::Rc<Self>,
        params: system_capnp::byte_stream::ReadParams,
        mut results: system_capnp::byte_stream::ReadResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        if matches!(self.mode, StreamMode::WriteOnly) {
            return Promise::from_future(async {
                Err(capnp::Error::failed("stream is write-only".into()))
            });
        }
        // ReadOnly and Bidirectional both allow read

        let max_bytes = (pry!(params.get()).get_max_bytes() as usize).min(MAX_READ_BYTES);
        let stream = self.stream.clone();
        Promise::from_future(async move {
            if max_bytes == 0 {
                results.get().set_data(&[]);
                return Ok(());
            }
            let mut buffer = vec![0u8; max_bytes];
            let mut locked = ByteStreamImpl::with_stream(&stream).await;
            let read = locked
                .read(&mut buffer)
                .await
                .map_err(|err| capnp::Error::failed(err.to_string()))?;
            buffer.truncate(read);
            results.get().set_data(&buffer);
            Ok(())
        })
    }

    fn write(
        self: capnp::capability::Rc<Self>,
        params: system_capnp::byte_stream::WriteParams,
        _results: system_capnp::byte_stream::WriteResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        if matches!(self.mode, StreamMode::ReadOnly) {
            return Promise::from_future(async {
                Err(capnp::Error::failed("stream is read-only".into()))
            });
        }
        // WriteOnly and Bidirectional both allow write

        let data = pry!(params.get()).get_data().unwrap_or(&[]).to_vec();
        let stream = self.stream.clone();
        Promise::from_future(async move {
            let mut locked = ByteStreamImpl::with_stream(&stream).await;
            if !data.is_empty() {
                locked
                    .write_all(&data)
                    .await
                    .map_err(|err| capnp::Error::failed(err.to_string()))?;
                locked
                    .flush()
                    .await
                    .map_err(|err| capnp::Error::failed(err.to_string()))?;
            }
            Ok(())
        })
    }

    fn close(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::byte_stream::CloseParams,
        _results: system_capnp::byte_stream::CloseResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let stream = self.stream.clone();
        Promise::from_future(async move {
            let mut locked = ByteStreamImpl::with_stream(&stream).await;
            let _ = locked.shutdown().await;
            Ok(())
        })
    }
}

pub struct ProcessImpl {
    stdin: system_capnp::byte_stream::Client,
    stdout: system_capnp::byte_stream::Client,
    stderr: system_capnp::byte_stream::Client,
    exit_rx: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<i32>>>>,
    bootstrap_cap: Option<capnp::capability::Client>,
    kill_tx: Arc<tokio::sync::watch::Sender<bool>>,
}

impl ProcessImpl {
    pub(crate) fn new(
        stdin: system_capnp::byte_stream::Client,
        stdout: system_capnp::byte_stream::Client,
        stderr: system_capnp::byte_stream::Client,
        exit_rx: tokio::sync::oneshot::Receiver<i32>,
        kill_tx: tokio::sync::watch::Sender<bool>,
    ) -> Self {
        Self {
            stdin,
            stdout,
            stderr,
            exit_rx: Arc::new(Mutex::new(Some(exit_rx))),
            bootstrap_cap: None,
            kill_tx: Arc::new(kill_tx),
        }
    }

    pub(crate) fn with_bootstrap(
        stdin: system_capnp::byte_stream::Client,
        stdout: system_capnp::byte_stream::Client,
        stderr: system_capnp::byte_stream::Client,
        exit_rx: tokio::sync::oneshot::Receiver<i32>,
        bootstrap_cap: capnp::capability::Client,
        kill_tx: tokio::sync::watch::Sender<bool>,
    ) -> Self {
        Self {
            stdin,
            stdout,
            stderr,
            exit_rx: Arc::new(Mutex::new(Some(exit_rx))),
            bootstrap_cap: Some(bootstrap_cap),
            kill_tx: Arc::new(kill_tx),
        }
    }
}

impl system_capnp::process::Server for ProcessImpl {
    fn stdin(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::process::StdinParams,
        mut results: system_capnp::process::StdinResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        results.get().set_stream(self.stdin.clone());
        Promise::ok(())
    }

    fn stdout(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::process::StdoutParams,
        mut results: system_capnp::process::StdoutResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        results.get().set_stream(self.stdout.clone());
        Promise::ok(())
    }

    fn stderr(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::process::StderrParams,
        mut results: system_capnp::process::StderrResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        results.get().set_stream(self.stderr.clone());
        Promise::ok(())
    }

    fn wait(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::process::WaitParams,
        mut results: system_capnp::process::WaitResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let exit_rx = Arc::clone(&self.exit_rx);
        Promise::from_future(async move {
            let mut guard = exit_rx.lock().await;
            let rx = guard.take().ok_or_else(|| {
                capnp::Error::failed("wait() already called for this process".into())
            })?;
            let code = rx.await.unwrap_or(1);
            results.get().set_exit_code(code);
            Ok(())
        })
    }

    fn bootstrap(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::process::BootstrapParams,
        mut results: system_capnp::process::BootstrapResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let cap = self.bootstrap_cap.clone();
        Promise::from_future(async move {
            let cap = cap.ok_or_else(|| {
                capnp::Error::failed(
                    "process did not export a bootstrap capability via system::serve()".into(),
                )
            })?;
            results.get().init_cap().set_as_capability(cap.hook);
            Ok(())
        })
    }

    fn kill(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::process::KillParams,
        _results: system_capnp::process::KillResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let _ = self.kill_tx.send(true);
        tracing::info!("process.kill: kill signal sent");
        Promise::ok(())
    }
}

#[allow(dead_code)] // swarm_cmd_tx and wasm_debug reserved for future Host methods
pub struct HostImpl {
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
    guard: Option<EpochGuard>,
    stream_control: Option<libp2p_stream::Control>,
    route_registry: Option<crate::dispatcher::server::RouteRegistry>,
}

impl HostImpl {
    pub fn new(
        network_state: NetworkState,
        swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
        wasm_debug: bool,
        guard: Option<EpochGuard>,
        stream_control: Option<libp2p_stream::Control>,
    ) -> Self {
        Self {
            network_state,
            swarm_cmd_tx,
            wasm_debug,
            guard,
            stream_control,
            route_registry: None,
        }
    }

    /// Set the HTTP route registry for WAGI service integration.
    pub fn with_route_registry(
        mut self,
        registry: crate::dispatcher::server::RouteRegistry,
    ) -> Self {
        self.route_registry = Some(registry);
        self
    }

    fn check_epoch(&self) -> Result<(), capnp::Error> {
        match self.guard {
            Some(ref g) => g.check(),
            None => Ok(()),
        }
    }
}

#[allow(refining_impl_trait)]
impl system_capnp::host::Server for HostImpl {
    fn id(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::host::IdParams,
        mut results: system_capnp::host::IdResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.check_epoch());
        let network_state = self.network_state.clone();
        Promise::from_future(async move {
            let snapshot = network_state.snapshot().await;
            results.get().set_peer_id(&snapshot.local_peer_id);
            Ok(())
        })
    }

    fn addrs(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::host::AddrsParams,
        mut results: system_capnp::host::AddrsResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.check_epoch());
        let network_state = self.network_state.clone();
        Promise::from_future(async move {
            let snapshot = network_state.snapshot().await;
            let mut list = results.get().init_addrs(snapshot.listen_addrs.len() as u32);
            for (i, addr) in snapshot.listen_addrs.iter().enumerate() {
                list.set(i as u32, addr);
            }
            Ok(())
        })
    }

    fn peers(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::host::PeersParams,
        mut results: system_capnp::host::PeersResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.check_epoch());
        let network_state = self.network_state.clone();
        Promise::from_future(async move {
            let snapshot = network_state.snapshot().await;
            let mut list = results.get().init_peers(snapshot.known_peers.len() as u32);
            for (i, peer) in snapshot.known_peers.iter().enumerate() {
                let mut entry = list.reborrow().get(i as u32);
                entry.set_peer_id(&peer.peer_id);
                let mut addrs = entry.init_addrs(peer.addrs.len() as u32);
                for (j, addr) in peer.addrs.iter().enumerate() {
                    addrs.set(j as u32, addr);
                }
            }
            Ok(())
        })
    }

    fn network(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::host::NetworkParams,
        mut results: system_capnp::host::NetworkResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.check_epoch());
        let guard = match &self.guard {
            Some(g) => g.clone(),
            None => {
                return Promise::err(capnp::Error::failed(
                    "network() requires an epoch-scoped Host".into(),
                ))
            }
        };
        let stream_control = match &self.stream_control {
            Some(c) => c.clone(),
            None => {
                return Promise::err(capnp::Error::failed(
                    "network() not available on this Host".into(),
                ))
            }
        };
        let stream_listener: system_capnp::stream_listener::Client = capnp_rpc::new_client(
            stream_listener::StreamListenerImpl::new(stream_control.clone(), guard.clone()),
        );
        let stream_dialer: system_capnp::stream_dialer::Client = capnp_rpc::new_client(
            stream_dialer::StreamDialerImpl::new(stream_control.clone(), guard.clone()),
        );
        let vat_listener: system_capnp::vat_listener::Client = capnp_rpc::new_client(
            vat_listener::VatListenerImpl::new(stream_control.clone(), guard.clone()),
        );
        let vat_client: system_capnp::vat_client::Client = capnp_rpc::new_client(
            vat_client::VatClientImpl::new(stream_control, guard.clone()),
        );
        let registry = self
            .route_registry
            .clone()
            .unwrap_or_else(crate::dispatcher::server::new_registry);
        let http_listener: system_capnp::http_listener::Client =
            capnp_rpc::new_client(http_listener::HttpListenerImpl::new(guard, registry));
        results.get().set_stream_listener(stream_listener);
        results.get().set_stream_dialer(stream_dialer);
        results.get().set_vat_listener(vat_listener);
        results.get().set_vat_client(vat_client);
        results.get().set_http_listener(http_listener);
        Promise::ok(())
    }
}

// =========================================================================
// CachePolicy — operator-level runtime cache configuration
// =========================================================================

/// Runtime-wide cache policy for `Runtime.load()`.
///
/// Set by `--runtime-cache-policy` CLI flag or `WW_RUNTIME_CACHE_POLICY` env var.
/// Default is `Shared` — the common case for performance.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CachePolicy {
    /// `load(same bytes)` → clone of cached Executor client (same server object).
    #[default]
    Shared,
    /// `load(same bytes)` → fresh Executor server every time.
    Isolated,
}

// =========================================================================
// RuntimeImpl — system-wide WASM compilation + execution runtime
// =========================================================================

/// The Runtime capability: compiles WASM and returns attenuated Executors.
///
/// **System-wide singleton**: RuntimeImpl is created once and every membrane
/// graft (including child cells) receives a clone of the same client. This
/// guarantees system-wide cache sharing by construction.
///
/// **OCAP discipline**: Runtime is the powerful capability (can load any binary).
/// Only pid0 gets it from `graft()`. Executor is the attenuated capability
/// (bound to one binary, can only spawn instances). pid0 hands Executors to
/// listeners, never Runtime.
pub struct RuntimeImpl {
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
    guard: Option<EpochGuard>,
    epoch_rx: Option<tokio::sync::watch::Receiver<::membrane::Epoch>>,
    signing_key: Option<Arc<ed25519_dalek::SigningKey>>,
    stream_control: Option<libp2p_stream::Control>,
    /// Runtime-wide cache policy (from `WW_RUNTIME_CACHE_POLICY` env var).
    cache_policy: CachePolicy,
    /// BLAKE3(wasm bytes) → cached Executor client (used when policy = Shared).
    ///
    /// RefCell is correct because Cap'n Proto server dispatch runs on a
    /// single-threaded LocalSet.
    executor_cache: RefCell<HashMap<[u8; 32], system_capnp::executor::Client>>,
    /// Back-reference to this Runtime's own client. Injected by
    /// [`create_runtime_client`] after construction. Cloned into each
    /// ExecutorImpl so child cells receive the same Runtime through their
    /// membrane graft.
    self_client: Rc<RefCell<Option<system_capnp::runtime::Client>>>,
}

impl RuntimeImpl {
    fn check_epoch(&self) -> Result<(), capnp::Error> {
        match self.guard {
            Some(ref g) => g.check(),
            None => Ok(()),
        }
    }

    /// Create a new ExecutorImpl bound to the given bytecode and wrap it as a client.
    fn make_executor(&self, bytecode: Arc<Vec<u8>>) -> system_capnp::executor::Client {
        let runtime_client = self
            .self_client
            .borrow()
            .clone()
            .expect("runtime self-reference must be set (use create_runtime_client)");
        capnp_rpc::new_client(ExecutorImpl {
            bytecode,
            wasm_debug: self.wasm_debug,
            network_state: self.network_state.clone(),
            swarm_cmd_tx: self.swarm_cmd_tx.clone(),
            guard: self.guard.clone(),
            epoch_rx: self.epoch_rx.clone(),
            signing_key: self.signing_key.clone(),
            stream_control: self.stream_control.clone(),
            runtime_client,
        })
    }
}

/// Create a RuntimeImpl, wrap it as a client, and inject the self-reference.
///
/// This is the only way to construct a `runtime::Client` backed by a real RuntimeImpl.
/// The returned client is a singleton — clone it wherever a Runtime is needed to
/// ensure all cells share the same compilation/executor cache.
#[allow(clippy::too_many_arguments)]
pub fn create_runtime_client(
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
    guard: Option<EpochGuard>,
    epoch_rx: Option<tokio::sync::watch::Receiver<::membrane::Epoch>>,
    signing_key: Option<Arc<ed25519_dalek::SigningKey>>,
    stream_control: Option<libp2p_stream::Control>,
    cache_policy: CachePolicy,
) -> system_capnp::runtime::Client {
    let self_client = Rc::new(RefCell::new(None));
    let runtime = RuntimeImpl {
        network_state,
        swarm_cmd_tx,
        wasm_debug,
        guard,
        epoch_rx,
        signing_key,
        stream_control,
        cache_policy,
        executor_cache: RefCell::new(HashMap::new()),
        self_client: self_client.clone(),
    };
    let client: system_capnp::runtime::Client = capnp_rpc::new_client(runtime);
    *self_client.borrow_mut() = Some(client.clone());
    client
}

fn read_text_list(list: capnp::text_list::Reader<'_>) -> Vec<String> {
    let mut out = Vec::with_capacity(list.len() as usize);
    for idx in 0..list.len() {
        if let Ok(text) = list.get(idx) {
            if let Ok(text) = text.to_str() {
                out.push(text.to_string());
            }
        }
    }
    out
}

fn read_text_list_result(list: capnp::Result<capnp::text_list::Reader<'_>>) -> Vec<String> {
    match list {
        Ok(reader) => read_text_list(reader),
        Err(_) => Vec::new(),
    }
}

fn read_data_result(data: capnp::Result<capnp::data::Reader<'_>>) -> Vec<u8> {
    match data {
        Ok(reader) => reader.to_vec(),
        Err(_) => Vec::new(),
    }
}

#[allow(refining_impl_trait)]
impl system_capnp::runtime::Server for RuntimeImpl {
    fn load(
        self: capnp::capability::Rc<Self>,
        params: system_capnp::runtime::LoadParams,
        mut results: system_capnp::runtime::LoadResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.check_epoch());
        let wasm = read_data_result(pry!(params.get()).get_wasm());

        if wasm.len() > MAX_WASM_BYTES {
            return Promise::err(capnp::Error::failed(format!(
                "WASM binary too large ({} bytes, max {})",
                wasm.len(),
                MAX_WASM_BYTES
            )));
        }

        let key = *blake3::hash(&wasm).as_bytes();

        let executor = match self.cache_policy {
            CachePolicy::Shared => {
                // Check cache with a short-lived borrow to avoid overlap with borrow_mut below.
                let cached = self.executor_cache.borrow().get(&key).cloned();
                if let Some(client) = cached {
                    tracing::debug!(?key, "runtime.load: executor cache hit (shared)");
                    client
                } else {
                    tracing::debug!(?key, "runtime.load: executor cache miss, creating");
                    let client = self.make_executor(Arc::new(wasm));
                    self.executor_cache.borrow_mut().insert(key, client.clone());
                    client
                }
            }
            CachePolicy::Isolated => {
                tracing::debug!(?key, "runtime.load: creating isolated executor");
                self.make_executor(Arc::new(wasm))
            }
        };

        results.get().set_executor(executor);
        Promise::ok(())
    }

    fn shutdown(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::runtime::ShutdownParams,
        _results: system_capnp::runtime::ShutdownResults,
    ) -> Promise<(), capnp::Error> {
        tracing::info!("runtime.shutdown: stub (tokio-runtime-per-Runtime is a future PR)");
        Promise::ok(())
    }
}

// =========================================================================
// ExecutorImpl — attenuated capability bound to one WASM binary
// =========================================================================

/// An Executor bound to a specific WASM binary. Each `spawn(args, env)` creates
/// a fresh WASI process from the stored bytecode with the given args and env.
///
/// This is the attenuated capability in the OCAP model: the holder can spawn
/// workers but cannot load arbitrary code. Args and env are late-bound per-spawn,
/// which solves the WAGI CGI env var problem (per-request env vars like
/// REQUEST_METHOD, PATH_INFO, etc.).
pub struct ExecutorImpl {
    bytecode: Arc<Vec<u8>>,
    wasm_debug: bool,
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    guard: Option<EpochGuard>,
    epoch_rx: Option<tokio::sync::watch::Receiver<::membrane::Epoch>>,
    signing_key: Option<Arc<ed25519_dalek::SigningKey>>,
    stream_control: Option<libp2p_stream::Control>,
    /// Runtime client (singleton) — passed to child cells through their membrane graft.
    runtime_client: system_capnp::runtime::Client,
}

#[allow(refining_impl_trait)]
impl system_capnp::executor::Server for ExecutorImpl {
    fn spawn(
        self: capnp::capability::Rc<Self>,
        params: system_capnp::executor::SpawnParams,
        mut results: system_capnp::executor::SpawnResults,
    ) -> Promise<(), capnp::Error> {
        if let Some(ref guard) = self.guard {
            pry!(guard.check());
        }

        let params = pry!(params.get());
        let args = read_text_list_result(params.get_args());
        let env = read_text_list_result(params.get_env());
        let bytecode = self.bytecode.clone();
        let wasm_debug = self.wasm_debug;
        let network_state = self.network_state.clone();
        let swarm_cmd_tx = self.swarm_cmd_tx.clone();
        let epoch_rx = self.epoch_rx.clone();
        let signing_key = self.signing_key.clone();
        let stream_control = self.stream_control.clone();
        let runtime_client = self.runtime_client.clone();

        Promise::from_future(async move {
            let (host_stderr, guest_stderr) = io::duplex(64 * 1024);
            let (host_stdin, guest_stdin) = io::duplex(64 * 1024);
            let (host_stdout, guest_stdout) = io::duplex(64 * 1024);

            let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
            let (kill_tx, kill_rx) = tokio::sync::watch::channel(false);
            // All cells get data_streams + membrane RPC.
            // stdin/stdout semantics vary by cell type (wire protocol, CGI,
            // or shutdown signal), but the WIT membrane channel is universal.
            let (builder, mut handles) = ProcBuilder::new()
                .with_env(env)
                .with_args(args)
                .with_wasm_debug(wasm_debug)
                .with_bytecode((*bytecode).clone())
                .with_stdio(guest_stdin, guest_stdout, guest_stderr)
                .with_data_streams();

            let proc = builder
                .build()
                .await
                .map_err(|err| capnp::Error::failed(err.to_string()))?;

            let (reader, writer) = handles
                .take_host_split()
                .ok_or_else(|| capnp::Error::failed("host stream missing".into()))?;

            let mut bootstrap_cap: Option<capnp::capability::Client> = None;
            let child_rpc_system = if let (Some(erx), Some(sc)) = (epoch_rx, stream_control) {
                let (rpc, guest) = membrane::build_membrane_rpc(
                    reader,
                    writer,
                    network_state,
                    swarm_cmd_tx,
                    wasm_debug,
                    erx,
                    signing_key,
                    sc,
                    None, // route_registry: spawned cells don't get HTTP routes
                    runtime_client,
                );
                bootstrap_cap = Some(guest.client);
                rpc
            } else {
                build_peer_rpc(reader, writer, network_state, swarm_cmd_tx, wasm_debug)
            };

            let mut kill_rx = kill_rx;
            // Spawn RPC system and stderr drain on the ambient LocalSet.
            tokio::task::spawn_local(child_rpc_system.map(|_| ()));

            tokio::task::spawn_local(async move {
                use tokio::io::AsyncBufReadExt;
                let reader = tokio::io::BufReader::new(host_stderr);
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::info!("{}", line);
                }
            });

            tokio::task::spawn_local(async move {
                let exit_code = tokio::select! {
                    result = proc.run() => {
                        match result {
                            Ok(()) => 0,
                            Err(e) => {
                                tracing::error!("executor: child process failed: {}", e);
                                1
                            }
                        }
                    }
                    _ = kill_rx.changed() => {
                        tracing::info!("executor: child process killed");
                        137 // SIGKILL convention
                    }
                };
                tracing::info!("executor: child process exited with code {}", exit_code);
                let _ = exit_tx.send(exit_code);
            });

            let stdin =
                capnp_rpc::new_client(ByteStreamImpl::new(host_stdin, StreamMode::WriteOnly));
            let stdout =
                capnp_rpc::new_client(ByteStreamImpl::new(host_stdout, StreamMode::ReadOnly));
            let (dummy_stderr, _) = io::duplex(1);
            let stderr =
                capnp_rpc::new_client(ByteStreamImpl::new(dummy_stderr, StreamMode::ReadOnly));

            let process_impl = if let Some(cap) = bootstrap_cap {
                ProcessImpl::with_bootstrap(stdin, stdout, stderr, exit_rx, cap, kill_tx)
            } else {
                ProcessImpl::new(stdin, stdout, stderr, exit_rx, kill_tx)
            };
            let process_client: system_capnp::process::Client = capnp_rpc::new_client(process_impl);
            results.get().set_process(process_client);

            Ok(())
        })
    }
}

pub fn build_peer_rpc<R, W>(
    reader: R,
    writer: W,
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
) -> RpcSystem<Side>
where
    R: AsyncRead + Unpin + 'static,
    W: AsyncWrite + Unpin + 'static,
{
    let host: system_capnp::host::Client = capnp_rpc::new_client(HostImpl::new(
        network_state,
        swarm_cmd_tx,
        wasm_debug,
        None,
        None,
    ));

    let rpc_network = VatNetwork::new(
        reader.compat(),
        writer.compat_write(),
        Side::Server,
        Default::default(),
    );
    RpcSystem::new(Box::new(rpc_network), Some(host.client))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    /// Helper: spin up server + client over in-memory duplex, return Host client.
    fn setup_rpc() -> (
        system_capnp::host::Client,
        tokio::task::JoinHandle<()>,
        mpsc::Receiver<SwarmCommand>,
    ) {
        let (client_stream, server_stream) = io::duplex(8 * 1024);
        let (client_read, client_write) = io::split(client_stream);
        let (server_read, server_write) = io::split(server_stream);

        let peer_id = vec![1, 2, 3, 4];
        let network_state = NetworkState::from_peer_id(peer_id);
        let (swarm_tx, swarm_rx) = mpsc::channel(16);

        let server_rpc = build_peer_rpc(server_read, server_write, network_state, swarm_tx, false);

        let server_handle = tokio::task::spawn_local(async move {
            let _ = server_rpc.await;
        });

        let client_network = VatNetwork::new(
            client_read.compat(),
            client_write.compat_write(),
            Side::Client,
            Default::default(),
        );
        let mut client_rpc = RpcSystem::new(Box::new(client_network), None);
        let host: system_capnp::host::Client = client_rpc.bootstrap(Side::Server);
        tokio::task::spawn_local(async move {
            let _ = client_rpc.await;
        });

        (host, server_handle, swarm_rx)
    }

    #[tokio::test]
    async fn test_host_id_returns_peer_id() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (host, _server, _rx) = setup_rpc();

                let resp = host.id_request().send().promise.await.unwrap();
                let peer_id = resp.get().unwrap().get_peer_id().unwrap();
                assert_eq!(peer_id, &[1, 2, 3, 4]);
            })
            .await;
    }

    #[tokio::test]
    async fn test_host_addrs_initially_empty() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (host, _server, _rx) = setup_rpc();

                let resp = host.addrs_request().send().promise.await.unwrap();
                let addrs = resp.get().unwrap().get_addrs().unwrap();
                assert_eq!(addrs.len(), 0);
            })
            .await;
    }

    #[tokio::test]
    async fn test_host_peers_initially_empty() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (host, _server, _rx) = setup_rpc();

                let resp = host.peers_request().send().promise.await.unwrap();
                let peers = resp.get().unwrap().get_peers().unwrap();
                assert_eq!(peers.len(), 0);
            })
            .await;
    }

    // Echo tests removed — echo method deleted from API.
    // Runtime.load() and Executor.spawn() are tested via integration tests
    // that compile real WASM (not mockable in unit tests without a wasmtime Engine).

    #[tokio::test]
    async fn test_network_state_snapshot() {
        let state = NetworkState::from_peer_id(vec![42]);

        let snap = state.snapshot().await;
        assert_eq!(snap.local_peer_id, vec![42]);
        assert!(snap.listen_addrs.is_empty());
        assert!(snap.known_peers.is_empty());
    }

    #[tokio::test]
    async fn test_network_state_add_remove_addr() {
        let state = NetworkState::from_peer_id(vec![1]);

        state.add_listen_addr(vec![10, 20]).await;
        state.add_listen_addr(vec![30, 40]).await;

        let snap = state.snapshot().await;
        assert_eq!(snap.listen_addrs.len(), 2);

        // Duplicate add is a no-op
        state.add_listen_addr(vec![10, 20]).await;
        let snap = state.snapshot().await;
        assert_eq!(snap.listen_addrs.len(), 2);

        // Remove
        state.remove_listen_addr(&[10, 20]).await;
        let snap = state.snapshot().await;
        assert_eq!(snap.listen_addrs.len(), 1);
        assert_eq!(snap.listen_addrs[0], vec![30, 40]);
    }

    #[tokio::test]
    async fn test_network_state_set_known_peers() {
        let state = NetworkState::from_peer_id(vec![1]);

        let peers = vec![
            PeerInfo {
                peer_id: vec![2],
                addrs: vec![vec![10]],
            },
            PeerInfo {
                peer_id: vec![3],
                addrs: vec![vec![20], vec![30]],
            },
        ];
        state.set_known_peers(peers).await;

        let snap = state.snapshot().await;
        assert_eq!(snap.known_peers.len(), 2);
        assert_eq!(snap.known_peers[0].peer_id, vec![2]);
        assert_eq!(snap.known_peers[1].addrs.len(), 2);
    }

    #[tokio::test]
    async fn test_network_state_set_peer_id() {
        let state = NetworkState::from_peer_id(vec![1]);
        state.set_local_peer_id(vec![99]).await;

        let snap = state.snapshot().await;
        assert_eq!(snap.local_peer_id, vec![99]);
    }

    #[tokio::test]
    async fn test_network_state_clone_shares_state() {
        let state1 = NetworkState::from_peer_id(vec![1]);
        let state2 = state1.clone();

        state1.add_listen_addr(vec![10]).await;

        let snap = state2.snapshot().await;
        assert_eq!(snap.listen_addrs.len(), 1);
    }

    // =========================================================================
    // schema_cid / schema_protocol tests
    // =========================================================================

    #[test]
    fn test_schema_cid_deterministic() {
        let schema = b"some canonical schema bytes with id 0xdeadbeef";
        let cid1 = super::schema_cid(schema);
        let cid2 = super::schema_cid(schema);
        assert_eq!(cid1, cid2, "same schema bytes must produce same CID");
    }

    #[test]
    fn test_schema_cid_different_for_different_schemas() {
        let schema_a = b"\x00\x00\x00\x00\x00\x00\x00\x01interface A";
        let schema_b = b"\x00\x00\x00\x00\x00\x00\x00\x02interface A";
        let cid_a = super::schema_cid(schema_a);
        let cid_b = super::schema_cid(schema_b);
        assert_ne!(
            cid_a, cid_b,
            "different type IDs must produce different CIDs"
        );
    }

    #[test]
    fn test_schema_cid_is_valid_cid() {
        let schema = b"test schema node bytes";
        let cid_str = super::schema_cid(schema);
        // Must parse back as a valid CID.
        let parsed = cid_str.parse::<cid::Cid>();
        assert!(parsed.is_ok(), "schema_cid must produce a valid CID string");
        let cid = parsed.unwrap();
        assert_eq!(cid.version(), cid::Version::V1);
        assert_eq!(cid.codec(), 0x55); // raw codec
    }

    #[test]
    fn test_schema_protocol_builds_valid_protocol() {
        let schema = b"test schema";
        let cid_str = super::schema_cid(schema);
        let protocol = super::schema_protocol(&cid_str);
        assert!(protocol.is_ok());
        let proto = protocol.unwrap();
        assert!(proto.as_ref().starts_with("/ww/0.1.0/"));
        assert!(proto.as_ref().contains(&cid_str));
    }

    // =========================================================================
    // Process.bootstrap() tests
    // =========================================================================

    /// Helper: create an in-memory RPC pair for a Process capability.
    fn setup_process_rpc(process_impl: ProcessImpl) -> system_capnp::process::Client {
        let (client_stream, server_stream) = io::duplex(8 * 1024);
        let (client_read, client_write) = io::split(client_stream);
        let (server_read, server_write) = io::split(server_stream);

        let process_cap: system_capnp::process::Client = capnp_rpc::new_client(process_impl);

        let server_network = VatNetwork::new(
            server_read.compat(),
            server_write.compat_write(),
            Side::Server,
            Default::default(),
        );
        let server_rpc = RpcSystem::new(Box::new(server_network), Some(process_cap.client));
        tokio::task::spawn_local(async move {
            let _ = server_rpc.await;
        });

        let client_network = VatNetwork::new(
            client_read.compat(),
            client_write.compat_write(),
            Side::Client,
            Default::default(),
        );
        let mut client_rpc = RpcSystem::new(Box::new(client_network), None);
        let client: system_capnp::process::Client = client_rpc.bootstrap(Side::Server);
        tokio::task::spawn_local(async move {
            let _ = client_rpc.await;
        });

        client
    }

    /// Helper: create a dummy ByteStream + exit channel for ProcessImpl.
    fn dummy_process_parts() -> (
        system_capnp::byte_stream::Client,
        system_capnp::byte_stream::Client,
        system_capnp::byte_stream::Client,
        tokio::sync::oneshot::Receiver<i32>,
        tokio::sync::watch::Sender<bool>,
    ) {
        let (dummy_in, _) = io::duplex(1);
        let (dummy_out, _) = io::duplex(1);
        let (dummy_err, _) = io::duplex(1);
        let stdin = capnp_rpc::new_client(ByteStreamImpl::new(dummy_in, StreamMode::WriteOnly));
        let stdout = capnp_rpc::new_client(ByteStreamImpl::new(dummy_out, StreamMode::ReadOnly));
        let stderr = capnp_rpc::new_client(ByteStreamImpl::new(dummy_err, StreamMode::ReadOnly));
        let (_tx, rx) = tokio::sync::oneshot::channel();
        let (kill_tx, _kill_rx) = tokio::sync::watch::channel(false);
        (stdin, stdout, stderr, rx, kill_tx)
    }

    #[tokio::test]
    async fn test_process_bootstrap_returns_stored_cap() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Use the Host cap as the bootstrap capability.
                let (host, _server, _rx) = setup_rpc();

                let (stdin, stdout, stderr, exit_rx, kill_tx) = dummy_process_parts();
                let process_impl = ProcessImpl::with_bootstrap(
                    stdin,
                    stdout,
                    stderr,
                    exit_rx,
                    host.client.clone(),
                    kill_tx,
                );
                let process = setup_process_rpc(process_impl);

                // Call bootstrap() — should return the stored cap.
                let resp = process.bootstrap_request().send().promise.await.unwrap();
                let cap = resp.get().unwrap().get_cap();

                // Cast it back to a Host and verify it works.
                let host2: system_capnp::host::Client = cap.get_as_capability().unwrap();
                let id_resp = host2.id_request().send().promise.await.unwrap();
                let peer_id = id_resp.get().unwrap().get_peer_id().unwrap();
                assert_eq!(peer_id, &[1, 2, 3, 4]);
            })
            .await;
    }

    #[tokio::test]
    async fn test_process_bootstrap_errors_without_cap() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (stdin, stdout, stderr, exit_rx, kill_tx) = dummy_process_parts();
                let process_impl = ProcessImpl::new(stdin, stdout, stderr, exit_rx, kill_tx);
                let process = setup_process_rpc(process_impl);

                // Call bootstrap() without a stored cap — should error.
                let result = process.bootstrap_request().send().promise.await;
                assert!(
                    result.is_err() || {
                        let resp = result.unwrap();
                        // The error may come from get_cap() trying to read a missing cap,
                        // or from the server returning an error in the response.
                        resp.get().is_err()
                    }
                );
            })
            .await;
    }

    #[tokio::test]
    async fn test_process_bootstrap_cap_survives_multiple_calls() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (host, _server, _rx) = setup_rpc();

                let (stdin, stdout, stderr, exit_rx, kill_tx) = dummy_process_parts();
                let process_impl = ProcessImpl::with_bootstrap(
                    stdin,
                    stdout,
                    stderr,
                    exit_rx,
                    host.client.clone(),
                    kill_tx,
                );
                let process = setup_process_rpc(process_impl);

                // Call bootstrap() twice — both should return working caps.
                for _ in 0..2 {
                    let resp = process.bootstrap_request().send().promise.await.unwrap();
                    let cap = resp.get().unwrap().get_cap();
                    let host2: system_capnp::host::Client = cap.get_as_capability().unwrap();
                    let id_resp = host2.id_request().send().promise.await.unwrap();
                    let peer_id = id_resp.get().unwrap().get_peer_id().unwrap();
                    assert_eq!(peer_id, &[1, 2, 3, 4]);
                }
            })
            .await;
    }

    #[tokio::test]
    async fn test_bootstrap_cap_resolves_after_delay() {
        // Simulate the real scenario: build_membrane_rpc returns a pipelined
        // bootstrap cap immediately, but the cell hasn't called serve() yet.
        // Cap'n Proto promise pipelining should queue requests and resolve them
        // once the underlying cap becomes available.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (host, _server, _rx) = setup_rpc();

                // Create a "delayed" host cap using new_future_client.
                // This simulates a pipelined cap that resolves after 200ms.
                let host_clone = host.clone();
                let delayed_host: system_capnp::host::Client =
                    capnp_rpc::new_future_client(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        Ok::<_, capnp::Error>(host_clone)
                    });

                // Store the delayed cap in ProcessImpl.
                let (stdin, stdout, stderr, exit_rx, kill_tx) = dummy_process_parts();
                let process_impl = ProcessImpl::with_bootstrap(
                    stdin,
                    stdout,
                    stderr,
                    exit_rx,
                    delayed_host.client.clone(),
                    kill_tx,
                );
                let process = setup_process_rpc(process_impl);

                // Call bootstrap() immediately — the cap hasn't resolved yet.
                let resp = process.bootstrap_request().send().promise.await.unwrap();
                let cap = resp.get().unwrap().get_cap();
                let host2: system_capnp::host::Client = cap.get_as_capability().unwrap();

                // Use the cap — should block until the delayed future resolves.
                let id_resp = host2.id_request().send().promise.await.unwrap();
                let peer_id = id_resp.get().unwrap().get_peer_id().unwrap();
                assert_eq!(peer_id, &[1, 2, 3, 4]);
            })
            .await;
    }

    // =========================================================================
    // Host.network() tests
    // =========================================================================

    #[tokio::test]
    async fn test_host_network_errors_without_epoch() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // setup_rpc() creates a non-epoch-scoped Host (no guard, no stream_control).
                let (host, _server, _rx) = setup_rpc();

                let result = host.network_request().send().promise.await;
                assert!(
                    result.is_err(),
                    "network() should fail on non-epoch-scoped Host"
                );
            })
            .await;
    }

    // =========================================================================
    // Process.wait() tests
    // =========================================================================

    #[tokio::test]
    async fn test_process_wait_returns_exit_code() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (stdin, stdout, stderr, _, kill_tx) = dummy_process_parts();
                // Create our own channel so we control the sender.
                let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
                let process_impl = ProcessImpl::new(stdin, stdout, stderr, exit_rx, kill_tx);
                let process = setup_process_rpc(process_impl);

                // Send exit code from the "cell" side.
                exit_tx.send(42).unwrap();

                let resp = process.wait_request().send().promise.await.unwrap();
                let exit_code = resp.get().unwrap().get_exit_code();
                assert_eq!(exit_code, 42);
            })
            .await;
    }

    #[tokio::test]
    async fn test_process_wait_double_call_errors() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (stdin, stdout, stderr, _, kill_tx) = dummy_process_parts();
                let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
                let process_impl = ProcessImpl::new(stdin, stdout, stderr, exit_rx, kill_tx);
                let process = setup_process_rpc(process_impl);

                exit_tx.send(0).unwrap();

                // First call succeeds.
                let resp = process.wait_request().send().promise.await.unwrap();
                assert_eq!(resp.get().unwrap().get_exit_code(), 0);

                // Second call should error (receiver already consumed).
                let result = process.wait_request().send().promise.await;
                assert!(result.is_err(), "wait() called twice should fail");
            })
            .await;
    }

    // =========================================================================
    // RPC bridge integration tests
    // =========================================================================
    //
    // These test the full capability bridge pattern used by VatListener/VatClient
    // without requiring libp2p or WASM. We simulate the bridge with duplex streams:
    //
    //   Cell (Host cap)
    //       ↓ bootstrap cap
    //   Process.bootstrap()
    //       ↓ cap over duplex
    //   Host bridge (Side::Server, bootstrap = cell_cap)
    //       ↓ duplex stream
    //   Remote peer (Side::Client, bootstraps → gets cell_cap)
    //       ↓
    //   Uses the cap (id request)

    /// Simulate the host bridge: serve a bootstrap cap over a duplex stream,
    /// return the "remote peer" side client that bootstrapped from it.
    fn setup_bridge<T: capnp::capability::FromClientHook>(
        bootstrap_cap: capnp::capability::Client,
    ) -> (T, tokio::task::JoinHandle<()>) {
        let (peer_stream, bridge_stream) = io::duplex(8 * 1024);
        let (bridge_read, bridge_write) = io::split(bridge_stream);
        let (peer_read, peer_write) = io::split(peer_stream);

        // Host bridge side: serve the cell's cap.
        let bridge_network = VatNetwork::new(
            bridge_read.compat(),
            bridge_write.compat_write(),
            Side::Server,
            Default::default(),
        );
        let bridge_rpc = RpcSystem::new(Box::new(bridge_network), Some(bootstrap_cap));
        let bridge_handle = tokio::task::spawn_local(async move {
            let _ = bridge_rpc.await;
        });

        // Remote peer side: bootstrap to get the cell's cap.
        let peer_network = VatNetwork::new(
            peer_read.compat(),
            peer_write.compat_write(),
            Side::Client,
            Default::default(),
        );
        let mut peer_rpc = RpcSystem::new(Box::new(peer_network), None);
        let remote_cap: T = peer_rpc.bootstrap(Side::Server);
        tokio::task::spawn_local(async move {
            let _ = peer_rpc.await;
        });

        (remote_cap, bridge_handle)
    }

    #[tokio::test]
    async fn test_rpc_bridge_cap_flows_to_remote_peer() {
        // The golden path: cell exports a cap → Process.bootstrap() →
        // host serves it over a stream → remote peer bootstraps and uses it.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // 1. Create a Host cap (the "cell's exported cap").
                let (host, _server, _rx) = setup_rpc();

                // 2. Store it in a ProcessImpl (simulates build_membrane_rpc capture).
                let (stdin, stdout, stderr, exit_rx, kill_tx) = dummy_process_parts();
                let process_impl = ProcessImpl::with_bootstrap(
                    stdin,
                    stdout,
                    stderr,
                    exit_rx,
                    host.client.clone(),
                    kill_tx,
                );
                let process = setup_process_rpc(process_impl);

                // 3. Call Process.bootstrap() to get the cap (what handle_rpc_connection does).
                let resp = process.bootstrap_request().send().promise.await.unwrap();
                let bootstrap_cap: capnp::capability::Client =
                    resp.get().unwrap().get_cap().get_as_capability().unwrap();

                // 4. Bridge: serve it over a duplex (simulates the libp2p stream bridge).
                let (remote_host, _bridge): (system_capnp::host::Client, _) =
                    setup_bridge(bootstrap_cap);

                // 5. Remote peer uses the cap through the bridge.
                let id_resp = remote_host.id_request().send().promise.await.unwrap();
                let peer_id = id_resp.get().unwrap().get_peer_id().unwrap();
                assert_eq!(peer_id, &[1, 2, 3, 4]);
            })
            .await;
    }

    #[tokio::test]
    async fn test_rpc_bridge_multiple_calls_through_bridge() {
        // Verify the bridge handles multiple sequential RPC calls, not just one.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (host, _server, _rx) = setup_rpc();

                let (stdin, stdout, stderr, exit_rx, kill_tx) = dummy_process_parts();
                let process_impl = ProcessImpl::with_bootstrap(
                    stdin,
                    stdout,
                    stderr,
                    exit_rx,
                    host.client.clone(),
                    kill_tx,
                );
                let process = setup_process_rpc(process_impl);

                let resp = process.bootstrap_request().send().promise.await.unwrap();
                let bootstrap_cap: capnp::capability::Client =
                    resp.get().unwrap().get_cap().get_as_capability().unwrap();

                let (remote_host, _bridge): (system_capnp::host::Client, _) =
                    setup_bridge(bootstrap_cap);

                // Make 5 calls through the bridge.
                for _ in 0..5 {
                    let id_resp = remote_host.id_request().send().promise.await.unwrap();
                    let peer_id = id_resp.get().unwrap().get_peer_id().unwrap();
                    assert_eq!(peer_id, &[1, 2, 3, 4]);
                }
            })
            .await;
    }

    #[tokio::test]
    async fn test_rpc_bridge_concurrent_calls() {
        // Verify pipelined (concurrent) calls work through the bridge.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (host, _server, _rx) = setup_rpc();

                let (stdin, stdout, stderr, exit_rx, kill_tx) = dummy_process_parts();
                let process_impl = ProcessImpl::with_bootstrap(
                    stdin,
                    stdout,
                    stderr,
                    exit_rx,
                    host.client.clone(),
                    kill_tx,
                );
                let process = setup_process_rpc(process_impl);

                let resp = process.bootstrap_request().send().promise.await.unwrap();
                let bootstrap_cap: capnp::capability::Client =
                    resp.get().unwrap().get_cap().get_as_capability().unwrap();

                let (remote_host, _bridge): (system_capnp::host::Client, _) =
                    setup_bridge(bootstrap_cap);

                // Fire 5 calls concurrently (pipelined), then collect results.
                let mut futures = Vec::new();
                for _ in 0..5 {
                    futures.push(remote_host.id_request().send().promise);
                }

                for fut in futures {
                    let resp = fut.await.unwrap();
                    let peer_id = resp.get().unwrap().get_peer_id().unwrap();
                    assert_eq!(peer_id, &[1, 2, 3, 4]);
                }
            })
            .await;
    }

    #[tokio::test]
    async fn test_rpc_bridge_distinct_caps_stay_independent() {
        // Two separate bridges with different bootstrap caps don't interfere.
        // This validates that the bridge correctly isolates per-connection state.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (host, _server, _rx) = setup_rpc();

                // Create two independent process+bridge chains.
                let mut remote_hosts = Vec::new();
                for _ in 0..2 {
                    let (stdin, stdout, stderr, exit_rx, kill_tx) = dummy_process_parts();
                    let process_impl = ProcessImpl::with_bootstrap(
                        stdin,
                        stdout,
                        stderr,
                        exit_rx,
                        host.client.clone(),
                        kill_tx,
                    );
                    let process = setup_process_rpc(process_impl);

                    let resp = process.bootstrap_request().send().promise.await.unwrap();
                    let cap: capnp::capability::Client =
                        resp.get().unwrap().get_cap().get_as_capability().unwrap();

                    let (remote, _bridge): (system_capnp::host::Client, _) = setup_bridge(cap);
                    remote_hosts.push(remote);
                }

                // Both bridges work independently.
                for remote in &remote_hosts {
                    let id_resp = remote.id_request().send().promise.await.unwrap();
                    let peer_id = id_resp.get().unwrap().get_peer_id().unwrap();
                    assert_eq!(peer_id, &[1, 2, 3, 4]);
                }
            })
            .await;
    }

    // =========================================================================
    // VatListener / VatClient validation tests
    // =========================================================================

    /// Helper: create an EpochGuard and its sender for test manipulation.
    fn test_epoch_guard(seq: u64) -> (tokio::sync::watch::Sender<::membrane::Epoch>, EpochGuard) {
        let epoch = ::membrane::Epoch {
            seq,
            head: vec![],
            adopted_block: 0,
        };
        let (tx, rx) = tokio::sync::watch::channel(epoch);
        let guard = EpochGuard {
            issued_seq: seq,
            receiver: rx,
        };
        (tx, guard)
    }

    /// Helper: create a dummy stream_control for validation tests.
    /// The control won't be used for actual I/O in these tests.
    fn dummy_stream_control() -> libp2p_stream::Control {
        libp2p_stream::Behaviour::new().new_control()
    }

    /// Stub Executor for tests that need an executor::Client without real WASM.
    struct StubExecutor;

    #[allow(refining_impl_trait)]
    impl system_capnp::executor::Server for StubExecutor {
        fn spawn(
            self: capnp::capability::Rc<Self>,
            _params: system_capnp::executor::SpawnParams,
            _results: system_capnp::executor::SpawnResults,
        ) -> Promise<(), capnp::Error> {
            Promise::err(capnp::Error::failed("stub executor".into()))
        }
    }

    /// Create a stub Executor client for tests.
    fn stub_executor() -> system_capnp::executor::Client {
        capnp_rpc::new_client(StubExecutor)
    }

    /// Build a minimal WASM component with an optional custom section.
    /// Returns bytes that wasmparser can parse (valid WASM component header).
    fn wasm_with_custom_section(section_name: &str, data: &[u8]) -> Vec<u8> {
        use wasm_encoder::ComponentSection;
        // Minimal WASM component: magic + version + layer
        let mut bytes = vec![
            0x00, 0x61, 0x73, 0x6d, // \0asm
            0x0d, 0x00, 0x01, 0x00, // component version (13.0)
        ];
        let custom = wasm_encoder::CustomSection {
            name: std::borrow::Cow::Borrowed(section_name),
            data: std::borrow::Cow::Borrowed(data),
        };
        custom.append_to_component(&mut bytes);
        bytes
    }

    /// Build a minimal WASM component with NO custom sections.
    fn wasm_without_custom_section() -> Vec<u8> {
        vec![
            0x00, 0x61, 0x73, 0x6d, // \0asm
            0x0d, 0x00, 0x01, 0x00, // component version (13.0)
        ]
    }

    #[tokio::test]
    async fn test_vat_listener_empty_schema_param_errors() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (_tx, guard) = test_epoch_guard(1);
                let listener_impl =
                    vat_listener::VatListenerImpl::new(dummy_stream_control(), guard);
                let listener: system_capnp::vat_listener::Client =
                    capnp_rpc::new_client(listener_impl);

                let executor = stub_executor();

                let mut req = listener.listen_request();
                {
                    let mut handler = req.get().init_handler();
                    handler.set_spawn(executor);
                }
                req.get().set_schema(&[]); // empty schema

                let result = req.send().promise.await;
                assert!(result.is_err(), "empty schema param should error");
            })
            .await;
    }

    // (test_vat_listener_empty_schema_section_errors removed — schema is now
    // an explicit param, tested by test_vat_listener_empty_schema_param_errors)

    #[tokio::test]
    async fn test_vat_client_empty_schema_errors() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (_tx, guard) = test_epoch_guard(1);
                let dialer_impl = vat_client::VatClientImpl::new(dummy_stream_control(), guard);
                let dialer: system_capnp::vat_client::Client = capnp_rpc::new_client(dialer_impl);

                let mut req = dialer.dial_request();
                // Valid peer ID (Ed25519 public key)
                let keypair = libp2p::identity::Keypair::generate_ed25519();
                let peer_id = libp2p::PeerId::from_public_key(&keypair.public());
                req.get().set_peer(&peer_id.to_bytes());
                req.get().set_schema(&[]); // empty schema

                let result = req.send().promise.await;
                assert!(result.is_err(), "empty schema should error");
            })
            .await;
    }

    #[tokio::test]
    async fn test_vat_client_invalid_peer_id_errors() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (_tx, guard) = test_epoch_guard(1);
                let dialer_impl = vat_client::VatClientImpl::new(dummy_stream_control(), guard);
                let dialer: system_capnp::vat_client::Client = capnp_rpc::new_client(dialer_impl);

                let mut req = dialer.dial_request();
                req.get().set_peer(&[0xFF, 0xFF, 0xFF]); // garbage peer ID
                req.get().set_schema(b"valid schema bytes");

                let result = req.send().promise.await;
                assert!(result.is_err(), "invalid peer ID should error");
            })
            .await;
    }

    #[tokio::test]
    async fn test_vat_listener_stale_epoch_errors() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (tx, guard) = test_epoch_guard(1);
                let listener_impl =
                    vat_listener::VatListenerImpl::new(dummy_stream_control(), guard);
                let listener: system_capnp::vat_listener::Client =
                    capnp_rpc::new_client(listener_impl);

                // Advance epoch to make guard stale.
                tx.send(::membrane::Epoch {
                    seq: 2,
                    head: vec![],
                    adopted_block: 0,
                })
                .unwrap();

                let executor = stub_executor();

                let mut req = listener.listen_request();
                {
                    let mut handler = req.get().init_handler();
                    handler.set_spawn(executor);
                }
                req.get().set_schema(b"some schema");

                let result = req.send().promise.await;
                assert!(result.is_err(), "stale epoch should error");
            })
            .await;
    }

    #[tokio::test]
    async fn test_vat_client_stale_epoch_errors() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (tx, guard) = test_epoch_guard(1);
                let dialer_impl = vat_client::VatClientImpl::new(dummy_stream_control(), guard);
                let dialer: system_capnp::vat_client::Client = capnp_rpc::new_client(dialer_impl);

                // Advance epoch to make guard stale.
                tx.send(::membrane::Epoch {
                    seq: 2,
                    head: vec![],
                    adopted_block: 0,
                })
                .unwrap();

                let keypair = libp2p::identity::Keypair::generate_ed25519();
                let peer_id = libp2p::PeerId::from_public_key(&keypair.public());

                let mut req = dialer.dial_request();
                req.get().set_peer(&peer_id.to_bytes());
                req.get().set_schema(b"some schema");

                let result = req.send().promise.await;
                assert!(result.is_err(), "stale epoch should error");
            })
            .await;
    }

    #[tokio::test]
    async fn test_vat_listener_protocol_collision_errors() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (_tx, guard) = test_epoch_guard(1);
                // Share the same Behaviour so both listeners see the same protocol registry.
                let behaviour = libp2p_stream::Behaviour::new();
                let control1 = behaviour.new_control();
                let control2 = behaviour.new_control();

                let listener1 = vat_listener::VatListenerImpl::new(control1, guard.clone());
                let client1: system_capnp::vat_listener::Client = capnp_rpc::new_client(listener1);

                let listener2 = vat_listener::VatListenerImpl::new(control2, guard);
                let client2: system_capnp::vat_listener::Client = capnp_rpc::new_client(listener2);

                let executor = stub_executor();

                // Both registrations use the same schema → same protocol CID.
                let schema = b"some schema bytes";

                // First registration should succeed.
                let mut req1 = client1.listen_request();
                {
                    let mut handler = req1.get().init_handler();
                    handler.set_spawn(executor.clone());
                }
                req1.get().set_schema(schema);
                req1.send()
                    .promise
                    .await
                    .expect("first listen should succeed");

                // Second registration with same schema should fail (same protocol CID).
                let mut req2 = client2.listen_request();
                {
                    let mut handler = req2.get().init_handler();
                    handler.set_spawn(executor);
                }
                req2.get().set_schema(schema);
                let result = req2.send().promise.await;
                assert!(
                    result.is_err(),
                    "duplicate protocol registration should error"
                );
            })
            .await;
    }

    #[test]
    fn test_schema_cid_matches_schema_id_compute_cid() {
        // Verify the runtime schema_cid() in mod.rs produces the same CID
        // as the build-time compute_cid() in the schema-id crate.
        let test_inputs: &[&[u8]] = &[b"test schema bytes", b"\x00\x01\x02\x03", b"", &[0xff; 256]];
        for input in test_inputs {
            let runtime_cid = super::schema_cid(input);
            let buildtime_cid = schema_id::compute_cid(input);
            assert_eq!(
                runtime_cid,
                buildtime_cid,
                "CID mismatch for input of length {}",
                input.len()
            );
        }
    }

    #[test]
    fn test_extract_wasm_custom_section_found() {
        let data = b"canonical schema bytes";
        let wasm = wasm_with_custom_section("schema.capnp", data);
        let result = super::extract_wasm_custom_section(&wasm, "schema.capnp").unwrap();
        assert_eq!(result, Some(data.as_slice()));
    }

    #[test]
    fn test_extract_wasm_custom_section_not_found() {
        let wasm = wasm_without_custom_section();
        let result = super::extract_wasm_custom_section(&wasm, "schema.capnp").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_wasm_custom_section_wrong_name() {
        let wasm = wasm_with_custom_section("other.section", b"data");
        let result = super::extract_wasm_custom_section(&wasm, "schema.capnp").unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_extract_wasm_custom_section_invalid_wasm() {
        let result = super::extract_wasm_custom_section(b"not wasm", "schema.capnp");
        assert!(result.is_err());
    }

    /// End-to-end: pass schema bytes and an Executor to VatListener,
    /// and verify it successfully registers a protocol.
    #[tokio::test]
    async fn test_vat_listener_accepts_valid_schema_and_handler() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (_tx, guard) = test_epoch_guard(1);
                let listener_impl =
                    vat_listener::VatListenerImpl::new(dummy_stream_control(), guard);
                let listener: system_capnp::vat_listener::Client =
                    capnp_rpc::new_client(listener_impl);

                let executor = stub_executor();

                let mut req = listener.listen_request();
                {
                    let mut handler = req.get().init_handler();
                    handler.set_spawn(executor);
                }
                req.get().set_schema(b"valid schema bytes");

                let result = req.send().promise.await;
                assert!(
                    result.is_ok(),
                    "valid schema + handler should be accepted: {:?}",
                    result.err()
                );
            })
            .await;
    }

    #[tokio::test]
    async fn test_rpc_bridge_dead_cell_returns_error() {
        // When the cell's RPC system dies (process exits), the cap served
        // through the bridge should break. We simulate this by creating a
        // cell-side RPC system we directly control, then abort() it.
        //
        // Topology:
        //   cell RPC (Side::Server, serves Host)
        //       ↓ bootstrap
        //   cell_cap (client ref to Host)
        //       ↓ bridged over
        //   bridge RPC (Side::Server, bootstrap = cell_cap)
        //       ↓ bootstrap
        //   remote_host (remote peer's view)
        //
        // We abort the cell RPC task, which drops the RPC system,
        // closes the duplex half, and disconnects the Host cap.
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                // Create a Host cap to serve as the cell's exported cap.
                let (host, _server, _rx) = setup_rpc();

                // Set up a cell-side RPC system we control.
                let (cell_stream, host_stream) = io::duplex(8 * 1024);
                let (h_read, h_write) = io::split(cell_stream);
                let (c_read, c_write) = io::split(host_stream);

                let cell_network = VatNetwork::new(
                    h_read.compat(),
                    h_write.compat_write(),
                    Side::Server,
                    Default::default(),
                );
                let cell_rpc = RpcSystem::new(Box::new(cell_network), Some(host.client));
                let cell_task = tokio::task::spawn_local(async move {
                    let _ = cell_rpc.await;
                });

                let client_network = VatNetwork::new(
                    c_read.compat(),
                    c_write.compat_write(),
                    Side::Client,
                    Default::default(),
                );
                let mut client_rpc = RpcSystem::new(Box::new(client_network), None);
                let cell_cap: system_capnp::host::Client = client_rpc.bootstrap(Side::Server);
                let cell_cap = cell_cap.client;
                tokio::task::spawn_local(async move {
                    let _ = client_rpc.await;
                });

                // Bridge the cell cap to a remote peer.
                let (remote_host, _bridge): (system_capnp::host::Client, _) =
                    setup_bridge(cell_cap);

                // Verify it works while alive.
                let id_resp = remote_host.id_request().send().promise.await.unwrap();
                let peer_id = id_resp.get().unwrap().get_peer_id().unwrap();
                assert_eq!(peer_id, &[1, 2, 3, 4]);

                // Kill the cell's RPC system.
                cell_task.abort();
                // Let the runtime propagate the disconnection.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;

                // Call through the bridge should now fail.
                let result = tokio::time::timeout(
                    std::time::Duration::from_millis(500),
                    remote_host.id_request().send().promise,
                )
                .await;

                // Either the inner call errors (disconnected) or the timeout fires
                // (cap is dead but stream hasn't noticed yet). Both prove the cell
                // death propagated — a live cap would return Ok instantly.
                let is_dead = match result {
                    Err(_) => true,     // timeout — stream stalled
                    Ok(Err(_)) => true, // RPC error — disconnected
                    Ok(Ok(resp)) => {
                        // If somehow a response came back, it should be an error.
                        resp.get().is_err()
                    }
                };
                assert!(is_dead, "call through bridge should fail after cell dies");
            })
            .await;
    }
}
