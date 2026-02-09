use anyhow::Result;
use std::process;

use clap::{Parser, Subcommand};
use ww::ipfs::HttpClient as IPFS;
use ww::{cell, host, loaders};

/// Parse a volume mount specification
///
/// Format: `hostpath:guestpath` or `hostpath:guestpath:ro` (or `rw`)
/// Returns: `(host_path, guest_path)`
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
        /// Path to root guest dir
        path: String, // TODO:  Path type that is a union{ipfs, local}

        /// Arguments to pass to the binary
        args: Vec<String>,

        /// IPFS node HTTP API endpoint
        #[arg(long, default_value = "http://localhost:5001")]
        ipfs: String,

        /// Environment variables to set for the process (e.g., DEBUG=1,LOG_LEVEL=info)
        #[arg(long, value_delimiter = ',')]
        env: Option<Vec<String>>,

        /// Enable WASM debug info
        #[arg(long)]
        wasm_debug: bool,

        /// Port to listen on
        #[arg(long, default_value = "2020")]
        port: u16,
        // Logging is configured via RUST_LOG.
    },
}

impl Commands {
    async fn run(self) -> Result<i32> {
        match self {
            Commands::Run {
                path,
                args,
                ipfs: ipfs_url,
                env,
                wasm_debug,
                port,
            } => {
                // Create IPFS clients (one for loader, one for cell)
                let ipfs = IPFS::new(ipfs_url.clone());
                let wetware_host = host::WetwareHost::new(port)?;
                let wasmtime_engine = wetware_host.wasmtime_engine();
                let network_state = wetware_host.network_state();
                let swarm_cmd_tx = wetware_host.swarm_cmd_tx();
                let host_task = tokio::spawn(wetware_host.run());

                // Build loader chain: IPFS first
                let mut loader_chain =
                    vec![Box::new(loaders::IpfsUnixfsLoader::new(ipfs.clone()))
                        as Box<dyn cell::Loader>];
                // finally, allow host paths (relative or absolute) as a fallback
                loader_chain.push(Box::new(loaders::HostPathLoader));
                // finally, build the chain loader
                let loader = Box::new(loaders::ChainLoader::new(loader_chain));

                let builder = cell::CommandBuilder::new(path)
                    .with_loader(loader)
                    .with_args(args)
                    .with_env(env.clone().unwrap_or_default())
                    .with_wasm_debug(wasm_debug)
                    .with_ipfs(ipfs)
                    .with_port(port)
                    .with_wasmtime_engine(wasmtime_engine)
                    .with_network_state(network_state)
                    .with_swarm_cmd_tx(swarm_cmd_tx);
                let cell = builder.build();
                let exit = cell.spawn().await;
                host_task.abort();
                exit
            }
        }
    }
}

#[tokio::main]
#[allow(unreachable_code)]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let exit_code = cli.command.run().await?;
    process::exit(exit_code);
    Ok(())
}
