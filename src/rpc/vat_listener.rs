//! VatListener capability: guest-exported subprotocols via Cap'n Proto RPC.
//!
//! The `VatListener` capability lets a guest register a libp2p subprotocol cell
//! that exports a typed capability. For each incoming connection, the host spawns a
//! cell process (via the guest-provided `Executor`), captures the cell's
//! `system::serve()` exported bootstrap capability, and serves it to the
//! connecting peer via Cap'n Proto RPC bootstrapping.
//!
//! This is the capability-mode counterpart of `StreamListener` (byte-stream mode).

use std::sync::Arc;

use capnp::capability::Promise;
use capnp_rpc::pry;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use futures::io::AsyncReadExt;
use futures::StreamExt;
use membrane::EpochGuard;

use crate::system_capnp;

pub struct VatListenerImpl {
    stream_control: libp2p_stream::Control,
    guard: EpochGuard,
}

impl VatListenerImpl {
    pub fn new(stream_control: libp2p_stream::Control, guard: EpochGuard) -> Self {
        Self {
            stream_control,
            guard,
        }
    }
}

#[allow(refining_impl_trait)]
impl system_capnp::vat_listener::Server for VatListenerImpl {
    fn listen(
        self: capnp::capability::Rc<Self>,
        params: system_capnp::vat_listener::ListenParams,
        _results: system_capnp::vat_listener::ListenResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());

        let params = pry!(params.get());
        let executor: system_capnp::executor::Client = pry!(params.get_executor());
        let wasm: Arc<[u8]> = pry!(params.get_wasm()).into();

        // Decode the Cell type tag from the "cell.capnp" custom section.
        let schema_bytes = match pry!(super::decode_cell_section(&wasm)) {
            Some(super::CellType::Capnp(bytes)) => bytes,
            Some(other) => {
                return Promise::err(capnp::Error::failed(format!(
                    "VatListener requires Cell::capnp variant, got {:?}",
                    std::mem::discriminant(&other)
                )));
            }
            None => {
                return Promise::err(capnp::Error::failed(
                    "WASM binary does not contain a 'cell.capnp' custom section \
                     (VatListener requires Cell::capnp)"
                        .into(),
                ));
            }
        };

        let protocol_cid = super::schema_cid(&schema_bytes);
        let stream_protocol = pry!(super::schema_protocol(&protocol_cid));

        let mut control = self.stream_control.clone();
        let mut incoming =
            pry!(control
                .accept(stream_protocol.clone())
                .map_err(|e| capnp::Error::failed(format!(
                    "failed to register vat protocol cell: {e}"
                ))));

        tracing::info!(protocol = %stream_protocol, "Registered vat subprotocol cell");

        // Accept loop: for each incoming connection, spawn a cell and bridge RPC.
        tokio::task::spawn_local(async move {
            while let Some((peer_id, stream)) = incoming.next().await {
                tracing::debug!(
                    peer = %peer_id,
                    protocol = %stream_protocol,
                    "Incoming vat connection"
                );
                let executor = executor.clone();
                let wasm = Arc::clone(&wasm);
                let protocol_cid = protocol_cid.clone();
                tokio::task::spawn_local(async move {
                    if let Err(e) =
                        handle_vat_connection(executor, &wasm, stream, &protocol_cid).await
                    {
                        tracing::error!(protocol = protocol_cid, "Vat cell connection error: {e}");
                    }
                });
            }
            tracing::warn!(protocol = %stream_protocol, "Vat accept loop ended unexpectedly");
        });

        Promise::ok(())
    }
}

/// Spawn a cell process for a single connection and bridge its
/// exported bootstrap capability to the connecting peer via Cap'n Proto RPC.
///
/// Architecture (two-RPC-system bridge):
///
/// ```text
/// Remote peer  ←──[libp2p stream]──→  Host bridge  ←──[WASI bidi]──→  Cell
///                  RPC system B                        RPC system A
///                  (Side::Server)                      (Side::Server)
///                  bootstrap =                         bootstrap = Membrane
///                  cell_cap ←── captures ←───────── cell exports via serve()
/// ```
async fn handle_vat_connection(
    executor: system_capnp::executor::Client,
    cell_wasm: &[u8],
    stream: libp2p::Stream,
    protocol_cid: &str,
) -> Result<(), capnp::Error> {
    // 1. Spawn cell process via executor.runBytes.
    let mut req = executor.run_bytes_request();
    req.get().set_wasm(cell_wasm);
    req.get().init_args(0);
    {
        let mut env = req.get().init_env(3);
        env.set(0, "WW_CELL=1");
        env.set(1, format!("WW_PROTOCOL={protocol_cid}"));
        env.set(2, "PATH=/bin");
    }
    let response = req.send().promise.await?;
    let process = response.get()?.get_process()?;

    // 2. Get the cell's exported bootstrap capability.
    //    Timeout guards against cells that never call system::serve().
    let bootstrap_resp = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        process.bootstrap_request().send().promise,
    )
    .await
    .map_err(|_| {
        capnp::Error::failed(
            "cell did not export bootstrap capability within 10s \
             (did the guest call system::serve()?)"
                .into(),
        )
    })??;
    let bootstrap_cap: capnp::capability::Client =
        bootstrap_resp.get()?.get_cap().get_as_capability()?;

    // 3. Bridge: serve the cell's cap to the remote peer over the libp2p stream.
    let (reader, writer) = Box::pin(stream).split();
    let network = VatNetwork::new(reader, writer, Side::Server, Default::default());
    let peer_rpc = RpcSystem::new(Box::new(network), Some(bootstrap_cap));

    // 4. Drive the peer RPC system and cell process concurrently.
    //    When EITHER side finishes (peer disconnects OR cell exits),
    //    we tear down both to avoid serving a dead capability or
    //    keeping a cell alive with no peer.
    let wait_fut = async {
        let resp = process.wait_request().send().promise.await;
        match resp {
            Ok(r) => r.get().map(|r| r.get_exit_code()).unwrap_or(1),
            Err(_) => 1,
        }
    };

    tokio::select! {
        _ = peer_rpc => {
            tracing::debug!(protocol = protocol_cid, "Peer disconnected, cleaning up cell");
            // Peer disconnected. Drop process capability to trigger cleanup.
            // The cell will see its host RPC connection close.
        }
        exit_code = wait_fut => {
            tracing::debug!(exit_code, protocol = protocol_cid, "Vat cell process exited");
            // Cell exited. The peer RPC will get disconnected errors
            // on subsequent calls since the bootstrap cap is dead.
        }
    }

    Ok(())
}
