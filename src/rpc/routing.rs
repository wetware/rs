//! Epoch-guarded Routing capability backed by the in-process Kademlia client.
//!
//! Implements `routing_capnp::routing::Server` by dispatching to the swarm
//! event loop via `SwarmCommand::KadProvide` / `SwarmCommand::KadFindProviders`.
//! All methods check the epoch guard before proceeding.
//!
//! Only content routing (provide/findProviders) lives here.
//! Data transfer (add/cat) is on the IPFS UnixFS capability.

use capnp::capability::Promise;
use capnp_rpc::pry;
use cid::Cid;
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, oneshot};

use ::membrane::EpochGuard;

use crate::host::SwarmCommand;
use crate::routing_capnp;

/// Convert a CID string to Kademlia record key bytes (multihash).
///
/// Provider records in the Amino DHT are keyed by the multihash of the CID.
fn cid_to_kad_key(cid_str: &str) -> Result<Vec<u8>, capnp::Error> {
    let cid: Cid = cid_str
        .parse()
        .map_err(|e| capnp::Error::failed(format!("invalid CID '{cid_str}': {e}")))?;
    Ok(cid.hash().to_bytes())
}

/// Routing capability served to guests via the Membrane graft.
pub struct RoutingImpl {
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    guard: EpochGuard,
}

impl RoutingImpl {
    pub fn new(swarm_cmd_tx: mpsc::Sender<SwarmCommand>, guard: EpochGuard) -> Self {
        Self {
            swarm_cmd_tx,
            guard,
        }
    }
}

#[allow(refining_impl_trait)]
impl routing_capnp::routing::Server for RoutingImpl {
    fn provide(
        self: capnp::capability::Rc<Self>,
        params: routing_capnp::routing::ProvideParams,
        _results: routing_capnp::routing::ProvideResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let key_str = pry!(pry!(params.get()).get_key())
            .to_string()
            .unwrap_or_default();
        let key_bytes = pry!(cid_to_kad_key(&key_str));
        let swarm_cmd_tx = self.swarm_cmd_tx.clone();
        Promise::from_future(async move {
            let (reply_tx, reply_rx) = oneshot::channel();
            swarm_cmd_tx
                .send(SwarmCommand::KadProvide {
                    key: key_bytes,
                    reply: reply_tx,
                })
                .await
                .map_err(|_| capnp::Error::failed("swarm channel closed".into()))?;
            reply_rx
                .await
                .map_err(|_| capnp::Error::failed("swarm reply dropped".into()))?
                .map_err(|e| capnp::Error::failed(format!("kad provide failed: {e}")))?;
            Ok(())
        })
    }

    fn find_providers(
        self: capnp::capability::Rc<Self>,
        params: routing_capnp::routing::FindProvidersParams,
        _results: routing_capnp::routing::FindProvidersResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());
        let reader = pry!(params.get());
        let key_str = pry!(reader.get_key()).to_string().unwrap_or_default();
        let sink = pry!(reader.get_sink());
        let key_bytes = pry!(cid_to_kad_key(&key_str));
        let swarm_cmd_tx = self.swarm_cmd_tx.clone();
        Promise::from_future(async move {
            let (provider_tx, mut provider_rx) = mpsc::unbounded_channel();
            swarm_cmd_tx
                .send(SwarmCommand::KadFindProviders {
                    key: key_bytes,
                    reply: provider_tx,
                })
                .await
                .map_err(|_| capnp::Error::failed("swarm channel closed".into()))?;

            // Stream each discovered provider into the caller's sink.
            while let Some(peer_info) = provider_rx.recv().await {
                let mut req = sink.provider_request();
                let mut info = req.get().get_info()?;
                info.set_peer_id(&peer_info.peer_id);
                let mut addr_list = info.init_addrs(peer_info.addrs.len() as u32);
                for (j, addr) in peer_info.addrs.iter().enumerate() {
                    addr_list.set(j as u32, addr);
                }
                // -> stream: awaits until flow control allows the next send.
                req.send().await?;
            }

            // Signal completion.
            sink.done_request().send().promise.await?;
            Ok(())
        })
    }

    fn hash(
        self: capnp::capability::Rc<Self>,
        params: routing_capnp::routing::HashParams,
        mut results: routing_capnp::routing::HashResults,
    ) -> Promise<(), capnp::Error> {
        let data = pry!(pry!(params.get()).get_data());
        let digest = Sha256::digest(data);
        // multihash: varint(0x12) ++ varint(32) ++ sha256(data)
        let mh = pry!(cid::multihash::Multihash::<64>::wrap(0x12, &digest)
            .map_err(|e| capnp::Error::failed(format!("multihash wrap: {e}"))));
        let c = Cid::new_v1(0x55, mh); // 0x55 = raw codec
        results.get().set_key(c.to_string());
        Promise::ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capnp_rpc::rpc_twoparty_capnp::Side;
    use capnp_rpc::twoparty::VatNetwork;
    use capnp_rpc::RpcSystem;
    use membrane::Epoch;
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

    /// Bootstrap a Routing client/server pair over in-memory duplex.
    ///
    /// Uses a fake swarm channel (receiver dropped); the epoch guard fires
    /// before the channel is ever used in the stale-epoch tests.
    fn setup_routing(guard: EpochGuard) -> routing_capnp::routing::Client {
        let (client_stream, server_stream) = io::duplex(64 * 1024);
        let (client_read, client_write) = io::split(client_stream);
        let (server_read, server_write) = io::split(server_stream);

        // Fake swarm channel: receiver is dropped immediately.
        // Epoch check fires before any send, so this is fine for rejection tests.
        let (swarm_tx, _swarm_rx) = mpsc::channel(16);
        let routing_impl = RoutingImpl::new(swarm_tx, guard);
        let routing_server: routing_capnp::routing::Client = capnp_rpc::new_client(routing_impl);

        let server_network = VatNetwork::new(
            server_read.compat(),
            server_write.compat_write(),
            Side::Server,
            Default::default(),
        );
        let server_rpc = RpcSystem::new(Box::new(server_network), Some(routing_server.client));
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
        let client: routing_capnp::routing::Client = client_rpc.bootstrap(Side::Server);
        tokio::task::spawn_local(async move {
            let _ = client_rpc.await;
        });

        client
    }

    // -------------------------------------------------------------------
    // Key derivation — both nodes must agree on the same Kad key
    // -------------------------------------------------------------------

    /// Build a CID the same way the `hash` RPC method does (CIDv1, raw, sha256).
    fn hash_to_cid(data: &[u8]) -> String {
        let digest = Sha256::digest(data);
        let mh = cid::multihash::Multihash::<64>::wrap(0x12, &digest).unwrap();
        Cid::new_v1(0x55, mh).to_string()
    }

    #[test]
    fn test_hash_is_deterministic() {
        let a = hash_to_cid(b"ww.chess.v1");
        let b = hash_to_cid(b"ww.chess.v1");
        assert_eq!(a, b, "same input must produce same CID");
    }

    #[test]
    fn test_cid_to_kad_key_deterministic() {
        let cid = hash_to_cid(b"ww.chess.v1");
        let key_a = cid_to_kad_key(&cid).unwrap();
        let key_b = cid_to_kad_key(&cid).unwrap();
        assert_eq!(key_a, key_b, "same CID must produce same Kad key");
        assert!(!key_a.is_empty());
    }

    #[test]
    fn test_different_inputs_different_keys() {
        let cid_a = hash_to_cid(b"ww.chess.v1");
        let cid_b = hash_to_cid(b"ww.chess.v2");
        let key_a = cid_to_kad_key(&cid_a).unwrap();
        let key_b = cid_to_kad_key(&cid_b).unwrap();
        assert_ne!(key_a, key_b);
    }

    #[test]
    fn test_cid_to_kad_key_rejects_invalid() {
        assert!(cid_to_kad_key("not-a-cid").is_err());
        assert!(cid_to_kad_key("").is_err());
    }

    // -------------------------------------------------------------------
    // Epoch guard tests
    // -------------------------------------------------------------------

    #[tokio::test]
    async fn test_provide_rejects_stale_epoch() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (tx, rx) = watch::channel(epoch(1));
                let guard = EpochGuard {
                    issued_seq: 1,
                    receiver: rx,
                };
                let client = setup_routing(guard);

                // Advance epoch → stale.
                tx.send(epoch(2)).unwrap();

                let mut req = client.provide_request();
                req.get().set_key("QmTest");
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

    #[tokio::test]
    async fn test_find_providers_rejects_stale_epoch() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let (tx, rx) = watch::channel(epoch(1));
                let guard = EpochGuard {
                    issued_seq: 1,
                    receiver: rx,
                };
                let client = setup_routing(guard);

                // Advance epoch → stale.
                tx.send(epoch(2)).unwrap();

                // findProviders needs a sink; we don't care about it since
                // the epoch check fires before the sink is read.
                let req = client.find_providers_request();
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
