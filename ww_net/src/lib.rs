pub mod net;

use libp2p::swarm;

#[derive(swarm::NetworkBehaviour)]
#[behaviour(to_swarm = "DefaultBehaviourEvent")]
pub struct DefaultBehaviour {
    pub mdns: libp2p::mdns::tokio::Behaviour,
    pub ping: libp2p::ping::Behaviour,
    pub kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
    pub identify: libp2p::identify::Behaviour,
}

#[derive(Debug)]
pub enum DefaultBehaviourEvent {
    // Events emitted by the MDNS behaviour.
    Mdns(libp2p::mdns::Event),
    // Events emitted by the Ping behaviour.
    Ping(libp2p::ping::Event),
    Kad(libp2p::kad::Event),
    Identify(libp2p::identify::Event),
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

impl From<libp2p::kad::Event> for DefaultBehaviourEvent {
    fn from(event: libp2p::kad::Event) -> Self {
        DefaultBehaviourEvent::Kad(event)
    }
}

impl From<libp2p::identify::Event> for DefaultBehaviourEvent {
    fn from(event: libp2p::identify::Event) -> Self {
        DefaultBehaviourEvent::Identify(event)
    }
}
