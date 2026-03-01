//! Wetware host runtime: libp2p host + Wasmtime host.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::{HashMap, HashSet};
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
        kubo_peers: Vec<(PeerId, Multiaddr)>,
    ) -> Result<Self> {
        let peer_id = keypair.public().to_peer_id();

        let stream_behaviour = libp2p_stream::Behaviour::new();
        let stream_control = stream_behaviour.new_control();

        // Build Kademlia client in Amino DHT mode.
        let kad_store = kad::store::MemoryStore::new(peer_id);
        let mut kad_config = kad::Config::new(kad::PROTOCOL_NAME);
        // Disable periodic bootstrap — we trigger a one-time walk below
        // and rely on provide/findProviders queries to keep the routing
        // table warm.  Periodic walks would reconnect to hundreds of DHT
        // servers every interval, flooding the swarm.
        kad_config.set_periodic_bootstrap_interval(None);
        // Use default parallelism α=3.  Previous α=1 caused iterative walks
        // to converge too slowly with a sparse routing table, preventing
        // provide/findProviders from reaching the correct DHT servers.
        let mut kad_behaviour = kad::Behaviour::with_config(peer_id, kad_store, kad_config);
        kad_behaviour.set_mode(Some(kad::Mode::Client));

        // Populate Kad routing table from Kubo's connected peers (K random
        // peers).  Kubo itself is added last as K+1, ensuring it is always
        // present regardless of the sample.
        for (peer_id, addr) in &kubo_peers {
            kad_behaviour.add_address(peer_id, addr.clone());
        }
        if let Some(ref bootstrap) = kubo_bootstrap {
            kad_behaviour.add_address(&bootstrap.peer_id, bootstrap.addr.clone());
        }

        // Trigger a one-time bootstrap walk to populate the routing table
        // with diverse DHT server peers.  Without this, the table contains
        // only K Kubo swarm peers — too sparse for iterative queries to
        // converge to the correct K-closest servers for a given key.
        if !kubo_peers.is_empty() || kubo_bootstrap.is_some() {
            match kad_behaviour.bootstrap() {
                Ok(_) => tracing::debug!("Kad bootstrap walk started"),
                Err(e) => tracing::warn!("Kad bootstrap failed to start: {e:?}"),
            }
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
        // Providers already forwarded to the guest for the current find_providers query.
        // Kademlia returns the same provider many times across successive FoundProviders
        // batches; this set deduplicates so we forward (and log) each peer only once.
        let mut forwarded_providers: HashMap<kad::QueryId, HashSet<PeerId>> = HashMap::new();
        // Peer address book populated from swarm events and peer routing results.
        let mut peer_addr_book: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();
        // Pending peer routing (RoutedHost-style): get_closest_peers query → target PeerId.
        let mut pending_peer_routing: HashMap<kad::QueryId, PeerId> = HashMap::new();
        // Peers we have already attempted peer routing for.  Prevents tight
        // re-query loops when a provider is genuinely unreachable — mirrors
        // go-libp2p RoutedHost which issues at most one FindPeer per target.
        let mut routed_peers: HashSet<PeerId> = HashSet::new();

        // Self-announcement: walk toward our own PeerId so that peers near us
        // in XOR space add us to their routing tables.  This makes us findable
        // via get_closest_peers(our_peer_id) when other nodes do peer routing.
        self.swarm
            .behaviour_mut()
            .kad
            .get_closest_peers(self.local_peer_id);
        tracing::debug!("Kad self-announcement walk started");

        loop {
            tokio::select! {
                event = self.swarm.select_next_some() => {
                    match event {
                        SwarmEvent::NewListenAddr { address, .. } => {
                            tracing::debug!(%address, "Promoting listen address to external");
                            self.swarm.add_external_address(address.clone());
                            network_state.add_listen_addr(address.to_vec()).await;
                        }
                        SwarmEvent::ExpiredListenAddr { address, .. } => {
                            self.swarm.remove_external_address(&address);
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
                        SwarmEvent::Behaviour(WetwareBehaviourEvent::Kad(
                            kad::Event::OutboundQueryProgressed { id, result, step, .. },
                        )) => {
                            match result {
                                kad::QueryResult::Bootstrap(Ok(ok)) => {
                                    tracing::debug!(
                                        peer = %ok.peer,
                                        remaining = ok.num_remaining,
                                        "Kad bootstrap progress"
                                    );
                                }
                                kad::QueryResult::Bootstrap(Err(e)) => {
                                    tracing::warn!("Kad bootstrap error: {e:?}");
                                }
                                kad::QueryResult::StartProviding(Ok(_)) => {
                                    tracing::debug!("Kad provide succeeded");
                                    if let Some(reply) = pending_kad_provides.remove(&id) {
                                        let _ = reply.send(Ok(()));
                                    }
                                }
                                kad::QueryResult::StartProviding(Err(e)) => {
                                    tracing::warn!("Kad provide FAILED: {e:?}");
                                    if let Some(reply) = pending_kad_provides.remove(&id) {
                                        let _ = reply.send(Err(format!("{e:?}")));
                                    }
                                }
                                kad::QueryResult::GetProviders(Ok(
                                    kad::GetProvidersOk::FoundProviders { providers, .. },
                                )) => {
                                    tracing::debug!(
                                        count = providers.len(),
                                        "Kad found providers batch"
                                    );
                                    if let Some(sender) = pending_kad_find_providers.get(&id) {
                                        let seen = forwarded_providers.entry(id).or_default();
                                        for provider in &providers {
                                            // Skip providers we already forwarded in this query.
                                            if !seen.insert(*provider) {
                                                continue;
                                            }

                                            // Check peer_addr_book (populated by peer routing
                                            // results and NewExternalAddrOfPeer events).
                                            let addrs: Vec<Multiaddr> = peer_addr_book
                                                .get(provider)
                                                .cloned()
                                                .unwrap_or_default();

                                            // Register addresses with the swarm so it can dial
                                            // this provider when the guest opens a stream.
                                            for addr in &addrs {
                                                self.swarm.add_peer_address(*provider, addr.clone());
                                            }

                                            if !addrs.is_empty() {
                                                // Provider has addresses — forward to guest.
                                                tracing::debug!(
                                                    peer = %provider,
                                                    addr_count = addrs.len(),
                                                    "Provider discovered with addresses"
                                                );
                                                let _ = sender.send(PeerInfo {
                                                    peer_id: provider.to_bytes(),
                                                    addrs: addrs.iter().map(|a| a.to_vec()).collect(),
                                                });
                                            } else if !routed_peers.contains(provider)
                                                && !pending_peer_routing.values().any(|p| p == provider)
                                            {
                                                // No addresses and not yet routed — issue a single
                                                // DHT peer routing query (RoutedHost-style FindPeer).
                                                tracing::debug!(
                                                    peer = %provider,
                                                    "No addresses for provider; issuing peer routing query"
                                                );
                                                let qid = self.swarm.behaviour_mut()
                                                    .kad.get_closest_peers(*provider);
                                                pending_peer_routing.insert(qid, *provider);
                                            }
                                            // else: no addresses, already routed — skip silently
                                        }
                                    }
                                }
                                kad::QueryResult::GetProviders(Ok(
                                    kad::GetProvidersOk::FinishedWithNoAdditionalRecord { closest_peers, .. },
                                )) => {
                                    tracing::debug!(
                                        closest = closest_peers.len(),
                                        "Kad find_providers finished (no more records)"
                                    );
                                }
                                kad::QueryResult::GetProviders(Err(e)) => {
                                    tracing::warn!("Kad find_providers FAILED: {e:?}");
                                }
                                // RoutedHost-style peer routing: resolve PeerId → addrs.
                                kad::QueryResult::GetClosestPeers(Ok(
                                    kad::GetClosestPeersOk { ref peers, .. },
                                )) => {
                                    if let Some(target) = pending_peer_routing.remove(&id) {
                                        routed_peers.insert(target);
                                        if let Some(info) = peers.iter()
                                            .find(|p| p.peer_id == target)
                                        {
                                            tracing::debug!(
                                                peer = %target,
                                                addr_count = info.addrs.len(),
                                                addrs = ?info.addrs.iter()
                                                    .map(|a| a.to_string()).collect::<Vec<_>>(),
                                                "Peer routing resolved addresses"
                                            );
                                            for addr in &info.addrs {
                                                self.swarm.add_peer_address(target, addr.clone());
                                            }
                                            peer_addr_book.entry(target).or_default()
                                                .extend(info.addrs.iter().cloned());
                                        } else {
                                            tracing::debug!(
                                                peer = %target,
                                                closest_returned = peers.len(),
                                                "Peer routing: target not found in closest peers"
                                            );
                                        }
                                    }
                                }
                                kad::QueryResult::GetClosestPeers(Err(ref e)) => {
                                    if let Some(target) = pending_peer_routing.remove(&id) {
                                        routed_peers.insert(target);
                                        tracing::warn!(peer = %target, "Peer routing query failed: {e:?}");
                                    }
                                }
                                _ => {
                                    tracing::debug!("Kad query progress (other): {result:?}");
                                }
                            }
                            tracing::debug!(
                                query_id = ?id,
                                step_count = step.count,
                                last = step.last,
                                "Kad query step"
                            );
                            // When the query finishes, close the reply channel so the
                            // receiver loop in routing.rs exits cleanly.
                            if step.last {
                                pending_kad_find_providers.remove(&id);
                                forwarded_providers.remove(&id);
                                pending_kad_provides.remove(&id);
                                pending_peer_routing.remove(&id);
                            }
                        }
                        SwarmEvent::Behaviour(WetwareBehaviourEvent::Kad(ref ev)) => {
                            tracing::debug!("Kad event: {ev:?}");
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
                            tracing::debug!(
                                key_len = key.len(),
                                peers_in_rt = self.swarm.behaviour_mut().kad.kbuckets()
                                    .map(|b| b.num_entries()).sum::<usize>(),
                                "Kad provide: starting"
                            );
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
                            // Allow peer routing retries across successive queries.
                            // The previous approach blacklisted peers permanently after
                            // one failed routing attempt; now each find_providers query
                            // starts fresh so transient routing failures don't prevent
                            // later discovery once the routing table has converged.
                            routed_peers.clear();
                            tracing::debug!(
                                key_len = key.len(),
                                peers_in_rt = self.swarm.behaviour_mut().kad.kbuckets()
                                    .map(|b| b.num_entries()).sum::<usize>(),
                                "Kad find_providers: starting"
                            );
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
        kubo_peers: Vec<(PeerId, Multiaddr)>,
    ) -> Result<Self> {
        let libp2p = Libp2pHost::new(port, keypair, kubo_bootstrap, kubo_peers)?;
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
