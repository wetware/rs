//! Default kernel example for Wetware Protocol
//!
//! This is a minimal WASM component that uses WASI Preview 2 (wasip2) via the
//! `ww::guest` module to initialize Cap'n Proto RPC over WASI streams.

#![feature(wasip2)]

use ww::guest::{self, rpc_twoparty_capnp, RpcSystem};
use capnp::capability::Client;
use futures::executor::LocalPool;
use futures::future::{join, ready};

mod router_capnp {
    include!(concat!(env!("OUT_DIR"), "/src/schema/router_capnp.rs"));
}

// IPFS bootstrap peers (from deleted boot.rs)
const BOOTSTRAP_PEERS: &[&str] = &[
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa",
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmbLHAnMoJPWSCR5ZphtHh5F5FjGp6YhjaQ1VyaeoLjuXm",
    "/dnsaddr/bootstrap.libp2p.io/p2p/QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt",
    "/ip4/104.131.131.82/tcp/4001/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ",
    "/ip4/104.131.131.82/udp/4001/quic/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ",
];

/// Extract peer ID from a multiaddr
fn extract_peer_id(multiaddr: &str) -> Option<(&str, &str)> {
    // Find the /p2p/ part and extract the peer ID
    if let Some(p2p_pos) = multiaddr.rfind("/p2p/") {
        let peer_id = &multiaddr[p2p_pos + 5..];
        let address = &multiaddr[..p2p_pos];
        Some((peer_id, address))
    } else {
        None
    }
}

/// Entry point for the WASM component
///
/// Initializes the RPC system and runs a minimal event loop.
#[no_mangle]
pub extern "C" fn _start() {
    // Initialize RPC system over wasi:cli streams
    let mut rpc_system: RpcSystem<rpc_twoparty_capnp::Side> = guest::init();
    let provider_client: Client = rpc_system.bootstrap(rpc_twoparty_capnp::Side::Server);

    // Drive the RPC system on a single-threaded executor
    let mut pool = LocalPool::new();
    pool.run_until(async move {
        // Cast provider client to Router client
        let router_client = provider_client.cast_as::<router_capnp::router::Owned>();
        
        // Group bootstrap peers by peer ID
        let mut peers: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        for multiaddr in BOOTSTRAP_PEERS {
            if let Some((peer_id, address)) = extract_peer_id(multiaddr) {
                peers.entry(peer_id.to_string())
                    .or_insert_with(Vec::new)
                    .push(address.to_string());
            }
        }
        
        // Add all bootstrap peers to the router
        for (peer_id, addresses) in peers {
            let mut request = router_client.add_peer_request();
            request.get().set_peer(&peer_id);
            {
                let mut addresses_builder = request.get().init_addresses(addresses.len() as u32);
                for (i, addr) in addresses.iter().enumerate() {
                    addresses_builder.set(i as u32, addr);
                }
            }
            let _ = request.send().promise.await;
        }
        
        // Bootstrap the DHT
        let mut bootstrap_request = router_client.bootstrap_request();
        bootstrap_request.get().set_timeout(30000); // 30 second timeout
        let _ = bootstrap_request.send().promise.await;
        
        // Keep RPC system running
        let rpc = async move {
            let _ = rpc_system.await;
        };
        let _ = join(ready(()), rpc).await;
    });
}

