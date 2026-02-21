//! Membrane-based RPC bootstrap: epoch-scoped Host + Executor capabilities.
//!
//! Instead of bootstrapping a bare `Host`, the membrane provides an epoch-scoped
//! `Session` containing `Host`, `Executor`, and `StatusPoller`.
//! All capabilities fail with `staleEpoch` when the epoch advances.
//!
//! The `membrane` crate owns the Membrane server, StatusPoller, and epoch machinery.
//! This module provides the `SessionBuilder` impl that injects wetware-specific
//! capabilities (Host + Executor + IPFS) into the session, plus the epoch-guarded
//! wrappers for those capabilities.

use capnp::capability::Promise;
use capnp_rpc::pry;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use membrane::{Epoch, EpochGuard, MembraneServer, SessionBuilder};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::host::SwarmCommand;
use crate::ipfs;
use crate::ipfs_capnp;
use crate::system_capnp;
use crate::rpc::{
    read_data_result, read_text_list_result, ByteStreamImpl, NetworkState, ProcessImpl, StreamMode,
};

use super::ProcBuilder;

// ---------------------------------------------------------------------------
// HostSessionBuilder — SessionBuilder for the concrete stem Session
// ---------------------------------------------------------------------------

/// Fills the Session with epoch-guarded Host, Executor, and IPFS Client.
pub struct HostSessionBuilder {
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
    ipfs_client: ipfs::HttpClient,
}

impl HostSessionBuilder {
    pub fn new(
        network_state: NetworkState,
        swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
        wasm_debug: bool,
        ipfs_client: ipfs::HttpClient,
    ) -> Self {
        Self {
            network_state,
            swarm_cmd_tx,
            wasm_debug,
            ipfs_client,
        }
    }
}

impl SessionBuilder for HostSessionBuilder {
    fn build(
        &self,
        guard: &EpochGuard,
        mut builder: membrane::stem_capnp::session::Builder<'_>,
    ) -> Result<(), capnp::Error> {
        let host: system_capnp::host::Client = capnp_rpc::new_client(EpochGuardedHost::new(
            self.network_state.clone(),
            self.swarm_cmd_tx.clone(),
            self.wasm_debug,
            guard.clone(),
        ));
        builder.set_host(host);

        let executor: system_capnp::executor::Client =
            capnp_rpc::new_client(EpochGuardedExecutor::new(
                self.network_state.clone(),
                self.swarm_cmd_tx.clone(),
                self.wasm_debug,
                guard.clone(),
            ));
        builder.set_executor(executor);

        let ipfs_client: ipfs_capnp::client::Client =
            capnp_rpc::new_client(EpochGuardedIpfsClient::new(
                self.ipfs_client.clone(),
                guard.clone(),
            ));
        builder.set_ipfs(ipfs_client);

        Ok(())
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
impl system_capnp::host::Server for EpochGuardedHost {
    fn id(
        self: capnp::capability::Rc<Self>,
        _params: system_capnp::host::IdParams,
        mut results: system_capnp::host::IdResults,
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
        _params: system_capnp::host::AddrsParams,
        mut results: system_capnp::host::AddrsResults,
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
        _params: system_capnp::host::PeersParams,
        mut results: system_capnp::host::PeersResults,
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
        params: system_capnp::host::ConnectParams,
        _results: system_capnp::host::ConnectResults,
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
        _params: system_capnp::host::ExecutorParams,
        mut results: system_capnp::host::ExecutorResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let executor: system_capnp::executor::Client =
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
impl system_capnp::executor::Server for EpochGuardedExecutor {
    fn echo(
        self: capnp::capability::Rc<Self>,
        params: system_capnp::executor::EchoParams,
        mut results: system_capnp::executor::EchoResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
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

            let process_client: system_capnp::process::Client =
                capnp_rpc::new_client(ProcessImpl::new(stdin, stdout, stderr, exit_rx));
            results.get().set_process(process_client);

            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// EpochGuardedIpfsClient — CoreAPI-style IPFS client
// ---------------------------------------------------------------------------

/// IPFS Client capability that checks epoch validity and delegates to sub-APIs.
struct EpochGuardedIpfsClient {
    ipfs_client: ipfs::HttpClient,
    guard: EpochGuard,
}

impl EpochGuardedIpfsClient {
    fn new(ipfs_client: ipfs::HttpClient, guard: EpochGuard) -> Self {
        Self { ipfs_client, guard }
    }
}

#[allow(refining_impl_trait)]
impl ipfs_capnp::client::Server for EpochGuardedIpfsClient {
    fn unixfs(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::UnixfsParams,
        mut results: ipfs_capnp::client::UnixfsResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let api: ipfs_capnp::unix_f_s::Client =
            capnp_rpc::new_client(EpochGuardedUnixFS::new(
                self.ipfs_client.clone(),
                self.guard.clone(),
            ));
        results.get().set_api(api);
        Promise::ok(())
    }

    fn block(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::BlockParams,
        _results: ipfs_capnp::client::BlockResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented("block API not yet implemented".into()))
    }

    fn dag(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::DagParams,
        _results: ipfs_capnp::client::DagResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented("dag API not yet implemented".into()))
    }

    fn name(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::NameParams,
        _results: ipfs_capnp::client::NameResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented("name API not yet implemented".into()))
    }

    fn key(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::KeyParams,
        _results: ipfs_capnp::client::KeyResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented("key API not yet implemented".into()))
    }

    fn pin(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::PinParams,
        _results: ipfs_capnp::client::PinResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented("pin API not yet implemented".into()))
    }

    fn object(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::ObjectParams,
        _results: ipfs_capnp::client::ObjectResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented("object API not yet implemented".into()))
    }

    fn swarm(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::SwarmParams,
        _results: ipfs_capnp::client::SwarmResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented("swarm API not yet implemented".into()))
    }

    fn pub_sub(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::PubSubParams,
        _results: ipfs_capnp::client::PubSubResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented("pubsub API not yet implemented".into()))
    }

    fn routing(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::RoutingParams,
        _results: ipfs_capnp::client::RoutingResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented("routing API not yet implemented".into()))
    }

    fn resolve_path(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::ResolvePathParams,
        _results: ipfs_capnp::client::ResolvePathResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented("resolvePath not yet implemented".into()))
    }

    fn resolve_node(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::ResolveNodeParams,
        _results: ipfs_capnp::client::ResolveNodeResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented("resolveNode not yet implemented".into()))
    }
}

// ---------------------------------------------------------------------------
// EpochGuardedUnixFS
// ---------------------------------------------------------------------------

/// UnixFS capability backed by Kubo HTTP API.
struct EpochGuardedUnixFS {
    ipfs_client: ipfs::HttpClient,
    guard: EpochGuard,
}

impl EpochGuardedUnixFS {
    fn new(ipfs_client: ipfs::HttpClient, guard: EpochGuard) -> Self {
        Self { ipfs_client, guard }
    }
}

#[allow(refining_impl_trait)]
impl ipfs_capnp::unix_f_s::Server for EpochGuardedUnixFS {
    fn cat(
        self: capnp::capability::Rc<Self>,
        params: ipfs_capnp::unix_f_s::CatParams,
        mut results: ipfs_capnp::unix_f_s::CatResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let path = pry!(pry!(params.get()).get_path()).to_string().unwrap_or_default();
        let client = self.ipfs_client.clone();
        Promise::from_future(async move {
            let data = client
                .unixfs()
                .get(&path)
                .await
                .map_err(|e| capnp::Error::failed(format!("ipfs cat failed: {e}")))?;
            results.get().set_data(&data);
            Ok(())
        })
    }

    fn ls(
        self: capnp::capability::Rc<Self>,
        params: ipfs_capnp::unix_f_s::LsParams,
        mut results: ipfs_capnp::unix_f_s::LsResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let path = pry!(pry!(params.get()).get_path()).to_string().unwrap_or_default();
        let client = self.ipfs_client.clone();
        Promise::from_future(async move {
            let entries = client
                .ls(&path)
                .await
                .map_err(|e| capnp::Error::failed(format!("ipfs ls failed: {e}")))?;
            let mut list = results.get().init_entries(entries.len() as u32);
            for (i, entry) in entries.iter().enumerate() {
                let mut builder = list.reborrow().get(i as u32);
                builder.set_name(&entry.name);
                builder.set_size(entry.size);
                builder.set_type(if entry.entry_type == 1 {
                    ipfs_capnp::unix_f_s::entry::EntryType::Directory
                } else {
                    ipfs_capnp::unix_f_s::entry::EntryType::File
                });
                builder.set_cid(entry.hash.as_bytes());
            }
            Ok(())
        })
    }
}

// ---------------------------------------------------------------------------
// build_membrane_rpc — bootstrap Membrane instead of Host
// ---------------------------------------------------------------------------

/// The Membrane type exported by WASM guests back to the host.
///
/// When a guest calls `wetware_guest::serve(my_membrane, ...)`, the host
/// captures it here.  The host can then re-serve it to external peers,
/// allowing the guest to attenuate or enrich the capability surface it exposes.
pub type GuestMembrane = membrane::stem_capnp::membrane::Client;

/// Build an RPC system that bootstraps a `Membrane` instead of a bare `Host`.
///
/// The membrane provides epoch-scoped sessions containing `Host`, `Executor`,
/// and `IPFS Client`.
///
/// Returns both the RPC system and the guest's exported [`GuestMembrane`], if
/// the guest called `wetware_guest::serve()`.  If the guest called `run()`
/// instead, the returned capability is broken and attempts to use it will fail.
pub fn build_membrane_rpc<R, W>(
    reader: R,
    writer: W,
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
    epoch_rx: watch::Receiver<Epoch>,
    ipfs_client: ipfs::HttpClient,
) -> (RpcSystem<Side>, GuestMembrane)
where
    R: AsyncRead + Unpin + 'static,
    W: AsyncWrite + Unpin + 'static,
{
    let sess_builder =
        HostSessionBuilder::new(network_state, swarm_cmd_tx, wasm_debug, ipfs_client);
    let membrane: GuestMembrane =
        capnp_rpc::new_client(MembraneServer::new(epoch_rx, sess_builder));

    let rpc_network = VatNetwork::new(
        reader.compat(),
        writer.compat_write(),
        Side::Server,
        Default::default(),
    );
    let mut rpc_system = RpcSystem::new(Box::new(rpc_network), Some(membrane.client));
    let guest_membrane: GuestMembrane = rpc_system.bootstrap(Side::Client);
    (rpc_system, guest_membrane)
}
