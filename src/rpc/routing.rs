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
