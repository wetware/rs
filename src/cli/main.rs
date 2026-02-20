use anyhow::{bail, Context, Result};

use clap::{Parser, Subcommand};
use membrane::Epoch;
use tokio::sync::watch;

use ww::cell::CellBuilder;
use ww::host;
use ww::image;
use ww::ipfs;
use ww::loaders::{ChainLoader, HostPathLoader, IpfsUnixfsLoader};

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
    /// Start the wetware daemon and boot an image.
    ///
    /// Each <IMAGE> is a local filesystem path or IPFS path (/ipfs/Qm...)
    /// pointing to an image directory with FHS layout. Multiple images are
    /// merged via per-file union — later layers override earlier layers.
    ///
    ///   <image>/bin/main.wasm   — guest entrypoint (required in merged result)
    ///   <image>/boot/<peerID>   — bootstrap peers (optional, one addr per line)
    ///   <image>/svc/<name>/     — background services (optional, nested images)
    ///   <image>/etc/            — reserved for configuration
    ///   <image>/usr/lib/        — reserved for shared libraries
    Run {
        /// Image layers: local directories or IPFS paths.
        /// Later layers override earlier layers (per-file union).
        /// The merged result must contain bin/main.wasm.
        #[arg(required = true)]
        images: Vec<String>,

        /// libp2p swarm port
        #[arg(long, default_value = "2020")]
        port: u16,

        /// Enable WASM debug info for guest processes
        #[arg(long)]
        wasm_debug: bool,

        /// Atom contract address (hex, 0x-prefixed). Enables the epoch
        /// pipeline: on-chain HEAD tracking, IPFS pinning, session
        /// invalidation on head changes.
        #[arg(long)]
        stem: Option<String>,

        /// HTTP JSON-RPC URL for eth_call / eth_getLogs.
        #[arg(long, default_value = "http://127.0.0.1:8545")]
        rpc_url: String,

        /// WebSocket JSON-RPC URL for eth_subscribe.
        #[arg(long, default_value = "ws://127.0.0.1:8545")]
        ws_url: String,

        /// Number of confirmations before finalizing a HeadUpdated event.
        #[arg(long, default_value = "6")]
        confirmation_depth: u64,
    },
}

/// Parse a hex-encoded contract address (with or without 0x prefix) into 20 bytes.
fn parse_contract_address(s: &str) -> Result<[u8; 20]> {
    let hex_str = s.strip_prefix("0x").unwrap_or(s);
    let bytes = hex::decode(hex_str).context("Invalid hex in --stem address")?;
    if bytes.len() != 20 {
        bail!(
            "Contract address must be 20 bytes, got {} bytes",
            bytes.len()
        );
    }
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&bytes);
    Ok(addr)
}

impl Commands {
    async fn run(self) -> Result<()> {
        match self {
            Commands::Run {
                images,
                port,
                wasm_debug,
                stem,
                rpc_url,
                ws_url,
                confirmation_depth,
            } => {
                ww::config::init_tracing();

                // Start the libp2p swarm.
                let wetware_host = host::WetwareHost::new(port)?;
                let network_state = wetware_host.network_state();
                let swarm_cmd_tx = wetware_host.swarm_cmd_tx();
                let stream_control = wetware_host.stream_control();
                tokio::spawn(wetware_host.run());

                // Build a chain loader: try IPFS first (if reachable), fall back to host FS.
                let ipfs_client = ipfs::HttpClient::new("http://localhost:5001".into());
                let loader = ChainLoader::new(vec![
                    Box::new(IpfsUnixfsLoader::new(ipfs_client.clone())),
                    Box::new(HostPathLoader),
                ]);

                // If --stem is provided, read the on-chain head and prepend it
                // as a base image layer.
                let mut all_images = Vec::new();
                let mut epoch_channel: Option<(watch::Sender<Epoch>, watch::Receiver<Epoch>)> =
                    None;
                let stem_config = if let Some(ref stem_addr) = stem {
                    let contract = parse_contract_address(stem_addr)?;
                    let head = image::read_contract_head(&rpc_url, &contract).await?;
                    let ipfs_path = image::cid_bytes_to_ipfs_path(&head.cid)?;

                    tracing::info!(
                        seq = head.seq,
                        path = %ipfs_path,
                        "Read on-chain HEAD; prepending as base image layer"
                    );

                    // Pin the initial head.
                    if let Err(e) = ipfs_client.pin_add(&ipfs_path).await {
                        tracing::warn!(path = %ipfs_path, "Failed to pin initial head: {e}");
                    }

                    all_images.push(ipfs_path);

                    let initial_epoch = Epoch {
                        seq: head.seq,
                        head: head.cid,
                        adopted_block: 0, // backfill will refine
                    };

                    epoch_channel = Some(watch::channel(initial_epoch));

                    Some(atom::IndexerConfig {
                        ws_url: ws_url.clone(),
                        http_url: rpc_url.clone(),
                        contract_address: contract,
                        start_block: 0,
                        getlogs_max_range: 1000,
                        reconnection: Default::default(),
                    })
                } else {
                    None
                };

                // Append user-specified layers after the on-chain base.
                all_images.extend(images);

                // Merge image layers into a single FHS root.
                let merged = image::merge_layers(&all_images, &ipfs_client).await?;
                let image_path = merged.path().to_string_lossy().to_string();

                tracing::info!(
                    layers = all_images.len(),
                    root = %image_path,
                    port,
                    "Booting merged image"
                );

                let mut builder = CellBuilder::new(image_path)
                    .with_loader(Box::new(loader))
                    .with_network_state(network_state)
                    .with_swarm_cmd_tx(swarm_cmd_tx)
                    .with_wasm_debug(wasm_debug)
                    .with_image_root(merged.path().into())
                    .with_ipfs_client(ipfs_client.clone());

                // If we have an epoch channel, give the receiver to the cell
                // and spawn the epoch pipeline with the sender.
                if let Some((epoch_tx, epoch_rx)) = epoch_channel {
                    builder = builder.with_epoch_rx(epoch_rx);

                    if let Some(config) = stem_config {
                        tokio::spawn(ww::epoch::run_epoch_pipeline(
                            config,
                            epoch_tx,
                            confirmation_depth,
                            ipfs_client,
                        ));
                    }
                }

                let cell = builder.build();

                // spawn_serving registers a /wetware/capnp/1.0.0 libp2p stream
                // handler that bootstraps each incoming connection with the
                // membrane exported by the kernel (pid0).
                let result = cell.spawn_serving(stream_control).await?;
                tracing::info!(code = result.exit_code, "Guest exited");

                // Hold `merged` alive until after guest exits.
                drop(merged);
                std::process::exit(result.exit_code);
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.command.run().await
}
