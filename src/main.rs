use anyhow::{anyhow, Result};
use libp2p::{
    identity,
    swarm::{NetworkBehaviour, Swarm, SwarmEvent},
    Multiaddr, PeerId,
    kad::{Event as KademliaEvent, QueryResult, RecordKey},
};
use serde_json::Value;
use std::time::Duration;
use futures::StreamExt;
use clap::Parser;

#[derive(Parser)]
#[command(name = "basic-p2p")]
#[command(about = "A minimal Rust libp2p app that joins the IPFS DHT via a Kubo node")]
struct Args {
    /// Kubo node HTTP API endpoint (e.g., http://127.0.0.1:5001)
    #[arg(required = true)]
    kubo_url: String,
}

/// Query a Kubo node to get its peer list
async fn get_kubo_peers(kubo_url: &str) -> Result<Vec<(PeerId, Multiaddr)>> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/v0/swarm/peers", kubo_url);
    
    let response = client.post(&url).send().await?;
    if !response.status().is_success() {
        return Err(anyhow!("Failed to get peers from Kubo: {}", response.status()));
    }
    
    let body: Value = response.json().await?;
    let peers = body["Peers"].as_array()
        .ok_or_else(|| anyhow!("Invalid response format from Kubo"))?;
    
    let mut peer_addrs = Vec::new();
    for peer in peers {
        // The Kubo API returns "Addr" (multiaddr) and "Peer" (peer ID) separately
        if let (Some(addr_str), Some(peer_id_str)) = (peer["Addr"].as_str(), peer["Peer"].as_str()) {
            if let (Ok(multiaddr), Ok(peer_id)) = (addr_str.parse::<Multiaddr>(), peer_id_str.parse::<PeerId>()) {
                // Construct the full multiaddr with peer ID
                let full_addr = multiaddr.with(libp2p::multiaddr::Protocol::P2p(peer_id));
                peer_addrs.push((peer_id, full_addr));
            }
        }
    }
    
    println!("Found {} peer addresses from Kubo node", peer_addrs.len());
    Ok(peer_addrs)
}

#[derive(NetworkBehaviour)]
struct AppBehaviour {
    kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
}

struct SwarmManager {
    swarm: Swarm<AppBehaviour>,
    peer_id: PeerId,
}

impl SwarmManager {
    fn new(swarm: Swarm<AppBehaviour>, peer_id: PeerId) -> Self {
        Self { swarm, peer_id }
    }

    async fn bootstrap_dht(&mut self, bootstrap_peers: Vec<(PeerId, Multiaddr)>) -> Result<()> {
        println!("Bootstrapping DHT with {} peers...", bootstrap_peers.len());
        
        for (peer_id, peer_addr) in bootstrap_peers {
            // Add the peer to Kademlia's routing table
            self.swarm.behaviour_mut().kad.add_address(&peer_id, peer_addr.clone());
            println!("Added bootstrap peer {} at {}", peer_id, peer_addr);
            
            // Start a bootstrap query to this peer
            let _ = self.swarm.behaviour_mut().kad.bootstrap();
        }
        Ok(())
    }

    async fn announce_provider(&mut self, key: &str) -> Result<()> {
        let record_key = RecordKey::new(&key.as_bytes());
        let _ = self.swarm.behaviour_mut().kad.start_providing(record_key);
        println!("Started providing key: {}", key);
        Ok(())
    }

    async fn query_providers(&mut self, key: &str) -> Result<()> {
        let record_key = RecordKey::new(&key.as_bytes());
        self.swarm.behaviour_mut().kad.get_providers(record_key);
        println!("Querying providers for key: {}", key);
        Ok(())
    }

    async fn run_event_loop(&mut self) -> Result<()> {
        println!("Starting DHT event loop...");
        
        loop {
            match self.swarm.next().await {
                Some(SwarmEvent::Behaviour(AppBehaviourEvent::Kad(event))) => {
                    match event {
                        KademliaEvent::OutboundQueryProgressed { result, .. } => {
                            match result {
                                QueryResult::GetProviders(Ok(providers_result)) => {
                                    match providers_result {
                                        libp2p::kad::GetProvidersOk::FoundProviders { providers, .. } => {
                                            println!("Found {} providers:", providers.len());
                                            for provider in providers {
                                                println!("  Provider: {}", provider);
                                            }
                                        }
                                        libp2p::kad::GetProvidersOk::FinishedWithNoAdditionalRecord { .. } => {
                                            println!("Finished querying providers with no additional records");
                                        }
                                    }
                                }
                                QueryResult::GetProviders(Err(e)) => {
                                    println!("Failed to get providers: {:?}", e);
                                }
                                QueryResult::StartProviding(Ok(_)) => {
                                    println!("Successfully started providing key");
                                }
                                QueryResult::StartProviding(Err(e)) => {
                                    println!("Failed to start providing: {:?}", e);
                                }
                                QueryResult::Bootstrap(Ok(_)) => {
                                    println!("DHT bootstrap completed successfully!");
                                }
                                QueryResult::Bootstrap(Err(e)) => {
                                    println!("DHT bootstrap failed: {:?}", e);
                                }
                                _ => {}
                            }
                        }
                        KademliaEvent::InboundRequest { request, .. } => {
                            match request {
                                libp2p::kad::InboundRequest::GetProvider { .. } => {
                                    println!("Received GetProvider request");
                                }
                                libp2p::kad::InboundRequest::GetRecord { .. } => {
                                    println!("Received GetRecord request");
                                }
                                _ => {}
                            }
                        }
                        _ => {}
                    }
                }
                Some(SwarmEvent::NewListenAddr { address, .. }) => {
                    println!("Listening on: {}", address);
                }
                Some(SwarmEvent::ConnectionEstablished { peer_id, .. }) => {
                    println!("Connected to peer: {}", peer_id);
                }
                Some(SwarmEvent::ConnectionClosed { peer_id, .. }) => {
                    println!("Disconnected from peer: {}", peer_id);
                }
                _ => {}
            }
        }
    }
}

/// Build a simple libp2p host
async fn build_host() -> Result<(identity::Keypair, PeerId, Vec<Multiaddr>, Swarm<AppBehaviour>)> {
    // Generate Ed25519 keypair
    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(keypair.public());

    // Create Kademlia behaviour with MemoryStore
    let mut kademlia_config = libp2p::kad::Config::new(libp2p::swarm::StreamProtocol::new("/ipfs/kad/1.0.0"));
    kademlia_config.set_periodic_bootstrap_interval(Some(Duration::from_secs(30)));
    
    let mut kademlia = libp2p::kad::Behaviour::with_config(
        peer_id,
        libp2p::kad::store::MemoryStore::new(peer_id),
        kademlia_config,
    );
    
    // Set Kademlia to client mode (we're not a bootstrap node)
    kademlia.set_mode(Some(libp2p::kad::Mode::Client));

    let behaviour = AppBehaviour { kad: kademlia };

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

    // Listen on all interfaces with random port
    let listen_addr: Multiaddr = "/ip4/0.0.0.0/tcp/0".parse()?;
    swarm.listen_on(listen_addr)?;

    // Wait a bit for the swarm to start and collect listen addresses
    let mut listen_addrs = Vec::new();
    let mut attempts = 0;
    
    while attempts < 10 && listen_addrs.is_empty() {
        tokio::time::sleep(Duration::from_millis(100)).await;
        
        // Process swarm events to collect listen addresses
        while let Some(event) = swarm.next().await {
            if let SwarmEvent::NewListenAddr { address, .. } = event {
                listen_addrs.push(address);
            }
        }
        attempts += 1;
    }

    Ok((keypair, peer_id, listen_addrs, swarm))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    
    println!("Starting basic-p2p application...");
    println!("Bootstrap Kubo node: {}", args.kubo_url);

    // 1. Query Kubo node to get its peers
    let kubo_peers = get_kubo_peers(&args.kubo_url).await?;
    if kubo_peers.is_empty() {
        println!("Warning: No peers found from Kubo node");
    } else {
        println!("Found {} peers from Kubo node", kubo_peers.len());
        for (i, (peer_id, peer_addr)) in kubo_peers.iter().enumerate() {
            println!("  Peer {}: {} (ID: {})", i + 1, peer_addr, peer_id);
        }
    }

    // 2. Build the host
    let (_keypair, peer_id, listen_addrs, swarm) = build_host().await?;
    
    println!("Local PeerId: {}", peer_id);
    if listen_addrs.is_empty() {
        println!("Warning: No listen addresses available");
    } else {
        println!("Listening on: {:?}", listen_addrs);
    }

    // 3. Create SwarmManager and bootstrap DHT
    let mut swarm_manager = SwarmManager::new(swarm, peer_id);
    
    // Bootstrap DHT with peers from Kubo
    swarm_manager.bootstrap_dht(kubo_peers).await?;
    
    // Announce ourselves as a provider of "ww"
    swarm_manager.announce_provider("ww").await?;
    
    // Query for providers of "ww" to see if we can find ourselves
    swarm_manager.query_providers("ww").await?;
    
    // 4. Run the DHT event loop
    println!("Starting DHT event loop...");
    swarm_manager.run_event_loop().await?;

    println!("Application ready! This is a basic working version.");
    println!("To add DHT functionality, we need to resolve the libp2p-kad feature issue.");
    println!("For now, we can successfully:");
    println!("  - Parse CLI arguments");
    println!("  - Connect to Kubo HTTP API");
    println!("  - Extract peer information");
    println!("  - Generate libp2p identity");
    println!("  - Start listening on network");

    Ok(())
}
