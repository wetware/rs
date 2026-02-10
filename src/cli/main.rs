use anyhow::Result;

use clap::{Parser, Subcommand};
use tokio::net::TcpListener;
use ww::host;
use ww::rpc::server::serve_rpc_clients;

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
    /// Start the wetware daemon
    Run {
        /// libp2p swarm port
        #[arg(long, default_value = "2020")]
        port: u16,

        /// TCP port for Cap'n Proto RPC clients
        #[arg(long, default_value = "2021")]
        rpc_port: u16,

        /// Enable WASM debug info for guest processes
        #[arg(long)]
        wasm_debug: bool,
    },
}

impl Commands {
    async fn run(self) -> Result<()> {
        match self {
            Commands::Run {
                port,
                rpc_port,
                wasm_debug,
            } => {
                ww::config::init_tracing();

                let wetware_host = host::WetwareHost::new(port)?;
                let network_state = wetware_host.network_state();
                let swarm_cmd_tx = wetware_host.swarm_cmd_tx();

                // Swarm event loop is Send — run it on the tokio runtime.
                tokio::spawn(wetware_host.run());

                let listener = TcpListener::bind(format!("0.0.0.0:{rpc_port}")).await?;
                tracing::info!(rpc_port, port, "Wetware daemon listening");

                // capnp-rpc futures are !Send, so the RPC server runs on a LocalSet.
                let local = tokio::task::LocalSet::new();
                local
                    .run_until(serve_rpc_clients(
                        listener,
                        network_state,
                        swarm_cmd_tx,
                        wasm_debug,
                    ))
                    .await?;

                Ok(())
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.command.run().await
}
