use anyhow::{bail, Context, Result};

use clap::{Parser, Subcommand};
use k256::ecdsa::VerifyingKey;
use membrane::Epoch;
use std::path::{Path, PathBuf};
use tokio::sync::watch;

use libp2p::Multiaddr;

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
    /// Every positional argument is a mount: `source[:target]`.
    /// Without `:target`, the source is mounted at `/` (image layer).
    /// With `:target`, the source is overlaid at that guest path.
    ///
    /// Examples:
    ///   ww run .                                    # dev mode
    ///   ww run images/app ~/.ww/identity:/etc/identity
    ///   ww run /ipfs/QmHash ~/data:/var/data
    Run {
        /// Mount(s): `source` (image at /) or `source:/guest/path` (targeted).
        #[arg(default_value = ".", value_name = "MOUNT")]
        mounts: Vec<String>,

        /// libp2p swarm port
        #[arg(long, default_value = "2025")]
        port: u16,

        /// Enable WASM debug info for guest processes
        #[arg(long)]
        wasm_debug: bool,

        /// Path to a secp256k1 identity file. Sugar for PATH:/etc/identity mount.
        /// Works well with direnv: `export WW_IDENTITY=~/.ww/identity` in .envrc.
        #[arg(long, env = "WW_IDENTITY", value_name = "PATH")]
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
    ///     ww keygen > ~/.ww/identity
    ///     ww keygen --output ~/.ww/identity   # equivalent
    Keygen {
        /// Write the secret to a file instead of stdout.
        #[arg(long, value_name = "PATH")]
        output: Option<PathBuf>,
    },

    /// Manage the wetware background daemon.
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
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

#[derive(Subcommand)]
enum DaemonAction {
    /// Register wetware as a user-level background service.
    ///
    /// Generates a key if missing, writes the daemon config, creates
    /// a platform service file (launchd on macOS, systemd on Linux),
    /// and prints the activation command.
    Install {
        /// Path to a secp256k1 identity file. Defaults to ~/.ww/identity;
        /// generated automatically if the file does not exist.
        #[arg(long, value_name = "PATH")]
        identity: Option<PathBuf>,

        /// libp2p swarm port.
        #[arg(long)]
        port: Option<u16>,

        /// Image layers to run (local paths or IPFS CIDs).
        #[arg(long, value_name = "PATH")]
        images: Vec<String>,
    },

    /// Remove the platform service file.
    ///
    /// Removes the launchd plist (macOS) or systemd unit (Linux) and
    /// prints the deactivation command. Does not touch ~/.ww/identity or
    /// ~/.ww/config.glia.
    Uninstall,
}

/// Strip the `/p2p/<peer-id>` suffix from a multiaddr string, if present.
fn strip_p2p_suffix(addr: &str) -> &str {
    if let Some(idx) = addr.find("/p2p/") {
        &addr[..idx]
    } else {
        addr
    }
}

/// Parse Kubo bootstrap info into a `KuboBootstrapInfo` suitable for seeding
/// the in-process Kademlia client.
///
/// Prefers a loopback TCP address (the typical case when Kubo runs locally).
/// Falls back to any parseable TCP multiaddr.  Returns `None` if no suitable
/// address can be found.
fn parse_kubo_bootstrap(info: &ipfs::KuboInfo) -> Option<host::KuboBootstrapInfo> {
    let peer_id: libp2p::PeerId = info.peer_id.parse().ok()?;

    // Try loopback TCP first (Kubo on same machine), then any TCP addr.
    let addr = info
        .swarm_addrs
        .iter()
        .filter_map(|s| strip_p2p_suffix(s).parse::<Multiaddr>().ok())
        .find(|a| {
            let s = a.to_string();
            s.contains("/ip4/127.0.0.1/tcp/") || s.contains("/ip4/127.0.0.1/udp/")
        })
        .or_else(|| {
            info.swarm_addrs
                .iter()
                .filter_map(|s| strip_p2p_suffix(s).parse::<Multiaddr>().ok())
                .find(|a| a.to_string().contains("/tcp/"))
        })?;

    tracing::info!(
        kubo_peer = %peer_id,
        %addr,
        "Bootstrapping Kad client against Kubo (Amino DHT)"
    );

    Some(host::KuboBootstrapInfo { peer_id, addr })
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
                mounts: mount_args,
                port,
                wasm_debug,
                identity,
                stem,
                rpc_url,
                ws_url,
                confirmation_depth,
            } => {
                let mut mounts = ww::mount::parse_args(&mount_args)?;
                // --identity PATH is sugar for PATH:/etc/identity mount.
                // Appended last so it overrides any /etc/identity from image layers.
                if let Some(id_path) = identity {
                    mounts.push(ww::mount::Mount {
                        source: id_path,
                        target: PathBuf::from("/etc/identity"),
                    });
                }
                Self::run_with_mounts(
                    mounts,
                    port,
                    wasm_debug,
                    stem,
                    rpc_url,
                    ws_url,
                    confirmation_depth,
                )
                .await
            }
            Commands::Daemon { action } => match action {
                DaemonAction::Install {
                    identity,
                    port,
                    images,
                } => Self::daemon_install(identity, port, images).await,
                DaemonAction::Uninstall => Self::daemon_uninstall().await,
            },
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
system = { path = "../../std/system" }
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
/// The `system::run()` function bootstraps the RPC connection to the host,
/// then executes the provided async closure.
///
/// Example with host communication:
/// ```no_run
/// system::run::<capnp::capability::Client, _, _>(|host| async move {
///     let executor = host.executor_request().send().pipeline.get_executor();
///     executor.echo_request().send().promise.await?;
///     Ok(())
/// });
/// ```

#[no_mangle]
pub extern "C" fn _start() {
    system::run::<capnp::capability::Client, _, _>(|_host| async move {
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

    /// Resolve the node's secp256k1 signing key from `/etc/identity` in the FHS.
    ///
    /// If `/etc/identity` exists (placed by an image layer or a targeted mount
    /// like `~/.ww/identity:/etc/identity`), the key is loaded from it.
    /// Otherwise an ephemeral key is generated and injected so the guest
    /// can still read `/etc/identity` via WASI.
    ///
    /// Returns `(signing_key, verifying_key, source_description)`.
    fn resolve_identity(
        merged_root: &std::path::Path,
    ) -> Result<(k256::ecdsa::SigningKey, VerifyingKey, &'static str)> {
        let identity_path = merged_root.join("etc/identity");
        if identity_path.exists() {
            let path_str = identity_path
                .to_str()
                .context("/etc/identity path is non-UTF-8")?;
            let sk = ww::keys::load(path_str)?;
            let vk = *sk.verifying_key();
            Ok((sk, vk, "/etc/identity"))
        } else {
            tracing::warn!("No /etc/identity found; using ephemeral key (will be lost on exit)");
            let sk = ww::keys::generate()?;
            // Inject so the guest can read /etc/identity via WASI.
            let etc = merged_root.join("etc");
            std::fs::create_dir_all(&etc).context("create /etc in merged FHS")?;
            std::fs::write(etc.join("identity"), ww::keys::encode(&sk))
                .context("write /etc/identity to merged FHS")?;
            let vk = *sk.verifying_key();
            Ok((sk, vk, "ephemeral"))
        }
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

    /// Register wetware as a user-level background service.
    async fn daemon_install(
        identity: Option<PathBuf>,
        port: Option<u16>,
        images: Vec<String>,
    ) -> Result<()> {
        let home = dirs::home_dir().context("cannot determine home directory")?;
        let ww_dir = home.join(".ww");

        // 1. Resolve identity path — default to ~/.ww/identity.
        let key_path = identity.unwrap_or_else(|| ww_dir.join("identity"));

        // Generate key if it doesn't exist.
        if !key_path.exists() {
            let sk = ww::keys::generate()?;
            ww::keys::save(&sk, &key_path)?;

            let kp = ww::keys::to_libp2p(&sk)?;
            let addr = ww::keys::ethereum_address(&sk);
            eprintln!("Generated new identity: {}", key_path.display());
            eprintln!("  EVM address: 0x{}", hex::encode(addr));
            eprintln!("  Peer ID:     {}", kp.public().to_peer_id());
        } else {
            eprintln!("Using existing identity: {}", key_path.display());
        }

        // 2. Build config: load existing, override with CLI flags.
        let config_path = ww::daemon_config::default_config_path();
        let mut config = ww::daemon_config::load(&config_path)?;

        if let Some(p) = port {
            config.port = p;
        }
        config.identity = Some(key_path.clone());
        if !images.is_empty() {
            config.images = images.iter().map(PathBuf::from).collect();
        }

        // 3. Write config.
        config.write(&config_path)?;
        eprintln!("Wrote config: {}", config_path.display());

        // 4. Write platform service file.
        let ww_bin = std::env::current_exe().context("cannot determine ww binary path")?;
        Self::write_service_file(&ww_bin, &config, &home)?;

        Ok(())
    }

    /// Remove the platform service file.
    async fn daemon_uninstall() -> Result<()> {
        let home = dirs::home_dir().context("cannot determine home directory")?;

        if cfg!(target_os = "macos") {
            let plist_path = home.join("Library/LaunchAgents/io.wetware.ww.plist");
            if plist_path.exists() {
                std::fs::remove_file(&plist_path)
                    .with_context(|| format!("remove {}", plist_path.display()))?;
                eprintln!("Removed: {}", plist_path.display());
                eprintln!();
                eprintln!("If the service is running, stop it with:");
                eprintln!("  launchctl unload {}", plist_path.display());
            } else {
                eprintln!("No service file found at: {}", plist_path.display());
            }
        } else if cfg!(target_os = "linux") {
            let unit_path = home.join(".config/systemd/user/ww.service");
            if unit_path.exists() {
                std::fs::remove_file(&unit_path)
                    .with_context(|| format!("remove {}", unit_path.display()))?;
                eprintln!("Removed: {}", unit_path.display());
                eprintln!();
                eprintln!("If the service is running, stop it with:");
                eprintln!("  systemctl --user disable --now ww");
            } else {
                eprintln!("No service file found at: {}", unit_path.display());
            }
        } else {
            bail!("unsupported platform; only macOS and Linux are supported");
        }

        Ok(())
    }

    /// Write a platform-specific service file and print the activation command.
    fn write_service_file(
        ww_bin: &Path,
        config: &ww::daemon_config::DaemonConfig,
        home: &Path,
    ) -> Result<()> {
        // Identity as a mount arg: path:/etc/identity
        let identity_mount = config
            .identity
            .as_ref()
            .map(|p| format!("{}:/etc/identity", p.display()))
            .unwrap_or_else(|| format!("{}:/etc/identity", home.join(".ww/identity").display()));

        if cfg!(target_os = "macos") {
            Self::write_launchd_plist(ww_bin, config, home, &identity_mount)
        } else if cfg!(target_os = "linux") {
            Self::write_systemd_unit(ww_bin, config, home, &identity_mount)
        } else {
            bail!("unsupported platform; only macOS and Linux are supported")
        }
    }

    /// Write a macOS launchd plist.
    fn write_launchd_plist(
        ww_bin: &Path,
        config: &ww::daemon_config::DaemonConfig,
        home: &Path,
        identity_mount: &str,
    ) -> Result<()> {
        let plist_dir = home.join("Library/LaunchAgents");
        std::fs::create_dir_all(&plist_dir).context("create ~/Library/LaunchAgents")?;

        let plist_path = plist_dir.join("io.wetware.ww.plist");

        let log_dir = home.join(".ww/logs");
        std::fs::create_dir_all(&log_dir).context("create ~/.ww/logs")?;
        let log_path = log_dir.join("ww.log");

        // Build ProgramArguments array entries.
        let mut args = vec![
            format!("        <string>{}</string>", ww_bin.display()),
            "        <string>run</string>".to_string(),
            format!("        <string>--port</string>"),
            format!("        <string>{}</string>", config.port),
        ];
        // Image layers (root mounts).
        for img in &config.images {
            args.push(format!("        <string>{}</string>", img.display()));
        }
        // Identity as a targeted mount.
        args.push(format!("        <string>{identity_mount}</string>"));

        let plist = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>io.wetware.ww</string>
    <key>ProgramArguments</key>
    <array>
{args}
    </array>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <false/>
</dict>
</plist>
"#,
            args = args.join("\n"),
            log = log_path.display(),
        );

        std::fs::write(&plist_path, plist)
            .with_context(|| format!("write plist: {}", plist_path.display()))?;
        eprintln!("Wrote service: {}", plist_path.display());
        eprintln!();
        eprintln!("Activate with:");
        eprintln!("  launchctl load {}", plist_path.display());

        Ok(())
    }

    /// Write a Linux systemd user unit.
    fn write_systemd_unit(
        ww_bin: &Path,
        config: &ww::daemon_config::DaemonConfig,
        home: &Path,
        identity_mount: &str,
    ) -> Result<()> {
        let unit_dir = home.join(".config/systemd/user");
        std::fs::create_dir_all(&unit_dir).context("create ~/.config/systemd/user")?;

        let unit_path = unit_dir.join("ww.service");

        // Build positional args: images (root mounts) + identity mount.
        let mut positional = Vec::new();
        for img in &config.images {
            positional.push(img.display().to_string());
        }
        positional.push(identity_mount.to_string());

        let exec_start = format!(
            "{} run --port {} {}",
            ww_bin.display(),
            config.port,
            positional.join(" "),
        );

        let unit = format!(
            "[Unit]\n\
             Description=Wetware daemon\n\
             After=network-online.target\n\
             Wants=network-online.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart={exec_start}\n\
             Restart=on-failure\n\
             RestartSec=5\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n"
        );

        std::fs::write(&unit_path, unit)
            .with_context(|| format!("write unit: {}", unit_path.display()))?;
        eprintln!("Wrote service: {}", unit_path.display());
        eprintln!();
        eprintln!("Activate with:");
        eprintln!("  systemctl --user enable --now ww");

        Ok(())
    }

    /// Run a wetware environment from parsed mounts.
    #[allow(clippy::too_many_arguments)]
    async fn run_with_mounts(
        mounts: Vec<ww::mount::Mount>,
        port: u16,
        wasm_debug: bool,
        stem: Option<String>,
        rpc_url: String,
        ws_url: String,
        confirmation_depth: u64,
    ) -> Result<()> {
        // Dev-mode compat: if a single local root mount has boot/main.wasm
        // but not bin/main.wasm, copy it over (the runtime expects bin/).
        for mount in &mounts {
            if mount.is_root() && !ipfs::is_ipfs_path(&mount.source) {
                let src = Path::new(&mount.source);
                let boot_wasm = src.join("boot/main.wasm");
                let bin_wasm = src.join("bin/main.wasm");
                if boot_wasm.exists() && !bin_wasm.exists() {
                    let bin_dir = src.join("bin");
                    std::fs::create_dir_all(&bin_dir).context("Failed to create bin directory")?;
                    std::fs::copy(&boot_wasm, &bin_wasm)
                        .context("Failed to prepare WASM artifact for runtime")?;
                }
            }
        }

        ww::config::init_tracing();

        // Build a chain loader: try IPFS first (if reachable), fall back to host FS.
        let ipfs_client = ipfs::HttpClient::new("http://localhost:5001".into());
        let loader = ChainLoader::new(vec![
            Box::new(IpfsUnixfsLoader::new(ipfs_client.clone())),
            Box::new(HostPathLoader),
        ]);

        // If --stem is provided, read the on-chain head and prepend it
        // as a base root mount.
        let mut all_mounts: Vec<ww::mount::Mount> = Vec::new();
        let mut epoch_channel: Option<(watch::Sender<Epoch>, watch::Receiver<Epoch>)> = None;
        let stem_config = if let Some(ref stem_addr) = stem {
            let contract = parse_contract_address(stem_addr)?;
            let head = image::read_contract_head(&rpc_url, &contract).await?;
            let ipfs_path = image::cid_bytes_to_ipfs_path(&head.cid)?;

            tracing::info!(
                seq = head.seq,
                path = %ipfs_path,
                "Read on-chain HEAD; prepending as base root mount"
            );

            // Pin the initial head.
            if let Err(e) = ipfs_client.pin_add(&ipfs_path).await {
                tracing::warn!(path = %ipfs_path, "Failed to pin initial head: {e}");
            }

            all_mounts.push(ww::mount::Mount {
                source: ipfs_path,
                target: PathBuf::from("/"),
            });

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

        // Append user-specified mounts after the on-chain base.
        all_mounts.extend(mounts);

        // Apply all mounts into a single FHS root.
        let merged = image::apply_mounts(&all_mounts, &ipfs_client).await?;
        let image_path = merged.path().to_string_lossy().to_string();

        // Resolve identity from /etc/identity in the merged FHS.
        let (sk, _verifying_key, identity_source) = Self::resolve_identity(merged.path())?;
        tracing::info!(source = identity_source, "Node identity resolved");

        let keypair = ww::keys::to_libp2p(&sk)?;

        // Attempt to fetch Kubo's identity so we can bootstrap the in-process
        // Kad client against the local node (Amino DHT /ipfs/kad/1.0.0).
        // Non-fatal: if Kubo is unreachable we still start, just without Kad.
        let kubo_bootstrap = match ipfs_client.kubo_info().await {
            Ok(info) => parse_kubo_bootstrap(&info),
            Err(e) => {
                tracing::warn!("Could not fetch Kubo identity (Kad DHT will not bootstrap): {e}");
                None
            }
        };

        // Start the libp2p swarm.
        let wetware_host = host::WetwareHost::new(port, keypair, kubo_bootstrap)?;
        let network_state = wetware_host.network_state();
        let swarm_cmd_tx = wetware_host.swarm_cmd_tx();
        let stream_control = wetware_host.stream_control();
        tokio::spawn(wetware_host.run());

        tracing::info!(
            mounts = all_mounts.len(),
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
        tracing::info!(code = result.exit_code, "Kernel exited");

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
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    cli.command.run().await
}
