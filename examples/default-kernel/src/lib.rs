//! Default kernel example for Wetware Protocol
//!
//! This is a minimal WASM component that uses WASI Preview 2 (wasip2) via the
//! `ww::guest` module to initialize Cap'n Proto RPC over WASI streams.

#![feature(wasip2)]

use ww::guest::{self, rpc_twoparty_capnp, RpcSystem};
use capnp::capability::{Client, FromClientHook};
use futures::executor::LocalPool;
use futures::future::{join, ready};

mod router_capnp {
    extern crate capnp;
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
        // DHT Bootstrap Sequence
        ////

        // Cast provider client to Router client
        let router_client: router_capnp::router::Client = provider_client.cast_to();
        
        // Group bootstrap peers by peer ID
        let mut peers: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        for multiaddr in BOOTSTRAP_PEERS {
            if let Some((peer_id, address)) = extract_peer_id(multiaddr) {
                peers.entry(peer_id.to_string())
                    .or_insert_with(Vec::new)
                    .push(address.to_string());
            }
        }
        
        // Add all bootstrap peers to the router concurrently and
        // wait until all have completed.
        let mut add_peer_futures = Vec::new();
        for (peer_id, addresses) in peers {
            let mut request = router_client.add_peer_request();
            request.get().set_peer(&peer_id);
            {
                let mut addresses_builder = request.get().init_addresses(addresses.len() as u32);
                for (i, addr) in addresses.iter().enumerate() {
                    addresses_builder.set(i as u32, addr);
                }
            }
            add_peer_futures.push(request.send().promise);
        }
        futures::future::join_all(add_peer_futures).await;
        
        // Bootstrap the DHT
        let mut bootstrap_request = router_client.bootstrap_request();
        bootstrap_request.get().set_timeout(10000); // 10s timeout
        let _ = bootstrap_request.send().promise.await;

        // Main Loop - Keep RPC system running
        ////
        let rpc = async move {
            let _ = rpc_system.await;
        };
        let _ = join(ready(()), rpc).await;
    });
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_peer_id() {
        // Test with standard multiaddr format
        let multiaddr = "/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN";
        let result = extract_peer_id(multiaddr);
        assert!(result.is_some());
        let (peer_id, address) = result.unwrap();
        assert_eq!(peer_id, "QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN");
        assert_eq!(address, "/dnsaddr/bootstrap.libp2p.io");

        // Test with IP address format
        let multiaddr = "/ip4/104.131.131.82/tcp/4001/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ";
        let result = extract_peer_id(multiaddr);
        assert!(result.is_some());
        let (peer_id, address) = result.unwrap();
        assert_eq!(peer_id, "QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ");
        assert_eq!(address, "/ip4/104.131.131.82/tcp/4001");

        // Test with no /p2p/ segment
        let multiaddr = "/ip4/127.0.0.1/tcp/4001";
        let result = extract_peer_id(multiaddr);
        assert!(result.is_none());

        // Test with multiple /p2p/ segments (should use the last one)
        let multiaddr = "/ip4/127.0.0.1/p2p/QmFirst/tcp/4001/p2p/QmLast";
        let result = extract_peer_id(multiaddr);
        assert!(result.is_some());
        let (peer_id, _) = result.unwrap();
        assert_eq!(peer_id, "QmLast");
    }

    #[test]
    fn test_group_peers_by_id() {
        // Test grouping multiple addresses for same peer ID
        let multiaddrs = vec![
            "/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
            "/ip4/104.131.131.82/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN",
        ];

        let mut peers: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
        for multiaddr in &multiaddrs {
            if let Some((peer_id, address)) = extract_peer_id(multiaddr) {
                peers.entry(peer_id.to_string())
                    .or_insert_with(Vec::new)
                    .push(address.to_string());
            }
        }

        assert_eq!(peers.len(), 1);
        let addresses = peers.get("QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN").unwrap();
        assert_eq!(addresses.len(), 2);
        assert!(addresses.contains(&"/dnsaddr/bootstrap.libp2p.io".to_string()));
        assert!(addresses.contains(&"/ip4/104.131.131.82/tcp/4001".to_string()));
    }
}
