//! Wetware host runtime: libp2p host + Wasmtime host.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::StreamExt;
use libp2p::core::connection::ConnectedPoint;
use libp2p::kad;
use libp2p::swarm::SwarmEvent;
use libp2p::{Multiaddr, PeerId, SwarmBuilder};
use tokio::sync::{mpsc, oneshot};
use wasmtime::{Config as WasmConfig, Engine};

use crate::rpc::{NetworkState, PeerInfo};

/// Bootstrap info for the in-process Kad client.
///
/// Obtained by calling [`crate::ipfs::HttpClient::kubo_info`] and parsing the
/// returned peer ID + swarm address.  Passed to [`WetwareHost::new`] so the
/// Kad client can bootstrap against the local Kubo node.
pub struct KuboBootstrapInfo {
    pub peer_id: PeerId,
    pub addr: Multiaddr,
}

/// Commands sent from RPC handlers to the swarm event loop.
pub enum SwarmCommand {
    Connect {
        peer_id: PeerId,
        addrs: Vec<Multiaddr>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Announce this Wetware node as a provider for the given DHT key
    /// (multihash bytes of a CID) on the Amino Kademlia DHT.
    KadProvide {
        key: Vec<u8>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    /// Find providers for the given DHT key (multihash bytes of a CID).
    ///
    /// Providers are sent over the unbounded channel as they are discovered.
    /// The channel is closed when the query completes.
    KadFindProviders {
        key: Vec<u8>,
        reply: mpsc::UnboundedSender<PeerInfo>,
    },
}

/// Network behavior for Wetware hosts.
#[derive(libp2p::swarm::NetworkBehaviour)]
pub struct WetwareBehaviour {
    pub identify: libp2p::identify::Behaviour,
    pub stream: libp2p_stream::Behaviour,
    /// Kademlia DHT client (Amino protocol `/ipfs/kad/1.0.0`) bootstrapped
    /// against the local Kubo node.  Runs in client mode — announces provider
    /// records under the Wetware peer ID and finds providers by DHT lookup.
    pub kad: kad::Behaviour<kad::store::MemoryStore>,
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
    ///
    /// `kubo_bootstrap` is optional Kubo node info for bootstrapping the Kad
    /// client.  When `None`, the Kad client starts without any seed peers.
    pub fn new(
        port: u16,
        keypair: libp2p::identity::Keypair,
        kubo_bootstrap: Option<KuboBootstrapInfo>,
    ) -> Result<Self> {
        let peer_id = keypair.public().to_peer_id();

        let stream_behaviour = libp2p_stream::Behaviour::new();
        let stream_control = stream_behaviour.new_control();

        // Build Kademlia client in Amino DHT mode.
        let kad_store = kad::store::MemoryStore::new(peer_id);
        let mut kad_behaviour =
            kad::Behaviour::with_config(peer_id, kad_store, kad::Config::new(kad::PROTOCOL_NAME));
        kad_behaviour.set_mode(Some(kad::Mode::Client));

        if let Some(bootstrap) = kubo_bootstrap {
            kad_behaviour.add_address(&bootstrap.peer_id, bootstrap.addr);
            // Start a bootstrap query so we fill our routing table early.
            let _ = kad_behaviour.bootstrap();
        }

        let behaviour = WetwareBehaviour {
            identify: libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
                "wetware/0.1.0".to_string(),
                keypair.public(),
            )),
            stream: stream_behaviour,
            kad: kad_behaviour,
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

        // Pending Kad provide queries: query_id → reply channel.
        let mut pending_kad_provides: HashMap<kad::QueryId, oneshot::Sender<Result<(), String>>> =
            HashMap::new();
        // Pending Kad findProviders queries: query_id → streaming reply channel.
        let mut pending_kad_find_providers: HashMap<kad::QueryId, mpsc::UnboundedSender<PeerInfo>> =
            HashMap::new();
        // Peer address book populated from swarm events (used to fill provider addrs).
        let mut peer_addr_book: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();

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
                        // Track external addresses for remote peers (used to fill
                        // provider records returned by KadFindProviders).
                        SwarmEvent::NewExternalAddrOfPeer { peer_id, address } => {
                            peer_addr_book.entry(peer_id).or_default().push(address);
                        }
                        SwarmEvent::Behaviour(WetwareBehaviourEvent::Kad(event)) => {
                            match event {
                                kad::Event::OutboundQueryProgressed { id, result, step, .. } => {
                                    match result {
                                        kad::QueryResult::StartProviding(Ok(_)) => {
                                            if let Some(reply) = pending_kad_provides.remove(&id) {
                                                let _ = reply.send(Ok(()));
                                            }
                                        }
                                        kad::QueryResult::StartProviding(Err(e)) => {
                                            if let Some(reply) = pending_kad_provides.remove(&id) {
                                                let _ = reply.send(Err(format!("{e:?}")));
                                            }
                                        }
                                        kad::QueryResult::GetProviders(Ok(
                                            kad::GetProvidersOk::FoundProviders { providers, .. },
                                        )) => {
                                            if let Some(sender) = pending_kad_find_providers.get(&id) {
                                                for provider in providers {
                                                    let addrs = peer_addr_book
                                                        .get(&provider)
                                                        .map(|v| v.iter().map(|a| a.to_vec()).collect())
                                                        .unwrap_or_default();
                                                    // Ignore send errors (receiver dropped = caller gone).
                                                    let _ = sender.send(PeerInfo {
                                                        peer_id: provider.to_bytes(),
                                                        addrs,
                                                    });
                                                }
                                            }
                                        }
                                        // Other GetProviders outcomes (no additional record, error)
                                        // are handled below by the `step.last` cleanup.
                                        _ => {}
                                    }
                                    // When the query finishes, close the reply channel so the
                                    // receiver loop in routing.rs exits cleanly.
                                    if step.last {
                                        pending_kad_find_providers.remove(&id);
                                        // provide queries are already removed above; remove is a no-op.
                                        pending_kad_provides.remove(&id);
                                    }
                                }
                                _ => {}
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
                        Some(SwarmCommand::KadProvide { key, reply }) => {
                            let record_key = kad::RecordKey::new(&key);
                            match self.swarm.behaviour_mut().kad.start_providing(record_key) {
                                Ok(query_id) => {
                                    pending_kad_provides.insert(query_id, reply);
                                }
                                Err(e) => {
                                    let _ = reply.send(Err(format!("{e:?}")));
                                }
                            }
                        }
                        Some(SwarmCommand::KadFindProviders { key, reply }) => {
                            let record_key = kad::RecordKey::new(&key);
                            let query_id =
                                self.swarm.behaviour_mut().kad.get_providers(record_key);
                            pending_kad_find_providers.insert(query_id, reply);
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
    pub fn new(
        port: u16,
        keypair: libp2p::identity::Keypair,
        kubo_bootstrap: Option<KuboBootstrapInfo>,
    ) -> Result<Self> {
        let libp2p = Libp2pHost::new(port, keypair, kubo_bootstrap)?;
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
