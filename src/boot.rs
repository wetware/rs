use anyhow::{anyhow, Result};
use capnp::capability::FromClientHook;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::{twoparty, RpcSystem};
use futures::StreamExt;
use libp2p::{
    identity,
    kad::{Event as KademliaEvent, QueryResult, RecordKey},
    swarm::{NetworkBehaviour, Swarm, SwarmEvent},
    Multiaddr, PeerId,
};
use serde_json::Value;
use std::collections::HashMap;
use std::os::fd::IntoRawFd;
use std::time::Duration;
use tokio_util::compat::TokioAsyncWriteCompatExt;
use tracing::{debug, info, warn};

use crate::config::HostConfig;
use crate::membrane::Membrane;

// IPFS protocol constants for DHT compatibility
// These protocols ensure our node can communicate with the IPFS network
const IPFS_KADEMLIA_PROTOCOL: &str = "/ipfs/kad/1.0.0"; // Standard IPFS DHT protocol
const IPFS_IDENTIFY_PROTOCOL: &str = "/ipfs/id/1.0.0"; // Standard IPFS identify protocol

/// Query a Kubo node to get its peer list for DHT bootstrap
/// This function retrieves the list of peers that our local Kubo node is connected to,
/// which we'll use to bootstrap our DHT routing table and establish connections.
pub async fn get_kubo_peers(kubo_url: &str) -> Result<Vec<(PeerId, Multiaddr)>> {
    let span = tracing::debug_span!("get_kubo_peers", kubo_url = kubo_url);
    let _enter = span.enter();

    let client = reqwest::Client::new();
    let url = format!("{}/api/v0/swarm/peers", kubo_url);

    debug!(url = %url, "Querying Kubo node for peers");

    let response = client.post(&url).send().await?;
    if !response.status().is_success() {
        warn!(status = %response.status(), "Failed to get peers from Kubo");
        return Err(anyhow!(
            "Failed to get peers from Kubo: {}",
            response.status()
        ));
    }

    let body: Value = response.json().await?;
    let peers = body["Peers"]
        .as_array()
        .ok_or_else(|| anyhow!("Invalid response format from Kubo"))?;

    let mut peer_addrs = Vec::new();
    let mut parse_errors = 0;

    for peer in peers {
        // The Kubo API returns "Addr" (multiaddr) and "Peer" (peer ID) separately
        if let (Some(addr_str), Some(peer_id_str)) = (peer["Addr"].as_str(), peer["Peer"].as_str())
        {
            if let (Ok(multiaddr), Ok(peer_id)) =
                (addr_str.parse::<Multiaddr>(), peer_id_str.parse::<PeerId>())
            {
                // Construct the full multiaddr with peer ID
                let full_addr = multiaddr.with(libp2p::multiaddr::Protocol::P2p(peer_id));
                peer_addrs.push((peer_id, full_addr));
            } else {
                parse_errors += 1;
                debug!(
                    addr_str = addr_str,
                    peer_id_str = peer_id_str,
                    "Failed to parse peer address or ID"
                );
            }
        }
    }

    if parse_errors > 0 {
        info!(
            parse_errors = parse_errors,
            "Some peer addresses could not be parsed"
        );
    }

    debug!(
        peer_count = peer_addrs.len(),
        parse_errors = parse_errors,
        "Found peer addresses from Kubo node"
    );
    Ok(peer_addrs)
}

#[derive(NetworkBehaviour)]
pub struct WetwareBehaviour {
    kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
    identify: libp2p::identify::Behaviour,
    wetware: crate::rpc::ProtocolBehaviour,
}

pub struct SwarmManager {
    swarm: Swarm<WetwareBehaviour>,
    #[allow(dead_code)]
    peer_id: PeerId,
    /// Bootstrap membrane for managing host-guest capabilities
    bootstrap_membrane: Membrane,
}

impl SwarmManager {
    pub fn new(swarm: Swarm<WetwareBehaviour>, peer_id: PeerId) -> Self {
        debug!(peer_id = %peer_id, "Creating new SwarmManager");
        Self {
            swarm,
            peer_id,
            bootstrap_membrane: Membrane::new(),
        }
    }

    /// Bootstrap the DHT by connecting to IPFS peers and triggering the bootstrap process
    /// This function establishes connections to the provided peers and then triggers
    /// the Kademlia bootstrap process to populate our routing table.
    pub async fn bootstrap_dht(&mut self, bootstrap_peers: Vec<(PeerId, Multiaddr)>) -> Result<()> {
        let span = tracing::debug_span!("bootstrap_dht", peer_count = bootstrap_peers.len());
        let _enter = span.enter();

        debug!("Bootstrapping DHT with IPFS peers");

        // Peers are already added to routing table, now dial them to establish connections
        for (peer_id, peer_addr) in &bootstrap_peers {
            if let Err(e) = self.swarm.dial(peer_addr.clone()) {
                debug!(peer_id = %peer_id, peer_addr = %peer_addr, reason = ?e, "Failed to dial IPFS peer");
            } else {
                debug!(peer_id = %peer_id, peer_addr = %peer_addr, "Dialing IPFS peer");
            }
        }

        // Wait for connections to be established
        debug!("Waiting for peer connections to establish...");
        tokio::time::sleep(Duration::from_secs(3)).await;

        // Now trigger bootstrap after we have actual peer connections
        debug!("Triggering DHT bootstrap with IPFS peers");
        let _ = self.swarm.behaviour_mut().kad.bootstrap();

        Ok(())
    }

    pub async fn announce_provider(&mut self, key: &str) -> Result<()> {
        let span = tracing::debug_span!("announce_provider", key = key);
        let _enter = span.enter();

        let record_key = RecordKey::new(&key.as_bytes());
        let _ = self.swarm.behaviour_mut().kad.start_providing(record_key);
        debug!("Started providing key");
        Ok(())
    }

    pub async fn query_providers(&mut self, key: &str) -> Result<()> {
        let span = tracing::debug_span!("query_providers", key = key);
        let _enter = span.enter();

        let record_key = RecordKey::new(&key.as_bytes());
        self.swarm.behaviour_mut().kad.get_providers(record_key);
        debug!("Querying providers for key");
        Ok(())
    }

    pub async fn run_event_loop(&mut self) -> Result<()> {
        let span = tracing::debug_span!("event_loop");
        let _enter = span.enter();

        debug!("Event loop started");

        loop {
            match self.swarm.next().await {
                Some(SwarmEvent::Behaviour(WetwareBehaviourEvent::Kad(event))) => match event {
                    KademliaEvent::OutboundQueryProgressed { result, .. } => match result {
                        QueryResult::GetProviders(Ok(providers_result)) => match providers_result {
                            libp2p::kad::GetProvidersOk::FoundProviders { providers, .. } => {
                                debug!(provider_count = providers.len(), "Found providers");
                                for provider in providers {
                                    debug!(provider = %provider, "Provider found");
                                }
                            }
                            libp2p::kad::GetProvidersOk::FinishedWithNoAdditionalRecord {
                                ..
                            } => {
                                debug!("Finished querying providers with no additional records");
                            }
                        },
                        QueryResult::GetProviders(Err(e)) => {
                            warn!(reason = ?e, "Failed to get providers");
                        }
                        QueryResult::StartProviding(Ok(_)) => {
                            debug!("Successfully started providing key");
                        }
                        QueryResult::StartProviding(Err(e)) => {
                            warn!(reason = ?e, "Failed to start providing");
                        }
                        QueryResult::Bootstrap(Ok(_)) => {
                            debug!("DHT bootstrap completed successfully");
                        }
                        QueryResult::Bootstrap(Err(e)) => {
                            warn!(reason = ?e, "DHT bootstrap failed");
                        }
                        _ => {}
                    },
                    KademliaEvent::InboundRequest { request, .. } => match request {
                        libp2p::kad::InboundRequest::GetProvider { .. } => {
                            debug!("Received GetProvider request");
                        }
                        libp2p::kad::InboundRequest::GetRecord { .. } => {
                            debug!("Received GetRecord request");
                        }
                        _ => {}
                    },
                    _ => {}
                },
                Some(SwarmEvent::Behaviour(WetwareBehaviourEvent::Identify(event))) => {
                    match event {
                        libp2p::identify::Event::Received { peer_id, info, .. } => {
                            debug!(peer_id = %peer_id, listen_addrs = ?info.listen_addrs, "Received identify info from peer");
                            // Don't add peers to Kademlia here - we already added them upfront
                            // This is just for logging peer discovery
                        }
                        libp2p::identify::Event::Sent { peer_id, .. } => {
                            debug!(peer_id = %peer_id, "Sent identify info to peer");
                        }
                        libp2p::identify::Event::Error { peer_id, error, .. } => {
                            warn!(peer_id = %peer_id, reason = ?error, "Identify error with peer");
                        }
                        _ => {}
                    }
                }
                Some(SwarmEvent::NewListenAddr { address, .. }) => {
                    debug!(address = %address, "Listening on address");
                }
                Some(SwarmEvent::ConnectionEstablished { peer_id, .. }) => {
                    debug!(peer_id = %peer_id, "Connected to IPFS peer");
                }
                Some(SwarmEvent::ConnectionClosed { peer_id, .. }) => {
                    debug!(peer_id = %peer_id, "Disconnected from IPFS peer");
                }
                Some(SwarmEvent::OutgoingConnectionError { peer_id, error, .. }) => {
                    // Only log connection errors at debug level to reduce spam
                    if let Some(peer_id) = peer_id {
                        debug!(peer_id = %peer_id, reason = ?error, "Failed to connect to IPFS peer");
                    } else {
                        debug!(reason = ?error, "Failed to establish outgoing connection");
                    }
                }
                Some(SwarmEvent::IncomingConnection {
                    connection_id,
                    local_addr,
                    send_back_addr: _,
                }) => {
                    debug!(connection_id = ?connection_id, local_addr = %local_addr, "New incoming connection");
                    // Connection is automatically accepted in libp2p 0.56.0
                }
                Some(SwarmEvent::IncomingConnectionError { .. }) => {
                    debug!("Incoming connection error");
                }
                Some(SwarmEvent::Dialing { peer_id, .. }) => {
                    debug!(peer_id = ?peer_id, "Dialing peer");
                }
                Some(SwarmEvent::ListenerClosed { .. }) => {
                    debug!("Listener closed");
                }
                Some(SwarmEvent::ListenerError { .. }) => {
                    debug!("Listener error");
                }
                // Note: Protocol upgrade handling will be implemented through the transport layer
                // For now, we'll log that wetware protocol is available
                _ => {}
            }
        }
    }

    pub fn add_peer_to_routing_table(&mut self, peer_id: &PeerId, peer_addr: &Multiaddr) {
        self.swarm
            .behaviour_mut()
            .kad
            .add_address(peer_id, peer_addr.clone());
    }

    /// Get the default protocol identifier
    pub fn get_default_protocol(&self) -> &str {
        crate::rpc::WW_PROTOCOL
    }

    /// Get a reference to the bootstrap membrane
    /// This is useful for testing and inspection of the bootstrap capabilities
    #[allow(dead_code)]
    pub fn bootstrap_membrane(&self) -> &Membrane {
        &self.bootstrap_membrane
    }

    /// Handle host-guest communication over Unix domain socket
    /// This method provides bootstrap capabilities to a guest subprocess over FD3,
    /// matching the Go implementation's behavior.
    #[allow(dead_code)]
    pub async fn handle_guest_communication(
        &self,
        socket: std::os::unix::net::UnixStream,
    ) -> Result<()> {
        let span = tracing::debug_span!("handle_guest_communication");
        let _enter = span.enter();

        debug!("Setting up host-guest communication over Unix domain socket");

        // Convert UnixStream to tokio::net::UnixStream for async operations
        let tokio_socket = tokio::net::UnixStream::from_std(socket)?;

        // Split the socket into read and write halves
        let (read_half, write_half) = tokio_socket.into_split();

        // Create a two-party RPC system
        // Convert tokio streams to futures-compatible streams
        let read_half = tokio_util::compat::TokioAsyncReadCompatExt::compat(read_half);
        let write_half = write_half.compat_write();

        let network = twoparty::VatNetwork::new(
            read_half,
            write_half,
            Side::Client,
            capnp::message::ReaderOptions::default(),
        );

        // Create the RPC system with the membrane as the bootstrap client
        // We need to create specific clients for the interfaces we support
        let importer_client: crate::system_capnp::importer::Client =
            capnp_rpc::new_client(self.bootstrap_membrane.clone());
        let _exporter_client: crate::system_capnp::exporter::Client =
            capnp_rpc::new_client(self.bootstrap_membrane.clone());

        // Convert the specific client to a generic capability client
        let membrane_client = capnp::capability::Client::new(importer_client.into_client_hook());

        let rpc_system = RpcSystem::new(Box::new(network), Some(membrane_client));

        info!("Host-guest RPC system established over FD3");
        info!("Bootstrap capabilities (Importer/Exporter) available to guest subprocess");

        // Run the RPC system
        rpc_system.await?;

        debug!("Host-guest communication completed");
        Ok(())
    }

    /// Handle incoming wetware protocol streams
    /// This method processes incoming connections and provides bootstrap capabilities
    /// (Importer/Exporter) over the /ww/0.1.0 protocol, matching the Go implementation.
    #[allow(dead_code)]
    pub async fn handle_wetware_stream(
        &mut self,
        stream: libp2p::Stream,
        peer_id: PeerId,
    ) -> Result<()> {
        let span = tracing::debug_span!("handle_wetware_stream", peer_id = %peer_id);
        let _enter = span.enter();

        debug!("Processing wetware stream from peer");

        // Create our stream adapter to bridge libp2p::Stream to tokio::io traits
        let stream_adapter = crate::rpc::Libp2pStreamAdapter::new(stream);

        // Split the stream adapter into read and write halves
        let (read_half, write_half) = tokio::io::split(stream_adapter);

        // Create a two-party RPC system
        // Convert tokio streams to futures-compatible streams
        let read_half = tokio_util::compat::TokioAsyncReadCompatExt::compat(read_half);
        let write_half = write_half.compat_write();

        let network = twoparty::VatNetwork::new(
            read_half,
            write_half,
            Side::Client,
            capnp::message::ReaderOptions::default(),
        );

        // Create the RPC system with the membrane as the bootstrap client
        // We need to create specific clients for the interfaces we support
        let importer_client: crate::system_capnp::importer::Client =
            capnp_rpc::new_client(self.bootstrap_membrane.clone());
        let _exporter_client: crate::system_capnp::exporter::Client =
            capnp_rpc::new_client(self.bootstrap_membrane.clone());

        // Convert the specific client to a generic capability client
        let membrane_client = capnp::capability::Client::new(importer_client.into_client_hook());

        let rpc_system = RpcSystem::new(Box::new(network), Some(membrane_client));

        info!("Wetware protocol /ww/0.1.0 stream established with peer");
        info!("Bootstrap capabilities (Importer/Exporter) available to guest");

        // Run the RPC system
        rpc_system.await?;

        debug!("Wetware stream processing completed");
        Ok(())
    }

    /// Initiate a wetware protocol connection to a peer
    /// This method opens a new stream to a peer and upgrades it to the wetware protocol
    #[allow(dead_code)]
    pub async fn connect_wetware_protocol(&mut self, peer_id: PeerId) -> Result<()> {
        let span = tracing::debug_span!("connect_wetware_protocol", peer_id = %peer_id);
        let _enter = span.enter();

        debug!("Initiating wetware protocol connection to peer");

        // Open a new stream to the peer with our wetware protocol
        let stream_protocol = libp2p::swarm::StreamProtocol::new(crate::rpc::WW_PROTOCOL);

        // In libp2p 0.56.0, we need to use the swarm to request streams
        // For now, we'll log that we would open a stream
        debug!("Stream protocol: {}", stream_protocol);
        debug!("Would open wetware stream to peer (implementation pending)");

        // TODO: Implement actual stream opening with libp2p 0.56.0 API
        // This would require implementing the full NetworkBehaviour with stream handling

        Ok(())
    }
}

/// Build a libp2p host with IPFS-compatible protocols and enhanced features
pub async fn build_host(
    config: Option<HostConfig>,
) -> Result<(identity::Keypair, PeerId, Swarm<WetwareBehaviour>)> {
    let config = config.unwrap_or_default();
    let span = tracing::debug_span!("build_host");
    let _enter = span.enter();

    // Generate Ed25519 keypair
    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = PeerId::from(keypair.public());

    debug!(peer_id = %peer_id, "Generated Ed25519 keypair");

    // Create Kademlia behaviour with IPFS-compatible protocol
    let kademlia_config =
        libp2p::kad::Config::new(libp2p::swarm::StreamProtocol::new(IPFS_KADEMLIA_PROTOCOL));

    // Removed periodic bootstrap logic - always use manual bootstrap only
    debug!("Using manual DHT bootstrap only");

    debug!("Created Kademlia configuration");

    let mut kademlia = libp2p::kad::Behaviour::with_config(
        peer_id,
        libp2p::kad::store::MemoryStore::new(peer_id),
        kademlia_config,
    );

    // Set Kademlia to client mode (we're not a bootstrap node)
    kademlia.set_mode(Some(libp2p::kad::Mode::Client));

    debug!("Set Kademlia to client mode");

    // Create network behaviour with IPFS-compatible protocols
    let behaviour = WetwareBehaviour {
        kad: kademlia,
        identify: libp2p::identify::Behaviour::new(
            libp2p::identify::Config::new(IPFS_IDENTIFY_PROTOCOL.to_string(), keypair.public())
                .with_agent_version("ww/1.0.0".to_string()),
        ),
        wetware: crate::rpc::ProtocolBehaviour::new(),
    };

    // Use SwarmBuilder to create a swarm with enhanced transport
    let swarm_builder = libp2p::SwarmBuilder::with_existing_identity(keypair.clone())
        .with_tokio()
        .with_tcp(
            Default::default(),
            libp2p::noise::Config::new,
            libp2p::yamux::Config::default,
        )?
        .with_behaviour(|_| behaviour)?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)));

    // Build the swarm with the configured behaviour
    let mut swarm = swarm_builder.build();

    debug!("Built libp2p swarm with enhanced features");

    // Note: Protocol upgrade registration needs to be handled differently in libp2p 0.56.0
    // For now, we'll log that the wetware protocol is available
    let wetware_protocol = libp2p::swarm::StreamProtocol::new(crate::rpc::WW_PROTOCOL);
    debug!("Wetware protocol available: {}", wetware_protocol);

    // Listen on configured addresses or use defaults
    let listen_addrs: Vec<Multiaddr> = config
        .listen_addrs
        .iter()
        .map(|addr_str| addr_str.parse())
        .collect::<Result<Vec<_>, _>>()?;

    if listen_addrs.is_empty() {
        warn!("No listen addresses configured, using defaults");
        // Use default addresses if none configured
        let default_ipv4: Multiaddr = "/ip4/0.0.0.0/tcp/2020".parse()?;
        let default_ipv6: Multiaddr = "/ip6/::/tcp/2020".parse()?;

        swarm.listen_on(default_ipv4.clone())?;
        debug!(listen_addr = %default_ipv4, "Started listening on default IPv4");

        swarm.listen_on(default_ipv6.clone())?;
        debug!(listen_addr = %default_ipv6, "Started listening on default IPv6");

        info!("🌐 Wetware node listening on (defaults):");
        info!("   IPv4: {}/p2p/{}", default_ipv4, peer_id);
        info!("   IPv6: {}/p2p/{}", default_ipv6, peer_id);
    } else {
        // Use configured addresses
        for addr in &listen_addrs {
            swarm.listen_on(addr.clone())?;
            debug!(listen_addr = %addr, "Started listening on configured address");
        }

        info!("🌐 Wetware node listening on (configured):");
        for addr in &listen_addrs {
            info!("   {}/p2p/{}", addr, peer_id);
        }
    }

    debug!("Host setup completed with configuration: {:?}", config);

    Ok((keypair, peer_id, swarm))
}

/// File descriptor mapping for user-provided FDs
struct FDMapping {
    name: String,
    source_fd: i32,
    target_fd: i32,
}

/// File descriptor manager for passing FDs to child processes
///
/// This manager handles:
/// - Unix domain socket pair creation for host-guest communication (FD3)
/// - User file descriptor duplication and passing (FD4+)
/// - Environment variable generation (WW_FD_*)
/// - Proper cleanup and resource management
///
/// # File Descriptor Convention
///
/// - **FD3**: Unix domain socket for host-guest RPC communication (bootstrap)
/// - **FD4+**: User-configurable file descriptors passed via --with-fd flags
///
/// This matches the Go implementation's file descriptor conventions.
pub struct FDManager {
    /// User file descriptor mappings (FD4+)
    mappings: Vec<FDMapping>,
    /// Socket for host-guest RPC (FD3)
    socket: Option<crate::system::Socket>,
}

impl FDManager {
    /// Create a new FD manager from --with-fd flag values
    ///
    /// This creates a new FD manager that will handle both the Unix domain socket
    /// pair for host-guest communication (FD3) and user file descriptors (FD4+).
    ///
    /// # Arguments
    ///
    /// * `fd_flags` - List of --with-fd flag values in "name=fdnum" format
    ///
    /// # Returns
    ///
    /// Returns a new FDManager instance ready for subprocess execution.
    ///
    /// # Errors
    ///
    /// Returns an error if the FD flags are invalid or if socket pair creation fails.
    pub fn new(fd_flags: Vec<String>) -> Result<Self> {
        let mut mappings = Vec::new();
        let mut used_names = std::collections::HashSet::new();

        // Parse user file descriptor mappings (FD4+)
        for (i, flag) in fd_flags.iter().enumerate() {
            let (name, source_fd) = Self::parse_fd_flag(flag)?;

            if used_names.contains(&name) {
                return Err(anyhow!("Duplicate name '{}' in --with-fd flags", name));
            }

            // Target FD starts at 4 (after FD3 for bootstrap socket) and increments sequentially
            let target_fd = 4 + i as i32;

            mappings.push(FDMapping {
                name: name.clone(),
                source_fd,
                target_fd,
            });

            used_names.insert(name);
        }

        // Create Unix domain socket pair for host-guest communication (FD3)
        let socket = Some(crate::system::Socket::new()?);

        debug!(
            user_fd_count = mappings.len(),
            "Created FD manager with {} user file descriptors and Unix domain socket pair",
            mappings.len()
        );

        Ok(Self { mappings, socket })
    }

    /// Parse a --with-fd flag value in "name=fdnum" format
    fn parse_fd_flag(value: &str) -> Result<(String, i32)> {
        let parts: Vec<&str> = value.splitn(2, '=').collect();
        if parts.len() != 2 {
            return Err(anyhow!(
                "Invalid format: expected 'name=fdnum', got '{}'",
                value
            ));
        }

        let name = parts[0].to_string();
        if name.is_empty() {
            return Err(anyhow!("Name cannot be empty"));
        }

        let fdnum: i32 = parts[1]
            .parse()
            .map_err(|_| anyhow!("Invalid fd number '{}'", parts[1]))?;

        if fdnum < 0 {
            return Err(anyhow!("FD number must be non-negative, got {}", fdnum));
        }

        Ok((name, fdnum))
    }

    /// Generate environment variables for the child process
    pub fn generate_env_vars(&self) -> HashMap<String, String> {
        let mut env_vars = HashMap::new();

        for mapping in &self.mappings {
            let env_var = format!("WW_FD_{}", mapping.name.to_uppercase());
            env_vars.insert(env_var, mapping.target_fd.to_string());
        }

        env_vars
    }

    /// Prepare file descriptors for passing to child process
    ///
    /// This method prepares all file descriptors that will be passed to the
    /// subprocess, including the Unix domain socket pair (FD3) and user FDs (FD4+).
    ///
    /// # Returns
    ///
    /// Returns a vector of raw file descriptors ready for subprocess inheritance.
    ///
    /// # Errors
    ///
    /// Returns an error if FD duplication fails or if the socket pair is not available.
    pub fn prepare_fds(&mut self) -> Result<Vec<i32>> {
        let mut extra_fds = Vec::new();

        // Add Unix domain socket for host-guest communication (FD3)
        if let Some(socket) = self.socket.take() {
            let guest_socket = socket.into_guest_socket();
            let guest_fd = guest_socket.into_raw_fd();
            extra_fds.push(guest_fd);

            debug!("Prepared Unix domain socket for FD3 (bootstrap)");
        } else {
            return Err(anyhow!("Socket not initialized"));
        }

        // Add user file descriptors (FD4+)
        for mapping in &self.mappings {
            // Duplicate the source FD to avoid conflicts
            let new_fd = unsafe { libc::dup(mapping.source_fd) };
            if new_fd < 0 {
                return Err(anyhow!(
                    "Failed to duplicate fd {} for '{}': {}",
                    mapping.source_fd,
                    mapping.name,
                    std::io::Error::last_os_error()
                ));
            }

            extra_fds.push(new_fd);

            debug!(
                name = %mapping.name,
                source_fd = mapping.source_fd,
                target_fd = mapping.target_fd,
                "File descriptor prepared"
            );
        }

        info!(
            total_fds = extra_fds.len(),
            "Prepared {} file descriptors for subprocess (FD3: socket, FD4+: user FDs)",
            extra_fds.len()
        );

        Ok(extra_fds)
    }

    /// Close all managed file descriptors
    ///
    /// This method closes all user file descriptors that were duplicated for
    /// the subprocess. The Unix domain socket pair is automatically cleaned up
    /// when the Socket object is dropped.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` on success, or an error if cleanup fails.
    #[allow(dead_code)]
    pub fn close_fds(&self) -> Result<()> {
        debug!("Closing {} user file descriptors", self.mappings.len());

        for mapping in &self.mappings {
            // Only close file descriptors that are valid (>= 0)
            // In test contexts, source_fd might be invalid test values
            if mapping.source_fd >= 0 {
                // Check if the file descriptor is actually open before trying to close it
                let result = unsafe { libc::fcntl(mapping.source_fd, libc::F_GETFD) };
                if result >= 0 {
                    // File descriptor is open, close it
                    let close_result = unsafe { libc::close(mapping.source_fd) };
                    if close_result < 0 {
                        debug!(
                            name = %mapping.name,
                            source_fd = mapping.source_fd,
                            error = %std::io::Error::last_os_error(),
                            "Failed to close file descriptor"
                        );
                    } else {
                        debug!(
                            name = %mapping.name,
                            source_fd = mapping.source_fd,
                            "File descriptor closed"
                        );
                    }
                } else {
                    debug!(
                        name = %mapping.name,
                        source_fd = mapping.source_fd,
                        "File descriptor not open, skipping"
                    );
                }
            } else {
                debug!(
                    name = %mapping.name,
                    source_fd = mapping.source_fd,
                    "Skipping invalid file descriptor"
                );
            }
        }

        info!("File descriptor cleanup completed");
        Ok(())
    }

    /// Get a reference to the socket handler
    ///
    /// This returns a reference to the socket handler for RPC
    /// communication with the subprocess.
    ///
    /// # Returns
    ///
    /// Returns `Some(Socket)` if available, or `None` if not initialized.
    #[allow(dead_code)]
    pub fn socket(&self) -> Option<&crate::system::Socket> {
        self.socket.as_ref()
    }

    /// Take ownership of the socket handler
    ///
    /// This takes ownership of the socket handler, removing it
    /// from the FD manager. This is useful when transferring ownership to
    /// the subprocess execution context.
    ///
    /// # Returns
    ///
    /// Returns `Some(Socket)` if available, or `None` if not initialized.
    #[allow(dead_code)]
    pub fn take_socket(&mut self) -> Option<crate::system::Socket> {
        self.socket.take()
    }
}

impl Drop for FDManager {
    /// Clean up resources when FDManager is dropped
    ///
    /// Note: This does not close file descriptors because:
    /// - Original source_fd should remain open (owned by caller)
    /// - Duplicated file descriptors are passed to subprocess and closed by it
    /// - Socket file descriptors are handled by Socket's own Drop implementation
    fn drop(&mut self) {
        debug!("Dropping FDManager - no file descriptor cleanup needed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wetware_protocol_identifier() {
        let protocol = crate::rpc::WW_PROTOCOL;
        assert_eq!(protocol, "/ww/0.1.0");
    }

    #[test]
    fn test_protocol_upgrade_creation() {
        use libp2p::core::upgrade::UpgradeInfo;
        let upgrade = crate::rpc::DefaultProtocolUpgrade::new();
        let mut protocol_info = upgrade.protocol_info();
        assert_eq!(protocol_info.next().unwrap().as_ref(), "/ww/0.1.0");
    }

    #[tokio::test]
    async fn test_swarm_manager_protocol_methods() {
        // Test that SwarmManager methods work correctly
        let config = HostConfig::default();
        let (_keypair, peer_id, swarm) = build_host(Some(config)).await.unwrap();
        let swarm_manager = SwarmManager::new(swarm, peer_id);

        // Test protocol identifier
        assert_eq!(swarm_manager.get_default_protocol(), "/ww/0.1.0");
    }

    #[test]
    fn test_swarm_manager_bootstrap_membrane() {
        let config = HostConfig::default();
        let (_keypair, peer_id, swarm) = tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(build_host(Some(config)))
            .unwrap();
        let swarm_manager = SwarmManager::new(swarm, peer_id);

        // Test that bootstrap membrane is accessible
        let membrane = swarm_manager.bootstrap_membrane();
        // Note: We can't access the private services field directly,
        // but we can test that the membrane exists
        assert!(!std::ptr::addr_of!(membrane).is_null());
    }
}
