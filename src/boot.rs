use anyhow::{anyhow, Result};
use futures::StreamExt;
use libp2p::{
    identity,
    kad::{Event as KademliaEvent, QueryResult, RecordKey},
    swarm::{NetworkBehaviour, Swarm, SwarmEvent},
    Multiaddr, PeerId,
};
use serde_json::Value;

use std::time::Duration;
use tracing::{debug, info, warn};

use crate::config::HostConfig;

// IPFS protocol constants for DHT compatibility
// These protocols ensure our node can communicate with the IPFS network
const IPFS_KADEMLIA_PROTOCOL: &str = "/ipfs/kad/1.0.0"; // Standard IPFS DHT protocol
const IPFS_IDENTIFY_PROTOCOL: &str = "/ipfs/id/1.0.0"; // Standard IPFS identify protocol

/// Query a Kubo node to get its peer list for DHT bootstrap
/// This function retrieves the list of peers that our local Kubo node is connected to,
/// which we'll use to bootstrap our DHT routing table and establish connections.
pub async fn get_kubo_peers(kubo_url: &str) -> Result<Vec<(PeerId, Multiaddr)>> {
    let span = tracing::debug_span!("get_kubo_peers", kubo_url = kubo_url);
    let _enter = span.enter();

    let client = reqwest::Client::new();
    let url = format!("{}/api/v0/swarm/peers", kubo_url);

    debug!(url = %url, "Querying Kubo node for peers");

    let response = client.post(&url).send().await?;
    if !response.status().is_success() {
        warn!(status = %response.status(), "Failed to get peers from Kubo");
        return Err(anyhow!(
            "Failed to get peers from Kubo: {}",
            response.status()
        ));
    }

    let body: Value = response.json().await?;
    let peers = body["Peers"]
        .as_array()
        .ok_or_else(|| anyhow!("Invalid response format from Kubo"))?;

    let mut peer_addrs = Vec::new();
    let mut parse_errors = 0;

    for peer in peers {
        // The Kubo API returns "Addr" (multiaddr) and "Peer" (peer ID) separately
        if let (Some(addr_str), Some(peer_id_str)) = (peer["Addr"].as_str(), peer["Peer"].as_str())
        {
            if let (Ok(multiaddr), Ok(peer_id)) =
                (addr_str.parse::<Multiaddr>(), peer_id_str.parse::<PeerId>())
            {
                // Construct the full multiaddr with peer ID
                let full_addr = multiaddr.with(libp2p::multiaddr::Protocol::P2p(peer_id));
                peer_addrs.push((peer_id, full_addr));
            } else {
                parse_errors += 1;
                debug!(
                    addr_str = addr_str,
                    peer_id_str = peer_id_str,
                    "Failed to parse peer address or ID"
                );
            }
        }
    }

    if parse_errors > 0 {
        info!(
            parse_errors = parse_errors,
            "Some peer addresses could not be parsed"
        );
    }

    debug!(
        peer_count = peer_addrs.len(),
        parse_errors = parse_errors,
        "Found peer addresses from Kubo node"
    );
    Ok(peer_addrs)
}

#[derive(NetworkBehaviour)]
pub struct WetwareBehaviour {
    kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
    identify: libp2p::identify::Behaviour,
    wetware: crate::rpc::ProtocolBehaviour,
}

pub struct SwarmManager {
    swarm: Swarm<WetwareBehaviour>,
    #[allow(dead_code)]
    peer_id: PeerId,
}

impl SwarmManager {
    pub fn new(swarm: Swarm<WetwareBehaviour>, peer_id: PeerId) -> Self {
        debug!(peer_id = %peer_id, "Creating new SwarmManager");
        Self { swarm, peer_id }
    }

    /// Bootstrap the DHT by connecting to IPFS peers and triggering the bootstrap process
    /// This function establishes connections to the provided peers and then triggers
    /// the Kademlia bootstrap process to populate our routing table.
    pub async fn bootstrap_dht(&mut self, bootstrap_peers: Vec<(PeerId, Multiaddr)>) -> Result<()> {
        let span = tracing::debug_span!("bootstrap_dht", peer_count = bootstrap_peers.len());
        let _enter = span.enter();

        debug!("Bootstrapping DHT with IPFS peers");

        // Peers are already added to routing table, now dial them to establish connections
        for (peer_id, peer_addr) in &bootstrap_peers {
            if let Err(e) = self.swarm.dial(peer_addr.clone()) {
                debug!(peer_id = %peer_id, peer_addr = %peer_addr, reason = ?e, "Failed to dial IPFS peer");
            } else {
                debug!(peer_id = %peer_id, peer_addr = %peer_addr, "Dialing IPFS peer");
            }
        }

        // Wait for connections to be established
        debug!("Waiting for peer connections to establish...");
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Now trigger bootstrap after we have actual peer connections
        debug!("Triggering DHT bootstrap with IPFS peers");
        let _ = self.swarm.behaviour_mut().kad.bootstrap();

        Ok(())
    }

    pub async fn announce_provider(&mut self, key: &str) -> Result<()> {
        let span = tracing::debug_span!("announce_provider", key = key);
        let _enter = span.enter();

        let record_key = RecordKey::new(&key.as_bytes());
        let _ = self.swarm.behaviour_mut().kad.start_providing(record_key);
        debug!("Started providing key");
        Ok(())
    }

    pub async fn query_providers(&mut self, key: &str) -> Result<()> {
        let span = tracing::debug_span!("query_providers", key = key);
        let _enter = span.enter();

        let record_key = RecordKey::new(&key.as_bytes());
        self.swarm.behaviour_mut().kad.get_providers(record_key);
        debug!("Querying providers for key");
        Ok(())
    }

    pub async fn run_event_loop(&mut self) -> Result<()> {
        let span = tracing::debug_span!("event_loop");
        let _enter = span.enter();

        debug!("Event loop started");

        loop {
            match self.swarm.next().await {
                Some(SwarmEvent::Behaviour(WetwareBehaviourEvent::Kad(event))) => match event {
                    KademliaEvent::OutboundQueryProgressed { result, .. } => match result {
                        QueryResult::GetProviders(Ok(providers_result)) => match providers_result {
                            libp2p::kad::GetProvidersOk::FoundProviders { providers, .. } => {
                                debug!(provider_count = providers.len(), "Found providers");
                                for provider in providers {
                                    debug!(provider = %provider, "Provider found");
                                }
                            }
                            libp2p::kad::GetProvidersOk::FinishedWithNoAdditionalRecord {
                                ..
                            } => {
                                debug!("Finished querying providers with no additional records");
                            }
                        },
                        QueryResult::GetProviders(Err(e)) => {
                            warn!(reason = ?e, "Failed to get providers");
                        }
                        QueryResult::StartProviding(Ok(_)) => {
                            debug!("Successfully started providing key");
                        }
                        QueryResult::StartProviding(Err(e)) => {
                            warn!(reason = ?e, "Failed to start providing");
                        }
                        QueryResult::Bootstrap(Ok(_)) => {
                            debug!("DHT bootstrap completed successfully");
                        }
                        QueryResult::Bootstrap(Err(e)) => {
                            warn!(reason = ?e, "DHT bootstrap failed");
                        }
                        _ => {}
                    },
                    KademliaEvent::InboundRequest { request, .. } => match request {
                        libp2p::kad::InboundRequest::GetProvider { .. } => {
                            debug!("Received GetProvider request");
                        }
                        libp2p::kad::InboundRequest::GetRecord { .. } => {
                            debug!("Received GetRecord request");
                        }
                        _ => {}
                    },
                    _ => {}
                },
                Some(SwarmEvent::Behaviour(WetwareBehaviourEvent::Identify(event))) => {
                    match event {
                        libp2p::identify::Event::Received { peer_id, info, .. } => {
                            debug!(peer_id = %peer_id, listen_addrs = ?info.listen_addrs, "Received identify info from peer");
                            // Don't add peers to Kademlia here - we already added them upfront
                            // This is just for logging peer discovery
                        }
                        libp2p::identify::Event::Sent { peer_id, .. } => {
                            debug!(peer_id = %peer_id, "Sent identify info to peer");
                        }
                        libp2p::identify::Event::Error { peer_id, error, .. } => {
                            warn!(peer_id = %peer_id, reason = ?error, "Identify error with peer");
                        }
                        _ => {}
                    }
                }
                Some(SwarmEvent::NewListenAddr { address, .. }) => {
                    debug!(address = %address, "Listening on address");
                }
                Some(SwarmEvent::ConnectionEstablished { peer_id, .. }) => {
                    debug!(peer_id = %peer_id, "Connected to IPFS peer");
                }
                Some(SwarmEvent::ConnectionClosed { peer_id, .. }) => {
                    debug!(peer_id = %peer_id, "Disconnected from IPFS peer");
                }
                Some(SwarmEvent::OutgoingConnectionError { peer_id, error, .. }) => {
                    // Only log connection errors at debug level to reduce spam
                    if let Some(peer_id) = peer_id {
                        debug!(peer_id = %peer_id, reason = ?error, "Failed to connect to IPFS peer");
                    } else {
                        debug!(reason = ?error, "Failed to establish outgoing connection");
                    }
                }
                Some(SwarmEvent::IncomingConnection {
                    connection_id,
                    local_addr,
                    send_back_addr: _,
                }) => {
                    debug!(connection_id = ?connection_id, local_addr = %local_addr, "New incoming connection");
                    // Connection is automatically accepted in libp2p 0.56.0
                }
                Some(SwarmEvent::IncomingConnectionError { .. }) => {
                    debug!("Incoming connection error");
                }
                Some(SwarmEvent::Dialing { peer_id, .. }) => {
                    debug!(peer_id = ?peer_id, "Dialing peer");
                }
                Some(SwarmEvent::ListenerClosed { .. }) => {
                    debug!("Listener closed");
                }
                Some(SwarmEvent::ListenerError { .. }) => {
                    debug!("Listener error");
                }
                // Note: Protocol upgrade handling will be implemented through the transport layer
                // For now, we'll log that wetware protocol is available
                _ => {}
            }
        }
    }

    pub fn add_peer_to_routing_table(&mut self, peer_id: &PeerId, peer_addr: &Multiaddr) {
        self.swarm
            .behaviour_mut()
            .kad
            .add_address(peer_id, peer_addr.clone());
    }

    /// Get the default protocol identifier
    pub fn get_default_protocol(&self) -> &str {
        crate::rpc::WW_PROTOCOL
    }

    /// Handle incoming wetware protocol streams
    /// This method processes incoming connections and upgrades them to wetware streams
    #[allow(dead_code)]
    pub async fn handle_wetware_stream(
        &mut self,
        stream: libp2p::Stream,
        peer_id: PeerId,
    ) -> Result<()> {
        let span = tracing::debug_span!("handle_wetware_stream", peer_id = %peer_id);
        let _enter = span.enter();

        debug!("Processing wetware stream from peer");

        // Create our stream adapter to bridge libp2p::Stream to tokio::io traits
        let stream_adapter = crate::rpc::Libp2pStreamAdapter::new(stream);

        // Create a wetware stream for RPC processing
        let mut wetware_stream = crate::rpc::Stream::new(stream_adapter);

        // Create a membrane for this connection
        #[allow(clippy::arc_with_non_send_sync)]
        let membrane = std::sync::Arc::new(std::sync::Mutex::new(crate::membrane::Membrane::new()));

        // Create RPC server to handle requests
        let mut rpc_server = crate::rpc::DefaultServer::new(membrane);

        debug!("Wetware stream processing started");

        // Process RPC requests in a loop
        loop {
            match wetware_stream.receive_capnp_message().await {
                Ok(Some(request_data)) => {
                    debug!("Received RPC request, processing...");
                    match rpc_server.process_rpc_request(&request_data).await {
                        Ok(response_data) => {
                            debug!("Sending RPC response");
                            if let Err(e) = wetware_stream.send_capnp_message(&response_data).await
                            {
                                warn!(reason = ?e, "Failed to send RPC response");
                                break;
                            }
                        }
                        Err(e) => {
                            warn!(reason = ?e, "Failed to process RPC request");
                            break;
                        }
                    }
                }
                Ok(None) => {
                    debug!("Stream closed by peer");
                    break;
                }
                Err(e) => {
                    warn!(reason = ?e, "Error receiving RPC request");
                    break;
                }
            }
        }

        debug!("Wetware stream processing completed");
        Ok(())
    }

    /// Initiate a wetware protocol connection to a peer
    /// This method opens a new stream to a peer and upgrades it to the wetware protocol
    #[allow(dead_code)]
    pub async fn connect_wetware_protocol(&mut self, peer_id: PeerId) -> Result<()> {
        let span = tracing::debug_span!("connect_wetware_protocol", peer_id = %peer_id);
        let _enter = span.enter();

        debug!("Initiating wetware protocol connection to peer");

        // Open a new stream to the peer with our wetware protocol
        let stream_protocol = libp2p::swarm::StreamProtocol::new(crate::rpc::WW_PROTOCOL);

        // In libp2p 0.56.0, we need to use the swarm to request streams
        // For now, we'll log that we would open a stream
        debug!("Stream protocol: {}", stream_protocol);
        debug!("Would open wetware stream to peer (implementation pending)");

        // TODO: Implement actual stream opening with libp2p 0.56.0 API
        // This would require implementing the full NetworkBehaviour with stream handling

        Ok(())
    }
}

/// Build a libp2p host with IPFS-compatible protocols and enhanced features
pub async fn build_host(
    config: Option<HostConfig>,
) -> Result<(identity::Keypair, PeerId, Swarm<WetwareBehaviour>)> {
    let config = config.unwrap_or_default();
    let span = tracing::debug_span!("build_host");
    let _enter = span.enter();

    // Generate Ed25519 keypair
    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(keypair.public());

    debug!(peer_id = %peer_id, "Generated Ed25519 keypair");

    // Create Kademlia behaviour with IPFS-compatible protocol
    let kademlia_config =
        libp2p::kad::Config::new(libp2p::swarm::StreamProtocol::new(IPFS_KADEMLIA_PROTOCOL));

    // Removed periodic bootstrap logic - always use manual bootstrap only
    debug!("Using manual DHT bootstrap only");

    debug!("Created Kademlia configuration");

    let mut kademlia = libp2p::kad::Behaviour::with_config(
        peer_id,
        libp2p::kad::store::MemoryStore::new(peer_id),
        kademlia_config,
    );

    // Set Kademlia to client mode (we're not a bootstrap node)
    kademlia.set_mode(Some(libp2p::kad::Mode::Client));

    debug!("Set Kademlia to client mode");

    // Create network behaviour with IPFS-compatible protocols
    let behaviour = WetwareBehaviour {
        kad: kademlia,
        identify: libp2p::identify::Behaviour::new(
            libp2p::identify::Config::new(IPFS_IDENTIFY_PROTOCOL.to_string(), keypair.public())
                .with_agent_version("ww/1.0.0".to_string()),
        ),
        wetware: crate::rpc::ProtocolBehaviour::new(),
    };

    // Use SwarmBuilder to create a swarm with enhanced transport
    let swarm_builder = libp2p::SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        .with_tcp(
            Default::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )?
        .with_behaviour(|_| behaviour)?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)));

    // Build the swarm with the configured behaviour
    let mut swarm = swarm_builder.build();

    debug!("Built libp2p swarm with enhanced features");

    // Note: Protocol upgrade registration needs to be handled differently in libp2p 0.56.0
    // For now, we'll log that the wetware protocol is available
    let wetware_protocol = libp2p::swarm::StreamProtocol::new(crate::rpc::WW_PROTOCOL);
    debug!("Wetware protocol available: {}", wetware_protocol);

    // Listen on configured addresses or use defaults
    let listen_addrs: Vec<Multiaddr> = config
        .listen_addrs
        .iter()
        .map(|addr_str| addr_str.parse())
        .collect::<Result<Vec<_>, _>>()?;

    if listen_addrs.is_empty() {
        warn!("No listen addresses configured, using defaults");
        // Use default addresses if none configured
        let default_ipv4: Multiaddr = "/ip4/0.0.0.0/tcp/2020".parse()?;
        let default_ipv6: Multiaddr = "/ip6/::/tcp/2020".parse()?;

        swarm.listen_on(default_ipv4.clone())?;
        debug!(listen_addr = %default_ipv4, "Started listening on default IPv4");

        swarm.listen_on(default_ipv6.clone())?;
        debug!(listen_addr = %default_ipv6, "Started listening on default IPv6");

        info!("🌐 Wetware node listening on (defaults):");
        info!("   IPv4: {}/p2p/{}", default_ipv4, peer_id);
        info!("   IPv6: {}/p2p/{}", default_ipv6, peer_id);
    } else {
        // Use configured addresses
        for addr in &listen_addrs {
            swarm.listen_on(addr.clone())?;
            debug!(listen_addr = %addr, "Started listening on configured address");
        }

        info!("🌐 Wetware node listening on (configured):");
        for addr in &listen_addrs {
            info!("   {}/p2p/{}", addr, peer_id);
        }
    }

    debug!("Host setup completed with configuration: {:?}", config);

    Ok((keypair, peer_id, swarm))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wetware_protocol_identifier() {
        let protocol = crate::rpc::WW_PROTOCOL;
        assert_eq!(protocol, "/ww/0.1.0");
    }

    #[test]
    fn test_protocol_upgrade_creation() {
        use libp2p::core::upgrade::UpgradeInfo;
        let upgrade = crate::rpc::DefaultProtocolUpgrade::new();
        let mut protocol_info = upgrade.protocol_info();
        assert_eq!(protocol_info.next().unwrap().as_ref(), "/ww/0.1.0");
    }

    #[tokio::test]
    async fn test_swarm_manager_protocol_methods() {
        // Test that SwarmManager methods work correctly
        let config = HostConfig::default();
        let (_keypair, peer_id, swarm) = build_host(Some(config)).await.unwrap();
        let swarm_manager = SwarmManager::new(swarm, peer_id);

        // Test protocol identifier
        assert_eq!(swarm_manager.get_default_protocol(), "/ww/0.1.0");
    }
}
