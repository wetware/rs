//! Cap'n Proto RPC for host-provided capabilities.
//!
//! The Host capability is served to each WASM guest over in-memory duplex
//! streams (no TCP listener). See [`build_peer_rpc`] for the entry point.
#![cfg(not(target_arch = "wasm32"))]

pub mod membrane;

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

use crate::cell::proc::Builder as ProcBuilder;
use crate::host::SwarmCommand;
use crate::system_capnp;

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

        let max_bytes = pry!(params.get()).get_max_bytes() as usize;
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
}

impl ProcessImpl {
    pub(crate) fn new(
        stdin: system_capnp::byte_stream::Client,
        stdout: system_capnp::byte_stream::Client,
        stderr: system_capnp::byte_stream::Client,
        exit_rx: tokio::sync::oneshot::Receiver<i32>,
    ) -> Self {
        Self {
            stdin,
            stdout,
            stderr,
            exit_rx: Arc::new(Mutex::new(Some(exit_rx))),
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
}

pub struct HostImpl {
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
    guard: Option<EpochGuard>,
}

impl HostImpl {
    pub fn new(
        network_state: NetworkState,
        swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
        wasm_debug: bool,
        guard: Option<EpochGuard>,
    ) -> Self {
        Self {
            network_state,
            swarm_cmd_tx,
            wasm_debug,
            guard,
        }
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

    fn connect(
        self: capnp::capability::Rc<Self>,
        params: system_capnp::host::ConnectParams,
        _results: system_capnp::host::ConnectResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.check_epoch());
        let params_reader = pry!(params.get());
        let peer_id_bytes = read_data_result(params_reader.get_peer_id());
        let addrs_bytes: Vec<Vec<u8>> = match params_reader.get_addrs() {
            Ok(list) => (0..list.len())
                .filter_map(|i| list.get(i).ok().map(|d| d.to_vec()))
                .collect(),
            Err(_) => Vec::new(),
        };
        let swarm_cmd_tx = self.swarm_cmd_tx.clone();
        Promise::from_future(async move {
            use libp2p::{Multiaddr, PeerId};

            let peer_id = PeerId::from_bytes(&peer_id_bytes)
                .map_err(|e| capnp::Error::failed(format!("invalid peer ID: {e}")))?;

            let addrs: Vec<Multiaddr> = addrs_bytes
                .iter()
                .filter_map(|bytes| Multiaddr::try_from(bytes.clone()).ok())
                .collect();

            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            swarm_cmd_tx
                .send(SwarmCommand::Connect {
                    peer_id,
                    addrs,
                    reply: reply_tx,
                })
                .await
                .map_err(|_| capnp::Error::failed("swarm channel closed".into()))?;

            reply_rx
                .await
                .map_err(|_| capnp::Error::failed("swarm reply dropped".into()))?
                .map_err(|e| capnp::Error::failed(format!("connect failed: {e}")))?;

            Ok(())
        })
    }

    fn executor(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::host::ExecutorParams,
        mut results: system_capnp::host::ExecutorResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.check_epoch());
        let executor: system_capnp::executor::Client = capnp_rpc::new_client(ExecutorImpl::new(
            self.network_state.clone(),
            self.swarm_cmd_tx.clone(),
            self.wasm_debug,
            self.guard.clone(),
        ));
        results.get().set_executor(executor);
        Promise::ok(())
    }
}

pub struct ExecutorImpl {
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
    guard: Option<EpochGuard>,
}

impl ExecutorImpl {
    pub fn new(
        network_state: NetworkState,
        swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
        wasm_debug: bool,
        guard: Option<EpochGuard>,
    ) -> Self {
        Self {
            network_state,
            swarm_cmd_tx,
            wasm_debug,
            guard,
        }
    }

    fn check_epoch(&self) -> Result<(), capnp::Error> {
        match self.guard {
            Some(ref g) => g.check(),
            None => Ok(()),
        }
    }
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
impl system_capnp::executor::Server for ExecutorImpl {
    fn echo(
        self: capnp::capability::Rc<Self>,
        params: system_capnp::executor::EchoParams,
        mut results: system_capnp::executor::EchoResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.check_epoch());
        let message = match pry!(params.get()).get_message() {
            Ok(s) => s.to_string().unwrap_or_else(|_| String::new()),
            Err(_) => String::new(),
        };
        tracing::debug!(message, "echo");
        let response = format!("Echo: {}", message);
        results.get().set_response(&response);
        Promise::ok(())
    }

    fn run_bytes(
        self: capnp::capability::Rc<Self>,
        params: system_capnp::executor::RunBytesParams,
        mut results: system_capnp::executor::RunBytesResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.check_epoch());
        let params = pry!(params.get());
        let args = read_text_list_result(params.get_args());
        let env = read_text_list_result(params.get_env());
        let wasm = read_data_result(params.get_wasm());
        let wasm_debug = self.wasm_debug;
        let network_state = self.network_state.clone();
        let swarm_cmd_tx = self.swarm_cmd_tx.clone();
        Promise::from_future(async move {
            tracing::info!("run_bytes: starting child process spawn");
            let bytecode = wasm;

            let (host_stderr, guest_stderr) = io::duplex(64 * 1024);
            let (host_stdin, guest_stdin) = io::duplex(64 * 1024);
            let (host_stdout, guest_stdout) = io::duplex(64 * 1024);

            let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();

            let (builder, mut handles) = ProcBuilder::new()
                .with_env(env)
                .with_args(args)
                .with_wasm_debug(wasm_debug)
                .with_bytecode(bytecode)
                .with_stdio(guest_stdin, guest_stdout, guest_stderr)
                .with_data_streams();

            let proc = builder
                .build()
                .await
                .map_err(|err| capnp::Error::failed(err.to_string()))?;

            let (reader, writer) = handles
                .take_host_split()
                .ok_or_else(|| capnp::Error::failed("host stream missing".into()))?;
            let child_rpc_system =
                build_peer_rpc(reader, writer, network_state, swarm_cmd_tx, wasm_debug);

            tokio::task::spawn_local(async move {
                let local = tokio::task::LocalSet::new();
                local.spawn_local(child_rpc_system.map(|_| ()));
                local
                    .run_until(async move {
                        let exit_code = match proc.run().await {
                            Ok(()) => 0,
                            Err(e) => {
                                tracing::error!("run_bytes: child process failed: {}", e);
                                1
                            }
                        };
                        let _ = exit_tx.send(exit_code);
                    })
                    .await;
            });

            let stdin =
                capnp_rpc::new_client(ByteStreamImpl::new(host_stdin, StreamMode::WriteOnly));
            let stdout =
                capnp_rpc::new_client(ByteStreamImpl::new(host_stdout, StreamMode::ReadOnly));
            let stderr =
                capnp_rpc::new_client(ByteStreamImpl::new(host_stderr, StreamMode::ReadOnly));

            let process_client: system_capnp::process::Client =
                capnp_rpc::new_client(ProcessImpl::new(stdin, stdout, stderr, exit_rx));
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
    let host: system_capnp::host::Client =
        capnp_rpc::new_client(HostImpl::new(network_state, swarm_cmd_tx, wasm_debug, None));

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
        let (client_stream, server_stream) = io::duplex(64 * 1024);
        let (client_read, client_write) = io::split(client_stream);
        let (server_read, server_write) = io::split(server_stream);

        let peer_id = vec![1, 2, 3, 4];
        let network_state = NetworkState::from_peer_id(peer_id);
        let (swarm_tx, swarm_rx) = mpsc::channel(16);

        let server_rpc =
            build_peer_rpc(server_read, server_write, network_state, swarm_tx, false);

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

    #[tokio::test]
    async fn test_executor_echo() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (host, _server, _rx) = setup_rpc();

                let executor = host
                    .executor_request()
                    .send()
                    .pipeline
                    .get_executor();

                let mut req = executor.echo_request();
                req.get().set_message("hello world");
                let resp = req.send().promise.await.unwrap();
                let response = resp.get().unwrap().get_response().unwrap().to_str().unwrap();
                assert_eq!(response, "Echo: hello world");
            })
            .await;
    }

    #[tokio::test]
    async fn test_executor_echo_empty_message() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (host, _server, _rx) = setup_rpc();

                let executor = host
                    .executor_request()
                    .send()
                    .pipeline
                    .get_executor();

                let req = executor.echo_request();
                let resp = req.send().promise.await.unwrap();
                let response = resp.get().unwrap().get_response().unwrap().to_str().unwrap();
                assert_eq!(response, "Echo: ");
            })
            .await;
    }

    #[tokio::test]
    async fn test_executor_echo_concurrent() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (host, _server, _rx) = setup_rpc();

                let executor = host
                    .executor_request()
                    .send()
                    .pipeline
                    .get_executor();

                let mut futures = Vec::new();
                for i in 0..5 {
                    let mut req = executor.echo_request();
                    req.get().set_message(&format!("msg-{i}"));
                    futures.push(req.send().promise);
                }

                for (i, fut) in futures.into_iter().enumerate() {
                    let resp = fut.await.unwrap();
                    let response = resp.get().unwrap().get_response().unwrap().to_str().unwrap();
                    assert_eq!(response, format!("Echo: msg-{i}"));
                }
            })
            .await;
    }

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
}
