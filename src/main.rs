use std::{error::Error, time::Duration};

use anyhow::Result;
use futures::StreamExt;
use libp2p::{
    identity, mdns, noise, ping,
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, PeerId,
};
use tracing_subscriber::EnvFilter;

// Combine multiple behaviours into one.
#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "BehaviourEvent")]
struct Behaviour {
    mdns: mdns::tokio::Behaviour,
    ping: ping::Behaviour,
}

#[derive(Debug)]
enum BehaviourEvent {
    // Events emitted by the Mdns behaviour.
    Mdns(mdns::Event),
    // Events emitted by the Ping behaviour.
    Ping(ping::Event),
}

impl From<mdns::Event> for BehaviourEvent {
    fn from(event: mdns::Event) -> Self {
        BehaviourEvent::Mdns(event)
    }
}

impl From<ping::Event> for BehaviourEvent {
    fn from(event: ping::Event) -> Self {
        BehaviourEvent::Ping(event)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .try_init();

    // Create a MDNS network behaviour.
    let id_keys = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(id_keys.public());
    let mdns_behaviour = mdns::tokio::Behaviour::new(mdns::Config::default(), peer_id)?;

    // Create Stream behaviour.
    let ping_behaviour = ping::Behaviour::default();

    // Combine behaviours.
    let behaviour = Behaviour {
        mdns: mdns_behaviour,
        ping: ping_behaviour,
    };

    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(id_keys)
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|_| behaviour)?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(u64::MAX)))
        .build();

    // Tell the swarm to listen on all interfaces and a random, OS-assigned
    // port.
    swarm.listen_on("/ip4/0.0.0.0/tcp/0".parse()?)?;

    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => println!("Listening on {address:?}"),
            SwarmEvent::Behaviour(BehaviourEvent::Mdns(event)) => {
                println!("mdns: {event:?}");
                match event {
                    mdns::Event::Discovered(peers) => {
                        for (peer_id, addr) in peers {
                            let result = swarm.dial(addr);
                            match result {
                                Ok(_) => println!("Dialed peer: {peer_id}"),
                                Err(e) => println!("Failed to dial peer: {e}"),
                            }
                        }
                    }
                    mdns::Event::Expired(_) => {}
                }
            }
            SwarmEvent::Behaviour(BehaviourEvent::Ping(event)) => {
                println!("ping: {event:?}")
            }
            event => {
                println!("other: {event:?}")
            }
        }
    }
}
