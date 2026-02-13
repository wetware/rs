//! Membrane-based RPC bootstrap: epoch-scoped Host + Executor capabilities.
//!
//! Instead of bootstrapping a bare `Host`, the membrane provides an epoch-scoped
//! `Session(WetwareSession)` containing `Host`, `Executor`, and `StatusPoller`.
//! All capabilities fail with `staleEpoch` when the epoch advances.
//!
//! We re-implement the Membrane and StatusPoller servers here against
//! `crate::stem_capnp` (rs's own codegen) because stem's `MembraneServer`
//! implements `stem::stem_capnp::membrane::Server` — a distinct trait from
//! `crate::stem_capnp::membrane::Server` even though both are generated from
//! the same schema. We reuse stem's `Epoch` and `EpochGuard` (plain Rust types).

use capnp::capability::Promise;
use capnp_rpc::pry;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use stem::membrane::{Epoch, EpochGuard};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::host::SwarmCommand;
use crate::membrane_capnp;
use crate::peer_capnp;
use crate::stem_capnp;
use crate::rpc::{
    read_data_result, read_text_list_result, ByteStreamImpl, NetworkState, ProcessImpl, StreamMode,
};

use super::ProcBuilder;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Fill an epoch builder from our crate's stem_capnp (not stem's).
fn fill_epoch(builder: &mut stem_capnp::epoch::Builder<'_>, epoch: &Epoch) {
    builder.set_seq(epoch.seq);
    builder.set_adopted_block(epoch.adopted_block);
    let head_builder = builder.reborrow().init_head(epoch.head.len() as u32);
    head_builder.copy_from_slice(epoch.head.as_slice());
}

// ---------------------------------------------------------------------------
// StatusPoller (local impl against crate::stem_capnp)
// ---------------------------------------------------------------------------

struct StatusPollerImpl {
    guard: EpochGuard,
}

#[allow(refining_impl_trait)]
impl stem_capnp::status_poller::Server for StatusPollerImpl {
    fn poll_status(
        self: capnp::capability::Rc<Self>,
        _: stem_capnp::status_poller::PollStatusParams,
        mut results: stem_capnp::status_poller::PollStatusResults,
    ) -> Promise<(), capnp::Error> {
        if let Err(e) = self.guard.check() {
            return Promise::err(e);
        }
        results.get().set_status(stem_capnp::Status::Ok);
        Promise::ok(())
    }
}

// ---------------------------------------------------------------------------
// WetwareMembraneServer
// ---------------------------------------------------------------------------

/// Membrane server that issues sessions containing epoch-guarded Host + Executor.
pub struct WetwareMembraneServer {
    epoch_rx: watch::Receiver<Epoch>,
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
}

impl WetwareMembraneServer {
    pub fn new(
        epoch_rx: watch::Receiver<Epoch>,
        network_state: NetworkState,
        swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
        wasm_debug: bool,
    ) -> Self {
        Self {
            epoch_rx,
            network_state,
            swarm_cmd_tx,
            wasm_debug,
        }
    }
}

#[allow(refining_impl_trait)]
impl stem_capnp::membrane::Server<membrane_capnp::wetware_session::Owned>
    for WetwareMembraneServer
{
    fn graft(
        self: capnp::capability::Rc<Self>,
        _params: stem_capnp::membrane::GraftParams<membrane_capnp::wetware_session::Owned>,
        mut results: stem_capnp::membrane::GraftResults<membrane_capnp::wetware_session::Owned>,
    ) -> Promise<(), capnp::Error> {
        let epoch = self.epoch_rx.borrow().clone();
        let guard = EpochGuard {
            issued_seq: epoch.seq,
            receiver: self.epoch_rx.clone(),
        };

        let mut session = results.get().init_session();

        // Fill issued epoch
        fill_epoch(&mut session.reborrow().init_issued_epoch(), &epoch);

        // Set status poller
        let poller: stem_capnp::status_poller::Client =
            capnp_rpc::new_client(StatusPollerImpl {
                guard: guard.clone(),
            });
        session.reborrow().set_status_poller(poller);

        // Fill extension (WetwareSession: Host + Executor)
        let mut ext = session.reborrow().init_extension();
        let host: peer_capnp::host::Client = capnp_rpc::new_client(EpochGuardedHost::new(
            self.network_state.clone(),
            self.swarm_cmd_tx.clone(),
            self.wasm_debug,
            guard.clone(),
        ));
        ext.set_host(host);

        let executor: peer_capnp::executor::Client =
            capnp_rpc::new_client(EpochGuardedExecutor::new(
                self.network_state.clone(),
                self.swarm_cmd_tx.clone(),
                self.wasm_debug,
                guard,
            ));
        ext.set_executor(executor);

        Promise::ok(())
    }
}

// ---------------------------------------------------------------------------
// EpochGuardedHost
// ---------------------------------------------------------------------------

/// Host capability that checks epoch validity before each RPC call.
pub struct EpochGuardedHost {
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
    guard: EpochGuard,
}

impl EpochGuardedHost {
    pub fn new(
        network_state: NetworkState,
        swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
        wasm_debug: bool,
        guard: EpochGuard,
    ) -> Self {
        Self {
            network_state,
            swarm_cmd_tx,
            wasm_debug,
            guard,
        }
    }
}

#[allow(refining_impl_trait)]
impl peer_capnp::host::Server for EpochGuardedHost {
    fn id(
        self: capnp::capability::Rc<Self>,
        _params: peer_capnp::host::IdParams,
        mut results: peer_capnp::host::IdResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let network_state = self.network_state.clone();
        Promise::from_future(async move {
            let snapshot = network_state.snapshot().await;
            results.get().set_peer_id(&snapshot.local_peer_id);
            Ok(())
        })
    }

    fn addrs(
        self: capnp::capability::Rc<Self>,
        _params: peer_capnp::host::AddrsParams,
        mut results: peer_capnp::host::AddrsResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
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
        _params: peer_capnp::host::PeersParams,
        mut results: peer_capnp::host::PeersResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
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
        params: peer_capnp::host::ConnectParams,
        _results: peer_capnp::host::ConnectResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
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
        _params: peer_capnp::host::ExecutorParams,
        mut results: peer_capnp::host::ExecutorResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let executor: peer_capnp::executor::Client =
            capnp_rpc::new_client(EpochGuardedExecutor::new(
                self.network_state.clone(),
                self.swarm_cmd_tx.clone(),
                self.wasm_debug,
                self.guard.clone(),
            ));
        results.get().set_executor(executor);
        Promise::ok(())
    }
}

// ---------------------------------------------------------------------------
// EpochGuardedExecutor
// ---------------------------------------------------------------------------

/// Executor capability that checks epoch validity before each RPC call.
pub struct EpochGuardedExecutor {
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
    guard: EpochGuard,
}

impl EpochGuardedExecutor {
    pub fn new(
        network_state: NetworkState,
        swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
        wasm_debug: bool,
        guard: EpochGuard,
    ) -> Self {
        Self {
            network_state,
            swarm_cmd_tx,
            wasm_debug,
            guard,
        }
    }
}

#[allow(refining_impl_trait)]
impl peer_capnp::executor::Server for EpochGuardedExecutor {
    fn echo(
        self: capnp::capability::Rc<Self>,
        params: peer_capnp::executor::EchoParams,
        mut results: peer_capnp::executor::EchoResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let message = match pry!(params.get()).get_message() {
            Ok(s) => s.to_string().unwrap_or_else(|_| String::new()),
            Err(_) => String::new(),
        };
        tracing::info!("EpochGuardedExecutor: echo request: {}", message);
        Promise::from_future(async move {
            let response = format!("Echo: {}", message);
            results.get().set_response(&response);
            Ok(())
        })
    }

    fn run_bytes(
        self: capnp::capability::Rc<Self>,
        params: peer_capnp::executor::RunBytesParams,
        mut results: peer_capnp::executor::RunBytesResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let params = pry!(params.get());
        let args = read_text_list_result(params.get_args());
        let env = read_text_list_result(params.get_env());
        let wasm = read_data_result(params.get_wasm());
        let wasm_debug = self.wasm_debug;
        let network_state = self.network_state.clone();
        let swarm_cmd_tx = self.swarm_cmd_tx.clone();
        Promise::from_future(async move {
            use tokio::io;

            tracing::info!("EpochGuardedExecutor: run_bytes starting");
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
            let child_rpc_system = super::build_peer_rpc(
                reader,
                writer,
                network_state,
                swarm_cmd_tx,
                wasm_debug,
            );

            tokio::task::spawn_local(async move {
                let local = tokio::task::LocalSet::new();
                local.spawn_local(futures::FutureExt::map(child_rpc_system, |_| ()));
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

            let process_client: peer_capnp::process::Client =
                capnp_rpc::new_client(ProcessImpl::new(stdin, stdout, stderr, exit_rx));
            results.get().set_process(process_client);

            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// build_membrane_rpc — bootstrap Membrane instead of Host
// ---------------------------------------------------------------------------

/// Build an RPC system that bootstraps a `Membrane(WetwareSession)` instead of
/// a bare `Host`. The membrane provides epoch-scoped sessions containing
/// `Host`, `Executor`, and `StatusPoller`.
pub fn build_membrane_rpc<R, W>(
    reader: R,
    writer: W,
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
    epoch_rx: watch::Receiver<Epoch>,
) -> RpcSystem<Side>
where
    R: AsyncRead + Unpin + 'static,
    W: AsyncWrite + Unpin + 'static,
{
    let membrane: stem_capnp::membrane::Client<membrane_capnp::wetware_session::Owned> =
        capnp_rpc::new_client(WetwareMembraneServer::new(
            epoch_rx,
            network_state,
            swarm_cmd_tx,
            wasm_debug,
        ));

    let rpc_network = VatNetwork::new(
        reader.compat(),
        writer.compat_write(),
        Side::Server,
        Default::default(),
    );
    RpcSystem::new(Box::new(rpc_network), Some(membrane.client))
}
