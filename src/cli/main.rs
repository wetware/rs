use anyhow::Result;

use clap::{Parser, Subcommand};

use ww::cell::CellBuilder;
use ww::host;
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
    /// <IMAGE> is either a local filesystem path or an IPFS path
    /// (/ipfs/Qm...) pointing to an image directory with FHS layout:
    ///
    ///   <image>/bin/main.wasm   — guest entrypoint (required)
    ///   <image>/boot/<peerID>   — bootstrap peers (optional, one addr per line)
    ///   <image>/etc/            — reserved for configuration
    ///   <image>/usr/lib/        — reserved for shared libraries
    Run {
        /// Image path: local directory or IPFS path containing bin/main.wasm
        image: String,

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
                image,
                port,
                wasm_debug,
            } => {
                ww::config::init_tracing();

                // Start the libp2p swarm.
                let wetware_host = host::WetwareHost::new(port)?;
                let network_state = wetware_host.network_state();
                let swarm_cmd_tx = wetware_host.swarm_cmd_tx();
                tokio::spawn(wetware_host.run());

                // Build a chain loader: try IPFS first (if reachable), fall back to host FS.
                let ipfs_client = ipfs::HttpClient::new("http://localhost:5001".into());
                let loader = ChainLoader::new(vec![
                    Box::new(IpfsUnixfsLoader::new(ipfs_client)),
                    Box::new(HostPathLoader),
                ]);

                tracing::info!(image = %image, port, "Booting image");

                let cell = CellBuilder::new(image)
                    .with_loader(Box::new(loader))
                    .with_network_state(network_state)
                    .with_swarm_cmd_tx(swarm_cmd_tx)
                    .with_wasm_debug(wasm_debug)
                    .build();

                let exit_code = cell.spawn().await?;
                tracing::info!(code = exit_code, "Guest exited");
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
