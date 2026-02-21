//! Membrane-based RPC bootstrap: epoch-scoped Host + Executor + node identity capabilities.
//!
//! Instead of bootstrapping a bare `Host`, the membrane provides an epoch-scoped
//! `Session` containing `Host`, `Executor`, `IPFS Client`, and a node `identity` signer.
//! All capabilities fail with `staleEpoch` when the epoch advances.
//!
//! The `membrane` crate owns the Membrane server and epoch machinery.
//! This module provides the `SessionBuilder` impl that injects wetware-specific
//! capabilities into the session, plus the epoch-guarded IPFS and identity wrappers.

use std::sync::Arc;

use capnp::capability::Promise;
use capnp_rpc::pry;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use k256::ecdsa::SigningKey;
use libp2p_core::SignedEnvelope;
use membrane::{stem_capnp, Epoch, EpochGuard, MembraneServer, SessionBuilder};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::host::SwarmCommand;
use crate::ipfs;
use crate::ipfs_capnp;
use crate::system_capnp;

use super::NetworkState;

// ---------------------------------------------------------------------------
// EpochGuardedKernelSigner — host-side secp256k1 signer for the kernel
// ---------------------------------------------------------------------------

/// Host-side secp256k1 signer provided to the kernel through the Session.
///
/// **Security invariant**: the identity secret key never leaves the host process.
/// The key is never copied into WASM memory or transmitted over the RPC channel.
/// The kernel receives only a capability reference; all signing happens host-side,
/// and the kernel's WASM sandbox cannot observe or extract the private key bytes.
///
/// Epoch-guarded so signing fails with `staleEpoch` after an epoch advance.
///
/// Returns a [`libp2p_core::SignedEnvelope`] (protobuf-encoded) with:
/// - domain:       `"ww-membrane-graft"`
/// - payload_type: `"/ww/membrane/graft-nonce"`
/// - payload:      nonce as 8-byte big-endian u64
struct EpochGuardedKernelSigner {
    sk: Arc<SigningKey>,
    guard: EpochGuard,
}

impl EpochGuardedKernelSigner {
    fn new(sk: Arc<SigningKey>, guard: EpochGuard) -> Self {
        Self { sk, guard }
    }
}

#[allow(refining_impl_trait)]
impl stem_capnp::signer::Server for EpochGuardedKernelSigner {
    fn sign(
        self: capnp::capability::Rc<Self>,
        params: stem_capnp::signer::SignParams,
        mut results: stem_capnp::signer::SignResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let nonce = pry!(params.get()).get_nonce();
        let keypair =
            pry!(crate::keys::to_libp2p(&self.sk).map_err(|e| capnp::Error::failed(e.to_string())));
        let envelope = pry!(SignedEnvelope::new(
            &keypair,
            "ww-membrane-graft".to_string(),
            b"/ww/membrane/graft-nonce".to_vec(),
            nonce.to_be_bytes().to_vec(),
        )
        .map_err(|e| capnp::Error::failed(e.to_string())));
        results.get().set_sig(&envelope.into_protobuf_encoding());
        Promise::ok(())
    }
}

// ---------------------------------------------------------------------------
// HostSessionBuilder — SessionBuilder for the concrete stem Session
// ---------------------------------------------------------------------------

/// Fills the Session with epoch-guarded Host, Executor, IPFS Client, and Signer.
pub struct HostSessionBuilder {
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
    ipfs_client: ipfs::HttpClient,
    signing_key: Option<Arc<SigningKey>>,
}

impl HostSessionBuilder {
    pub fn new(
        network_state: NetworkState,
        swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
        wasm_debug: bool,
        ipfs_client: ipfs::HttpClient,
        signing_key: Option<Arc<SigningKey>>,
    ) -> Self {
        Self {
            network_state,
            swarm_cmd_tx,
            wasm_debug,
            ipfs_client,
            signing_key,
        }
    }
}

impl SessionBuilder for HostSessionBuilder {
    fn build(
        &self,
        guard: &EpochGuard,
        mut builder: stem_capnp::session::Builder<'_>,
    ) -> Result<(), capnp::Error> {
        let host: system_capnp::host::Client = capnp_rpc::new_client(super::HostImpl::new(
            self.network_state.clone(),
            self.swarm_cmd_tx.clone(),
            self.wasm_debug,
            Some(guard.clone()),
        ));
        builder.set_host(host);

        let executor: system_capnp::executor::Client =
            capnp_rpc::new_client(super::ExecutorImpl::new(
                self.network_state.clone(),
                self.swarm_cmd_tx.clone(),
                self.wasm_debug,
                Some(guard.clone()),
            ));
        builder.set_executor(executor);

        let ipfs_client: ipfs_capnp::client::Client = capnp_rpc::new_client(
            EpochGuardedIpfsClient::new(self.ipfs_client.clone(), guard.clone()),
        );
        builder.set_ipfs(ipfs_client);

        if let Some(sk) = &self.signing_key {
            let signer: stem_capnp::signer::Client =
                capnp_rpc::new_client(EpochGuardedKernelSigner::new(Arc::clone(sk), guard.clone()));
            builder.set_identity(signer);
        }

        Ok(())
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
        let api: ipfs_capnp::unix_f_s::Client = capnp_rpc::new_client(EpochGuardedUnixFS::new(
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
        Promise::err(capnp::Error::unimplemented(
            "block API not yet implemented".into(),
        ))
    }

    fn dag(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::DagParams,
        _results: ipfs_capnp::client::DagResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented(
            "dag API not yet implemented".into(),
        ))
    }

    fn name(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::NameParams,
        _results: ipfs_capnp::client::NameResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented(
            "name API not yet implemented".into(),
        ))
    }

    fn key(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::KeyParams,
        _results: ipfs_capnp::client::KeyResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented(
            "key API not yet implemented".into(),
        ))
    }

    fn pin(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::PinParams,
        _results: ipfs_capnp::client::PinResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented(
            "pin API not yet implemented".into(),
        ))
    }

    fn object(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::ObjectParams,
        _results: ipfs_capnp::client::ObjectResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented(
            "object API not yet implemented".into(),
        ))
    }

    fn swarm(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::SwarmParams,
        _results: ipfs_capnp::client::SwarmResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented(
            "swarm API not yet implemented".into(),
        ))
    }

    fn pub_sub(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::PubSubParams,
        _results: ipfs_capnp::client::PubSubResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented(
            "pubsub API not yet implemented".into(),
        ))
    }

    fn routing(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::RoutingParams,
        _results: ipfs_capnp::client::RoutingResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented(
            "routing API not yet implemented".into(),
        ))
    }

    fn resolve_path(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::ResolvePathParams,
        _results: ipfs_capnp::client::ResolvePathResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented(
            "resolvePath not yet implemented".into(),
        ))
    }

    fn resolve_node(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::client::ResolveNodeParams,
        _results: ipfs_capnp::client::ResolveNodeResults,
    ) -> Promise<(), capnp::Error> {
        Promise::err(capnp::Error::unimplemented(
            "resolveNode not yet implemented".into(),
        ))
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
        let path = pry!(pry!(params.get()).get_path())
            .to_string()
            .unwrap_or_default();
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
        let path = pry!(pry!(params.get()).get_path())
            .to_string()
            .unwrap_or_default();
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
/// When a guest calls `runtime::serve(my_membrane, ...)`, the host
/// captures it here. The host can then re-serve it to external peers,
/// allowing the guest to attenuate or enrich the capability surface it exposes.
pub type GuestMembrane = membrane::stem_capnp::membrane::Client;

/// Build an RPC system that bootstraps a `Membrane` instead of a bare `Host`.
///
/// The membrane provides epoch-scoped sessions containing `Host`, `Executor`,
/// `IPFS Client`, and (when `signing_key` is `Some`) a host-side `Signer`.
///
/// When `signing_key` is `Some`:
/// - The `VerifyingKey` is stored in `MembraneServer` for challenge-response
///   authentication (implemented in issue #57).
/// - An `EpochGuardedKernelSigner` is injected into every session so the kernel
///   can sign messages using the node identity without holding the private key.
///
/// Returns both the RPC system and the guest's exported [`GuestMembrane`], if
/// the guest called `runtime::serve()`. If the guest called `runtime::run()`
/// instead, the returned capability is broken and attempts to use it will fail.
#[allow(clippy::too_many_arguments)]
pub fn build_membrane_rpc<R, W>(
    reader: R,
    writer: W,
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
    epoch_rx: watch::Receiver<Epoch>,
    ipfs_client: ipfs::HttpClient,
    signing_key: Option<Arc<SigningKey>>,
) -> (RpcSystem<Side>, GuestMembrane)
where
    R: AsyncRead + Unpin + 'static,
    W: AsyncWrite + Unpin + 'static,
{
    let verifying_key = signing_key.as_ref().map(|sk| *sk.verifying_key());
    let sess_builder = HostSessionBuilder::new(
        network_state,
        swarm_cmd_tx,
        wasm_debug,
        ipfs_client,
        signing_key,
    );
    let mut membrane_server = MembraneServer::new(epoch_rx, sess_builder);
    if let Some(vk) = verifying_key {
        membrane_server = membrane_server.with_verifying_key(vk);
    }
    let membrane: GuestMembrane = capnp_rpc::new_client(membrane_server);

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
