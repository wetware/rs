use anyhow::{anyhow, Result};
use libp2p::{
    identity,
    swarm::{NetworkBehaviour, Swarm, SwarmEvent},
    Multiaddr, PeerId,
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
async fn get_kubo_peers(kubo_url: &str) -> Result<Vec<Multiaddr>> {
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
        // The Kubo API returns "Addr" (singular) not "Addrs"
        if let Some(addr_str) = peer["Addr"].as_str() {
            if let Ok(multiaddr) = addr_str.parse::<Multiaddr>() {
                peer_addrs.push(multiaddr);
            }
        }
    }
    
    println!("Found {} peer addresses from Kubo node", peer_addrs.len());
    Ok(peer_addrs)
}

/// Build a simple libp2p host
async fn build_host() -> Result<(identity::Keypair, PeerId, Vec<Multiaddr>, Swarm<impl NetworkBehaviour>)> {
    // Generate Ed25519 keypair
    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(keypair.public());

    // Use SwarmBuilder to create a simple swarm
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        .with_tcp(
            Default::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )?
        .with_behaviour(|_| {
            // Use a simple dummy behaviour for now
            libp2p::swarm::dummy::Behaviour
        })
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
        for (i, peer) in kubo_peers.iter().enumerate() {
            println!("  Peer {}: {}", i + 1, peer);
        }
    }

    // 2. Build the host
    let (_keypair, peer_id, listen_addrs, _swarm) = build_host().await?;
    
    println!("Local PeerId: {}", peer_id);
    if listen_addrs.is_empty() {
        println!("Warning: No listen addresses available");
    } else {
        println!("Listening on: {:?}", listen_addrs);
    }

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
