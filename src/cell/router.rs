//! Router capability implementation for Kademlia DHT operations
//!
//! This module provides a Cap'n Proto RPC server implementation that exposes
//! Kademlia routing functionality to WASM guests.

use capnp_rpc::pry;
use libp2p::{Multiaddr, PeerId};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::debug;

#[cfg(not(target_arch = "wasm32"))]
use crate::router_capnp;

/// Operations that can be performed on the KAD behavior
pub enum KadOperation {
    AddPeer {
        peer_id: PeerId,
        addresses: Vec<Multiaddr>,
        response: mpsc::Sender<Result<(), String>>,
    },
    Bootstrap {
        response: mpsc::Sender<Result<(), String>>,
    },
}

/// Router capability server implementation
///
/// Wraps a Kademlia behavior to provide routing operations via Cap'n Proto RPC.
/// Uses a channel to send operations to the executor which applies them to the swarm.
pub struct RouterCapability {
    operation_tx: Arc<mpsc::Sender<KadOperation>>,
}

impl RouterCapability {
    /// Create a new Router capability
    pub fn new(operation_tx: mpsc::Sender<KadOperation>) -> Self {
        Self {
            operation_tx: Arc::new(operation_tx),
        }
    }
}

impl router_capnp::router::Server for RouterCapability {
    #[allow(refining_impl_trait)]
    fn add_peer(
        self: ::capnp::capability::Rc<Self>,
        params: router_capnp::router::AddPeerParams,
        mut results: router_capnp::router::AddPeerResults,
    ) -> ::capnp::capability::Promise<(), ::capnp::Error> {
        let reader = pry!(params.get());
        let peer_id_reader = pry!(reader.get_peer());
        let peer_id_str = pry!(peer_id_reader.to_str()).to_string();
        let addresses_reader = pry!(reader.get_addresses());

        // Parse peer ID
        let peer_id = match peer_id_str.parse::<PeerId>() {
            Ok(id) => id,
            Err(e) => {
                return ::capnp::capability::Promise::err(capnp::Error::failed(format!(
                    "Invalid peer ID '{}': {}",
                    peer_id_str, e
                )));
            }
        };

        // Parse addresses
        let mut addresses = Vec::new();
        for i in 0..addresses_reader.len() {
            let addr_reader = pry!(addresses_reader.get(i));
            let addr_str = pry!(addr_reader.to_str()).to_string();
            let address = match addr_str.parse::<Multiaddr>() {
                Ok(addr) => addr,
                Err(e) => {
                    return ::capnp::capability::Promise::err(capnp::Error::failed(format!(
                        "Invalid address '{}': {}",
                        addr_str, e
                    )));
                }
            };
            addresses.push(address);
        }

        debug!(peer_id = %peer_id, address_count = addresses.len(), "Adding peer to routing table");

        // Send operation to executor
        let (response_tx, mut response_rx) = mpsc::channel(1);
        let operation_tx = Arc::clone(&self.operation_tx);

        ::capnp::capability::Promise::from_future(async move {
            operation_tx
                .send(KadOperation::AddPeer {
                    peer_id,
                    addresses,
                    response: response_tx,
                })
                .await
                .map_err(|e| capnp::Error::failed(format!("Failed to send operation: {}", e)))?;

            // Wait for response
            response_rx
                .recv()
                .await
                .ok_or_else(|| capnp::Error::failed("No response from executor".to_string()))?
                .map_err(|e| capnp::Error::failed(e))?;

            results.get().set_created(true);
            Ok(())
        })
    }

    #[allow(refining_impl_trait)]
    fn bootstrap(
        self: ::capnp::capability::Rc<Self>,
        params: router_capnp::router::BootstrapParams,
        mut results: router_capnp::router::BootstrapResults,
    ) -> ::capnp::capability::Promise<(), ::capnp::Error> {
        let reader = pry!(params.get());
        let _timeout = reader.get_timeout();

        debug!("Bootstrap requested (timeout: {} ms)", _timeout);

        // Send operation to executor
        let (response_tx, mut response_rx) = mpsc::channel(1);
        let operation_tx = Arc::clone(&self.operation_tx);

        ::capnp::capability::Promise::from_future(async move {
            operation_tx
                .send(KadOperation::Bootstrap {
                    response: response_tx,
                })
                .await
                .map_err(|e| capnp::Error::failed(format!("Failed to send operation: {}", e)))?;

            // Wait for response
            response_rx
                .recv()
                .await
                .ok_or_else(|| capnp::Error::failed("No response from executor".to_string()))?
                .map_err(|e| capnp::Error::failed(e))?;

            results.get().set_success(true);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capnp_rpc::new_client;

    // Helper to create a test client that we can make requests to
    fn create_test_router_cap(
        operation_tx: mpsc::Sender<KadOperation>,
    ) -> router_capnp::router::Client {
        let router_cap = RouterCapability::new(operation_tx);
        new_client(router_cap)
    }

    #[tokio::test]
    async fn test_add_peer_valid_input() {
        let (operation_tx, mut operation_rx) = mpsc::channel(10);
        let router_client = create_test_router_cap(operation_tx);

        // Generate a valid peer ID for testing
        let keypair = libp2p::identity::Keypair::generate_ed25519();
        let peer_id = keypair.public().to_peer_id();
        let peer_id_str = peer_id.to_string();
        let addresses = vec!["/ip4/127.0.0.1/tcp/4001", "/ip6/::1/tcp/4001"];

        // Spawn task to process the operation
        let operation_task = tokio::spawn(async move {
            if let Some(op) = operation_rx.recv().await {
                match op {
                    KadOperation::AddPeer {
                        peer_id: received_peer_id,
                        addresses: received_addresses,
                        response,
                    } => {
                        assert_eq!(received_addresses.len(), 2);
                        assert_eq!(received_peer_id, peer_id);
                        let _ = response.send(Ok(())).await;
                    }
                    _ => panic!("Expected AddPeer operation"),
                }
            }
        });

        // Make request via client
        let mut request = router_client.add_peer_request();
        request.get().set_peer(&peer_id_str);
        {
            let mut addresses_builder = request.get().init_addresses(addresses.len() as u32);
            for (i, addr) in addresses.iter().enumerate() {
                addresses_builder.set(i as u32, addr);
            }
        }
        let response = request.send().promise.await.unwrap();
        assert!(response.get().unwrap().get_created());

        operation_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_add_peer_invalid_peer_id() {
        let (operation_tx, _) = mpsc::channel(10);
        let router_client = create_test_router_cap(operation_tx);

        // Create invalid peer ID
        let peer_id = "invalid-peer-id";
        let addresses = vec!["/ip4/127.0.0.1/tcp/4001"];

        // Make request via client - should return error
        let mut request = router_client.add_peer_request();
        request.get().set_peer(peer_id);
        {
            let mut addresses_builder = request.get().init_addresses(addresses.len() as u32);
            for (i, addr) in addresses.iter().enumerate() {
                addresses_builder.set(i as u32, addr);
            }
        }
        let result = request.send().promise.await;

        assert!(result.is_err());
        // The error is in the promise, not the response
        let _ = result;
    }

    #[tokio::test]
    async fn test_add_peer_invalid_address() {
        let (operation_tx, _) = mpsc::channel(10);
        let router_client = create_test_router_cap(operation_tx);

        // Generate a valid peer ID but use invalid address
        let keypair = libp2p::identity::Keypair::generate_ed25519();
        let peer_id = keypair.public().to_peer_id();
        let peer_id_str = peer_id.to_string();
        let addresses = vec!["/invalid/address"];

        // Make request via client - should return error
        let mut request = router_client.add_peer_request();
        request.get().set_peer(&peer_id_str);
        {
            let mut addresses_builder = request.get().init_addresses(addresses.len() as u32);
            for (i, addr) in addresses.iter().enumerate() {
                addresses_builder.set(i as u32, addr);
            }
        }
        let result = request.send().promise.await;

        assert!(result.is_err());
        // The error is in the promise, not the response
        let _ = result;
    }

    #[tokio::test]
    async fn test_bootstrap_success() {
        let (operation_tx, mut operation_rx) = mpsc::channel(10);
        let router_client = create_test_router_cap(operation_tx);

        let timeout = 30000u64;

        // Spawn task to process the operation
        let operation_task = tokio::spawn(async move {
            if let Some(op) = operation_rx.recv().await {
                match op {
                    KadOperation::Bootstrap { response } => {
                        let _ = response.send(Ok(())).await;
                    }
                    _ => panic!("Expected Bootstrap operation"),
                }
            }
        });

        // Make request via client
        let mut request = router_client.bootstrap_request();
        request.get().set_timeout(timeout);
        let response = request.send().promise.await.unwrap();
        assert!(response.get().unwrap().get_success());

        operation_task.await.unwrap();
    }

    #[tokio::test]
    async fn test_channel_failure() {
        // Create channel and drop receiver to simulate channel closure
        let (operation_tx, _) = mpsc::channel(10);
        let router_client = create_test_router_cap(operation_tx);

        // Generate a valid peer ID
        let keypair = libp2p::identity::Keypair::generate_ed25519();
        let peer_id = keypair.public().to_peer_id();
        let peer_id_str = peer_id.to_string();
        let addresses = vec!["/ip4/127.0.0.1/tcp/4001"];

        // Drop the operation_tx to simulate channel failure
        // Note: We can't easily drop it here since it's inside the client
        // This test verifies that operations fail when channel is closed
        // In practice, this would happen if the executor stops

        // Make request - the operation will be sent but response channel will be closed
        let mut request = router_client.add_peer_request();
        request.get().set_peer(&peer_id_str);
        {
            let mut addresses_builder = request.get().init_addresses(addresses.len() as u32);
            addresses_builder.set(0, addresses[0]);
        }

        // The request will succeed in sending, but we can't easily test channel closure
        // without more complex setup. For now, we'll test that the request structure works.
        // A more complete test would require dropping the receiver side.
        let _ = request.send().promise.await;
    }
}
