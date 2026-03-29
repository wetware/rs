//! VatListener capability: guest-exported subprotocols via Cap'n Proto RPC.
//!
//! The `VatListener` capability lets a guest register a libp2p subprotocol handler
//! that exports a typed capability. For each incoming connection, the host spawns a
//! handler process (via the guest-provided `Executor`), captures the handler's
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

        // Extract schema from the WASM binary's "schema.capnp" custom section.
        let schema_bytes = match pry!(super::extract_wasm_custom_section(&wasm, "schema.capnp")) {
            Some(bytes) if !bytes.is_empty() => bytes.to_vec(),
            Some(_) => {
                return Promise::err(capnp::Error::failed(
                    "schema.capnp custom section is empty".into(),
                ));
            }
            None => {
                return Promise::err(capnp::Error::failed(
                    "WASM binary does not contain a 'schema.capnp' custom section".into(),
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
                    "failed to register vat protocol handler: {e}"
                ))));

        tracing::info!(protocol = %stream_protocol, "Registered vat subprotocol handler");

        // Accept loop: for each incoming connection, spawn a handler and bridge RPC.
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
                        tracing::error!(
                            protocol = protocol_cid,
                            "Vat handler connection error: {e}"
                        );
                    }
                });
            }
            tracing::warn!(protocol = %stream_protocol, "Vat accept loop ended unexpectedly");
        });

        Promise::ok(())
    }
}

/// Spawn a handler process for a single connection and bridge its
/// exported bootstrap capability to the connecting peer via Cap'n Proto RPC.
///
/// Architecture (two-RPC-system bridge):
///
/// ```text
/// Remote peer  ←──[libp2p stream]──→  Host bridge  ←──[WASI bidi]──→  Handler
///                  RPC system B                        RPC system A
///                  (Side::Server)                      (Side::Server)
///                  bootstrap =                         bootstrap = Membrane
///                  handler_cap ←── captures ←───────── handler exports via serve()
/// ```
async fn handle_vat_connection(
    executor: system_capnp::executor::Client,
    handler_wasm: &[u8],
    stream: libp2p::Stream,
    protocol_cid: &str,
) -> Result<(), capnp::Error> {
    // 1. Spawn handler process via executor.runBytes.
    let mut req = executor.run_bytes_request();
    req.get().set_wasm(handler_wasm);
    req.get().init_args(0);
    {
        let mut env = req.get().init_env(3);
        env.set(0, "WW_HANDLER=1");
        env.set(1, format!("WW_PROTOCOL={protocol_cid}"));
        env.set(2, "PATH=/bin");
    }
    let response = req.send().promise.await?;
    let process = response.get()?.get_process()?;

    // 2. Get the handler's exported bootstrap capability.
    let bootstrap_resp = process.bootstrap_request().send().promise.await?;
    let bootstrap_cap: capnp::capability::Client =
        bootstrap_resp.get()?.get_cap().get_as_capability()?;

    // 3. Bridge: serve the handler's cap to the remote peer over the libp2p stream.
    let (reader, writer) = Box::pin(stream).split();
    let network = VatNetwork::new(reader, writer, Side::Server, Default::default());
    let peer_rpc = RpcSystem::new(Box::new(network), Some(bootstrap_cap));

    // 4. Drive the peer RPC system and handler process concurrently.
    //    When EITHER side finishes (peer disconnects OR handler exits),
    //    we tear down both to avoid serving a dead capability or
    //    keeping a handler alive with no peer.
    let wait_fut = async {
        let resp = process.wait_request().send().promise.await;
        match resp {
            Ok(r) => r.get().map(|r| r.get_exit_code()).unwrap_or(1),
            Err(_) => 1,
        }
    };

    tokio::select! {
        _ = peer_rpc => {
            tracing::debug!(protocol = protocol_cid, "Peer disconnected, cleaning up handler");
            // Peer disconnected. Drop process capability to trigger cleanup.
            // The handler will see its host RPC connection close.
        }
        exit_code = wait_fut => {
            tracing::debug!(exit_code, protocol = protocol_cid, "Vat handler process exited");
            // Handler exited. The peer RPC will get disconnected errors
            // on subsequent calls since the bootstrap cap is dead.
        }
    }

    Ok(())
}
