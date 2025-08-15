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
use std::sync::Arc;
use std::sync::Mutex;

use crate::config::HostConfig;
use crate::rpc::DefaultStreamHandler;
use crate::membrane::Membrane;

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
    // TODO: Add wetware protocol when DefaultProtocolBehaviour implements NetworkBehaviour
    // wetware: crate::rpc::DefaultProtocolBehaviour,
}

pub struct SwarmManager {
    swarm: Swarm<WetwareBehaviour>,
    #[allow(dead_code)] // TODO:  remove once we start using the peer_id
    peer_id: PeerId,
    /// Default protocol handler for managing RPC connections
    wetware_handler: DefaultStreamHandler,
}

impl SwarmManager {
    pub fn new(swarm: Swarm<WetwareBehaviour>, peer_id: PeerId) -> Self {
        debug!(peer_id = %peer_id, "Creating new SwarmManager");
        Self {
            swarm,
            peer_id,
            wetware_handler: DefaultStreamHandler::new(),
        }
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
                Some(SwarmEvent::Behaviour(WetwareBehaviourEvent::Identify(event))) => match event {
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
                },
                // TODO: Add wetware protocol event handling when it's implemented

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

    /// Get the default protocol handler
    pub fn get_default_handler(&self) -> &DefaultStreamHandler {
        &self.wetware_handler
    }

    /// Get the default protocol handler mutably
    pub fn get_default_handler_mut(&mut self) -> &mut DefaultStreamHandler {
        &mut self.wetware_handler
    }

    /// Get the default protocol identifier
    pub fn get_default_protocol(&self) -> &str {
        crate::rpc::WW_PROTOCOL
    }

    /// Handle default protocol stream
    pub async fn handle_default_stream(
        &mut self,
        connection_id: libp2p::swarm::ConnectionId,
        stream: libp2p::Stream,
    ) -> Result<()> {
        use crate::rpc::DefaultStream;

        // Create default stream wrapper
        let _default_stream = DefaultStream::new(stream);

        // Handle the stream in the default protocol behaviour
        // For now, we'll just log that we received it
        info!(
            "Received default protocol stream on connection {}",
            connection_id
        );

        // TODO: Process the stream and handle RPC requests

        Ok(())
    }

    /// Handle wetware protocol stream and create RPC connection with importer capability
    pub async fn handle_wetware_stream(&mut self, stream: libp2p::Stream) -> Result<()> {
        debug!("Handling wetware protocol stream");
        
        // Create a membrane for this connection
        let membrane = Arc::new(Mutex::new(Membrane::new()));
        
        // Create RPC server with the membrane
        let rpc_server = crate::rpc::SimpleRpcServer::new(membrane);
        
        // TODO: Set up the stream handling and RPC processing
        // For now, just log that we received a stream
        info!("Wetware stream received, RPC server created with importer capability");
        
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
        // TODO: Add wetware protocol when DefaultProtocolBehaviour implements NetworkBehaviour
        // wetware: crate::rpc::DefaultProtocolBehaviour::new(),
    };

    // Use SwarmBuilder to create a swarm with enhanced transport
    let swarm_builder = libp2p::SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        .with_tcp(
            Default::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )?;

    // Build the swarm with the configured behaviour
    let mut swarm = swarm_builder.with_behaviour(|_| behaviour).unwrap().build();

    debug!("Built libp2p swarm with enhanced features");

    // Listen on all interfaces with random port for TCP
    let tcp_listen_addr: Multiaddr = "/ip4/0.0.0.0/tcp/0".parse()?;
    swarm.listen_on(tcp_listen_addr.clone())?;
    debug!(listen_addr = %tcp_listen_addr, "Started listening on TCP");

    debug!("Host setup completed with configuration: {:?}", config);

    Ok((keypair, peer_id, swarm))
}
