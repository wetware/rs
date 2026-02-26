//! Wetware host runtime: libp2p host + Wasmtime host.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use libp2p::core::connection::ConnectedPoint;
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId, SwarmBuilder};
use tokio::sync::{mpsc, oneshot};
use wasmtime::{Config as WasmConfig, Engine};

use crate::rpc::{NetworkState, PeerInfo};

/// Commands sent from RPC handlers to the swarm event loop.
pub enum SwarmCommand {
    Connect {
        peer_id: PeerId,
        addrs: Vec<Multiaddr>,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

/// Network behavior for Wetware hosts.
#[derive(libp2p::swarm::NetworkBehaviour)]
pub struct WetwareBehaviour {
    pub identify: libp2p::identify::Behaviour,
    pub stream: libp2p_stream::Behaviour,
}

/// Libp2p host wrapper for Wetware.
pub struct Libp2pHost {
    swarm: libp2p::swarm::Swarm<WetwareBehaviour>,
    local_peer_id: PeerId,
    stream_control: libp2p_stream::Control,
}

impl Libp2pHost {
    /// Create a new libp2p host and start listening on the given TCP port.
    ///
    /// `keypair` is the node's identity — load it with [`crate::keys::to_libp2p`]
    /// or supply an ephemeral key for dev/test use.
    pub fn new(port: u16, keypair: libp2p::identity::Keypair) -> Result<Self> {
        let peer_id = keypair.public().to_peer_id();

        let stream_behaviour = libp2p_stream::Behaviour::new();
        let stream_control = stream_behaviour.new_control();

        let behaviour = WetwareBehaviour {
            identify: libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
                "wetware/0.1.0".to_string(),
                keypair.public(),
            )),
            stream: stream_behaviour,
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
            stream_control,
        })
    }

    pub fn local_peer_id(&self) -> PeerId {
        self.local_peer_id
    }

    pub fn stream_control(&self) -> libp2p_stream::Control {
        self.stream_control.clone()
    }

    pub async fn run(
        mut self,
        network_state: NetworkState,
        mut cmd_rx: mpsc::Receiver<SwarmCommand>,
    ) -> Result<()> {
        let mut known_peers: HashMap<PeerId, PeerInfo> = HashMap::new();
        let mut pending_connects: HashMap<PeerId, Vec<oneshot::Sender<Result<(), String>>>> =
            HashMap::new();

        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => {
                    match event {
                        SwarmEvent::NewListenAddr { address, .. } => {
                            network_state.add_listen_addr(address.to_vec()).await;
                        }
                        SwarmEvent::ExpiredListenAddr { address, .. } => {
                            network_state.remove_listen_addr(&address.to_vec()).await;
                        }
                        SwarmEvent::ConnectionEstablished {
                            peer_id,
                            endpoint,
                            ..
                        } => {
                            let addrs = match endpoint {
                                ConnectedPoint::Dialer { address, .. } => vec![address.to_vec()],
                                ConnectedPoint::Listener { send_back_addr, .. } => {
                                    vec![send_back_addr.to_vec()]
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

                            // Reply to pending connect requests
                            if let Some(senders) = pending_connects.remove(&peer_id) {
                                for sender in senders {
                                    let _ = sender.send(Ok(()));
                                }
                            }
                        }
                        SwarmEvent::ConnectionClosed { peer_id, .. } => {
                            known_peers.remove(&peer_id);
                            network_state
                                .set_known_peers(known_peers.values().cloned().collect())
                                .await;
                        }
                        SwarmEvent::OutgoingConnectionError {
                            peer_id: Some(peer_id),
                            error,
                            ..
                        } => {
                            if let Some(senders) = pending_connects.remove(&peer_id) {
                                for sender in senders {
                                    let _ = sender.send(Err(error.to_string()));
                                }
                            }
                        }
                        _ => {}
                    }
                }
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(SwarmCommand::Connect { peer_id, addrs, reply }) => {
                            // If already connected, reply immediately
                            if self.swarm.is_connected(&peer_id) {
                                let _ = reply.send(Ok(()));
                                continue;
                            }

                            for addr in &addrs {
                                self.swarm.add_peer_address(peer_id, addr.clone());
                            }

                            match self.swarm.dial(peer_id) {
                                Ok(()) => {
                                    pending_connects.entry(peer_id).or_default().push(reply);
                                }
                                Err(e) => {
                                    let _ = reply.send(Err(e.to_string()));
                                }
                            }
                        }
                        None => {
                            // Channel closed, shut down
                            break;
                        }
                    }
                }
            }
        }

        Ok(())
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
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    swarm_cmd_rx: Option<mpsc::Receiver<SwarmCommand>>,
}

impl WetwareHost {
    pub fn new(port: u16, keypair: libp2p::identity::Keypair) -> Result<Self> {
        let libp2p = Libp2pHost::new(port, keypair)?;
        let wasmtime = WasmtimeHost::new()?;
        let network_state = NetworkState::from_peer_id(libp2p.local_peer_id().to_bytes());
        let (swarm_cmd_tx, swarm_cmd_rx) = mpsc::channel(64);
        Ok(Self {
            libp2p,
            wasmtime,
            network_state,
            swarm_cmd_tx,
            swarm_cmd_rx: Some(swarm_cmd_rx),
        })
    }

    pub fn network_state(&self) -> NetworkState {
        self.network_state.clone()
    }

    pub fn swarm_cmd_tx(&self) -> mpsc::Sender<SwarmCommand> {
        self.swarm_cmd_tx.clone()
    }

    pub fn wasmtime_engine(&self) -> Arc<Engine> {
        self.wasmtime.engine()
    }

    pub fn stream_control(&self) -> libp2p_stream::Control {
        self.libp2p.stream_control()
    }

    pub async fn run(mut self) -> Result<()> {
        let cmd_rx = self
            .swarm_cmd_rx
            .take()
            .expect("run() called more than once");
        self.libp2p.run(self.network_state, cmd_rx).await
    }
}
