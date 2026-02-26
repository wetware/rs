//! Epoch-guarded Routing capability backed by Kubo HTTP API.
//!
//! Implements `routing_capnp::routing::Server` by delegating to
//! `ipfs::HttpClient` methods.  All methods check the epoch guard
//! before proceeding.
//!
//! Only content routing (provide/findProviders) lives here.
//! Data transfer (add/cat) is on the IPFS UnixFS capability.

use capnp::capability::Promise;
use capnp_rpc::pry;

use ::membrane::EpochGuard;

use crate::ipfs;
use crate::routing_capnp;

/// Routing capability served to guests via the Membrane graft.
pub struct RoutingImpl {
    ipfs_client: ipfs::HttpClient,
    guard: EpochGuard,
}

impl RoutingImpl {
    pub fn new(ipfs_client: ipfs::HttpClient, guard: EpochGuard) -> Self {
        Self { ipfs_client, guard }
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
        let key = pry!(pry!(params.get()).get_key())
            .to_string()
            .unwrap_or_default();
        let client = self.ipfs_client.clone();
        Promise::from_future(async move {
            client
                .routing_provide(&key)
                .await
                .map_err(|e| capnp::Error::failed(format!("routing provide failed: {e}")))?;
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
        let key = pry!(reader.get_key()).to_string().unwrap_or_default();
        let count = reader.get_count();
        let sink = pry!(reader.get_sink());
        let client = self.ipfs_client.clone();
        Promise::from_future(async move {
            let providers = client
                .routing_find_providers(&key, count)
                .await
                .map_err(|e| capnp::Error::failed(format!("routing findProviders failed: {e}")))?;

            // Stream each provider into the caller's sink.
            for provider in &providers {
                let mut req = sink.provider_request();
                let mut info = req.get().get_info()?;
                info.set_peer_id(provider.id.as_bytes());
                if let Some(ref addrs) = provider.addrs {
                    let mut addr_list = info.init_addrs(addrs.len() as u32);
                    for (j, addr) in addrs.iter().enumerate() {
                        addr_list.set(j as u32, addr.as_bytes());
                    }
                }
                // -> stream: awaits until flow control allows the next send.
                req.send().await?;
            }

            // Signal completion.
            sink.done_request().send().promise.await?;
            Ok(())
        })
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
    fn setup_routing(guard: EpochGuard) -> routing_capnp::routing::Client {
        let (client_stream, server_stream) = io::duplex(64 * 1024);
        let (client_read, client_write) = io::split(client_stream);
        let (server_read, server_write) = io::split(server_stream);

        // Server: RoutingImpl with a dummy IPFS client (epoch guard fires first).
        let ipfs_client = ipfs::HttpClient::new("http://127.0.0.1:1".to_string());
        let routing_impl = RoutingImpl::new(ipfs_client, guard);
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
