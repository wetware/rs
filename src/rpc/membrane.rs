//! Membrane-based RPC bootstrap: epoch-scoped Host + Executor + storage capabilities.
//!
//! Instead of bootstrapping a bare `Host`, the membrane's `graft()` returns
//! epoch-scoped `Host`, `Executor`, `PrivateStore`, `PublicStore`, and a node
//! `identity` signer directly as result fields. All capabilities fail with
//! `staleEpoch` when the epoch advances.
//!
//! The `membrane` crate owns the Membrane server and epoch machinery.
//! This module provides the `GraftBuilder` impl that injects wetware-specific
//! capabilities into the graft response, plus the epoch-guarded storage and
//! identity wrappers.

use std::sync::Arc;

use capnp::capability::Promise;
use capnp_rpc::pry;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use k256::ecdsa::SigningKey;
use libp2p::identity::Keypair;
use libp2p_core::SignedEnvelope;
use membrane::{stem_capnp, Epoch, EpochGuard, GraftBuilder, MembraneServer};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::host::SwarmCommand;
use crate::ipfs;
use crate::ipfs_capnp;
use crate::system_capnp;
use auth::SigningDomain;

use super::NetworkState;

// ---------------------------------------------------------------------------
// EpochGuardedIdentity — host-side node identity hub
// ---------------------------------------------------------------------------

/// Host-side node identity hub provided to the kernel through the Session.
///
/// **Security invariant**: the identity secret key never leaves the host process.
/// The key is never copied into WASM memory or transmitted over the RPC channel.
/// The kernel receives only a capability reference; all signing happens host-side,
/// and the kernel's WASM sandbox cannot observe or extract the private key bytes.
///
/// Epoch-guarded: the hub and all domain signers it issues fail with `staleEpoch`
/// once the epoch advances.
///
/// Incoming domain strings are validated against [`system::SigningDomain`].
/// Unknown domains are rejected with an RPC error before any signing occurs.
struct EpochGuardedIdentity {
    /// Pre-converted libp2p keypair (k256 → Keypair done once at session construction).
    keypair: Keypair,
    guard: EpochGuard,
}

impl EpochGuardedIdentity {
    fn new(keypair: Keypair, guard: EpochGuard) -> Self {
        Self { keypair, guard }
    }
}

#[allow(refining_impl_trait)]
impl stem_capnp::identity::Server for EpochGuardedIdentity {
    fn signer(
        self: capnp::capability::Rc<Self>,
        params: stem_capnp::identity::SignerParams,
        mut results: stem_capnp::identity::SignerResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let domain_reader = pry!(pry!(params.get()).get_domain());
        let domain_str = pry!(domain_reader
            .to_str()
            .map_err(|e| capnp::Error::failed(e.to_string())));
        pry!(
            SigningDomain::parse(domain_str).ok_or_else(|| capnp::Error::failed(format!(
                "unknown signing domain: {domain_str:?}"
            )))
        );
        let signer: stem_capnp::signer::Client =
            capnp_rpc::new_client(EpochGuardedMembraneSigner {
                keypair: self.keypair.clone(),
                guard: self.guard.clone(),
            });
        results.get().set_signer(signer);
        Promise::ok(())
    }
}

// ---------------------------------------------------------------------------
// EpochGuardedMembraneSigner — membrane/graft signer
// ---------------------------------------------------------------------------

/// Signs graft nonces for the `Membrane::graft` challenge-response protocol.
///
/// Constructed by [`EpochGuardedIdentity::signer()`] after validating that the
/// requested domain is [`SigningDomain::MembraneGraft`].  Domain string and
/// payload type are fixed constants; callers supply only the nonce.
/// Returns a protobuf-encoded `libp2p_core::SignedEnvelope`.
struct EpochGuardedMembraneSigner {
    keypair: Keypair,
    guard: EpochGuard,
}

#[allow(refining_impl_trait)]
impl stem_capnp::signer::Server for EpochGuardedMembraneSigner {
    fn sign(
        self: capnp::capability::Rc<Self>,
        params: stem_capnp::signer::SignParams,
        mut results: stem_capnp::signer::SignResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let nonce = pry!(params.get()).get_nonce();
        let envelope = pry!(SignedEnvelope::new(
            &self.keypair,
            SigningDomain::MembraneGraft.as_str().to_string(),
            SigningDomain::MembraneGraft.payload_type().to_vec(),
            nonce.to_be_bytes().to_vec(),
        )
        .map_err(|e| capnp::Error::failed(e.to_string())));
        results.get().set_sig(&envelope.into_protobuf_encoding());
        Promise::ok(())
    }
}

// ---------------------------------------------------------------------------
// HostGraftBuilder — GraftBuilder for the concrete stem graft response
// ---------------------------------------------------------------------------

/// Fills the graft response with epoch-guarded Host, Executor, PrivateStore,
/// PublicStore, and node identity.
pub struct HostGraftBuilder {
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
    #[allow(dead_code)]
    ipfs_client: ipfs::HttpClient,
    #[allow(dead_code)]
    peer_id: Vec<u8>,
    signing_key: Option<Arc<SigningKey>>,
}

impl HostGraftBuilder {
    pub fn new(
        network_state: NetworkState,
        swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
        wasm_debug: bool,
        ipfs_client: ipfs::HttpClient,
        peer_id: Vec<u8>,
        signing_key: Option<Arc<SigningKey>>,
    ) -> Self {
        Self {
            network_state,
            swarm_cmd_tx,
            wasm_debug,
            ipfs_client,
            peer_id,
            signing_key,
        }
    }
}

impl GraftBuilder for HostGraftBuilder {
    fn build(
        &self,
        guard: &EpochGuard,
        mut builder: stem_capnp::membrane::graft_results::Builder<'_>,
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

        let private: ipfs_capnp::private_store::Client =
            capnp_rpc::new_client(EpochGuardedTempUnixFS::new(guard.clone()));
        builder.set_private(private);

        let public: ipfs_capnp::public_store::Client =
            capnp_rpc::new_client(EpochGuardedPublicUnixFS::new(guard.clone()));
        builder.set_public(public);

        if let Some(sk) = &self.signing_key {
            let keypair =
                crate::keys::to_libp2p(sk).map_err(|e| capnp::Error::failed(e.to_string()))?;
            let identity: stem_capnp::identity::Client =
                capnp_rpc::new_client(EpochGuardedIdentity::new(keypair, guard.clone()));
            builder.set_identity(identity);
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// EpochGuardedTempUnixFS — private (in-memory) store (stub)
// ---------------------------------------------------------------------------

/// Private UnixFS capability backed by an in-memory map.
///
/// CIDs are computed locally (sha2-256 hash for integrity, not published).
/// Content is destroyed when the capability is dropped (epoch advance or
/// process exit).
struct EpochGuardedTempUnixFS {
    guard: EpochGuard,
}

impl EpochGuardedTempUnixFS {
    fn new(guard: EpochGuard) -> Self {
        Self { guard }
    }
}

#[allow(refining_impl_trait)]
impl ipfs_capnp::unix_f_s::Server for EpochGuardedTempUnixFS {
    fn add(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::unix_f_s::AddParams,
        _results: ipfs_capnp::unix_f_s::AddResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        Promise::err(capnp::Error::unimplemented(
            "private add not yet implemented".into(),
        ))
    }

    fn get(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::unix_f_s::GetParams,
        _results: ipfs_capnp::unix_f_s::GetResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        Promise::err(capnp::Error::unimplemented(
            "private get not yet implemented".into(),
        ))
    }

    fn ls(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::unix_f_s::LsParams,
        _results: ipfs_capnp::unix_f_s::LsResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        Promise::err(capnp::Error::unimplemented(
            "private ls not yet implemented".into(),
        ))
    }
}

impl ipfs_capnp::private_store::Server for EpochGuardedTempUnixFS {}

// ---------------------------------------------------------------------------
// EpochGuardedPublicUnixFS — public (Kubo-backed) store (stub)
// ---------------------------------------------------------------------------

/// Public UnixFS capability backed by the Kubo HTTP API.
///
/// Content written here is content-addressed, pinned, and network-retrievable.
/// Scoped per-agent to `/ww/<peer-id>/public/` on the shared Kubo node.
struct EpochGuardedPublicUnixFS {
    guard: EpochGuard,
}

impl EpochGuardedPublicUnixFS {
    fn new(guard: EpochGuard) -> Self {
        Self { guard }
    }
}

#[allow(refining_impl_trait)]
impl ipfs_capnp::unix_f_s::Server for EpochGuardedPublicUnixFS {
    fn add(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::unix_f_s::AddParams,
        _results: ipfs_capnp::unix_f_s::AddResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        Promise::err(capnp::Error::unimplemented(
            "public add not yet implemented".into(),
        ))
    }

    fn get(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::unix_f_s::GetParams,
        _results: ipfs_capnp::unix_f_s::GetResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        Promise::err(capnp::Error::unimplemented(
            "public get not yet implemented".into(),
        ))
    }

    fn ls(
        self: capnp::capability::Rc<Self>,
        _params: ipfs_capnp::unix_f_s::LsParams,
        _results: ipfs_capnp::unix_f_s::LsResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        Promise::err(capnp::Error::unimplemented(
            "public ls not yet implemented".into(),
        ))
    }
}

impl ipfs_capnp::public_store::Server for EpochGuardedPublicUnixFS {}

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
/// `PrivateStore`, `PublicStore`, and (when `signing_key` is `Some`) a host-side
/// node identity signer.
///
/// When `signing_key` is `Some`:
/// - The `VerifyingKey` is stored in `MembraneServer` for graft challenge-response.
/// - An [`EpochGuardedIdentity`] hub is injected into every session so the kernel
///   can request domain-scoped signers without holding the private key.
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
    peer_id: Vec<u8>,
    signing_key: Option<Arc<SigningKey>>,
) -> (RpcSystem<Side>, GuestMembrane)
where
    R: AsyncRead + Unpin + 'static,
    W: AsyncWrite + Unpin + 'static,
{
    let sess_builder = HostGraftBuilder::new(
        network_state,
        swarm_cmd_tx,
        wasm_debug,
        ipfs_client,
        peer_id,
        signing_key,
    );
    // The local kernel is a trusted process — no challenge-response auth needed.
    // Auth applies to external peers connecting via libp2p to the guest's exported membrane.
    let membrane_server = MembraneServer::new(epoch_rx, sess_builder);
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
