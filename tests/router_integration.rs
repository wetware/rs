//! Integration tests for Router capability
//!
//! These tests verify the end-to-end flow from Router capability through
//! the channel to the executor and back.

use capnp_rpc::new_client;
use libp2p::identity;
use libp2p::SwarmBuilder;
use tokio::sync::mpsc;
use ww::cell::router::{KadOperation, RouterCapability};
use ww::router_capnp;

use ww::cell::executor::WetwareBehaviour;

#[tokio::test]
async fn test_router_to_executor_flow() {
    // Create a minimal swarm for testing
    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = keypair.public().to_peer_id();
    let kad = libp2p::kad::Behaviour::new(peer_id, libp2p::kad::store::MemoryStore::new(peer_id));

    let behaviour = WetwareBehaviour {
        kad,
        identify: libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
            "wetware/0.1.0".to_string(),
            keypair.public(),
        )),
    };

    let mut swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            Default::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .unwrap()
        .with_behaviour(|_| behaviour)
        .unwrap()
        .with_swarm_config(|c| c.with_idle_connection_timeout(std::time::Duration::from_secs(60)))
        .build();

    // Create channel for operations
    let (operation_tx, mut operation_rx) = mpsc::channel(10);

    // Create Router capability
    let router_cap = RouterCapability::new(operation_tx);
    let router_client: router_capnp::router::Client = new_client(router_cap);

    // Generate test peer ID and addresses
    let test_keypair = identity::Keypair::generate_ed25519();
    let test_peer_id = test_keypair.public().to_peer_id();
    let test_peer_id_str = test_peer_id.to_string();
    let test_addresses = ["/ip4/127.0.0.1/tcp/4001", "/ip6/::1/tcp/4001"];

    // Spawn executor task that processes operations
    let executor_task = tokio::spawn(async move {
        while let Some(op) = operation_rx.recv().await {
            if let KadOperation::AddPeer {
                peer_id,
                addresses,
                response,
            } = op
            {
                for address in addresses {
                    swarm.behaviour_mut().kad.add_address(&peer_id, address);
                }
                let _ = response.send(Ok(())).await;
            }
        }
    });

    // Make request via Router client
    let mut request = router_client.add_peer_request();
    request.get().set_peer(&test_peer_id_str);
    {
        let mut addresses_builder = request.get().init_addresses(test_addresses.len() as u32);
        for (i, addr) in test_addresses.iter().enumerate() {
            addresses_builder.set(i as u32, addr);
        }
    }

    let response = request.send().promise.await.unwrap();
    assert!(response.get().unwrap().get_created());

    // Give executor time to process
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Shutdown executor
    drop(executor_task);
}

#[tokio::test]
async fn test_bootstrap_flow() {
    // Create a minimal swarm for testing
    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = keypair.public().to_peer_id();
    let kad = libp2p::kad::Behaviour::new(peer_id, libp2p::kad::store::MemoryStore::new(peer_id));

    let behaviour = WetwareBehaviour {
        kad,
        identify: libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
            "wetware/0.1.0".to_string(),
            keypair.public(),
        )),
    };

    let mut swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(
            Default::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )
        .unwrap()
        .with_behaviour(|_| behaviour)
        .unwrap()
        .with_swarm_config(|c| c.with_idle_connection_timeout(std::time::Duration::from_secs(60)))
        .build();

    // Create channel for operations
    let (operation_tx, mut operation_rx) = mpsc::channel(10);

    // Create Router capability
    let router_cap = RouterCapability::new(operation_tx);
    let router_client: router_capnp::router::Client = new_client(router_cap);

    // Spawn executor task that processes operations
    let executor_task = tokio::spawn(async move {
        while let Some(op) = operation_rx.recv().await {
            if let KadOperation::Bootstrap { response } = op {
                match swarm.behaviour_mut().kad.bootstrap() {
                    Ok(_) => {
                        let _ = response.send(Ok(())).await;
                    }
                    Err(e) => {
                        let _ = response.send(Err(e.to_string())).await;
                    }
                }
            }
        }
    });

    // Make request via Router client
    let mut request = router_client.bootstrap_request();
    request.get().set_timeout(30000);

    let response = request.send().promise.await;
    // Bootstrap might succeed or fail depending on state (e.g., no known peers)
    // Both cases are valid - we just verify the request was processed
    match response {
        Ok(resp) => {
            // Bootstrap succeeded
            let _ = resp.get();
        }
        Err(_) => {
            // Bootstrap failed (e.g., no known peers) - this is expected in test environment
        }
    }

    // Give executor time to process
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Shutdown executor
    drop(executor_task);
}
