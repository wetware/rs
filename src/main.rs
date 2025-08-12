use anyhow::{anyhow, Result};
use clap::Parser;
use futures::StreamExt;
use libp2p::{
    identity,
    kad::{Event as KademliaEvent, QueryResult, RecordKey},
    swarm::{NetworkBehaviour, Swarm, SwarmEvent},
    Multiaddr, PeerId,
};
use serde_json::Value;
use std::time::Duration;
use tracing::{debug, error, info, warn};

// IPFS protocol constants for DHT compatibility
// These protocols ensure our node can communicate with the IPFS network
const IPFS_KADEMLIA_PROTOCOL: &str = "/ipfs/kad/1.0.0";  // Standard IPFS DHT protocol
const IPFS_IDENTIFY_PROTOCOL: &str = "/ipfs/id/1.0.0";    // Standard IPFS identify protocol

#[derive(Parser)]
#[command(name = "ww")]
#[command(
    about = "P2P sandbox for Web3 applications that execute untrusted code on public networks."
)]
struct Args {
    /// Kubo node HTTP API endpoint (e.g., http://127.0.0.1:5001)
    #[arg(required = true)]
    kubo_url: String,
}

/// Query a Kubo node to get its peer list for DHT bootstrap
/// This function retrieves the list of peers that our local Kubo node is connected to,
/// which we'll use to bootstrap our DHT routing table and establish connections.
async fn get_kubo_peers(kubo_url: &str) -> Result<Vec<(PeerId, Multiaddr)>> {
    let span = tracing::info_span!("get_kubo_peers", kubo_url = kubo_url);
    let _enter = span.enter();

    let client = reqwest::Client::new();
    let url = format!("{}/api/v0/swarm/peers", kubo_url);

    info!(url = %url, "Querying Kubo node for peers");

    let response = client.post(&url).send().await?;
    if !response.status().is_success() {
        error!(status = %response.status(), "Failed to get peers from Kubo");
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
        warn!(
            parse_errors = parse_errors,
            "Some peer addresses could not be parsed"
        );
    }

    info!(
        peer_count = peer_addrs.len(),
        parse_errors = parse_errors,
        "Found peer addresses from Kubo node"
    );
    Ok(peer_addrs)
}

#[derive(NetworkBehaviour)]
struct AppBehaviour {
    kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
    identify: libp2p::identify::Behaviour,
}

struct SwarmManager {
    swarm: Swarm<AppBehaviour>,
    #[allow(dead_code)] // TODO:  remove once we start using the peer_id
    peer_id: PeerId,
}

impl SwarmManager {
    fn new(swarm: Swarm<AppBehaviour>, peer_id: PeerId) -> Self {
        info!(peer_id = %peer_id, "Creating new SwarmManager");
        Self { swarm, peer_id }
    }

    /// Bootstrap the DHT by connecting to IPFS peers and triggering the bootstrap process
    /// This function establishes connections to the provided peers and then triggers
    /// the Kademlia bootstrap process to populate our routing table.
    async fn bootstrap_dht(&mut self, bootstrap_peers: Vec<(PeerId, Multiaddr)>) -> Result<()> {
        let span = tracing::info_span!("bootstrap_dht", peer_count = bootstrap_peers.len());
        let _enter = span.enter();

        info!("Bootstrapping DHT with IPFS peers");

        // Peers are already added to routing table, now dial them to establish connections
        for (peer_id, peer_addr) in &bootstrap_peers {
            if let Err(e) = self.swarm.dial(peer_addr.clone()) {
                warn!(peer_id = %peer_id, peer_addr = %peer_addr, error = ?e, "Failed to dial IPFS peer");
            } else {
                info!(peer_id = %peer_id, peer_addr = %peer_addr, "Dialing IPFS peer");
            }
        }

        // Wait for connections to be established
        info!("Waiting for peer connections to establish...");
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Now trigger bootstrap after we have actual peer connections
        info!("Triggering DHT bootstrap with IPFS peers");
        let _ = self.swarm.behaviour_mut().kad.bootstrap();

        Ok(())
    }

    async fn announce_provider(&mut self, key: &str) -> Result<()> {
        let span = tracing::info_span!("announce_provider", key = key);
        let _enter = span.enter();

        let record_key = RecordKey::new(&key.as_bytes());
        let _ = self.swarm.behaviour_mut().kad.start_providing(record_key);
        info!("Started providing key");
        Ok(())
    }

    async fn query_providers(&mut self, key: &str) -> Result<()> {
        let span = tracing::info_span!("query_providers", key = key);
        let _enter = span.enter();

        let record_key = RecordKey::new(&key.as_bytes());
        self.swarm.behaviour_mut().kad.get_providers(record_key);
        info!("Querying providers for key");
        Ok(())
    }

    async fn run_event_loop(&mut self) -> Result<()> {
        let span = tracing::info_span!("event_loop");
        let _enter = span.enter();

        info!("Starting DHT event loop");

        loop {
            match self.swarm.next().await {
                Some(SwarmEvent::Behaviour(AppBehaviourEvent::Kad(event))) => match event {
                    KademliaEvent::OutboundQueryProgressed { result, .. } => match result {
                        QueryResult::GetProviders(Ok(providers_result)) => match providers_result {
                            libp2p::kad::GetProvidersOk::FoundProviders { providers, .. } => {
                                info!(provider_count = providers.len(), "Found providers");
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
                            error!(error = ?e, "Failed to get providers");
                        }
                        QueryResult::StartProviding(Ok(_)) => {
                            info!("Successfully started providing key");
                        }
                        QueryResult::StartProviding(Err(e)) => {
                            error!(error = ?e, "Failed to start providing");
                        }
                        QueryResult::Bootstrap(Ok(_)) => {
                            info!("DHT bootstrap completed successfully");
                        }
                        QueryResult::Bootstrap(Err(e)) => {
                            error!(error = ?e, "DHT bootstrap failed");
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
                Some(SwarmEvent::Behaviour(AppBehaviourEvent::Identify(event))) => match event {
                    libp2p::identify::Event::Received { peer_id, info, .. } => {
                        info!(peer_id = %peer_id, listen_addrs = ?info.listen_addrs, "Received identify info from peer");
                        // Don't add peers to Kademlia here - we already added them upfront
                        // This is just for logging peer discovery
                    }
                    libp2p::identify::Event::Sent { peer_id, .. } => {
                        debug!(peer_id = %peer_id, "Sent identify info to peer");
                    }
                    libp2p::identify::Event::Error { peer_id, error, .. } => {
                        warn!(peer_id = %peer_id, error = ?error, "Identify error with peer");
                    }
                    _ => {}
                },
                Some(SwarmEvent::NewListenAddr { address, .. }) => {
                    info!(address = %address, "Listening on address");
                }
                Some(SwarmEvent::ConnectionEstablished { peer_id, .. }) => {
                    info!(peer_id = %peer_id, "Connected to IPFS peer");
                }
                Some(SwarmEvent::ConnectionClosed { peer_id, .. }) => {
                    info!(peer_id = %peer_id, "Disconnected from IPFS peer");
                }
                Some(SwarmEvent::OutgoingConnectionError { peer_id, error, .. }) => {
                    if let Some(peer_id) = peer_id {
                        warn!(peer_id = %peer_id, error = ?error, "Failed to connect to IPFS peer");
                    } else {
                        warn!(error = ?error, "Failed to establish outgoing connection");
                    }
                }
                _ => {}
            }
        }
    }
}

/// Build a libp2p host with IPFS-compatible protocols
/// This function creates a libp2p swarm configured to use standard IPFS protocols
/// for Kademlia DHT and identify, ensuring compatibility with the IPFS network.
async fn build_host() -> Result<(identity::Keypair, PeerId, Swarm<AppBehaviour>)> {
    let span = tracing::info_span!("build_host");
    let _enter = span.enter();

    // Generate Ed25519 keypair
    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(keypair.public());

    info!(peer_id = %peer_id, "Generated Ed25519 keypair");

    // Create Kademlia behaviour with IPFS-compatible protocol
    // Using the standard IPFS DHT protocol ensures we can communicate with IPFS nodes
    let mut kademlia_config =
        libp2p::kad::Config::new(libp2p::swarm::StreamProtocol::new(IPFS_KADEMLIA_PROTOCOL));
    // Don't set periodic bootstrap - we'll do it manually when ready
    kademlia_config.set_periodic_bootstrap_interval(None);

    info!("Created Kademlia configuration");

    let mut kademlia = libp2p::kad::Behaviour::with_config(
        peer_id,
        libp2p::kad::store::MemoryStore::new(peer_id),
        kademlia_config,
    );

    // Set Kademlia to client mode (we're not a bootstrap node)
    kademlia.set_mode(Some(libp2p::kad::Mode::Client));

    info!("Set Kademlia to client mode");

    // Create network behaviour with IPFS-compatible protocols
    let behaviour = AppBehaviour {
        kad: kademlia,
        identify: libp2p::identify::Behaviour::new(
            libp2p::identify::Config::new(IPFS_IDENTIFY_PROTOCOL.to_string(), keypair.public())
                .with_agent_version("ww/1.0.0".to_string()),
        ),
    };

    // Use SwarmBuilder to create a swarm with Kademlia
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        .with_tcp(
            Default::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )?
        .with_behaviour(|_| behaviour)
        .unwrap()
        .build();

    info!("Built libp2p swarm");

    // Listen on all interfaces with random port
    let listen_addr: Multiaddr = "/ip4/0.0.0.0/tcp/0".parse()?;
    swarm.listen_on(listen_addr.clone())?;

    info!(listen_addr = %listen_addr, "Started listening on address");

    Ok((keypair, peer_id, swarm))
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing subscriber with better configuration
    let env_filter = tracing_subscriber::EnvFilter::try_from_env("WW_LOGLVL")
        .unwrap_or_else(|_| "ww=info,libp2p=debug".into());

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true)
        .init();

    let args = Args::parse();

    let start_time = std::time::Instant::now();
    info!("Starting basic-p2p application");
    info!(kubo_url = %args.kubo_url, "Bootstrap Kubo node");

    // 1. Query Kubo node to get its peers for DHT bootstrap
    let kubo_start = std::time::Instant::now();
    let kubo_peers = get_kubo_peers(&args.kubo_url).await?;
    let kubo_duration = kubo_start.elapsed();

    if kubo_peers.is_empty() {
        warn!("No peers found from Kubo node");
    } else {
        info!(peer_count = kubo_peers.len(), "Found peers from Kubo node");
        for (i, (peer_id, peer_addr)) in kubo_peers.iter().enumerate() {
            debug!(index = i + 1, peer_id = %peer_id, peer_addr = %peer_addr, "Peer details");
        }
    }
    info!(
        duration_ms = kubo_duration.as_millis(),
        "Kubo peer discovery completed"
    );

    // 2. Build the host
    let host_start = std::time::Instant::now();
    let (_keypair, peer_id, mut swarm) = build_host().await?;
    let host_duration = host_start.elapsed();

    info!(peer_id = %peer_id, "Local PeerId");
    
    // Add IPFS peers to Kademlia routing table for DHT bootstrap
    // This populates our routing table with known good peers before we start connecting
    info!("Adding {} IPFS peers to Kademlia routing table", kubo_peers.len());
    for (peer_id, peer_addr) in &kubo_peers {
        swarm.behaviour_mut().kad.add_address(peer_id, peer_addr.clone());
        debug!(peer_id = %peer_id, peer_addr = %peer_addr, "Added IPFS peer to routing table");
    }
    
    // Wait a bit for the swarm to start and collect listen addresses
    let mut listen_addrs = Vec::new();
    let mut attempts = 0;

    while attempts < 10 && listen_addrs.is_empty() {
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Process swarm events to collect listen addresses
        while let Some(event) = swarm.next().await {
            match event {
                SwarmEvent::NewListenAddr { address, .. } => {
                    listen_addrs.push(address);
                }
                SwarmEvent::Behaviour(_) => {
                    // Handle behaviour events
                }
                SwarmEvent::ConnectionEstablished { .. } => {
                    // Handle connection events
                }
                SwarmEvent::ConnectionClosed { .. } => {
                    // Handle disconnection events
                }
                _ => {}
            }
        }
        attempts += 1;
    }
    
    if listen_addrs.is_empty() {
        warn!(
            attempts = attempts,
            "No listen addresses collected after attempts"
        );
    } else {
        info!(listen_addresses = ?listen_addrs, attempts = attempts, "Collected listen addresses");
    }
    info!(
        duration_ms = host_duration.as_millis(),
        "Host setup completed"
    );

    // 3. Create SwarmManager and bootstrap DHT
    let mut swarm_manager = SwarmManager::new(swarm, peer_id);

    // Bootstrap DHT with peers from Kubo
    let bootstrap_start = std::time::Instant::now();
    swarm_manager.bootstrap_dht(kubo_peers).await?;
    let bootstrap_duration = bootstrap_start.elapsed();
    info!(
        duration_ms = bootstrap_duration.as_millis(),
        "DHT bootstrap completed"
    );

    // Announce ourselves as a provider of "ww"
    let announce_start = std::time::Instant::now();
    swarm_manager.announce_provider("ww").await?;
    let announce_duration = announce_start.elapsed();
    info!(
        duration_ms = announce_duration.as_millis(),
        "Provider announcement completed"
    );

    // Query for providers of "ww" to see if we can find ourselves
    let query_start = std::time::Instant::now();
    swarm_manager.query_providers("ww").await?;
    let query_duration = query_start.elapsed();
    info!(
        duration_ms = query_duration.as_millis(),
        "Provider query completed"
    );

    // 4. Run the DHT event loop
    info!("Starting DHT event loop");
    swarm_manager.run_event_loop().await?;

    let total_duration = start_time.elapsed();
    info!(
        total_duration_ms = total_duration.as_millis(),
        "Application completed successfully"
    );

    info!("Application ready! Successfully joined the IPFS DHT network");
    info!("We can now successfully:");
    info!("  - Parse CLI arguments and connect to Kubo HTTP API");
    info!("  - Extract peer information from Kubo node");
    info!("  - Generate libp2p identity with Ed25519 keypair");
    info!("  - Start listening on network with IPFS-compatible protocols");
    info!("  - Bootstrap DHT using peers from Kubo node");
    info!("  - Participate in IPFS DHT operations (provide/query)");

    Ok(())
}
