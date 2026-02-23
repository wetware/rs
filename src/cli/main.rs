use anyhow::{bail, Context, Result};

use clap::{Parser, Subcommand};
use k256::ecdsa::VerifyingKey;
use membrane::Epoch;
use std::path::{Path, PathBuf};
use tokio::sync::watch;

use ww::cell::CellBuilder;
use ww::host;
use ww::image;
use ww::ipfs;
use ww::loaders::{ChainLoader, HostPathLoader, IpfsUnixfsLoader};

#[derive(Parser)]
#[command(name = "ww")]
#[command(about = "Agentic OS for autonomous programs that coordinate across trust boundaries.")]
#[command(version = "0.1.0")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new wetware environment.
    ///
    /// Creates a boot/ directory for the WASM artifact. Other FHS
    /// directories are created lazily as needed.
    Init {
        /// Optional subdirectory to initialize
        #[arg(value_name = "SUBDIR")]
        subdir: Option<PathBuf>,

        /// Optional template to scaffold (e.g., 'rust')
        #[arg(long, value_name = "TEMPLATE")]
        template: Option<String>,
    },

    /// Build a guest project, placing the compiled WASM at boot/main.wasm.
    ///
    /// Compiles a Rust project targeting wasm32-wasip2 and copies the
    /// artifact into the project's FHS root at boot/main.wasm.
    ///
    /// Expects Cargo.toml at the root of <path>.
    Build {
        /// Path to the wetware environment (default: current directory)
        #[arg(default_value = ".", value_name = "PATH")]
        path: PathBuf,
    },

    /// Run a wetware environment.
    ///
    /// Single path (developer mode):
    ///   ww run [<path>]
    ///   Loads and executes boot/main.wasm from the FHS environment.
    ///
    /// Multiple paths (daemon/advanced mode):
    ///   ww run <image1> [<image2> ...]
    ///   Merges multiple image layers (local or IPFS) and boots the merged result.
    Run {
        /// Path(s) to environment or image layers.
        /// Single path: local FHS environment or IPFS path (defaults to ".")
        /// Multiple paths: image layers to merge (later layers override earlier)
        #[arg(default_value = ".", value_name = "PATH")]
        paths: Vec<String>,

        /// libp2p swarm port
        #[arg(long, default_value = "2025")]
        port: u16,

        /// Enable WASM debug info for guest processes
        #[arg(long)]
        wasm_debug: bool,

        /// Path to a secp256k1 identity file (base58btc or hex, 32 bytes).
        /// Generate one with `ww keygen`. Resolution order:
        ///   1. This flag  2. $WW_IDENTITY env var
        ///   3. /etc/identity in image layers  4. ephemeral (not persisted)
        #[arg(long, value_name = "PATH")]
        identity: Option<String>,

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

    /// Generate a new secp256k1 identity secret.
    ///
    /// Prints a base58btc-encoded secret key to stdout.  Metadata (EVM
    /// address, Peer ID) is printed to stderr so stdout stays pipeable:
    ///
    ///     ww keygen > ~/.ww/key
    ///     ww keygen --output ~/.ww/key   # equivalent
    Keygen {
        /// Write the secret to a file instead of stdout.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },

    /// Snapshot a project and push it to IPFS.
    ///
    /// Adds the entire FHS tree to IPFS as a UnixFS directory and returns
    /// the resulting CID. Optionally updates the on-chain Atom contract.
    ///
    /// Expects boot/main.wasm to exist (run 'ww build' first).
    Push {
        /// Path to the wetware environment (default: current directory)
        #[arg(default_value = ".", value_name = "PATH")]
        path: PathBuf,

        /// IPFS HTTP API endpoint
        #[arg(long, default_value = "http://localhost:5001")]
        ipfs_url: String,

        /// Atom contract address (hex, 0x-prefixed). If provided, updates
        /// the on-chain HEAD to point to the published CID.
        #[arg(long)]
        stem: Option<String>,

        /// HTTP JSON-RPC URL for eth_sendTransaction.
        #[arg(long, default_value = "http://127.0.0.1:8545")]
        rpc_url: String,

        /// Private key (hex, 0x-prefixed) to sign contract transactions.
        /// Required only if --stem is provided.
        #[arg(long)]
        private_key: Option<String>,
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
            Commands::Init { subdir, template } => Self::init(subdir, template).await,
            Commands::Build { path } => Self::build(path).await,
            Commands::Run {
                paths,
                port,
                wasm_debug,
                identity,
                stem,
                rpc_url,
                ws_url,
                confirmation_depth,
            } => {
                if paths.len() == 1 {
                    // Single path: developer mode
                    Self::run_env(
                        PathBuf::from(&paths[0]),
                        port,
                        wasm_debug,
                        identity,
                        stem,
                        rpc_url,
                        ws_url,
                        confirmation_depth,
                    )
                    .await
                } else {
                    // Multiple paths: daemon mode
                    Self::run_daemon(
                        paths,
                        port,
                        wasm_debug,
                        identity,
                        stem,
                        rpc_url,
                        ws_url,
                        confirmation_depth,
                    )
                    .await
                }
            }
            Commands::Push {
                path,
                ipfs_url,
                stem,
                rpc_url,
                private_key,
            } => Self::push(path, ipfs_url, stem, rpc_url, private_key).await,
            Commands::Keygen { output } => Self::keygen(output).await,
        }
    }

    /// Initialize a new wetware environment with FHS skeleton
    async fn init(subdir: Option<PathBuf>, template: Option<String>) -> Result<()> {
        let target_dir = match subdir {
            Some(dir) => dir,
            None => PathBuf::from("."),
        };

        // Expand "." to the current directory path for better error messages
        let target_dir = if target_dir == Path::new(".") {
            std::env::current_dir()?
        } else {
            target_dir
        };

        // Check if the environment already exists
        let boot_dir = target_dir.join("boot");
        if boot_dir.exists() {
            bail!(
                "Wetware environment already exists at: {}",
                target_dir.display()
            );
        }

        // Create boot/ — the only required directory.
        // Other FHS dirs (etc/, usr/, var/, etc.) are created lazily as needed.
        std::fs::create_dir_all(target_dir.join("boot"))
            .context("Failed to create boot directory")?;

        // Handle template scaffolding if requested
        if let Some(template_name) = template {
            match template_name.as_str() {
                "rust" => Self::scaffold_rust_template(&target_dir)?,
                _ => bail!(
                    "Unknown template: '{}'. Supported templates: rust",
                    template_name
                ),
            }
        }

        println!(
            "Initialized wetware environment at: {}",
            target_dir.display()
        );
        Ok(())
    }

    /// Scaffold a Rust guest template with Cargo.toml and src/lib.rs
    fn scaffold_rust_template(target_dir: &std::path::Path) -> Result<()> {
        // Create src directory
        let src_dir = target_dir.join("src");
        std::fs::create_dir_all(&src_dir).context("Failed to create src directory")?;

        // Create Cargo.toml
        let cargo_toml_content = r#"[package]
name = "my-guest"
version = "0.1.0"
edition = "2021"

# Wetware guest runtime - adjust the path if running outside the repo
[dependencies]
runtime = { path = "../../std/runtime" }
capnp = "0.23.2"
capnp-rpc = "0.23.0"

[lib]
crate-type = ["cdylib"]

[build-dependencies]
capnpc = "0.23.3"
"#;

        let cargo_toml_path = target_dir.join("Cargo.toml");
        std::fs::write(&cargo_toml_path, cargo_toml_content)
            .context("Failed to write Cargo.toml")?;

        // Create src/lib.rs - WASM guest entry point
        let lib_rs_content = r#"/// Simple Wetware guest that runs an async task.
///
/// The `runtime::run()` function bootstraps the RPC connection to the host,
/// then executes the provided async closure.
///
/// Example with host communication:
/// ```no_run
/// runtime::run::<capnp::capability::Client, _, _>(|host| async move {
///     let executor = host.executor_request().send().pipeline.get_executor();
///     executor.echo_request().send().promise.await?;
///     Ok(())
/// });
/// ```

#[no_mangle]
pub extern "C" fn _start() {
    runtime::run::<capnp::capability::Client, _, _>(|_host| async move {
        println!("Hello from Wetware!");
        Ok(())
    });
}
"#;

        let lib_rs_path = src_dir.join("lib.rs");
        std::fs::write(&lib_rs_path, lib_rs_content).context("Failed to write src/lib.rs")?;

        println!("Scaffolded Rust template:");
        println!("  Cargo.toml - Guest project configuration");
        println!("  src/lib.rs - Guest entry point with example");
        Ok(())
    }

    /// Build a guest project, placing the compiled WASM at boot/main.wasm
    async fn build(path: PathBuf) -> Result<()> {
        let cargo_toml = path.join("Cargo.toml");

        if !cargo_toml.exists() {
            bail!(
                "Cargo.toml not found at: {}\n\
                 \n\
                 Please run 'ww init' first to scaffold a new environment.",
                cargo_toml.display()
            );
        }

        println!("Building WASM artifact for: {}", path.display());

        // Run cargo build for wasm32-wasip2 target
        let output = std::process::Command::new("cargo")
            .args([
                "build",
                "--target",
                "wasm32-wasip2",
                "--release",
                "--manifest-path",
                cargo_toml
                    .to_str()
                    .ok_or_else(|| anyhow::anyhow!("Invalid path"))?,
            ])
            .output()
            .context("Failed to execute cargo build")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);

            if stderr.contains("cannot find `wasm32-wasip2` target")
                || stderr.contains("target `wasm32-wasip2` not installed")
                || stderr.contains("target `wasm32-wasip1` not installed")
            {
                bail!(
                    "wasm32-wasip2 target is not installed.\n\
                     \n\
                     Install it with:\n\
                       rustup target add wasm32-wasip2"
                );
            }

            bail!("cargo build failed:\n{}", stderr);
        }

        // Find the built WASM artifact
        let target_dir = path.join("target/wasm32-wasip2/release");
        let mut wasm_file = None;

        // Look for the first .wasm file (or the crate name if it matches)
        for entry in std::fs::read_dir(&target_dir).context("Failed to read target directory")? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "wasm") {
                wasm_file = Some(path);
                break;
            }
        }

        let src_wasm = wasm_file.ok_or_else(|| {
            anyhow::anyhow!(
                "WASM artifact not found in {}. Check your Cargo.toml configuration.",
                target_dir.display()
            )
        })?;

        // Copy to boot/main.wasm
        let boot_dir = path.join("boot");
        std::fs::create_dir_all(&boot_dir).context("Failed to create boot directory")?;

        let dst_wasm = boot_dir.join("main.wasm");
        std::fs::copy(&src_wasm, &dst_wasm).context(format!(
            "Failed to copy {} to {}",
            src_wasm.display(),
            dst_wasm.display()
        ))?;

        println!("Successfully built: {}", dst_wasm.display());
        Ok(())
    }

    /// Resolve the node's secp256k1 signing key and verifying key.
    ///
    /// Resolution order (first match wins):
    ///   1. `--identity PATH` CLI flag
    ///   2. `$WW_IDENTITY` environment variable
    ///   3. `/etc/identity` present in the merged image layers
    ///   4. Ephemeral key (generated at runtime, discarded on exit)
    ///
    /// For sources 1, 2, and 4 the key is written to `merged_root/etc/identity`
    /// so the guest can read it from `/etc/identity` via WASI.  Source 3 already
    /// has the file in place — no injection needed.
    ///
    /// Returns `(signing_key, verifying_key, source_description)`.
    fn resolve_identity(
        flag: Option<&str>,
        merged_root: &std::path::Path,
    ) -> Result<(k256::ecdsa::SigningKey, VerifyingKey, &'static str)> {
        // 1. --identity CLI flag
        if let Some(path) = flag {
            let sk = ww::keys::load(path)?;
            Self::inject_identity(&sk, merged_root)?;
            let vk = *sk.verifying_key();
            return Ok((sk, vk, "--identity"));
        }

        // 2. $WW_IDENTITY environment variable
        if let Ok(path) = std::env::var("WW_IDENTITY") {
            if !path.is_empty() {
                let sk = ww::keys::load(&path)?;
                Self::inject_identity(&sk, merged_root)?;
                let vk = *sk.verifying_key();
                return Ok((sk, vk, "$WW_IDENTITY"));
            }
        }

        // 3. /etc/identity already present in the merged image layers
        let identity_path = merged_root.join("etc/identity");
        if identity_path.exists() {
            let path_str = identity_path
                .to_str()
                .context("/etc/identity path is non-UTF-8")?;
            let sk = ww::keys::load(path_str)?;
            let vk = *sk.verifying_key();
            return Ok((sk, vk, "/etc/identity (image)"));
        }

        // 4. Ephemeral: generate, inject, and return
        let sk = ww::keys::generate()?;
        Self::inject_identity(&sk, merged_root)?;
        let vk = *sk.verifying_key();
        Ok((sk, vk, "ephemeral"))
    }

    /// Write a base58btc-encoded signing key to `<merged_root>/etc/identity`.
    fn inject_identity(sk: &k256::ecdsa::SigningKey, merged_root: &std::path::Path) -> Result<()> {
        let etc = merged_root.join("etc");
        std::fs::create_dir_all(&etc).context("create /etc in merged FHS")?;
        std::fs::write(etc.join("identity"), ww::keys::encode(sk))
            .context("write /etc/identity to merged FHS")
    }

    /// Generate a new secp256k1 identity secret.
    async fn keygen(output: Option<PathBuf>) -> Result<()> {
        let sk = ww::keys::generate()?;
        let encoded = ww::keys::encode(&sk);

        let kp = ww::keys::to_libp2p(&sk)?;
        let addr = ww::keys::ethereum_address(&sk);
        let peer_id = kp.public().to_peer_id();

        if let Some(path) = output {
            ww::keys::save(&sk, &path)?;
            eprintln!("Secret written to: {}", path.display());
        } else {
            println!("{encoded}");
        }

        eprintln!("EVM address:    0x{}", hex::encode(addr));
        eprintln!("Peer ID:        {peer_id}");
        Ok(())
    }

    /// Run a wetware environment
    #[allow(clippy::too_many_arguments)]
    async fn run_env(
        path: PathBuf,
        port: u16,
        wasm_debug: bool,
        identity: Option<String>,
        stem: Option<String>,
        rpc_url: String,
        ws_url: String,
        confirmation_depth: u64,
    ) -> Result<()> {
        let path_str = path.to_string_lossy().to_string();
        let is_ipfs_path = ipfs::is_ipfs_path(&path_str);

        if !is_ipfs_path {
            // For local paths, verify boot/main.wasm exists
            let boot_wasm = path.join("boot/main.wasm");

            if !boot_wasm.exists() {
                bail!(
                    "boot/main.wasm not found at: {}\n\
                     \n\
                     Please run 'ww build' first to compile your guest program.",
                    boot_wasm.display()
                );
            }

            // Create a compatibility layer: the runtime expects bin/main.wasm.
            // We copy boot/main.wasm to bin/main.wasm for the image merging logic.
            let bin_dir = path.join("bin");
            std::fs::create_dir_all(&bin_dir).context("Failed to create bin directory")?;

            let bin_wasm = bin_dir.join("main.wasm");
            std::fs::copy(&boot_wasm, &bin_wasm)
                .context("Failed to prepare WASM artifact for runtime")?;
        }

        // Pass the environment path as a single image layer to the daemon logic
        // (supports both local paths and IPFS paths)
        let images = vec![path_str];

        ww::config::init_tracing();

        // Build a chain loader: try IPFS first (if reachable), fall back to host FS.
        let ipfs_client = ipfs::HttpClient::new("http://localhost:5001".into());
        let loader = ChainLoader::new(vec![
            Box::new(IpfsUnixfsLoader::new(ipfs_client.clone())),
            Box::new(HostPathLoader),
        ]);

        // If --stem is provided, read the on-chain head and prepend it
        // as a base image layer.
        let mut all_images = Vec::new();
        let mut epoch_channel: Option<(watch::Sender<Epoch>, watch::Receiver<Epoch>)> = None;
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
                adopted_block: 0,
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

        // Resolve identity after merge so /etc/identity in image layers is visible.
        let (sk, _verifying_key, identity_source) =
            Self::resolve_identity(identity.as_deref(), merged.path())?;
        tracing::info!(source = identity_source, "Node identity resolved");

        let keypair = ww::keys::to_libp2p(&sk)?;

        // Start the libp2p swarm.
        let wetware_host = host::WetwareHost::new(port, keypair)?;
        let network_state = wetware_host.network_state();
        let swarm_cmd_tx = wetware_host.swarm_cmd_tx();
        let stream_control = wetware_host.stream_control();
        tokio::spawn(wetware_host.run());

        tracing::info!(
            layers = all_images.len(),
            root = %image_path,
            port,
            "Booting environment"
        );

        let mut builder = CellBuilder::new(image_path)
            .with_loader(Box::new(loader))
            .with_network_state(network_state)
            .with_swarm_cmd_tx(swarm_cmd_tx)
            .with_wasm_debug(wasm_debug)
            .with_image_root(merged.path().into())
            .with_ipfs_client(ipfs_client.clone())
            .with_signing_key(std::sync::Arc::new(sk));

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
        // membrane exported by the kernel.
        let result = cell.spawn_serving(stream_control).await?;
        tracing::info!(code = result.exit_code, "Guest exited");

        // Hold `merged` alive until after guest exits.
        drop(merged);
        std::process::exit(result.exit_code);
    }

    /// Publish a wetware environment to IPFS
    async fn push(
        path: PathBuf,
        ipfs_url: String,
        stem: Option<String>,
        _rpc_url: String,
        _private_key: Option<String>,
    ) -> Result<()> {
        // Verify environment is built
        let boot_wasm = path.join("boot/main.wasm");
        if !boot_wasm.exists() {
            bail!(
                "boot/main.wasm not found at: {}\n\
                 \n\
                 Please run 'ww build' first to compile your guest program.",
                boot_wasm.display()
            );
        }

        // Create a compatibility layer: the runtime expects bin/main.wasm.
        // We copy boot/main.wasm to bin/main.wasm for the published image.
        let bin_dir = path.join("bin");
        std::fs::create_dir_all(&bin_dir).context("Failed to create bin directory")?;

        let bin_wasm = bin_dir.join("main.wasm");
        std::fs::copy(&boot_wasm, &bin_wasm).context("Failed to prepare WASM artifact for IPFS")?;

        println!("Publishing to IPFS...");

        // Add the environment directory to IPFS
        let ipfs_client = ipfs::HttpClient::new(ipfs_url);
        let cid = ipfs_client
            .add_dir(&path)
            .await
            .context("Failed to publish environment to IPFS")?;

        println!("Published to IPFS!");
        println!("CID: {}", cid);
        println!("IPFS path: /ipfs/{}", cid);
        println!("\nTo run this environment:");
        println!("  ww run /ipfs/{}", cid);

        // Optionally update on-chain Atom contract
        if let Some(stem_addr) = stem {
            if _private_key.is_none() {
                bail!("--private-key is required when --stem is provided");
            }

            println!("\nUpdating on-chain Atom contract...");

            let contract = parse_contract_address(&stem_addr)?;

            // Note: Full on-chain update implementation would go here.
            // For now, we'll just acknowledge the request.
            println!("Note: On-chain contract update via CLI is not yet implemented.");
            println!(
                "The CID can be manually updated at contract: 0x{}",
                hex::encode(contract)
            );
        }

        Ok(())
    }

    /// Run daemon mode with multiple image layers
    #[allow(clippy::too_many_arguments)]
    async fn run_daemon(
        images: Vec<String>,
        port: u16,
        wasm_debug: bool,
        identity: Option<String>,
        stem: Option<String>,
        rpc_url: String,
        ws_url: String,
        confirmation_depth: u64,
    ) -> Result<()> {
        ww::config::init_tracing();

        // Build a chain loader: try IPFS first (if reachable), fall back to host FS.
        let ipfs_client = ipfs::HttpClient::new("http://localhost:5001".into());
        let loader = ChainLoader::new(vec![
            Box::new(IpfsUnixfsLoader::new(ipfs_client.clone())),
            Box::new(HostPathLoader),
        ]);

        // If --stem is provided, read the on-chain head and prepend it
        // as a base image layer.
        let mut all_images = Vec::new();
        let mut epoch_channel: Option<(watch::Sender<Epoch>, watch::Receiver<Epoch>)> = None;
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
                adopted_block: 0,
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

        // Resolve identity after merge so /etc/identity in image layers is visible.
        let (sk, _verifying_key, identity_source) =
            Self::resolve_identity(identity.as_deref(), merged.path())?;
        tracing::info!(source = identity_source, "Node identity resolved");

        let keypair = ww::keys::to_libp2p(&sk)?;

        // Start the libp2p swarm.
        let wetware_host = host::WetwareHost::new(port, keypair)?;
        let network_state = wetware_host.network_state();
        let swarm_cmd_tx = wetware_host.swarm_cmd_tx();
        let stream_control = wetware_host.stream_control();
        tokio::spawn(wetware_host.run());

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
            .with_ipfs_client(ipfs_client.clone())
            .with_signing_key(std::sync::Arc::new(sk));

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
        // membrane exported by the kernel.
        let result = cell.spawn_serving(stream_control).await?;
        tracing::info!(code = result.exit_code, "Guest exited");

        // Hold `merged` alive until after guest exits.
        drop(merged);
        std::process::exit(result.exit_code);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.command.run().await
}
