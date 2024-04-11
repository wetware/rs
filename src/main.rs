use std::{error::Error, time::Duration};

use anyhow::Result;
use futures::StreamExt;
use libp2p::{
    PeerId,
    swarm, identity, mdns, noise, ping, tcp, yamux,
    swarm::dial_opts::DialOpts,
};
use tracing_subscriber::EnvFilter;

mod net;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Start configuring a `fmt` subscriber
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .compact()  // use abbreviated log format
        // .with_file(true)
        // .with_thread_ids(true)
        .with_max_level(tracing::Level::INFO)
        .finish();

    // Set the subscriber as global default
    tracing::subscriber::set_global_default(subscriber).unwrap();

    // Create a MDNS network behaviour.
    let id_keys = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(id_keys.public());
    let mdns_behaviour = mdns::tokio::Behaviour::new(mdns::Config::default(), peer_id)?;

    // Create Stream behaviour.
    let ping_behaviour = ping::Behaviour::default();

    // Combine behaviours.
    let behaviour = net::DefaultBehaviour {
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
            swarm::SwarmEvent::NewListenAddr { address, .. } => {
                // TODO:  seal & sign a PeerRecord and announce it to the DHT,
                // using our PeerID as the key.
                tracing::info!("listening on {address:?}")
            }

            swarm::SwarmEvent::Behaviour(net::DefaultBehaviourEvent::Mdns(event)) => {
                net::mdns::default_handler(&mut swarm, event);
            }
        
            swarm::SwarmEvent::Behaviour(net::DefaultBehaviourEvent::Ping(event)) => {
                tracing::info!("got PING event: {event:?}");
            }
            event => {
                tracing::info!("got event: {event:?}");
            }
        }
    }
}

impl net::mdns::Dialer for swarm::Swarm<net::DefaultBehaviour> {
    fn dial(&mut self, opts: DialOpts) -> Result<(), swarm::DialError> {
        return self.dial(opts);
    }
}
