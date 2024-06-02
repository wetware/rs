pub mod net;

use core::ops::{Deref, DerefMut};

use futures::stream::SelectNextSome;
use futures::StreamExt;
use libp2p::{swarm, Swarm};

pub struct DefaultSwarm(pub swarm::Swarm<DefaultBehaviour>);

impl DefaultSwarm {
    // Forward tge call to the inner Swarm.
    pub fn select_next_some(&mut self) -> SelectNextSome<'_, Swarm<DefaultBehaviour>> {
        self.0.select_next_some()
    }
}

// Required to use DefaultSwarm as Swarm in our modules.
impl Deref for DefaultSwarm {
    type Target = swarm::Swarm<DefaultBehaviour>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// Required to use DefaultSwarm as Swarm in our modules.
impl DerefMut for DefaultSwarm {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl net::Dialer for DefaultSwarm {
    // Forward the call to the inner Swarm.
    fn dial(&mut self, opts: swarm::dial_opts::DialOpts) -> Result<(), swarm::DialError> {
        self.0.dial(opts)
    }
}

#[derive(swarm::NetworkBehaviour)]
#[behaviour(to_swarm = "DefaultBehaviourEvent")]
pub struct DefaultBehaviour {
    pub mdns: libp2p::mdns::tokio::Behaviour,
    pub ping: libp2p::ping::Behaviour,
    pub kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
    pub identify: libp2p::identify::Behaviour,
}

// Events explicitly managed or intercepted by the DefaultBehaviour.
#[derive(Debug)]
pub enum DefaultBehaviourEvent {
    Mdns(libp2p::mdns::Event),
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
