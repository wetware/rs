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
    fn add_peer(
        &mut self,
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
                return ::capnp::capability::Promise::err(
                    capnp::Error::failed(format!("Invalid peer ID '{}': {}", peer_id_str, e))
                );
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
                    return ::capnp::capability::Promise::err(
                        capnp::Error::failed(format!("Invalid address '{}': {}", addr_str, e))
                    );
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

    fn bootstrap(
        &mut self,
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
                .send(KadOperation::Bootstrap { response: response_tx })
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

