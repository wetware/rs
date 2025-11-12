use anyhow::{Context, Result};
use futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{identity, Multiaddr, SwarmBuilder};
use std::time::Duration;
use tracing::{debug, info, warn};

use super::{Config, Loader, Proc, ServiceInfo};
use crate::net::boot;

/// Configuration for running a cell
pub struct Command {
    pub binary: String,
    pub args: Vec<String>,
    pub loader: Box<dyn Loader>,
    pub ipfs: String,
    pub env: Option<Vec<String>>,
    pub wasm_debug: bool,
    pub port: u16,
    pub loglvl: Option<crate::config::LogLevel>,
}

impl Command {
    /// Execute the cell command
    pub async fn run(self) -> Result<()> {
        run_cell(self).await
    }
}

/// Network behavior for Wetware cells
#[derive(libp2p::swarm::NetworkBehaviour)]
pub struct WetwareBehaviour {
    pub kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
    pub identify: libp2p::identify::Behaviour,
}

/// Main entry point for running a cell
pub async fn run_cell(config: Command) -> Result<()> {
    // Initialize tracing with the determined log level
    let log_level = config.loglvl.unwrap_or_else(crate::config::get_log_level);
    crate::config::init_tracing(log_level, config.loglvl);

    // All binaries are treated as WASM (like Go implementation)
    run_wasm(
        config.binary,
        config.args,
        &*config.loader,
        config.ipfs,
        config.env,
        config.wasm_debug,
        config.port,
    )
    .await
}

/// Run a WASM binary as a cell
async fn run_wasm(
    binary: String,
    args: Vec<String>,
    loader: &dyn Loader,
    ipfs: String,
    env: Option<Vec<String>>,
    wasm_debug: bool,
    port: u16,
) -> Result<()> {
    info!(binary = %binary, "Starting cell execution");

    // Resolve binary path using the provided loader
    let bytecode = loader
        .load(&binary)
        .await
        .with_context(|| format!("Failed to resolve binary: {binary}"))?;

    // Create process configuration
    let mut proc_config = Config::new().with_wasm_debug(wasm_debug).with_args(args);

    // Parse environment variables
    if let Some(env_vars) = env {
        proc_config = proc_config.with_env(env_vars);
    }

    // Create service info for the cell process
    let service_info = ServiceInfo {
        service_path: binary.to_string(),
        version: "0.1.0".to_string(),
        service_name: "main".to_string(),
        protocol: String::new(),
    };

    // Create cell process
    let proc = Proc::new_with_duplex_pipes(proc_config, &bytecode, service_info)
        .await
        .with_context(|| format!("Failed to create cell process from binary: {binary}"))?;

    // Run with libp2p host
    // Default to current directory as boot path (can be overridden in future)
    let boot_path = std::env::current_dir()?.to_string_lossy().to_string();
    run_cell_async(proc, port, boot_path, ipfs).await
}

/// Run the cell asynchronously with libp2p networking
async fn run_cell_async(
    _proc: Proc, // TODO: process management.
    port: u16,
    boot_path: String,
    ipfs_url: String,
) -> Result<()> {
    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = keypair.public().to_peer_id();

    // Configure Kademlia in client mode
    let kad = libp2p::kad::Behaviour::new(peer_id, libp2p::kad::store::MemoryStore::new(peer_id));

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

    info!(peer_id = %peer_id, port = port, "Cell process started");

    // Load boot configuration and bootstrap DHT
    let boot_config = boot::BootConfig::new(boot_path, ipfs_url.clone());

    // Get boot peers and connect to them
    let boot_peers = boot_config.get_all_boot_peers().await?;
    for multiaddr in boot_peers {
        debug!(multiaddr = %multiaddr, "Connecting to boot peer");
        if let Err(e) = swarm.dial(multiaddr) {
            warn!(error = ?e, "Failed to dial boot peer");
        }
    }

    // Bootstrap Kademlia DHT
    info!("Bootstrapping Kademlia DHT");
    swarm.behaviour_mut().kad.bootstrap()?;

    // CANONICAL BEHAVIOR: _start was already called during instantiation for initialization
    // Now we start the libp2p event loop to handle incoming streams
    info!(
        "Cell initialized and ready for connections. Incoming streams will be routed to services."
    );

    // Run the event loop with service resolution and per-stream instances
    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(address = %address, peer_id = %peer_id, "Listening on address");
            }
            SwarmEvent::IncomingConnection { .. } => {
                debug!("Incoming connection");
            }
            SwarmEvent::ConnectionEstablished {
                peer_id: remote_peer,
                ..
            } => {
                info!(peer_id = %remote_peer, "Connection established");
            }
            SwarmEvent::ConnectionClosed {
                peer_id: remote_peer,
                ..
            } => {
                debug!(peer_id = %remote_peer, "Connection closed");
            }
            SwarmEvent::IncomingConnectionError { error, .. } => {
                warn!(error = ?error, "Incoming connection error");
            }
            // Handle incoming service requests
            // TODO: Implement proper protocol handler for service execution
            // For now, we'll log incoming connections and prepare for service routing

            // Example of how we would handle service requests:
            // 1. Parse incoming stream protocol to extract version and service path
            // 2. Resolve service bytecode (from filesystem or IPFS)
            // 3. Create new Proc instance with duplex pipes
            // 4. Spawn task to handle stream I/O between libp2p and WASM
            SwarmEvent::Behaviour(_event) => {
                debug!("Behaviour event received");
            }
            _ => {}
        }
    }
}
