use std::{error::Error, time::Duration};

use anyhow::Result;
use libp2p::{identify, kad, mdns, noise, ping, swarm, tcp, yamux};
use tracing_subscriber::EnvFilter;

use ww_net;

pub mod cfg;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Use the default configuration.
    let config: &dyn cfg::Cfg = &cfg::DefaultCfg::new();

    // Start configuring a `fmt` subscriber
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .compact() // use abbreviated log format
        // .with_file(true)
        // .with_thread_ids(true)
        .with_max_level(tracing::Level::INFO)
        .finish();

    // Set the subscriber as global default
    tracing::subscriber::set_global_default(subscriber).unwrap();

    // Create a MDNS network behaviour.
    let mdns_behaviour = mdns::tokio::Behaviour::new(mdns::Config::default(), config.peer_id())?;

    // Create Stream behaviour.
    let ping_behaviour = ping::Behaviour::default();

    // Create Kademlia and Identify behaviours.
    let kad_cfg = kad::Config::default();
    let kad_store = kad::store::MemoryStore::new(config.id_keys().public().to_peer_id());
    let kad_behaviour =
        kad::Behaviour::with_config(config.id_keys().public().to_peer_id(), kad_store, kad_cfg);
    let identify_behaviour = identify::Behaviour::new(identify::Config::new(
        config.identify_protocol(),
        config.id_keys().public(),
    ));

    // Combine behaviours.
    let behaviour = ww_net::DefaultBehaviour {
        mdns: mdns_behaviour,
        ping: ping_behaviour,
        kad: kad_behaviour,
        identify: identify_behaviour,
    };

    let raw_swarm = libp2p::SwarmBuilder::with_existing_identity(config.id_keys())
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|_| behaviour)?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(u64::MAX)))
        .build();

    // Wrap the swarm in our custom type to overwrite its behaviour and event management.
    let mut swarm = ww_net::DefaultSwarm(raw_swarm);

    // Set the Kademlia mode.
    swarm.behaviour_mut().kad.set_mode(Some(config.kad_mode()));

    // Tell the swarm to listen on all interfaces and a random, OS-assigned port.
    swarm.listen_on(config.listen_addr().parse()?)?;

    loop {
        match swarm.select_next_some().await {
            swarm::SwarmEvent::NewListenAddr { address, .. } => {
                // TODO:  seal & sign a PeerRecord and announce it to the DHT,
                // using our PeerID as the key.
                tracing::info!("listening on {address:?}")
            }
            swarm::SwarmEvent::Behaviour(ww_net::DefaultBehaviourEvent::Mdns(event)) => {
                ww_net::net::default_mdns_handler(&mut swarm, event);
            }
            swarm::SwarmEvent::Behaviour(ww_net::DefaultBehaviourEvent::Ping(event)) => {
                tracing::info!("got PING event: {event:?}");
            }
            swarm::SwarmEvent::Behaviour(ww_net::DefaultBehaviourEvent::Kad(event)) => {
                tracing::info!("got KAD event: {event:?}");
            }
            swarm::SwarmEvent::Behaviour(ww_net::DefaultBehaviourEvent::Identify(event)) => {
                tracing::info!("got IDENTIFY event: {event:?}");
            }
            event => {
                tracing::info!("got event: {event:?}");
            }
        }
    }
}
