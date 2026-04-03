use anyhow::{bail, Context, Result};

use clap::{Parser, Subcommand};
use ed25519_dalek::VerifyingKey;
use membrane::Epoch;
use std::path::{Path, PathBuf};
use tokio::sync::watch;

use libp2p::Multiaddr;

mod shell;

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
    /// Initialize a new typed cell guest project.
    ///
    /// Scaffolds a complete Rust project for a Wetware guest with
    /// Cap'n Proto schema, build script, and FHS boot layout.
    Init {
        /// Project name (e.g., 'oracle', 'greeter')
        #[arg(value_name = "NAME")]
        name: String,
    },

    /// Build a guest project, placing artifacts in bin/.
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

        /// Path to an Ed25519 identity file. Sugar for PATH:/etc/identity mount.
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

        /// Number of executor worker threads for cell scheduling.
        /// Each worker runs its own single-threaded tokio runtime.
        /// 0 = auto-detect (one per CPU core).
        #[arg(long, default_value = "0")]
        executor_threads: usize,

        /// Run as an MCP server. Reads MCP JSON-RPC from stdin, routes
        /// tools/call requests to a cell WASM process, writes responses
        /// to stdout. The cell binary is specified by the MOUNT arg
        /// (must contain boot/main.wasm). MCP lifecycle methods (initialize,
        /// tools/list) are handled by the adapter, not the cell.
        #[arg(long)]
        mcp: bool,

        /// Enable the WAGI HTTP server on the given address.
        /// Example: --http-listen 127.0.0.1:8080
        #[arg(long, value_name = "ADDR")]
        http_listen: Option<String>,

        /// Runtime cache policy for `Runtime.load()`.
        /// "shared" (default): same WASM bytes → same Executor server.
        /// "isolated": always create a fresh Executor server.
        #[arg(long, default_value = "shared", env = "WW_RUNTIME_CACHE_POLICY")]
        runtime_cache_policy: String,
    },

    /// Generate a new Ed25519 identity secret.
    ///
    /// Prints a base58btc-encoded secret key to stdout.  Metadata (Peer ID)
    /// is printed to stderr so stdout stays pipeable:
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

    /// Connect to a running node and open a Glia REPL.
    ///
    /// Evaluates Glia expressions on the remote node. State persists
    /// across evals (def sticks). Ctrl-D or (exit) to disconnect.
    ///
    /// Example:
    ///   ww shell --addr /ip4/127.0.0.1/tcp/2025/p2p/12D3KooW...
    Shell {
        /// Multiaddr of the target node (must include /p2p/<peer-id>)
        #[arg(long)]
        addr: Multiaddr,

        /// Path to Ed25519 identity file for auth.
        #[arg(long, env = "WW_IDENTITY", value_name = "PATH")]
        identity: Option<PathBuf>,
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
        /// Path to an Ed25519 identity file. Defaults to ~/.ww/identity;
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
            Commands::Init { name } => Self::init(name).await,
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
                executor_threads,
                mcp,
                http_listen,
                runtime_cache_policy,
            } => {
                if mcp {
                    // TODO(mikel): Wire MCP adapter here.
                    // The infrastructure is ready:
                    //   - ProtocolAdapter trait: src/dispatcher/mod.rs
                    //   - HttpServer::run() accepts any ProtocolAdapter
                    //   - Executor: capnp/system.capnp + src/rpc/mod.rs
                    //   - SwappableReader/Writer: src/cell/swappable.rs (for Mode B)
                    //
                    // Implementation steps:
                    //   1. Implement McpAdapter: ProtocolAdapter for MCP JSON-RPC
                    //   2. Load cell WASM from boot/main.wasm
                    //   3. runtime.load(wasm) → Executor
                    //   4. HttpServer::new(executor).run(&mut adapter).await
                    eprintln!("MCP mode is not yet implemented. See src/dispatcher/mod.rs for the ProtocolAdapter trait.");
                    std::process::exit(1);
                }
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
                    executor_threads,
                    http_listen,
                    runtime_cache_policy,
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
            Commands::Shell { addr, identity } => {
                let local = tokio::task::LocalSet::new();
                local.run_until(shell::run_shell(addr, identity)).await
            }
        }
    }

    /// Initialize a new typed cell guest project.
    async fn init(name: String) -> Result<()> {
        let target_dir = PathBuf::from(&name);

        if target_dir.exists() {
            bail!("Directory already exists: {}", target_dir.display());
        }

        // Create directory structure
        std::fs::create_dir_all(target_dir.join("src"))?;
        std::fs::create_dir_all(target_dir.join("etc/init.d"))?;

        // foo.capnp — skeleton interface
        let iface_name = to_pascal_case(&name);
        let file_id: u64 = {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut h = DefaultHasher::new();
            name.hash(&mut h);
            h.finish() | (1u64 << 63)
        };
        let capnp_content = format!(
            r#"# {name} capability interface.

@0x{file_id:016x};

interface {iface_name} {{
  hello @0 (name :Text) -> (greeting :Text);
  # Replace with your methods.
}}
"#,
        );
        std::fs::write(target_dir.join(format!("{name}.capnp")), capnp_content)?;

        // Cargo.toml
        let cargo_toml = format!(
            r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[workspace]  # standalone — not part of the host workspace

[dependencies]
capnp     = "0.23.2"
capnp-rpc = "0.23.0"
log       = "0.4"
wasip2    = "1.0.2"
system    = {{ path = "../../std/system" }}

[lib]
crate-type = ["cdylib"]

[build-dependencies]
capnpc    = "0.23.3"
capnp     = "0.23.2"
schema-id = {{ path = "../../crates/schema-id" }}
"#
        );
        std::fs::write(target_dir.join("Cargo.toml"), cargo_toml)?;

        // build.rs — compiles schema, extracts CID
        let build_rs = format!(
            r#"use std::env;
use std::path::{{Path, PathBuf}};

fn main() {{
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let manifest_path = Path::new(&manifest_dir);
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());

    let capnp_dir = manifest_path
        .join("../..")
        .join("capnp")
        .canonicalize()
        .expect("capnp dir not found");

    let local_schema = manifest_path
        .join("{name}.capnp")
        .canonicalize()
        .expect("{name}.capnp not found next to Cargo.toml");

    // Pass 1: shared schemas
    capnpc::CompilerCommand::new()
        .src_prefix(&capnp_dir)
        .file(capnp_dir.join("system.capnp"))
        .file(capnp_dir.join("ipfs.capnp"))
        .file(capnp_dir.join("routing.capnp"))
        .file(capnp_dir.join("stem.capnp"))
        .file(capnp_dir.join("http.capnp"))
        .run()
        .expect("failed to compile shared capnp schemas");

    // Pass 2: local schema + schema CID
    let raw_request = out_dir.join("{name}_request.bin");
    capnpc::CompilerCommand::new()
        .src_prefix(manifest_path)
        .file(&local_schema)
        .raw_code_generator_request_path(&raw_request)
        .run()
        .expect("failed to compile {name}.capnp");

    let iface_id = find_interface_id(&raw_request, "{iface_name}")
        .expect("{iface_name} interface not found in CodeGeneratorRequest");

    let schemas = schema_id::extract_schemas(
        &raw_request,
        &[("{const_name}", iface_id)],
    )
    .expect("extract schema");

    schema_id::emit_schema_consts(&out_dir.join("schema_ids.rs"), &schemas)
        .expect("emit schema consts");

    schema_id::write_schema_bytes(&out_dir.join("{name}_schema.bin"), &schemas[0])
        .expect("write schema bytes");

    for schema in &["system", "ipfs", "routing", "stem", "http"] {{
        println!(
            "cargo:rerun-if-changed={{}}",
            capnp_dir.join(format!("{{schema}}.capnp")).display()
        );
    }}
    println!("cargo:rerun-if-changed={{}}", local_schema.display());
}}

fn find_interface_id(raw_request_path: &Path, name: &str) -> Option<u64> {{
    let data = std::fs::read(raw_request_path).ok()?;
    let reader =
        capnp::serialize::read_message(&mut data.as_slice(), capnp::message::ReaderOptions::new())
            .ok()?;
    let request: capnp::schema_capnp::code_generator_request::Reader = reader.get_root().ok()?;
    for node in request.get_nodes().ok()?.iter() {{
        if let Ok(n) = node.get_display_name() {{
            if n.to_str().ok()?.ends_with(&format!(":{{}}", name)) || n.to_str().ok()? == name {{
                if matches!(
                    node.which(),
                    Ok(capnp::schema_capnp::node::Which::Interface(_))
                ) {{
                    return Some(node.get_id());
                }}
            }}
        }}
    }}
    None
}}
"#,
            const_name = name.to_uppercase().replace('-', "_"),
        );
        std::fs::write(target_dir.join("build.rs"), build_rs)?;

        // src/lib.rs — guest entry point
        let lib_rs = format!(
            r#"use std::rc::Rc;

use capnp::capability::Promise;
use wasip2::exports::cli::run::Guest;

#[allow(dead_code)]
mod system_capnp {{
    include!(concat!(env!("OUT_DIR"), "/system_capnp.rs"));
}}

#[allow(dead_code)]
mod stem_capnp {{
    include!(concat!(env!("OUT_DIR"), "/stem_capnp.rs"));
}}

#[allow(dead_code)]
mod ipfs_capnp {{
    include!(concat!(env!("OUT_DIR"), "/ipfs_capnp.rs"));
}}

#[allow(dead_code)]
mod routing_capnp {{
    include!(concat!(env!("OUT_DIR"), "/routing_capnp.rs"));
}}

#[allow(dead_code)]
mod http_capnp {{
    include!(concat!(env!("OUT_DIR"), "/http_capnp.rs"));
}}

#[allow(dead_code)]
mod {name}_capnp {{
    include!(concat!(env!("OUT_DIR"), "/{name}_capnp.rs"));
}}

include!(concat!(env!("OUT_DIR"), "/schema_ids.rs"));

type Membrane = stem_capnp::membrane::Client;

// ---------------------------------------------------------------------------
// {iface_name} implementation
// ---------------------------------------------------------------------------

struct {iface_name}Impl;

#[allow(refining_impl_trait)]
impl {name}_capnp::{snake_name}::Server for {iface_name}Impl {{
    fn hello(
        self: Rc<Self>,
        params: {name}_capnp::{snake_name}::HelloParams,
        mut results: {name}_capnp::{snake_name}::HelloResults,
    ) -> Promise<(), capnp::Error> {{
        let name = capnp_rpc::pry!(capnp_rpc::pry!(params.get()).get_name())
            .to_str()
            .unwrap_or("world");
        results
            .get()
            .set_greeting(&format!("Hello, {{name}}!"));
        Promise::ok(())
    }}
}}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

struct {iface_name}Guest;

impl Guest for {iface_name}Guest {{
    fn run() -> Result<(), ()> {{
        match std::env::args().nth(1).as_deref() {{
            Some("serve") => {{
                log::info!("{name}: serve");
                system::run(|membrane: Membrane| async move {{
                    let graft_resp = membrane.graft_request().send().promise.await?;
                    let results = graft_resp.get()?;
                    let host = results.get_host()?;

                    let id_resp = host.id_request().send().promise.await?;
                    let peer_id = id_resp.get()?.get_peer_id()?;
                    log::info!("{name}: peer {{:?}}", peer_id);

                    // TODO: provide on DHT, discover peers, etc.

                    Ok(())
                }});
            }}
            _ => {{
                // Default (no args): cell mode — spawned by VatListener.
                let impl_ = {iface_name}Impl;
                let client: {name}_capnp::{snake_name}::Client = capnp_rpc::new_client(impl_);
                log::info!("{name}: cell mode");
                system::serve(client.client, |_membrane: Membrane| async move {{
                    std::future::pending().await
                }});
            }}
        }}
        Ok(())
    }}
}}

wasip2::cli::command::export!({iface_name}Guest);
"#,
            snake_name = name.replace('-', "_"),
        );
        std::fs::write(target_dir.join("src/lib.rs"), lib_rs)?;

        // etc/init.d/<name>.glia — skeleton init script
        let glia = format!(
            r#"; {name} init.d script — evaluated by the kernel at boot.
;
; Registers the RPC cell. VatListener spawns a cell per connection;
; each cell exports its capability via system::serve().
;
; To run the service from the shell:
;   (executor run (load "bin/{name}.wasm") "serve")

(def {snake_name}-wasm (load "bin/{name}.wasm"))
(def {snake_name}-schema (load "bin/{name}.schema"))

(perform host :listen executor {snake_name}-wasm {snake_name}-schema)
"#,
            snake_name = name.replace('-', "_"),
        );
        std::fs::write(target_dir.join(format!("etc/init.d/{name}.glia")), glia)?;

        println!("Initialized cell project: {name}/");
        println!("  {name}.capnp            — capability interface (edit this)");
        println!("  Cargo.toml              — project configuration");
        println!("  build.rs                — schema compilation");
        println!("  src/lib.rs              — guest entry point");
        println!("  etc/init.d/{name}.glia  — kernel init script");
        println!();
        println!("Next steps:");
        println!("  1. Edit {name}.capnp with your interface methods");
        println!("  2. Implement the server in src/lib.rs");
        println!("  3. ww build {name}");
        println!("  4. ww run crates/kernel {name}");
        Ok(())
    }

    /// Build a guest project, placing artifacts in bin/
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

        // Copy WASM to bin/<name>.wasm
        let bin_dir = path.join("bin");
        std::fs::create_dir_all(&bin_dir).context("Failed to create bin directory")?;

        let crate_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("main");
        let dst_wasm = bin_dir.join(format!("{crate_name}.wasm"));
        std::fs::copy(&src_wasm, &dst_wasm).context(format!(
            "Failed to copy {} to {}",
            src_wasm.display(),
            dst_wasm.display()
        ))?;

        println!("  bin/{crate_name}.wasm");

        // Copy schema bytes next to the WASM if the build produced them.
        let build_dir = path.join("target/wasm32-wasip2/release/build");
        if let Ok(schema_bin) = Self::find_schema_bin(&build_dir, &path) {
            let dst_schema = bin_dir.join(format!("{crate_name}.schema"));
            std::fs::copy(&schema_bin, &dst_schema).context(format!(
                "Failed to copy {} to {}",
                schema_bin.display(),
                dst_schema.display(),
            ))?;
            println!("  bin/{crate_name}.schema");
        }

        println!("Build complete: {}", path.display());
        Ok(())
    }

    /// Find the `*_schema.bin` file in the cargo build output.
    ///
    /// The project's build.rs writes `{name}_schema.bin` to OUT_DIR during
    /// compilation. We search the build directory for it, matching against
    /// the project name from Cargo.toml.
    fn find_schema_bin(build_dir: &Path, project_dir: &Path) -> Result<PathBuf> {
        // Get the crate name from the project directory name.
        let project_name = project_dir
            .file_name()
            .and_then(|n| n.to_str())
            .context("Invalid project directory name")?
            .replace('-', "_");

        let pattern = format!("{project_name}_schema.bin");

        // Walk the build dir looking for the schema.bin file.
        // It's at: target/wasm32-wasip2/release/build/{crate}-{hash}/out/{name}_schema.bin
        if build_dir.exists() {
            for entry in std::fs::read_dir(build_dir)? {
                let entry = entry?;
                let out_dir = entry.path().join("out");
                let candidate = out_dir.join(&pattern);
                if candidate.exists() {
                    return Ok(candidate);
                }
            }
        }

        bail!(
            "Schema binary not found: {pattern}\n\
             \n\
             The project's build.rs should produce this file.\n\
             Make sure the build.rs calls schema_id::write_schema_bytes()."
        )
    }

    /// Resolve the node's Ed25519 signing key from `/etc/identity` in the FHS.
    ///
    /// If `/etc/identity` exists (placed by an image layer or a targeted mount
    /// like `~/.ww/identity:/etc/identity`), the key is loaded from it.
    /// Otherwise an ephemeral key is generated and injected so the guest
    /// can still read `/etc/identity` via WASI.
    ///
    /// Returns `(signing_key, verifying_key, source_description)`.
    fn resolve_identity(
        merged_root: &std::path::Path,
    ) -> Result<(ed25519_dalek::SigningKey, VerifyingKey, &'static str)> {
        let identity_path = merged_root.join("etc/identity");
        if identity_path.exists() {
            let path_str = identity_path
                .to_str()
                .context("/etc/identity path is non-UTF-8")?;
            let sk = ww::keys::load(path_str)?;
            let vk = sk.verifying_key();
            Ok((sk, vk, "/etc/identity"))
        } else {
            tracing::warn!("No /etc/identity found; using ephemeral key (will be lost on exit)");
            let sk = ww::keys::generate()?;
            let vk = sk.verifying_key();
            Ok((sk, vk, "ephemeral"))
        }
    }

    /// Generate a new Ed25519 identity secret.
    async fn keygen(output: Option<PathBuf>) -> Result<()> {
        let sk = ww::keys::generate()?;
        let encoded = ww::keys::encode(&sk);

        let kp = ww::keys::to_libp2p(&sk)?;
        let peer_id = kp.public().to_peer_id();

        if let Some(path) = output {
            ww::keys::save(&sk, &path)?;
            eprintln!("Secret written to: {}", path.display());
        } else {
            println!("{encoded}");
        }

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
            eprintln!("Generated new identity: {}", key_path.display());
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
        executor_threads: usize,
        http_listen: Option<String>,
        runtime_cache_policy: String,
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
        let content_store: std::sync::Arc<dyn ipfs::ContentStore> =
            std::sync::Arc::new(ipfs_client.clone());
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

        // Publish the merged image to IPFS so guests can resolve content via
        // the UnixFS capability.  $WW_ROOT is set to /ipfs/<cid>.
        let root_cid = ipfs_client.add_dir(merged.path()).await?;
        let image_path = format!("/ipfs/{}", root_cid);

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

        // Fetch a random sample of Kubo's connected peers to seed the Kad
        // routing table.  Capped at K_VALUE (20) — the Kademlia replication
        // factor and the minimum for a single query to converge.  The
        // automatic bootstrap walk (triggered when entries < K) will
        // discover additional peers if needed.
        let kubo_peers: Vec<(libp2p::PeerId, Multiaddr)> = match ipfs_client.swarm_peers().await {
            Ok(raw) => {
                use rand::seq::SliceRandom;
                let mut parsed: Vec<_> = raw
                    .into_iter()
                    .filter_map(|(peer_str, addr_str)| {
                        let peer_id: libp2p::PeerId = peer_str.parse().ok()?;
                        let addr: Multiaddr = addr_str.parse().ok()?;
                        Some((peer_id, addr))
                    })
                    .collect();
                const MAX_KUBO_PEERS: usize = 20; // K_VALUE
                if parsed.len() > MAX_KUBO_PEERS {
                    let mut rng = rand::rng();
                    parsed.shuffle(&mut rng);
                    parsed.truncate(MAX_KUBO_PEERS);
                }
                parsed
            }
            Err(e) => {
                tracing::warn!("Could not fetch Kubo swarm peers: {e}");
                Vec::new()
            }
        };

        // ---- Thread-per-subsystem runtime (Pingora-inspired) ----
        //
        // Each subsystem gets its own OS thread + single-threaded tokio
        // runtime.  The Host supervisor coordinates shutdown.
        let (swarm_cmd_tx, swarm_cmd_rx) = tokio::sync::mpsc::channel(64);
        let (swarm_ready_tx, swarm_ready_rx) = tokio::sync::oneshot::channel();

        let mut supervisor = ww::runtime::Host::new();

        // Swarm thread: libp2p event loop.
        // The Libp2pHost is constructed inside the swarm thread so that
        // TCP listeners register with the correct tokio reactor.
        supervisor.spawn(
            "swarm",
            ww::runtime::SwarmService {
                params: ww::runtime::SwarmServiceParams {
                    port,
                    keypair,
                    kubo_bootstrap,
                    kubo_peers,
                },
                cmd_rx: swarm_cmd_rx,
                ready_tx: swarm_ready_tx,
            },
        );

        // Wait for the swarm thread to construct the host and send back
        // the stream control + network state.
        let swarm_ready = swarm_ready_rx
            .await
            .context("Swarm service failed to start")?;
        let network_state = swarm_ready.network_state;
        let stream_control = swarm_ready.stream_control;

        // Epoch thread: on-chain watcher (only when --stem is provided).
        let epoch_channel_rx = if let Some((epoch_tx, epoch_rx)) = epoch_channel {
            if let Some(config) = stem_config {
                supervisor.spawn(
                    "epoch",
                    ww::runtime::EpochService {
                        config,
                        epoch_tx,
                        confirmation_depth,
                        ipfs_client,
                        cid_tree: None, // TODO: pass CidTree when virtual FS is wired
                    },
                );
            }
            Some(epoch_rx)
        } else {
            None
        };

        // Executor pool: M:N cell scheduling across N worker threads.
        let executor_pool =
            ww::runtime::ExecutorPool::new(executor_threads, supervisor.shutdown_rx());

        // WAGI HTTP server thread (only when --http-listen is provided).
        let route_registry = if let Some(ref addr) = http_listen {
            let listen_addr: std::net::SocketAddr = addr
                .parse()
                .context("invalid --http-listen address (expected host:port)")?;
            let registry = ww::dispatcher::server::new_registry();
            supervisor.spawn(
                "wagi-http",
                ww::runtime::WagiService {
                    listen_addr,
                    registry: registry.clone(),
                },
            );
            Some(registry)
        } else {
            None
        };

        tracing::info!(
            mounts = all_mounts.len(),
            root = %image_path,
            port,
            http = http_listen.as_deref().unwrap_or("disabled"),
            "Booting environment"
        );

        let cache_policy = match runtime_cache_policy.as_str() {
            "shared" => ww::rpc::CachePolicy::Shared,
            "isolated" => ww::rpc::CachePolicy::Isolated,
            other => anyhow::bail!(
                "invalid --runtime-cache-policy '{}' (expected 'shared' or 'isolated')",
                other
            ),
        };

        let mut builder = CellBuilder::new(image_path)
            .with_loader(Box::new(loader))
            .with_network_state(network_state)
            .with_swarm_cmd_tx(swarm_cmd_tx)
            .with_wasm_debug(wasm_debug)
            .with_image_root(merged.path().into())
            .with_content_store(content_store.clone())
            .with_signing_key(std::sync::Arc::new(sk))
            .with_cache_policy(cache_policy)
            .with_wasmtime_engine(executor_pool.engine());

        if let Some(registry) = route_registry {
            builder = builder.with_route_registry(registry);
        }

        if let Some(epoch_rx) = epoch_channel_rx {
            builder = builder.with_epoch_rx(epoch_rx);
        }

        let cell = builder.build();

        // Spawn the kernel cell into the executor pool. The kernel's exit
        // code flows back through the oneshot channel.
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();
        executor_pool
            .spawn(ww::runtime::SpawnRequest {
                name: "kernel".into(),
                factory: Box::new(move |_shutdown| {
                    Box::pin(async move {
                        match cell.spawn_serving(stream_control).await {
                            Ok(result) => {
                                let _ = result_tx.send(Ok(result.exit_code));
                            }
                            Err(e) => {
                                tracing::error!("kernel failed: {}", e);
                                let _ = result_tx.send(Err(e));
                            }
                        }
                    })
                }),
                // Exit code is sent explicitly by the factory above.
                result_tx: None,
            })
            .map_err(|_| anyhow::anyhow!("executor pool rejected kernel spawn"))?;

        // Wait for the kernel to exit.
        let exit_code = match result_rx.await {
            Ok(Ok(code)) => code,
            Ok(Err(e)) => {
                tracing::error!("Kernel error: {}", e);
                1
            }
            Err(_) => {
                tracing::error!("Kernel result channel dropped");
                1
            }
        };
        tracing::info!(code = exit_code, "Kernel exited");

        supervisor.shutdown();

        // Hold `merged` alive until after guest exits.
        // ExecutorPool must also be dropped after the kernel exits but
        // before process exit, to join worker threads cleanly.
        drop(executor_pool);
        drop(merged);
        std::process::exit(exit_code);
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

/// Convert a kebab-case or snake_case name to PascalCase.
/// "price-oracle" → "PriceOracle", "foo_bar" → "FooBar", "foo" → "Foo"
fn to_pascal_case(s: &str) -> String {
    s.split(['-', '_'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_identity_ephemeral_no_disk_write() {
        let dir = tempfile::TempDir::new().unwrap();
        // No etc/identity in the directory.
        let (_, _, source) = Commands::resolve_identity(dir.path()).unwrap();
        assert_eq!(source, "ephemeral");
        // Must NOT write back to disk.
        assert!(!dir.path().join("etc/identity").exists());
    }

    #[test]
    fn test_resolve_identity_loads_existing() {
        let dir = tempfile::TempDir::new().unwrap();
        let etc = dir.path().join("etc");
        std::fs::create_dir_all(&etc).unwrap();
        // Write a known key.
        let sk = ww::keys::generate().unwrap();
        let encoded = ww::keys::encode(&sk);
        std::fs::write(etc.join("identity"), &encoded).unwrap();

        let (loaded_sk, _, source) = Commands::resolve_identity(dir.path()).unwrap();
        assert_eq!(source, "/etc/identity");
        assert_eq!(ww::keys::encode(&loaded_sk), encoded);
    }
}
