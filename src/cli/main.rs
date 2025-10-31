use anyhow::Result;

use clap::{Parser, Subcommand};
use ww::{cli::Command, config, loaders};

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

impl Commands {
    async fn run(self) -> Result<()> {
        match self {
            Commands::Run {
                binary,
                args,
                ipfs,
                env,
                wasm_debug,
                port,
                loglvl,
            } => {
                // Create chain of loaders: try IpfsFSLoader first, then LocalFSLoader
                use ww::net::ipfs;
                let ipfs_url = ipfs.unwrap_or_else(|| ipfs::get_ipfs_url());
                let loader = Box::new(loaders::ChainLoader::new(vec![
                    Box::new(loaders::IpfsFSLoader::new(ipfs_url.clone())),
                    Box::new(loaders::LocalFSLoader),
                ]));

                Command {
                    binary,
                    args,
                    loader,
                    ipfs: Some(ipfs_url),
                    env,
                    wasm_debug,
                    port,
                    loglvl,
                }
                .run()
                .await
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.command.run().await
}
