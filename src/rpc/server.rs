//! TCP listener that serves Cap'n Proto RPC clients.
//!
//! Each accepted connection gets its own RPC system with a `Host` bootstrap
//! capability. Must be run inside a `tokio::task::LocalSet` because capnp-rpc
//! futures are `!Send`.
#![cfg(not(target_arch = "wasm32"))]

use futures::FutureExt;
use tokio::net::TcpListener;
use tokio::sync::mpsc;

use crate::host::SwarmCommand;
use crate::rpc::{build_peer_rpc, NetworkState};

/// Accept TCP connections and serve Cap'n Proto RPC to each client.
///
/// This function never returns under normal operation. It must be called
/// from within a `tokio::task::LocalSet`.
pub async fn serve_rpc_clients(
    listener: TcpListener,
    network_state: NetworkState,
    swarm_cmd_tx: mpsc::Sender<SwarmCommand>,
    wasm_debug: bool,
) -> anyhow::Result<()> {
    loop {
        let (stream, addr) = listener.accept().await?;
        tracing::info!(%addr, "RPC client connected");

        let (reader, writer) = tokio::io::split(stream);
        let rpc_system = build_peer_rpc(
            reader,
            writer,
            network_state.clone(),
            swarm_cmd_tx.clone(),
            wasm_debug,
        );

        tokio::task::spawn_local(async move {
            match rpc_system.map(|r| r.map_err(|e| e.to_string())).await {
                Ok(()) => tracing::info!(%addr, "RPC client disconnected"),
                Err(e) => tracing::warn!(%addr, error = %e, "RPC client session error"),
            }
        });
    }
}
