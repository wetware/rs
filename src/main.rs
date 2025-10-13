use anyhow::{Result, Context};
use tracing::{info, debug, warn};

mod boot;
mod config;
mod system;
mod cell;
use clap::{Parser, Subcommand};

/// Network behavior for Wetware cells
#[derive(libp2p::swarm::NetworkBehaviour)]
struct WetwareBehaviour {
    kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
    identify: libp2p::identify::Behaviour,
}

#[derive(Parser)]
#[command(name = "ww")]
#[command(
    about = "P2P sandbox for Web3 applications that execute untrusted code on public networks."
)]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Execute a binary (WASM or subprocess)
    Run {
        /// Binary to execute
        binary: String,

        /// Arguments to pass to the binary
        args: Vec<String>,

        /// IPFS node HTTP API endpoint (e.g., http://127.0.0.1:5001)
        /// If not provided, uses WW_IPFS environment variable or defaults to http://localhost:5001
        #[arg(long)]
        ipfs: Option<String>,

        /// Environment variables to set for the process (e.g., DEBUG=1,LOG_LEVEL=info)
        #[arg(long, value_delimiter = ',')]
        env: Option<Vec<String>>,

        /// Enable WASM debug info
        #[arg(long)]
        wasm_debug: bool,

        /// Port to listen on
        #[arg(long, default_value = "2020")]
        port: u16,

        /// Log level (trace, debug, info, warn, error)
        /// If not provided, uses WW_LOGLVL environment variable or defaults to info
        #[arg(long, value_name = "LEVEL")]
        loglvl: Option<config::LogLevel>,
    },

}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            binary,
            args,
            ipfs,
            env,
            wasm_debug,
            port,
            loglvl,
        } => {
            run_cell(CellRunConfig {
                binary,
                args,
                ipfs,
                env,
                wasm_debug,
                port,
                loglvl,
            }).await?;
        }
    }

    Ok(())
}

/// Configuration for running a cell
#[derive(Debug)]
struct CellRunConfig {
    binary: String,
    args: Vec<String>,
    ipfs: Option<String>,
    env: Option<Vec<String>>,
    wasm_debug: bool,
    port: u16,
    loglvl: Option<config::LogLevel>,
}

async fn run_cell(config: CellRunConfig) -> Result<()> {
    // Initialize tracing with the determined log level
    let log_level = config.loglvl.unwrap_or_else(config::get_log_level);
    config::init_tracing(log_level, config.loglvl);

    // Get IPFS URL
    let ipfs_url = config.ipfs.unwrap_or_else(config::get_ipfs_url);

    // All binaries are treated as WASM (like Go implementation)
    run_wasm(config.binary, config.args, ipfs_url, config.env, config.wasm_debug, config.port).await
}

async fn run_wasm(
    binary: String,
    args: Vec<String>,
    ipfs_url: String,
    env: Option<Vec<String>>,
    wasm_debug: bool,
    port: u16,
) -> Result<()> {
    use cell::{Proc, Config};

    info!(binary = %binary, "Starting cell execution");

    // Resolve binary path (like Go implementation)
    let bytecode = resolve_binary(&binary, &ipfs_url).await
        .with_context(|| format!("Failed to resolve binary: {}", binary))?;

    // Create process configuration
    let mut config = Config::new()
        .with_wasm_debug(wasm_debug)
        .with_args(args);

    // Parse environment variables
    if let Some(env_vars) = env {
        config = config.with_env(env_vars);
    }

    // Create cell process
    let proc = Proc::new(config, &bytecode)
        .with_context(|| format!("Failed to create cell process from binary: {}", binary))?;

    // Run with libp2p host
    run_cell_async(proc, port).await
}

async fn run_cell_async(proc: cell::Proc, port: u16) -> Result<()> {
    use libp2p::{identity, Multiaddr, SwarmBuilder};
    use libp2p::swarm::SwarmEvent;
    use futures::StreamExt;
    use std::time::Duration;

    let keypair = identity::Keypair::generate_ed25519();
    let peer_id = keypair.public().to_peer_id();

    let behaviour = WetwareBehaviour {
        kad: libp2p::kad::Behaviour::new(peer_id, libp2p::kad::store::MemoryStore::new(peer_id)),
        identify: libp2p::identify::Behaviour::new(
            libp2p::identify::Config::new("ww/1.0.0".to_string(), keypair.public())
        ),
    };

    let mut swarm = SwarmBuilder::with_existing_identity(keypair)
        .with_tokio()
        .with_tcp(Default::default(), libp2p::noise::Config::new, libp2p::yamux::Config::default)?
        .with_behaviour(|_| behaviour)?
        .with_swarm_config(|c| c.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    let listen_addr: Multiaddr = format!("/ip4/0.0.0.0/tcp/{}", port).parse()?;
    swarm.listen_on(listen_addr)?;

    info!(peer_id = %peer_id, port = port, "Cell process started in async mode");

    // Get the protocol for this process
    let protocol = proc.protocol();
    let proc_id = proc.id().to_string();
    info!(protocol = %protocol, proc_id = %proc_id, "Cell protocol available for connections");

    // CANONICAL BEHAVIOR: _start was already called during instantiation for initialization
    // Now we start the libp2p event loop to handle incoming streams
    info!("Cell initialized and ready for connections. Incoming streams will be routed to exported methods.");
    
    // Run the event loop (simplified for now)
    loop {
        match swarm.select_next_some().await {
            SwarmEvent::NewListenAddr { address, .. } => {
                info!(address = %address, peer_id = %peer_id, protocol = %protocol, "Listening on address");
            }
            SwarmEvent::IncomingConnection { .. } => {
                debug!("Incoming connection");
            }
            SwarmEvent::ConnectionEstablished { peer_id: remote_peer, .. } => {
                info!(peer_id = %remote_peer, "Connection established");
            }
            SwarmEvent::ConnectionClosed { peer_id: remote_peer, .. } => {
                debug!(peer_id = %remote_peer, "Connection closed");
            }
            SwarmEvent::IncomingConnectionError { error, .. } => {
                warn!(error = ?error, "Incoming connection error");
            }
            _ => {}
        }
    }
}


/// Resolve binary path to WASM bytecode (like Go implementation)
async fn resolve_binary(name: &str, ipfs_url: &str) -> Result<Vec<u8>> {
    use std::fs;
    use std::path::Path;
    use std::process::Command;

    // Check if it's an IPFS path
    if name.starts_with("/ipfs/") {
        // Download from IPFS
        let client = reqwest::Client::new();
        let url = format!("{}/api/v0/cat?arg={}", ipfs_url, name);
        let response = client.post(&url).send().await
            .with_context(|| format!("Failed to connect to IPFS node at {}", ipfs_url))?;
        if !response.status().is_success() {
            return Err(anyhow::anyhow!("Failed to download from IPFS: {}", response.status()));
        }
        return Ok(response.bytes().await?.to_vec());
    }

    // Check if it's an absolute path
    if Path::new(name).is_absolute() {
        return fs::read(name)
            .with_context(|| format!("Failed to read file: {}", name));
    }

    // Check if it's a relative path (starts with . or /)
    if name.starts_with('.') || name.starts_with('/') {
        return fs::read(name)
            .with_context(|| format!("Failed to read file: {}", name));
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
        return fs::read(name)
            .with_context(|| format!("Failed to read file: {}", name));
    }

    Err(anyhow::anyhow!("Binary not found: {}", name))
}

