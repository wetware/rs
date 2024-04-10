pub mod mdns;

use libp2p;

#[derive(libp2p::swarm::NetworkBehaviour)]
#[behaviour(to_swarm = "DefaultBehaviourEvent")]
pub struct DefaultBehaviour {
    pub mdns: libp2p::mdns::tokio::Behaviour,
    pub ping: libp2p::ping::Behaviour,
}

#[derive(Debug)]
pub enum DefaultBehaviourEvent {
    // Events emitted by the MDNS behaviour.
    Mdns(libp2p::mdns::Event),
    // Events emitted by the Ping behaviour.
    Ping(libp2p::ping::Event),
}

impl From<libp2p::mdns::Event> for DefaultBehaviourEvent {
    fn from(event: libp2p::mdns::Event) -> Self {
        DefaultBehaviourEvent::Mdns(event)
    }
}

impl From<libp2p::ping::Event> for DefaultBehaviourEvent {
    fn from(event: libp2p::ping::Event) -> Self {
        DefaultBehaviourEvent::Ping(event)
    }
}
