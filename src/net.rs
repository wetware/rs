use libp2p::{
    mdns, ping,
    swarm::NetworkBehaviour,
};

#[derive(NetworkBehaviour)]
#[behaviour(to_swarm = "DefaultBehaviourEvent")]
pub struct DefaultBehaviour {
    pub mdns: mdns::tokio::Behaviour,
    pub ping: ping::Behaviour,
}

#[derive(Debug)]
pub enum DefaultBehaviourEvent {
    // Events emitted by the MDNS behaviour.
    Mdns(mdns::Event),
    // Events emitted by the Ping behaviour.
    Ping(ping::Event),
}

impl From<mdns::Event> for DefaultBehaviourEvent {
    fn from(event: mdns::Event) -> Self {
        DefaultBehaviourEvent::Mdns(event)
    }
}

impl From<ping::Event> for DefaultBehaviourEvent {
    fn from(event: ping::Event) -> Self {
        DefaultBehaviourEvent::Ping(event)
    }
}
