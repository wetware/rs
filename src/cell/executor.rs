use anyhow::{Context, Result};
use futures::StreamExt;
use libp2p::swarm::SwarmEvent;
use libp2p::{identity, Multiaddr, SwarmBuilder};
use std::time::Duration;
use tokio::io::{self, AsyncReadExt, AsyncWriteExt};
use tracing::{debug, info, warn};

use super::{Config, Loader, Proc, ServiceInfo};
use crate::net::boot;
use crate::net::resolver;

/// Configuration for running a cell
pub struct Command {
    pub binary: String,
    pub args: Vec<String>,
    pub loader: Box<dyn Loader>,
    #[allow(dead_code)]
    pub ipfs: Option<String>,
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
    ipfs: Option<String>,
    env: Option<Vec<String>>,
    wasm_debug: bool,
    port: u16,
) -> Result<()> {
    info!(binary = %binary, "Starting cell execution");

    // Resolve binary path using the provided loader
    let bytecode = loader
        .load(&binary)
        .await
        .with_context(|| format!("Failed to resolve binary: {}", binary))?;

    // Create process configuration
    let mut config = Config::new().with_wasm_debug(wasm_debug).with_args(args);

    // Parse environment variables
    if let Some(env_vars) = env {
        config = config.with_env(env_vars);
    }

    // Create service info for the cell process
    let service_info = ServiceInfo {
        service_path: binary.to_string(),
        version: "0.1.0".to_string(),
        service_name: "main".to_string(),
        protocol: "/ww/0.1.0/".to_string(),
    };

    // Create cell process
    let proc = Proc::new_with_duplex_pipes(config, &bytecode, service_info)
        .await
        .with_context(|| format!("Failed to create cell process from binary: {}", binary))?;

    // Extract host-side I/O streams for terminal bridging
    let mut host_stdin = proc.host_stdin;
    let mut host_stdout = proc.host_stdout;

    // Keep store and instance for polling completion
    let mut store = proc.store;
    let _instance = proc.instance;

    // Run with libp2p host
    let root_dir = std::env::current_dir()?;
    let ipfs_url = ipfs.unwrap_or_else(crate::net::ipfs::get_ipfs_url);

    // Spawn terminal I/O bridging tasks
    let stdin_task = tokio::spawn(async move {
        let mut terminal_stdin = io::stdin();
        if let Err(e) = io::copy(&mut terminal_stdin, &mut host_stdin).await {
            warn!(error = ?e, "Error copying from terminal stdin to host_stdin");
        }
    });

    let stdout_task = tokio::spawn(async move {
        let mut terminal_stdout = io::stdout();
        if let Err(e) = io::copy(&mut host_stdout, &mut terminal_stdout).await {
            warn!(error = ?e, "Error copying from host_stdout to terminal stdout");
        }
    });

    // Keep store alive while waiting for guest to complete
    let _store_guard = store;

    // Wait for stdout to finish, which indicates the guest has completed execution
    stdout_task.await??;

    info!("WASM guest execution completed");
    Ok(())
}

/// Run the cell asynchronously with libp2p networking
async fn run_cell_async(port: u16, root_dir: std::path::PathBuf, ipfs_url: String) -> Result<()> {
    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = keypair.public().to_peer_id();

    // Configure Kademlia in client mode
    let kad = libp2p::kad::Behaviour::new(peer_id, libp2p::kad::store::MemoryStore::new(peer_id));

    let behaviour = WetwareBehaviour {
        kad,
        identify: libp2p::identify::Behaviour::new(libp2p::identify::Config::new(
            "ww/1.0.0".to_string(),
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

    let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", port).parse()?;
    swarm.listen_on(listen_addr)?;

    info!(peer_id = %peer_id, port = port, "Cell process started");

    // Load boot configuration and bootstrap DHT
    let boot_config = boot::BootConfig::new(root_dir);
    let latest_version = resolver::ServiceResolver::new(boot_config.root.clone(), ipfs_url.clone())
        .get_latest_version()?
        .unwrap_or_else(|| "0.1.0".to_string());

    info!(version = %latest_version, "Using version for boot peers");

    // Get boot peers and connect to them
    let boot_peers = boot_config.get_all_boot_peers(&latest_version)?;
    for multiaddr in boot_peers {
        debug!(multiaddr = %multiaddr, "Connecting to boot peer");
        if let Err(e) = swarm.dial(multiaddr) {
            warn!(error = ?e, "Failed to dial boot peer");
        }
    }

    // Bootstrap Kademlia DHT
    info!("Bootstrapping Kademlia DHT");
    swarm.behaviour_mut().kad.bootstrap()?;

    // Register multiple protocol versions
    let resolver = resolver::ServiceResolver::new(boot_config.root.clone(), ipfs_url.clone());
    let available_versions = resolver.detect_versions()?;

    info!(versions = ?available_versions, "Registering protocol versions");

    // Register each version as a protocol
    for version in &available_versions {
        let protocol = resolver.get_protocol(version);
        debug!(protocol = %protocol, version = %version, "Registering protocol");
        // Note: In libp2p, we'll handle protocol negotiation in the event loop
        // rather than registering multiple protocols in the behavior
    }

    // For now, we'll use the latest version as the primary protocol
    let primary_protocol = resolver.get_protocol(&latest_version);
    info!(primary_protocol = %primary_protocol, "Using primary protocol");

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
            // 2. Resolve service bytecode via resolver
            // 3. Create new Proc instance with duplex pipes
            // 4. Spawn task to handle stream I/O between libp2p and WASM
            SwarmEvent::Behaviour(_event) => {
                debug!("Behaviour event received");
            }
            _ => {}
        }
    }
}

/// Check if a path is a valid IPFS-family path (IPFS, IPNS, or IPLD)
///
/// This centralizes IPFS path validation similar to Go's `path.NewPath(str)`.
/// Returns true if the path starts with a valid IPFS namespace prefix.
#[allow(dead_code)]
fn is_ipfs_path(path: &str) -> bool {
    path.starts_with("/ipfs/") || path.starts_with("/ipns/") || path.starts_with("/ipld/")
}

/// Download content from IPFS via HTTP API
#[allow(dead_code)]
async fn download_from_ipfs(ipfs_path: &str, ipfs_url: &str) -> Result<Vec<u8>> {
    let client = reqwest::Client::new();
    let url = format!("{}/api/v0/cat?arg={}", ipfs_url, ipfs_path);
    let response = client
        .post(&url)
        .send()
        .await
        .with_context(|| format!("Failed to connect to IPFS node at {}", ipfs_url))?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to download from IPFS: {}",
            response.status()
        ));
    }

    Ok(response.bytes().await?.to_vec())
}

/// Resolve binary path to WASM bytecode (like Go implementation)
#[allow(dead_code)]
async fn resolve_binary(name: &str, ipfs_url: &str) -> Result<Vec<u8>> {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    // Check if it's an IPFS-family path (IPFS, IPNS, or IPLD)
    if is_ipfs_path(name) {
        return download_from_ipfs(name, ipfs_url).await;
    }

    // Check if it's an absolute path
    if Path::new(name).is_absolute() {
        return fs::read(name).with_context(|| format!("Failed to read file: {}", name));
    }

    // Check if it's a relative path (starts with . or /)
    if name.starts_with('.') || name.starts_with('/') {
        return fs::read(name).with_context(|| format!("Failed to read file: {}", name));
    }

    // Check if it's in $PATH
    if let Ok(resolved_path) = Command::new("which").arg(name).output() {
        if resolved_path.status.success() {
            let path_str = String::from_utf8(resolved_path.stdout)?;
            let path_str = path_str.trim();
            if !path_str.is_empty() {
                return fs::read(path_str)
                    .with_context(|| format!("Failed to read resolved binary: {}", path_str));
            }
        }
    }

    // Try as a relative path in current directory
    if Path::new(name).exists() {
        return fs::read(name).with_context(|| format!("Failed to read file: {}", name));
    }

    Err(anyhow::anyhow!("Binary not found: {}", name))
}
