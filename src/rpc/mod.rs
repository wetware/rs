//! Cap'n Proto RPC server for host-provided capabilities.
#![cfg(not(target_arch = "wasm32"))]

use std::sync::Arc;

use capnp::capability::Promise;
use capnp_rpc::pry;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use futures::FutureExt;
use tokio::io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, RwLock};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::cell::proc::Builder as ProcBuilder;
use crate::peer_capnp;

#[derive(Clone, Debug)]
pub struct PeerInfo {
    pub peer_id: Vec<u8>,
    pub addrs: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct NetworkSnapshot {
    pub local_peer_id: Vec<u8>,
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

    pub async fn set_known_peers(&self, peers: Vec<PeerInfo>) {
        let mut guard = self.inner.write().await;
        guard.known_peers = peers;
    }
}

#[derive(Clone, Copy, Debug)]
enum StreamMode {
    ReadOnly,
    WriteOnly,
}

pub struct ByteStreamImpl {
    stream: Arc<Mutex<io::DuplexStream>>,
    mode: StreamMode,
}

impl ByteStreamImpl {
    fn new(stream: io::DuplexStream, mode: StreamMode) -> Self {
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

impl peer_capnp::byte_stream::Server for ByteStreamImpl {
    fn read(
        self: capnp::capability::Rc<Self>,
        params: peer_capnp::byte_stream::ReadParams,
        mut results: peer_capnp::byte_stream::ReadResults,
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
        params: peer_capnp::byte_stream::WriteParams,
        _results: peer_capnp::byte_stream::WriteResults,
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
        _params: peer_capnp::byte_stream::CloseParams,
        _results: peer_capnp::byte_stream::CloseResults,
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
    stdin: peer_capnp::byte_stream::Client,
    stdout: peer_capnp::byte_stream::Client,
    stderr: peer_capnp::byte_stream::Client,
    exit_rx: Arc<Mutex<Option<tokio::sync::oneshot::Receiver<i32>>>>,
}

impl ProcessImpl {
    fn new(
        stdin: peer_capnp::byte_stream::Client,
        stdout: peer_capnp::byte_stream::Client,
        stderr: peer_capnp::byte_stream::Client,
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

impl peer_capnp::process::Server for ProcessImpl {
    fn stdin(
        self: capnp::capability::Rc<Self>,
        _params: peer_capnp::process::StdinParams,
        mut results: peer_capnp::process::StdinResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        results.get().set_stream(self.stdin.clone());
        Promise::ok(())
    }

    fn stdout(
        self: capnp::capability::Rc<Self>,
        _params: peer_capnp::process::StdoutParams,
        mut results: peer_capnp::process::StdoutResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        results.get().set_stream(self.stdout.clone());
        Promise::ok(())
    }

    fn stderr(
        self: capnp::capability::Rc<Self>,
        _params: peer_capnp::process::StderrParams,
        mut results: peer_capnp::process::StderrResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        results.get().set_stream(self.stderr.clone());
        Promise::ok(())
    }

    fn wait(
        self: capnp::capability::Rc<Self>,
        _params: peer_capnp::process::WaitParams,
        mut results: peer_capnp::process::WaitResults,
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

pub struct ExecutorImpl {
    wasm_debug: bool,
}

impl ExecutorImpl {
    pub fn new(wasm_debug: bool) -> Self {
        Self { wasm_debug }
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

impl peer_capnp::executor::Server for ExecutorImpl {
    fn echo(
        self: capnp::capability::Rc<Self>,
        params: peer_capnp::executor::EchoParams,
        mut results: peer_capnp::executor::EchoResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let message = match pry!(params.get()).get_message() {
            Ok(s) => s.to_string().unwrap_or_else(|_| String::new()),
            Err(_) => String::new(),
        };
        tracing::info!("Host: Received echo request: {}", message);
        Promise::from_future(async move {
            // Echo back the message with a prefix
            let response = format!("Echo: {}", message);
            tracing::info!("Host: Sending echo response: {}", response);
            results.get().set_response(&response);
            Ok(())
        })
    }

    fn run_bytes(
        self: capnp::capability::Rc<Self>,
        params: peer_capnp::executor::RunBytesParams,
        mut results: peer_capnp::executor::RunBytesResults,
    ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
        let params = pry!(params.get());
        let args = read_text_list_result(params.get_args());
        let env = read_text_list_result(params.get_env());
        let wasm = read_data_result(params.get_wasm());
        let wasm_debug = self.wasm_debug;
        Promise::from_future(async move {
            tracing::info!("run_bytes: starting child process spawn");
            let bytecode = wasm;

            // Create stderr duplex for process interface
            let (host_stderr, guest_stderr) = io::duplex(64 * 1024);

            // Create stdin/stdout duplex for guest's logging (not RPC)
            let (_, guest_stdin) = io::duplex(64);
            let (guest_stdout, _) = io::duplex(64);

            let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();

            // Build guest with wetware streams enabled for RPC
            let (builder, mut handles) = ProcBuilder::new()
                .with_env(env)
                .with_args(args)
                .with_wasm_debug(wasm_debug)
                .with_bytecode(bytecode)
                .with_stdio(guest_stdin, guest_stdout, guest_stderr)
                .with_data_streams();

            tracing::info!("run_bytes: building child process");
            let proc = builder
                .build()
                .await
                .map_err(|err| capnp::Error::failed(err.to_string()))?;
            tracing::info!("run_bytes: child process built");

            let (reader, writer) = handles
                .take_host_split()
                .ok_or_else(|| capnp::Error::failed("host stream missing".into()))?;
            let child_rpc_system = build_peer_rpc(reader, writer, wasm_debug);

            // Spawn both the child process and its RPC server in a LocalSet
            tracing::info!("run_bytes: spawning child task");
            tokio::task::spawn_local(async move {
                tracing::info!("run_bytes: child task started");
                let local = tokio::task::LocalSet::new();

                // Spawn RPC system
                local.spawn_local(child_rpc_system.map(|_| ()));

                // Run child process and wait for exit
                local
                    .run_until(async move {
                        tracing::info!("run_bytes: running child process");
                        let exit_code = match proc.run().await {
                            Ok(()) => {
                                tracing::info!("run_bytes: child process exited successfully");
                                0
                            }
                            Err(e) => {
                                tracing::error!("run_bytes: child process failed: {}", e);
                                1
                            }
                        };
                        let _ = exit_tx.send(exit_code);
                    })
                    .await;
                tracing::info!("run_bytes: child task completed");
            });
            tracing::info!("run_bytes: returning process client");

            // Create dummy ByteStream capabilities for Process interface
            // The child is using wetware streams for RPC, so these won't be used for normal I/O
            let (dummy_stdin, _) = io::duplex(64);
            let (_, dummy_stdout) = io::duplex(64);
            let stdin =
                capnp_rpc::new_client(ByteStreamImpl::new(dummy_stdin, StreamMode::WriteOnly));
            let stdout =
                capnp_rpc::new_client(ByteStreamImpl::new(dummy_stdout, StreamMode::ReadOnly));
            let stderr =
                capnp_rpc::new_client(ByteStreamImpl::new(host_stderr, StreamMode::ReadOnly));

            let process_client: peer_capnp::process::Client =
                capnp_rpc::new_client(ProcessImpl::new(stdin, stdout, stderr, exit_rx));
            results.get().set_process(process_client);

            Ok(())
        })
    }
}

pub fn build_peer_rpc<R, W>(reader: R, writer: W, wasm_debug: bool) -> RpcSystem<Side>
where
    R: AsyncRead + Unpin + 'static,
    W: AsyncWrite + Unpin + 'static,
{
    let executor: peer_capnp::executor::Client =
        capnp_rpc::new_client(ExecutorImpl::new(wasm_debug));

    let rpc_network = VatNetwork::new(
        reader.compat(),
        writer.compat_write(),
        Side::Server,
        Default::default(),
    );
    RpcSystem::new(Box::new(rpc_network), Some(executor.client))
}
