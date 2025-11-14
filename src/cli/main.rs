use anyhow::{anyhow, Result};

use clap::{Parser, Subcommand};
use ww::ipfs::HttpClient as IPFS;
use ww::{cell, config, loaders};

/// Parse a volume mount specification
///
/// Format: `hostpath:guestpath` or `hostpath:guestpath:ro` (or `rw`)
/// Returns: `(host_path, guest_path)`
fn parse_volume_mount(vol: &str) -> Result<(String, String)> {
    let parts: Vec<&str> = vol.split(':').collect();

    match parts.len() {
        2 => {
            // hostpath:guestpath
            Ok((parts[0].to_string(), parts[1].to_string()))
        }
        3 => {
            // hostpath:guestpath:ro or hostpath:guestpath:rw
            let mode = parts[2];
            if mode != "ro" && mode != "rw" {
                return Err(anyhow!("Invalid volume mount mode: {} (expected 'ro' or 'rw')", mode));
            }
            // TODO: Store mode for future read-only enforcement
            Ok((parts[0].to_string(), parts[1].to_string()))
        }
        _ => Err(anyhow!(
            "Invalid volume mount format: {} (expected 'hostpath:guestpath' or 'hostpath:guestpath:ro')",
            vol
        )),
    }
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

        /// Log level (trace, debug, info, warn, error)
        /// If not provided, uses WW_LOGLVL environment variable or defaults to info
        #[arg(long, value_name = "LEVEL")]
        loglvl: Option<config::LogLevel>,

        /// Volume mounts in format hostpath:guestpath or hostpath:guestpath:ro
        /// Can be specified multiple times
        #[arg(short = 'v', long = "volume")]
        volume: Vec<String>,
    },
}

impl Commands {
    async fn run(self) -> Result<()> {
        match self {
            Commands::Run {
                path,
                args,
                ipfs: ipfs_url,
                env,
                wasm_debug,
                port,
                loglvl,
                volume,
            } => {
                // Create IPFS clients (one for loader, one for cell)
                let ipfs = IPFS::new(ipfs_url.clone());

                // Build loader chain: IPFS first
                let mut loader_chain =
                    vec![Box::new(loaders::IpfsUnixfsLoader::new(ipfs.clone()))
                        as Box<dyn cell::Loader>];
                // then parse & add volume mounts
                for vol in volume {
                    let (host_path, guest_path) = parse_volume_mount(&vol)?;
                    loader_chain.push(Box::new(loaders::LocalFSLoader::new(guest_path, host_path)));
                }
                // finally, build the chain loader
                let loader = Box::new(loaders::ChainLoader::new(loader_chain));

                let cell = cell::CommandBuilder::new(path)
                    .with_loader(loader)
                    .with_args(args)
                    .with_env(env.clone().unwrap_or_default())
                    .with_wasm_debug(wasm_debug)
                    .with_ipfs(ipfs)
                    .with_port(port)
                    .with_loglvl(loglvl)
                    .build();
                cell.spawn().await
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.command.run().await
}
