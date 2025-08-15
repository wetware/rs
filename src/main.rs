use anyhow::Result;
use tracing::{debug, info};

mod boot;
mod config;
mod membrane;
mod rpc;
mod swarm_capnp;
use boot::{build_host, get_kubo_peers, SwarmManager};
use clap::{Parser, Subcommand};
use config::{init_tracing, AppConfig};

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
    /// Run a wetware node
    Run {
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
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            ipfs,
            loglvl,
            preset,
        } => {
            run_node(ipfs, loglvl, preset).await?;
        }
    }

    Ok(())
}

async fn run_node(
    ipfs: Option<String>,
    loglvl: Option<config::LogLevel>,
    preset: Option<String>,
) -> Result<()> {
    // Create application configuration from command line arguments and environment
    let app_config = AppConfig::from_args_and_env(ipfs, loglvl, preset);

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
