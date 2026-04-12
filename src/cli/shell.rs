//! `ww shell` — thin REPL client that dials a shell cell over libp2p.
//!
//! Spins up a client-mode swarm (identify + stream only), dials the target
//! peer, bootstraps Cap'n Proto RPC on the shell protocol, and enters a
//! rustyline REPL loop.
//!
//! Accepts an optional multiaddr as a positional argument:
//!   - `/ip4/.../tcp/.../p2p/...` — dial directly
//!   - `/dnsaddr/...` — resolve via DNS TXT records, then dial
//!   - *(omitted)* — discover a local node via Kubo's LAN DHT

use anyhow::{Context, Result};
use libp2p::Multiaddr;
use std::path::PathBuf;

use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use futures::io::AsyncReadExt;

use ww::shell_capnp;

/// Discover a local wetware node via Kubo's routing API.
///
/// Queries `findprovs` for the well-known discovery CID and returns the
/// first provider's multiaddr (preferring loopback addresses).
async fn discover_local_node() -> Result<Multiaddr> {
    let kubo_url =
        std::env::var("IPFS_API").unwrap_or_else(|_| "http://localhost:5001".to_string());
    let client = ww::ipfs::HttpClient::new(kubo_url);
    let cid_str = ww::discovery::DISCOVERY_CID.to_string();

    eprintln!("Discovering local node via Kubo...");

    let providers = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        client.find_providers(&cid_str, 1),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "discovery timeout (15s)\n  Is a wetware node running?  Start one with: ww run ."
        )
    })?
    .context("discovery query failed")?;

    if providers.is_empty() {
        anyhow::bail!("no wetware node found on local network\n  Start one with: ww run .");
    }

    let (peer_id_str, addrs) = &providers[0];

    // Prefer a loopback address, then any address.
    let transport_addr = addrs
        .iter()
        .find(|a| a.contains("/ip4/127.") || a.contains("/ip6/::1/"))
        .or_else(|| addrs.first())
        .with_context(|| format!("found node {peer_id_str} but it has no addresses"))?;

    let full_addr: Multiaddr = format!("{transport_addr}/p2p/{peer_id_str}")
        .parse()
        .with_context(|| format!("invalid multiaddr: {transport_addr}/p2p/{peer_id_str}"))?;

    Ok(full_addr)
}

/// Run the interactive shell client.
pub async fn run_shell(addr: Option<Multiaddr>, identity: Option<PathBuf>) -> Result<()> {
    // 1. Resolve target address.
    let addr = match addr {
        Some(a) => a,
        None => discover_local_node().await?,
    };

    // 2. Load identity key.
    let keypair = if let Some(path) = identity {
        let path_str = path.to_str().context("identity path is non-UTF-8")?;
        let sk = ww::keys::load(path_str)?;
        ww::keys::to_libp2p(&sk)?
    } else {
        // Check default location
        let default_path = dirs::home_dir()
            .map(|h| h.join(".ww/identity"))
            .filter(|p| p.exists());
        if let Some(path) = default_path {
            let path_str = path.to_str().context("identity path is non-UTF-8")?;
            let sk = ww::keys::load(path_str)?;
            ww::keys::to_libp2p(&sk)?
        } else {
            // Ephemeral key for dev/testing
            let sk = ww::keys::generate()?;
            ww::keys::to_libp2p(&sk)?
        }
    };

    // 3. Build client swarm and extract peer ID from the address.
    //    For /dnsaddr/ multiaddrs, the peer ID is discovered after the DNS
    //    transport resolves the address and the connection is established.
    let mut client = ww::host::ClientSwarm::new(keypair)?;
    let mut stream_control = client.stream_control();

    let peer_id_from_addr = addr.iter().find_map(|proto| match proto {
        libp2p::multiaddr::Protocol::P2p(id) => Some(id),
        _ => None,
    });

    let (connected_tx, connected_rx) = tokio::sync::oneshot::channel();

    if let Some(peer_id) = peer_id_from_addr {
        let transport_addr: Multiaddr = addr
            .iter()
            .filter(|p| !matches!(p, libp2p::multiaddr::Protocol::P2p(_)))
            .collect();
        client.add_peer_addr(peer_id, transport_addr);
    } else {
        client
            .dial(addr.clone())
            .map_err(|e| anyhow::anyhow!("failed to dial {addr}: {e}"))?;
    }

    // 4. Spawn swarm event loop (reports first connected peer via channel).
    tokio::task::spawn_local(client.run(Some(connected_tx)));

    // 5. Resolve peer ID: either known from the multiaddr or discovered via connection.
    let peer_id = if let Some(id) = peer_id_from_addr {
        id
    } else {
        eprintln!("Resolving {addr}...");
        tokio::time::timeout(std::time::Duration::from_secs(30), connected_rx)
            .await
            .map_err(|_| anyhow::anyhow!("connection timeout (30s) — is the address correct?"))?
            .map_err(|_| anyhow::anyhow!("swarm event loop ended before connection"))?
    };

    // 6. Compute the shell protocol from schema bytes.
    // The schema bytes are compiled into the shell cell's build output.
    // We need the same bytes to compute the matching protocol CID.
    let schema_bytes = include_bytes!(concat!(env!("OUT_DIR"), "/shell_schema.bin"));
    let protocol_cid = ww::rpc::schema_cid(schema_bytes);
    let stream_protocol = ww::rpc::schema_protocol(&protocol_cid)?;

    // 7. Dial the shell protocol.
    eprintln!("Connecting to {peer_id}...");
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        stream_control.open_stream(peer_id, stream_protocol),
    )
    .await
    .map_err(|_| anyhow::anyhow!("connection timeout after 30s"))?
    .map_err(|e| anyhow::anyhow!("failed to open stream: {e}"))?;

    // 8. Bootstrap Cap'n Proto RPC.
    let (reader, writer) = Box::pin(stream).split();
    let network = VatNetwork::new(reader, writer, Side::Client, Default::default());
    let mut rpc_system = RpcSystem::new(Box::new(network), None);
    let shell: shell_capnp::shell::Client = rpc_system.bootstrap(Side::Server);

    // Verify the bootstrap resolves.
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        shell.client.when_resolved(),
    )
    .await
    .map_err(|_| anyhow::anyhow!("RPC handshake timeout (10s)"))?
    .map_err(|e| anyhow::anyhow!("RPC handshake failed: {e}"))?;

    // Drive RPC in background.
    tokio::task::spawn_local(async move {
        if let Err(e) = rpc_system.await {
            tracing::debug!("Shell RPC session ended: {e}");
        }
    });

    eprintln!("{}", glia::banner());
    eprintln!("Connected to {peer_id}");
    eprintln!("AI agents:  ipfs cat /ipns/releases.wetware.run/.agents/prompt.md");

    // 9. REPL loop.
    // rustyline blocks the thread, so run in spawn_blocking with mpsc bridge.
    let (line_tx, mut line_rx) = tokio::sync::mpsc::channel::<String>(1);

    std::thread::spawn(move || {
        let mut rl = rustyline::DefaultEditor::new().expect("failed to create editor");
        loop {
            match rl.readline("/ > ") {
                Ok(line) => {
                    if !line.trim().is_empty() {
                        let _ = rl.add_history_entry(&line);
                    }
                    if line_tx.blocking_send(line).is_err() {
                        break; // Channel closed — eval loop exited.
                    }
                }
                Err(rustyline::error::ReadlineError::Interrupted) => continue, // Ctrl-C
                Err(rustyline::error::ReadlineError::Eof) => break,            // Ctrl-D
                Err(e) => {
                    eprintln!("readline error: {e}");
                    break;
                }
            }
        }
    });

    while let Some(line) = line_rx.recv().await {
        if line.trim().is_empty() {
            continue;
        }

        // Send eval request with 30s timeout.
        let mut req = shell.eval_request();
        req.get().set_text(&line);

        match tokio::time::timeout(std::time::Duration::from_secs(30), req.send().promise).await {
            Ok(Ok(response)) => {
                let result: shell_capnp::shell::eval_results::Reader<'_> = response.get()?;
                let text = result.get_result()?.to_str().unwrap_or("(invalid UTF-8)");
                let is_error = result.get_is_error();

                if text == "exit" && !is_error {
                    break; // Exit sentinel from shell cell
                }

                if !text.is_empty() {
                    if is_error {
                        eprintln!("error: {text}");
                    } else {
                        println!("{text}");
                    }
                }
            }
            Ok(Err(e)) => {
                eprintln!("RPC error: {e}");
                break; // Remote probably disconnected.
            }
            Err(_) => {
                eprintln!("eval timeout (30s)");
            }
        }
    }

    Ok(())
}
