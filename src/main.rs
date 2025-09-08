use anyhow::Result;
use std::collections::HashMap;
use tracing::{debug, info};

mod boot;
mod config;
mod executor;
mod membrane;
mod rpc;
mod system;
mod system_capnp;
use boot::{build_host, get_kubo_peers, SwarmManager};
use clap::{Parser, Subcommand};
use config::{init_tracing, AppConfig};
use executor::{execute_subprocess, ExecutorEnv};
use system::FDManager;

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
    /// Execute a binary in a subprocess (like the Go implementation)
    Run {
        /// Binary to execute
        binary: String,

        /// Arguments to pass to the binary
        args: Vec<String>,

        /// IPFS node HTTP API endpoint (e.g., http://127.0.0.1:5001)
        /// If not provided, uses WW_IPFS environment variable or defaults to http://localhost:5001
        #[arg(long)]
        ipfs: Option<String>,

        /// Environment variables to set for the child process (e.g., DEBUG=1,LOG_LEVEL=info)
        #[arg(long, value_delimiter = ',')]
        env: Option<Vec<String>>,

        /// Map existing parent fd to name (e.g., db=3). Use --with-fd multiple times for multiple fds.
        #[arg(long, value_delimiter = ',')]
        with_fd: Option<Vec<String>>,

        /// Log level (trace, debug, info, warn, error)
        /// If not provided, uses WW_LOGLVL environment variable or defaults to info
        #[arg(long, value_name = "LEVEL")]
        loglvl: Option<config::LogLevel>,
    },

    /// Run a wetware node (daemon mode)
    Daemon {
        /// IPFS node HTTP API endpoint (e.g., http://127.0.0.1:5001)
        /// If not provided, uses WW_IPFS environment variable or defaults to http://localhost:5001
        #[arg(long)]
        ipfs: Option<String>,

        /// Log level (trace, debug, info, warn, error)
        /// If not provided, uses WW_LOGLVL environment variable or defaults to info
        #[arg(long, value_name = "LEVEL")]
        loglvl: Option<config::LogLevel>,

        /// Use preset configuration (minimal, development, production)
        #[arg(long, value_name = "PRESET")]
        preset: Option<String>,

        /// List of multiaddrs to listen on (e.g., "/ip4/0.0.0.0/tcp/2020,/ip6/::/tcp/2020")
        /// If not provided, defaults to IPv4 and IPv6 on port 2020
        #[arg(long, value_delimiter = ',', value_name = "MULTIADDRS")]
        listen_addrs: Option<Vec<String>>,
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
            with_fd,
            loglvl,
        } => {
            run_subprocess(binary, args, ipfs, env, with_fd, loglvl).await?;
        }
        Commands::Daemon {
            ipfs,
            loglvl,
            preset,
            listen_addrs,
        } => {
            run_daemon(ipfs, loglvl, preset, listen_addrs).await?;
        }
    }

    Ok(())
}

async fn run_subprocess(
    binary: String,
    args: Vec<String>,
    ipfs: Option<String>,
    env: Option<Vec<String>>,
    with_fd: Option<Vec<String>>,
    loglvl: Option<config::LogLevel>,
) -> Result<()> {
    // Initialize tracing with the determined log level
    let log_level = loglvl.unwrap_or_else(config::get_log_level);
    config::init_tracing(log_level, loglvl);

    // Get IPFS URL
    let ipfs_url = ipfs.unwrap_or_else(config::get_ipfs_url);

    info!(binary = %binary, args = ?args, ipfs_url = %ipfs_url, "Starting subprocess execution");

    // Create executor environment
    let executor_env = ExecutorEnv::new(ipfs_url)?;

    // Parse environment variables
    let mut env_vars = HashMap::new();
    if let Some(env_flags) = env {
        for env_flag in env_flags {
            if let Some((key, value)) = env_flag.split_once('=') {
                env_vars.insert(key.to_string(), value.to_string());
            } else {
                return Err(anyhow::anyhow!(
                    "Invalid environment variable format: {}",
                    env_flag
                ));
            }
        }
    }

    // Set up file descriptor manager
    let fd_manager = if let Some(fd_flags) = with_fd {
        Some(FDManager::new(fd_flags)?)
    } else {
        None
    };

    // Add FD environment variables
    if let Some(ref fd_manager) = fd_manager {
        let fd_env_vars = fd_manager.generate_env_vars();
        env_vars.extend(fd_env_vars);
    }

    // Execute the subprocess
    let exit_code = execute_subprocess(binary, args, env_vars, fd_manager, &executor_env).await?;

    // Exit with the same code as the subprocess
    std::process::exit(exit_code);
}

async fn run_daemon(
    ipfs: Option<String>,
    loglvl: Option<config::LogLevel>,
    preset: Option<String>,
    listen_addrs: Option<Vec<String>>,
) -> Result<()> {
    // Create application configuration from command line arguments and environment
    let app_config = AppConfig::from_args_and_env(ipfs, loglvl, preset, listen_addrs);

    // Initialize tracing with the determined log level
    init_tracing(app_config.log_level, loglvl);

    // Log the configuration summary
    app_config.log_summary();

    // 1. Query Kubo node to get its peers for DHT bootstrap
    let kubo_start = std::time::Instant::now();
    let kubo_peers = get_kubo_peers(&app_config.ipfs_url).await?;
    let kubo_duration = kubo_start.elapsed();

    if kubo_peers.is_empty() {
        info!("No peers found from Kubo node");
    } else {
        info!(peer_count = kubo_peers.len(), "Found peers from Kubo node");
        // Individual peer details are debug level to reduce spam
        for (i, (peer_id, peer_addr)) in kubo_peers.iter().enumerate() {
            debug!(index = i + 1, peer_id = %peer_id, peer_addr = %peer_addr, "Peer details");
        }
    }
    debug!(
        duration_ms = kubo_duration.as_millis(),
        "Kubo peer discovery completed"
    );

    // 2. Build the host with configuration
    let host_start = std::time::Instant::now();

    let (_keypair, peer_id, swarm) = build_host(Some(app_config.host_config)).await?;
    let _host_duration = host_start.elapsed();

    // Get listen addresses for the startup message
    let listen_addrs: Vec<_> = swarm.listeners().collect();

    // Clean startup message with key info
    info!(
        peer_id = %peer_id,
        listen_addrs = ?listen_addrs,
        version = "0.1.0",
        "Wetware started"
    );

    info!("⚗️ Starting wetware cell...");
    info!("Wetware protocol /ww/0.1.0 is available and exported through transport layer");
    info!("Cap'n Proto RPC with importer capability is ready for connections");

    // 3. Create SwarmManager and bootstrap DHT
    let mut swarm_manager = SwarmManager::new(swarm, peer_id);

    // Log wetware protocol information
    info!(
        protocol = swarm_manager.get_default_protocol(),
        "Wetware protocol available"
    );

    // Add IPFS peers to Kademlia routing table for DHT bootstrap
    // This populates our routing table with known good peers before we start connecting
    info!(
        "Adding {} IPFS peers to Kademlia routing table",
        kubo_peers.len()
    );
    // Individual peer additions are debug level to reduce spam
    for (peer_id, peer_addr) in &kubo_peers {
        swarm_manager.add_peer_to_routing_table(peer_id, peer_addr);
        debug!(peer_id = %peer_id, peer_addr = %peer_addr, "Added IPFS peer to routing table");
    }

    // Bootstrap DHT with peers from Kubo
    let bootstrap_start = std::time::Instant::now();
    swarm_manager.bootstrap_dht(kubo_peers).await?;
    let bootstrap_duration = bootstrap_start.elapsed();
    info!(
        duration_ms = bootstrap_duration.as_millis(),
        "DHT bootstrap completed"
    );

    // Announce ourselves as a provider of "ww"
    let announce_start = std::time::Instant::now();
    swarm_manager.announce_provider("ww").await?;
    let announce_duration = announce_start.elapsed();
    info!(
        duration_ms = announce_duration.as_millis(),
        "Provider announcement completed"
    );

    // Query for providers of "ww" to see if we can find ourselves
    let query_start = std::time::Instant::now();
    swarm_manager.query_providers("ww").await?;
    let query_duration = query_start.elapsed();
    info!(
        duration_ms = query_duration.as_millis(),
        "Provider query completed"
    );

    // 6. Run the DHT event loop
    swarm_manager.run_event_loop().await?;
    Ok(())
}
