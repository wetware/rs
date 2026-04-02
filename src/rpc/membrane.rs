//! Membrane-based RPC bootstrap: epoch-scoped Host + Executor + node identity capabilities.
//!
//! Instead of bootstrapping a bare `Host`, the membrane's `graft()` returns
//! epoch-scoped `Host`, `Executor`, `IPFS Client`, and a node `identity` signer
//! directly as result fields. All capabilities fail with `staleEpoch` when the
//! epoch advances.
//!
//! The `membrane` crate owns the Membrane server and epoch machinery.
//! This module provides the `GraftBuilder` impl that injects wetware-specific
//! capabilities into the graft response, plus the epoch-guarded IPFS and identity wrappers.

use std::sync::Arc;

use capnp::capability::Promise;
use capnp_rpc::pry;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use ed25519_dalek::SigningKey;
use libp2p::identity::Keypair;
use libp2p_core::SignedEnvelope;
use membrane::{stem_capnp, Epoch, EpochGuard, GraftBuilder, MembraneServer};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, watch};
use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

use crate::host::SwarmCommand;
use crate::http_capnp;
use crate::ipfs;
use crate::ipfs_capnp;
use crate::routing_capnp;
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
/// Incoming domain strings are accepted if non-empty — the guest chooses
/// the signing context. Empty domains are rejected with an RPC error.
struct EpochGuardedIdentity {
    /// Pre-converted libp2p keypair (Ed25519 → Keypair done once at session construction).
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
        if domain_str.is_empty() {
            return Promise::err(capnp::Error::failed(
                "signing domain must not be empty".into(),
            ));
        }
        // Accept any non-empty domain — the guest chooses the signing context.
        // The domain string is opaque to the host; it just constructs the
        // domain-separated signing buffer using whatever the guest requested.
        let domain = SigningDomain::new(domain_str);
        let signer: stem_capnp::signer::Client = capnp_rpc::new_client(EpochGuardedDomainSigner {
            domain,
            keypair: self.keypair.clone(),
            guard: self.guard.clone(),
        });
        results.get().set_signer(signer);
        Promise::ok(())
    }
}

// ---------------------------------------------------------------------------
// EpochGuardedDomainSigner — domain-scoped signer
// ---------------------------------------------------------------------------

/// Signs nonces for a specific [`SigningDomain`] (e.g. `terminal_membrane`, `membrane_graft`).
///
/// Constructed by [`EpochGuardedIdentity::signer()`] after validating the
/// requested domain.  Returns a protobuf-encoded `libp2p_core::SignedEnvelope`.
struct EpochGuardedDomainSigner {
    domain: SigningDomain,
    keypair: Keypair,
    guard: EpochGuard,
}

#[allow(refining_impl_trait)]
impl stem_capnp::signer::Server for EpochGuardedDomainSigner {
    fn sign(
        self: capnp::capability::Rc<Self>,
        params: stem_capnp::signer::SignParams,
        mut results: stem_capnp::signer::SignResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let nonce = pry!(params.get()).get_nonce();
        let envelope = pry!(SignedEnvelope::new(
            &self.keypair,
            self.domain.as_str().to_string(),
            self.domain.payload_type().to_vec(),
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

/// Fills the graft response with epoch-guarded Host, Executor, IPFS Client, Routing, HttpClient, and node identity.
pub struct HostGraftBuilder {
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
    content_store: Arc<dyn ipfs::ContentStore>,
    signing_key: Option<Arc<SigningKey>>,
    stream_control: libp2p_stream::Control,
    epoch_rx: watch::Receiver<Epoch>,
    allowed_hosts: Vec<String>,
    route_registry: Option<crate::dispatcher::server::RouteRegistry>,
}

impl HostGraftBuilder {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        network_state: NetworkState,
        swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
        wasm_debug: bool,
        content_store: Arc<dyn ipfs::ContentStore>,
        signing_key: Option<Arc<SigningKey>>,
        stream_control: libp2p_stream::Control,
        epoch_rx: watch::Receiver<Epoch>,
        allowed_hosts: Vec<String>,
    ) -> Self {
        Self {
            network_state,
            swarm_cmd_tx,
            wasm_debug,
            content_store,
            signing_key,
            stream_control,
            epoch_rx,
            allowed_hosts,
            route_registry: None,
        }
    }

    /// Set the HTTP route registry for WAGI integration.
    pub fn with_route_registry(
        mut self,
        registry: crate::dispatcher::server::RouteRegistry,
    ) -> Self {
        self.route_registry = Some(registry);
        self
    }
}

impl GraftBuilder for HostGraftBuilder {
    fn build(
        &self,
        guard: &EpochGuard,
        mut builder: stem_capnp::membrane::graft_results::Builder<'_>,
    ) -> Result<(), capnp::Error> {
        let mut host_impl = super::HostImpl::new(
            self.network_state.clone(),
            self.swarm_cmd_tx.clone(),
            self.wasm_debug,
            Some(guard.clone()),
            Some(self.stream_control.clone()),
        );
        if let Some(ref registry) = self.route_registry {
            host_impl = host_impl.with_route_registry(registry.clone());
        }
        let host: system_capnp::host::Client = capnp_rpc::new_client(host_impl);
        builder.set_host(host);

        let executor: system_capnp::executor::Client =
            capnp_rpc::new_client(super::ExecutorImpl::new_full(
                self.network_state.clone(),
                self.swarm_cmd_tx.clone(),
                self.wasm_debug,
                Some(guard.clone()),
                Some(self.epoch_rx.clone()),
                Some(self.content_store.clone()),
                self.signing_key.clone(),
                Some(self.stream_control.clone()),
            ));
        builder.set_executor(executor);

        let ipfs_client: ipfs_capnp::client::Client = capnp_rpc::new_client(
            EpochGuardedIpfsClient::new(self.content_store.clone(), guard.clone()),
        );
        builder.set_ipfs(ipfs_client);

        let routing: routing_capnp::routing::Client = capnp_rpc::new_client(
            super::routing::RoutingImpl::new(self.swarm_cmd_tx.clone(), guard.clone()),
        );
        builder.set_routing(routing);

        let http_client: http_capnp::http_client::Client =
            capnp_rpc::new_client(super::http_client::EpochGuardedHttpProxy::new(
                self.allowed_hosts.clone(),
                guard.clone(),
            ));
        builder.set_http_client(http_client);

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
// EpochGuardedIpfsClient — CoreAPI-style IPFS client
// ---------------------------------------------------------------------------

/// IPFS Client capability that checks epoch validity and delegates to sub-APIs.
struct EpochGuardedIpfsClient {
    content_store: Arc<dyn ipfs::ContentStore>,
    guard: EpochGuard,
}

impl EpochGuardedIpfsClient {
    fn new(content_store: Arc<dyn ipfs::ContentStore>, guard: EpochGuard) -> Self {
        Self {
            content_store,
            guard,
        }
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
            self.content_store.clone(),
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

/// UnixFS capability backed by a [`ContentStore`](ipfs::ContentStore) implementation.
struct EpochGuardedUnixFS {
    content_store: Arc<dyn ipfs::ContentStore>,
    guard: EpochGuard,
}

impl EpochGuardedUnixFS {
    fn new(content_store: Arc<dyn ipfs::ContentStore>, guard: EpochGuard) -> Self {
        Self {
            content_store,
            guard,
        }
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
        let store = self.content_store.clone();
        Promise::from_future(async move {
            let data = store
                .cat(&path)
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
        let store = self.content_store.clone();
        Promise::from_future(async move {
            let entries = store
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

    fn add(
        self: capnp::capability::Rc<Self>,
        params: ipfs_capnp::unix_f_s::AddParams,
        mut results: ipfs_capnp::unix_f_s::AddResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let data = pry!(pry!(params.get()).get_data()).to_vec();
        let store = self.content_store.clone();
        Promise::from_future(async move {
            let cid = store
                .add(&data)
                .await
                .map_err(|e| capnp::Error::failed(format!("ipfs add failed: {e}")))?;
            results.get().set_cid(&cid);
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
/// `IPFS Client`, and (when `signing_key` is `Some`) a host-side node identity signer.
///
/// When `signing_key` is `Some`, an [`EpochGuardedIdentity`] hub is injected into
/// every session so the kernel can request domain-scoped signers without holding
/// the private key. Auth (if needed) is handled by wrapping in `TerminalServer`
/// at the transport layer, not here.
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
    content_store: Arc<dyn ipfs::ContentStore>,
    signing_key: Option<Arc<SigningKey>>,
    stream_control: libp2p_stream::Control,
    route_registry: Option<crate::dispatcher::server::RouteRegistry>,
) -> (RpcSystem<Side>, GuestMembrane)
where
    R: AsyncRead + Unpin + 'static,
    W: AsyncWrite + Unpin + 'static,
{
    let mut sess_builder = HostGraftBuilder::new(
        network_state,
        swarm_cmd_tx,
        wasm_debug,
        content_store,
        signing_key,
        stream_control,
        epoch_rx.clone(),
        Vec::new(), // allowed_hosts: empty = allow all (default)
    );
    if let Some(registry) = route_registry {
        sess_builder = sess_builder.with_route_registry(registry);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use capnp_rpc::rpc_twoparty_capnp::Side;
    use capnp_rpc::twoparty::VatNetwork;
    use capnp_rpc::RpcSystem;
    use membrane::{Epoch, EpochGuard};
    use tokio::io;
    use tokio::sync::watch;
    use tokio_util::compat::{TokioAsyncReadCompatExt, TokioAsyncWriteCompatExt};

    fn epoch(seq: u64) -> Epoch {
        Epoch {
            seq,
            head: vec![],
            adopted_block: 0,
        }
    }

    /// Bootstrap an EpochGuardedUnixFS client/server pair backed by a mock
    /// HTTP server that mimics Kubo's `/api/v0/add` endpoint.
    async fn setup_unixfs_with_mock_kubo(
        guard: EpochGuard,
    ) -> (ipfs_capnp::unix_f_s::Client, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        // Start a mock HTTP server.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mock_addr = listener.local_addr().unwrap();
        let mock_url = format!("http://{mock_addr}");

        let mock_handle = tokio::spawn(async move {
            // Accept connections in a loop so we can handle multiple requests.
            loop {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 8192];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);

                    if request.contains("/api/v0/add") {
                        let body = r#"{"Name":"data","Hash":"QmMockCid12345","Size":"42"}"#;
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes()).await;
                    } else {
                        let _ = stream
                            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                            .await;
                    }
                });
            }
        });

        let ipfs_client: Arc<dyn ipfs::ContentStore> = Arc::new(ipfs::HttpClient::new(mock_url));
        let unixfs_impl = EpochGuardedUnixFS::new(ipfs_client, guard);
        let unixfs_server: ipfs_capnp::unix_f_s::Client = capnp_rpc::new_client(unixfs_impl);

        let (client_stream, server_stream) = io::duplex(64 * 1024);
        let (client_read, client_write) = io::split(client_stream);
        let (server_read, server_write) = io::split(server_stream);

        let server_network = VatNetwork::new(
            server_read.compat(),
            server_write.compat_write(),
            Side::Server,
            Default::default(),
        );
        let server_rpc = RpcSystem::new(Box::new(server_network), Some(unixfs_server.client));
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
        let client: ipfs_capnp::unix_f_s::Client = client_rpc.bootstrap(Side::Server);
        tokio::task::spawn_local(async move {
            let _ = client_rpc.await;
        });

        (client, mock_handle)
    }

    /// RPC round-trip for `UnixFS::add`: data → CID through Cap'n Proto.
    #[tokio::test]
    async fn test_unixfs_add_rpc_round_trip() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (_tx, rx) = watch::channel(epoch(1));
                let guard = EpochGuard {
                    issued_seq: 1,
                    receiver: rx,
                };
                let (client, mock_handle) = setup_unixfs_with_mock_kubo(guard).await;

                let mut req = client.add_request();
                req.get().set_data(b"hello from RPC test");
                let response = req.send().promise.await.expect("add RPC");
                let cid = response
                    .get()
                    .expect("get results")
                    .get_cid()
                    .expect("get cid");

                assert_eq!(cid, "QmMockCid12345", "should return CID from mock Kubo");

                mock_handle.abort();
            })
            .await;
    }

    /// `UnixFS::add` rejects stale epochs.
    #[tokio::test]
    async fn test_unixfs_add_rejects_stale_epoch() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (tx, rx) = watch::channel(epoch(1));
                let guard = EpochGuard {
                    issued_seq: 1,
                    receiver: rx,
                };
                let (client, mock_handle) = setup_unixfs_with_mock_kubo(guard).await;

                // Advance epoch → stale.
                tx.send(epoch(2)).unwrap();

                let mut req = client.add_request();
                req.get().set_data(b"stale data");
                match req.send().promise.await {
                    Err(e) => assert!(
                        e.to_string().contains("staleEpoch"),
                        "expected staleEpoch, got: {e}"
                    ),
                    Ok(_) => panic!("expected staleEpoch error"),
                }

                mock_handle.abort();
            })
            .await;
    }

    // ── MemoryStore-backed tests (no HTTP, no Kubo) ────────────────

    /// Bootstrap an EpochGuardedUnixFS client/server pair backed by a MemoryStore.
    async fn setup_unixfs_with_memory_store(
        store: Arc<dyn ipfs::ContentStore>,
        guard: EpochGuard,
    ) -> ipfs_capnp::unix_f_s::Client {
        let unixfs_impl = EpochGuardedUnixFS::new(store, guard);
        let unixfs_server: ipfs_capnp::unix_f_s::Client = capnp_rpc::new_client(unixfs_impl);

        let (client_stream, server_stream) = io::duplex(64 * 1024);
        let (client_read, client_write) = io::split(client_stream);
        let (server_read, server_write) = io::split(server_stream);

        let server_network = VatNetwork::new(
            server_read.compat(),
            server_write.compat_write(),
            Side::Server,
            Default::default(),
        );
        let server_rpc = RpcSystem::new(Box::new(server_network), Some(unixfs_server.client));
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
        let client: ipfs_capnp::unix_f_s::Client = client_rpc.bootstrap(Side::Server);
        tokio::task::spawn_local(async move {
            let _ = client_rpc.await;
        });

        client
    }

    /// Round-trip add → cat through Cap'n Proto with MemoryStore (no HTTP).
    #[tokio::test]
    async fn test_memory_store_add_then_cat() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let store = Arc::new(ipfs::MemoryStore::new());
                let (_tx, rx) = watch::channel(epoch(1));
                let guard = EpochGuard {
                    issued_seq: 1,
                    receiver: rx,
                };
                let client = setup_unixfs_with_memory_store(
                    store.clone() as Arc<dyn ipfs::ContentStore>,
                    guard,
                )
                .await;

                // Add data through RPC.
                let mut add_req = client.add_request();
                add_req.get().set_data(b"hello wetware");
                let add_resp = add_req.send().promise.await.expect("add RPC");
                let cid = add_resp
                    .get()
                    .expect("get results")
                    .get_cid()
                    .expect("get cid");
                let cid = cid.to_str().expect("valid utf8");
                assert!(
                    cid.starts_with("/ipfs/"),
                    "CID should be an IPFS path: {cid}"
                );
                let cid = cid.to_string();

                // Cat the data back through RPC.
                let mut cat_req = client.cat_request();
                cat_req.get().set_path(&cid);
                let cat_resp = cat_req.send().promise.await.expect("cat RPC");
                let data = cat_resp
                    .get()
                    .expect("get results")
                    .get_data()
                    .expect("get data");
                assert_eq!(data, b"hello wetware");
            })
            .await;
    }

    /// Cat a pre-seeded entry through Cap'n Proto with MemoryStore.
    #[tokio::test]
    async fn test_memory_store_cat_preseeded() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let store = Arc::new(ipfs::MemoryStore::new());
                store.insert("/ipfs/QmTest123", b"pre-seeded content".to_vec());

                let (_tx, rx) = watch::channel(epoch(1));
                let guard = EpochGuard {
                    issued_seq: 1,
                    receiver: rx,
                };
                let client =
                    setup_unixfs_with_memory_store(store as Arc<dyn ipfs::ContentStore>, guard)
                        .await;

                let mut req = client.cat_request();
                req.get().set_path("/ipfs/QmTest123");
                let resp = req.send().promise.await.expect("cat RPC");
                let data = resp
                    .get()
                    .expect("get results")
                    .get_data()
                    .expect("get data");
                assert_eq!(data, b"pre-seeded content");
            })
            .await;
    }

    /// Ls lists pre-seeded directory entries through Cap'n Proto with MemoryStore.
    #[tokio::test]
    async fn test_memory_store_ls() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let store = Arc::new(ipfs::MemoryStore::new());
                store.insert("/ipfs/QmDir/file_a.txt", b"aaa".to_vec());
                store.insert("/ipfs/QmDir/file_b.txt", b"bbb".to_vec());

                let (_tx, rx) = watch::channel(epoch(1));
                let guard = EpochGuard {
                    issued_seq: 1,
                    receiver: rx,
                };
                let client =
                    setup_unixfs_with_memory_store(store as Arc<dyn ipfs::ContentStore>, guard)
                        .await;

                let mut req = client.ls_request();
                req.get().set_path("/ipfs/QmDir");
                let resp = req.send().promise.await.expect("ls RPC");
                let entries = resp
                    .get()
                    .expect("get results")
                    .get_entries()
                    .expect("get entries");
                assert_eq!(entries.len(), 2);

                let mut names: Vec<String> = (0..entries.len())
                    .map(|i| {
                        entries
                            .get(i)
                            .get_name()
                            .expect("get name")
                            .to_str()
                            .expect("valid utf8")
                            .to_string()
                    })
                    .collect();
                names.sort();
                assert_eq!(names, vec!["file_a.txt", "file_b.txt"]);
            })
            .await;
    }

    /// Cat on a missing path returns an error through Cap'n Proto.
    #[tokio::test]
    async fn test_memory_store_cat_not_found() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let store: Arc<dyn ipfs::ContentStore> = Arc::new(ipfs::MemoryStore::new());
                let (_tx, rx) = watch::channel(epoch(1));
                let guard = EpochGuard {
                    issued_seq: 1,
                    receiver: rx,
                };
                let client = setup_unixfs_with_memory_store(store, guard).await;

                let mut req = client.cat_request();
                req.get().set_path("/ipfs/QmNonexistent");
                match req.send().promise.await {
                    Err(e) => assert!(
                        e.to_string().contains("not found"),
                        "error should mention 'not found': {}",
                        e
                    ),
                    Ok(_) => panic!("cat of missing path should fail"),
                }
            })
            .await;
    }

    /// MemoryStore-backed UnixFS rejects stale epochs.
    #[tokio::test]
    async fn test_memory_store_rejects_stale_epoch() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let store: Arc<dyn ipfs::ContentStore> = Arc::new(ipfs::MemoryStore::new());
                let (tx, rx) = watch::channel(epoch(1));
                let guard = EpochGuard {
                    issued_seq: 1,
                    receiver: rx,
                };
                let client = setup_unixfs_with_memory_store(store, guard).await;

                // Advance epoch → stale.
                tx.send(epoch(2)).unwrap();

                let mut req = client.add_request();
                req.get().set_data(b"stale data");
                match req.send().promise.await {
                    Err(e) => assert!(
                        e.to_string().contains("staleEpoch"),
                        "expected staleEpoch, got: {e}"
                    ),
                    Ok(_) => panic!("expected staleEpoch error"),
                }
            })
            .await;
    }
}
