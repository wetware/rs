use std::collections::HashMap;
use tracing;

use libp2p::{
    mdns,
    swarm::{self, dial_opts::DialOpts},
    Multiaddr, PeerId,
};

pub trait Dialer {
    fn dial(&mut self, opts: DialOpts) -> Result<(), swarm::DialError>;
}

pub fn default_mdns_handler(d: &mut dyn Dialer, event: mdns::Event) {
    match event {
        mdns::Event::Discovered(peers) => {
            handle_discovered(d, peers);
        }

        mdns::Event::Expired(_) => {
            // ignore
        }
    }
}

fn handle_discovered(d: &mut dyn Dialer, peers: Vec<(PeerId, Multiaddr)>) {
    // build a map[PeerId] -> Vec<Multiaddr>
    let mut peers_map: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();
    for (peer, addr) in peers {
        peers_map.entry(peer).or_insert_with(Vec::new).push(addr);
    }

    // iterate through the hashmap and dial each peer
    for (peer, addrs) in peers_map {
        let opts = DialOpts::peer_id(peer).addresses(addrs).build();

        let result = d.dial(opts);
        match result {
            Ok(_) => tracing::info!("Dialed peer: {peer}"),
            Err(e) => tracing::debug!("Failed to dial peer: {e}"),
        }
    }
}
