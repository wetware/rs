use anyhow::Result;

use clap::{Parser, Subcommand};

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
    },
}

impl Commands {
    async fn run(self) -> Result<()> {
        match self {
            Commands::Run {
                images,
                port,
                wasm_debug,
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

                // Merge image layers into a single FHS root.
                let merged = image::merge_layers(&images, &ipfs_client).await?;
                let image_path = merged.path().to_string_lossy().to_string();

                tracing::info!(
                    layers = images.len(),
                    root = %image_path,
                    port,
                    "Booting merged image"
                );

                let cell = CellBuilder::new(image_path)
                    .with_loader(Box::new(loader))
                    .with_network_state(network_state)
                    .with_swarm_cmd_tx(swarm_cmd_tx)
                    .with_wasm_debug(wasm_debug)
                    .with_image_root(merged.path().into())
                    .build();

                // spawn_serving registers a /wetware/capnp/1.0.0 libp2p stream
                // handler that bootstraps each incoming connection with the
                // membrane exported by the kernel (pid0).
                let (exit_code, _guest_membrane) =
                    cell.spawn_serving(stream_control).await?;
                tracing::info!(code = exit_code, "Guest exited");

                // Hold `merged` alive until after guest exits.
                drop(merged);
                std::process::exit(exit_code);
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.command.run().await
}
