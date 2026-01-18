//! Wetware host runtime: libp2p host + Wasmtime host.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use libp2p::core::connection::ConnectedPoint;
use libp2p::swarm::SwarmEvent;
use libp2p::{identity, Multiaddr, PeerId, SwarmBuilder};
use wasmtime::{Config as WasmConfig, Engine};

use crate::rpc::{NetworkState, PeerInfo};

/// Network behavior for Wetware hosts.
#[derive(libp2p::swarm::NetworkBehaviour)]
pub struct WetwareBehaviour {
    pub kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
    pub identify: libp2p::identify::Behaviour,
}

/// Libp2p host wrapper for Wetware.
pub struct Libp2pHost {
    swarm: libp2p::swarm::Swarm<WetwareBehaviour>,
    local_peer_id: PeerId,
}

impl Libp2pHost {
    /// Create a new libp2p host and start listening on the given TCP port.
    pub fn new(port: u16) -> Result<Self> {
        let keypair = identity::Keypair::generate_ed25519();
        let peer_id = keypair.public().to_peer_id();
        let kad =
            libp2p::kad::Behaviour::new(peer_id, libp2p::kad::store::MemoryStore::new(peer_id));

        let behaviour = WetwareBehaviour {
            kad,
            identify: libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
                "wetware/0.1.0".to_string(),
                keypair.public(),
            )),
        };

        let mut swarm = SwarmBuilder::with_existing_identity(keypair)
            .with_tokio()
            .with_tcp(
                Default::default(),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )?
            .with_behaviour(|_| behaviour)?
            .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
            .build();

        let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{port}").parse()?;
        swarm.listen_on(listen_addr)?;

        Ok(Self {
            swarm,
            local_peer_id: peer_id,
        })
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.local_peer_id
    }

    pub async fn run(mut self, network_state: NetworkState) -> Result<()> {
        let mut known_peers: HashMap<PeerId, PeerInfo> = HashMap::new();

        loop {
            match self.swarm.select_next_some().await {
                SwarmEvent::ConnectionEstablished {
                    peer_id,
                    endpoint,
                    ..
                } => {
                    let addrs = match endpoint {
                        ConnectedPoint::Dialer { address, .. } => vec![address.to_string()],
                        ConnectedPoint::Listener { send_back_addr, .. } => {
                            vec![send_back_addr.to_string()]
                        }
                    };
                    known_peers.insert(
                        peer_id,
                        PeerInfo {
                            peer_id: peer_id.to_bytes(),
                            addrs,
                        },
                    );
                    network_state
                        .set_known_peers(known_peers.values().cloned().collect())
                        .await;
                }
                SwarmEvent::ConnectionClosed { peer_id, .. } => {
                    known_peers.remove(&peer_id);
                    network_state
                        .set_known_peers(known_peers.values().cloned().collect())
                        .await;
                }
                _ => {}
            }
        }
    }
}

/// Shared Wasmtime runtime state for Wetware hosts.
pub struct WasmtimeHost {
    engine: Arc<Engine>,
}

impl WasmtimeHost {
    pub fn new() -> Result<Self> {
        let mut config = WasmConfig::new();
        config.async_support(true);
        let engine = Engine::new(&config)?;
        Ok(Self {
            engine: Arc::new(engine),
        })
    }

    pub fn engine(&self) -> Arc<Engine> {
        Arc::clone(&self.engine)
    }
}

/// Wetware host combines libp2p and Wasmtime runtimes.
pub struct WetwareHost {
    libp2p: Libp2pHost,
    wasmtime: WasmtimeHost,
    network_state: NetworkState,
}

impl WetwareHost {
    pub fn new(port: u16) -> Result<Self> {
        let libp2p = Libp2pHost::new(port)?;
        let wasmtime = WasmtimeHost::new()?;
        let network_state = NetworkState::from_peer_id(libp2p.local_peer_id().to_bytes());
        Ok(Self {
            libp2p,
            wasmtime,
            network_state,
        })
    }

    pub fn network_state(&self) -> NetworkState {
        self.network_state.clone()
    }

    pub fn wasmtime_engine(&self) -> Arc<Engine> {
        self.wasmtime.engine()
    }

    pub async fn run(self) -> Result<()> {
        self.libp2p.run(self.network_state).await
    }
}
