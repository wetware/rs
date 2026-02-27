//! Listener capability: guest-exported subprotocols via process-per-connection.
//!
//! The `Listener` capability lets a guest register a libp2p subprotocol handler.
//! For each incoming stream on that subprotocol, the host spawns a fresh WASI
//! process (via the guest-provided `Executor`) with stdin/stdout wired to the
//! stream — the handler speaks whatever wire protocol it wants over stdio.

use capnp::capability::Promise;
use capnp_rpc::pry;
use futures::io::{AsyncReadExt, AsyncWriteExt};
use futures::StreamExt;
use libp2p::StreamProtocol;
use membrane::EpochGuard;

use crate::system_capnp;

pub struct ListenerImpl {
    stream_control: libp2p_stream::Control,
    guard: EpochGuard,
}

impl ListenerImpl {
    pub fn new(stream_control: libp2p_stream::Control, guard: EpochGuard) -> Self {
        Self {
            stream_control,
            guard,
        }
    }
}

#[allow(refining_impl_trait)]
impl system_capnp::listener::Server for ListenerImpl {
    fn listen(
        self: capnp::capability::Rc<Self>,
        params: system_capnp::listener::ListenParams,
        _results: system_capnp::listener::ListenResults,
    ) -> Promise<(), capnp::Error> {
        pry!(self.guard.check());

        let params = pry!(params.get());
        let executor: system_capnp::executor::Client = pry!(params.get_executor());
        let protocol_str = pry!(pry!(params.get_protocol())
            .to_str()
            .map_err(|e| capnp::Error::failed(e.to_string())));
        let handler = pry!(params.get_handler()).to_vec();

        if protocol_str.is_empty() {
            return Promise::err(capnp::Error::failed(
                "protocol name must not be empty".into(),
            ));
        }
        if protocol_str.contains('/') {
            return Promise::err(capnp::Error::failed(
                "protocol name must not contain '/'".into(),
            ));
        }

        let protocol_suffix = protocol_str.to_string();
        let stream_protocol = pry!(StreamProtocol::try_from_owned(format!(
            "/ww/0.1.0/{protocol_suffix}"
        ))
        .map_err(|e| capnp::Error::failed(format!("invalid protocol: {e}"))));

        let mut control = self.stream_control.clone();
        let mut incoming =
            pry!(control
                .accept(stream_protocol.clone())
                .map_err(|e| capnp::Error::failed(format!(
                    "failed to register protocol handler: {e}"
                ))));

        tracing::info!(protocol = %stream_protocol, "Registered subprotocol handler");

        // Accept loop: for each incoming connection, spawn a handler process.
        tokio::task::spawn_local(async move {
            while let Some((peer_id, stream)) = incoming.next().await {
                tracing::info!(
                    peer = %peer_id,
                    protocol = %stream_protocol,
                    "Incoming subprotocol connection"
                );
                let executor = executor.clone();
                let handler = handler.clone();
                let protocol = protocol_suffix.clone();
                tokio::task::spawn_local(async move {
                    if let Err(e) = handle_connection(executor, &handler, stream, &protocol).await {
                        tracing::error!(protocol, "Handler connection error: {e}");
                    }
                });
            }
            tracing::debug!("Subprotocol accept loop ended");
        });

        Promise::ok(())
    }
}

/// Spawn a handler process for a single connection and pump
/// stdin/stdout between the libp2p stream and the process.
pub(crate) async fn handle_connection(
    executor: system_capnp::executor::Client,
    handler_wasm: &[u8],
    stream: libp2p::Stream,
    protocol: &str,
) -> Result<(), capnp::Error> {
    // Spawn handler process via executor.runBytes.
    let mut req = executor.run_bytes_request();
    req.get().set_wasm(handler_wasm);
    {
        let mut env = req.get().init_env(3);
        env.set(0, "WW_HANDLER=1");
        env.set(1, format!("WW_PROTOCOL={protocol}"));
        env.set(2, "PATH=/bin");
    }
    let response = req.send().promise.await?;
    let process = response.get()?.get_process()?;

    // Get stdin (write-only) and stdout (read-only) ByteStream clients.
    let stdin_resp = process.stdin_request().send().promise.await?;
    let stdin = stdin_resp.get()?.get_stream()?;

    let stdout_resp = process.stdout_request().send().promise.await?;
    let stdout = stdout_resp.get()?.get_stream()?;

    // Split the libp2p stream into read and write halves.
    let (reader, writer) = Box::pin(stream).split();

    // Keep a handle to close stdin after the pumps finish.
    let stdin_close = stdin.clone();

    // Pump data concurrently: stream->stdin and stdout->stream.
    // When either pump finishes, drop the other and clean up.
    futures::future::select(
        Box::pin(pump_stream_to_stdin(reader, stdin)),
        Box::pin(pump_stdout_to_stream(stdout, writer)),
    )
    .await;

    // Ensure stdin is closed so the handler sees EOF.
    let _ = stdin_close.close_request().send().promise.await;

    // Wait for the handler process to exit.
    let wait_resp = process.wait_request().send().promise.await?;
    let exit_code = wait_resp.get()?.get_exit_code();
    tracing::info!(exit_code, protocol, "Handler process exited");

    Ok(())
}

/// Read from the libp2p stream and write to the handler's stdin.
pub(crate) async fn pump_stream_to_stdin(
    mut reader: impl futures::io::AsyncRead + Unpin,
    stdin: system_capnp::byte_stream::Client,
) {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match reader.read(&mut buf).await {
            Ok(0) => {
                let _ = stdin.close_request().send().promise.await;
                break;
            }
            Ok(n) => {
                let mut req = stdin.write_request();
                req.get().set_data(&buf[..n]);
                if let Err(e) = req.send().promise.await {
                    tracing::debug!("stdin write failed: {e}");
                    break;
                }
            }
            Err(e) => {
                tracing::debug!("stream read error: {e}");
                let _ = stdin.close_request().send().promise.await;
                break;
            }
        }
    }
}

/// Read from the handler's stdout and write to the libp2p stream.
pub(crate) async fn pump_stdout_to_stream(
    stdout: system_capnp::byte_stream::Client,
    mut writer: impl futures::io::AsyncWrite + Unpin,
) {
    loop {
        let mut req = stdout.read_request();
        req.get().set_max_bytes(64 * 1024);
        let result: Result<Vec<u8>, capnp::Error> = req.send().promise.await.and_then(|response| {
            let data = response.get()?.get_data()?.to_vec();
            Ok(data)
        });
        match result {
            Ok(data) if data.is_empty() => break,
            Ok(data) => {
                if let Err(e) = writer.write_all(&data).await {
                    tracing::debug!("stream write error: {e}");
                    break;
                }
                if let Err(e) = writer.flush().await {
                    tracing::debug!("stream flush error: {e}");
                    break;
                }
            }
            Err(e) => {
                tracing::debug!("stdout read error: {e}");
                break;
            }
        }
    }
}
