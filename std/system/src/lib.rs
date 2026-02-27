use capnp::capability::FromClientHook;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use futures::task::noop_waker;
use futures::FutureExt;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

mod bindings {
    wit_bindgen::generate!({
        path: "../../wit",
        world: "guest-streams",
        with: {
            "wasi:io/error@0.2.9": wasip2::io::error,
            "wasi:io/poll@0.2.9": wasip2::io::poll,
            "wasi:io/streams@0.2.9": wasip2::io::streams,
        },
    });
}

use bindings::wetware::streams::streams::create_connection;
use wasip2::io::poll as wasi_poll;
use wasip2::io::streams::{
    InputStream as WasiInputStream, OutputStream as WasiOutputStream, Pollable as WasiPollable,
    StreamError as WasiStreamError,
};

pub struct StreamReader {
    stream: WasiInputStream,
    buffer: Vec<u8>,
    offset: usize,
}

impl StreamReader {
    pub fn new(stream: WasiInputStream) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
            offset: 0,
        }
    }

    pub fn pollable(&self) -> WasiPollable {
        self.stream.subscribe()
    }
}

impl futures::io::AsyncRead for StreamReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.offset < self.buffer.len() {
            let available = &self.buffer[self.offset..];
            let to_copy = available.len().min(buf.len());
            buf[..to_copy].copy_from_slice(&available[..to_copy]);
            self.offset += to_copy;
            if self.offset >= self.buffer.len() {
                self.buffer.clear();
                self.offset = 0;
            }
            return std::task::Poll::Ready(Ok(to_copy));
        }

        let len = buf.len() as u64;
        match self.stream.read(len) {
            Ok(bytes) => {
                if bytes.is_empty() {
                    cx.waker().wake_by_ref();
                    return std::task::Poll::Pending;
                }
                self.buffer = bytes;
                self.offset = 0;
                let available = &self.buffer[self.offset..];
                let to_copy = available.len().min(buf.len());
                buf[..to_copy].copy_from_slice(&available[..to_copy]);
                self.offset += to_copy;
                std::task::Poll::Ready(Ok(to_copy))
            }
            Err(WasiStreamError::Closed) => std::task::Poll::Ready(Ok(0)),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("stream read error: {:?}", err),
            ))),
        }
    }
}

pub struct StreamWriter {
    stream: WasiOutputStream,
}

impl StreamWriter {
    pub fn new(stream: WasiOutputStream) -> Self {
        Self { stream }
    }

    pub fn pollable(&self) -> WasiPollable {
        self.stream.subscribe()
    }
}

impl futures::io::AsyncWrite for StreamWriter {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if buf.is_empty() {
            return std::task::Poll::Ready(Ok(0));
        }
        match self.stream.check_write() {
            Ok(0) => {
                cx.waker().wake_by_ref();
                std::task::Poll::Pending
            }
            Ok(budget) => {
                let to_write = buf.len().min(budget as usize);
                match self.stream.write(&buf[..to_write]) {
                    Ok(_written) => std::task::Poll::Ready(Ok(to_write)),
                    Err(WasiStreamError::Closed) => std::task::Poll::Ready(Ok(0)),
                    Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("stream write error: {:?}", err),
                    ))),
                }
            }
            Err(WasiStreamError::Closed) => std::task::Poll::Ready(Ok(0)),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("stream write error: {:?}", err),
            ))),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.stream.flush() {
            Ok(()) => std::task::Poll::Ready(Ok(())),
            Err(WasiStreamError::Closed) => std::task::Poll::Ready(Ok(())),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("stream flush error: {:?}", err),
            ))),
        }
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.stream.flush() {
            Ok(()) => std::task::Poll::Ready(Ok(())),
            Err(WasiStreamError::Closed) => std::task::Poll::Ready(Ok(())),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("stream close error: {:?}", err),
            ))),
        }
    }
}

pub struct StreamPollables {
    pub reader: WasiPollable,
    pub writer: WasiPollable,
}

pub struct GuestStreams {
    pub reader: StreamReader,
    pub writer: StreamWriter,
    pub pollables: StreamPollables,
}

pub fn connect_streams() -> GuestStreams {
    let connection = create_connection();
    let input_stream = connection.get_input_stream();
    let output_stream = connection.get_output_stream();

    let reader = StreamReader::new(input_stream);
    let writer = StreamWriter::new(output_stream);
    let pollables = StreamPollables {
        reader: reader.pollable(),
        writer: writer.pollable(),
    };

    GuestStreams {
        reader,
        writer,
        pollables,
    }
}

pub struct RpcSession<C> {
    pub rpc_system: RpcSystem<Side>,
    pub client: C,
    pub pollables: StreamPollables,
}

impl<C: FromClientHook> RpcSession<C> {
    pub fn connect() -> Self {
        Self::connect_with_export(None)
    }

    /// Connect and export `bootstrap` as this vat's bootstrap capability.
    ///
    /// The host can retrieve the exported cap via `rpc_system.bootstrap(Side::Client)`.
    /// Pass `None` for guests that do not export a capability (equivalent to `connect()`).
    pub fn connect_with_export(bootstrap: Option<capnp::capability::Client>) -> Self {
        let streams = connect_streams();
        let pollables = streams.pollables;
        let network = VatNetwork::new(
            streams.reader,
            streams.writer,
            Side::Client,
            Default::default(),
        );
        let mut rpc_system = RpcSystem::new(Box::new(network), bootstrap);
        let client = rpc_system.bootstrap(Side::Server);
        Self {
            rpc_system,
            client,
            pollables,
        }
    }

    pub fn forget(self) {
        std::mem::forget(self.client);
        std::mem::forget(self.rpc_system);
        std::mem::forget(self.pollables);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DriveOutcome {
    pub done: bool,
    pub progressed: bool,
}

impl DriveOutcome {
    pub fn done() -> Self {
        Self {
            done: true,
            progressed: true,
        }
    }

    pub fn progress() -> Self {
        Self {
            done: false,
            progressed: true,
        }
    }

    pub fn pending() -> Self {
        Self {
            done: false,
            progressed: false,
        }
    }
}

pub struct RpcDriver {
    waker: Waker,
}

impl RpcDriver {
    pub fn new() -> Self {
        Self {
            waker: noop_waker(),
        }
    }

    pub fn drive_until<F>(
        &self,
        rpc_system: &mut RpcSystem<Side>,
        pollables: &StreamPollables,
        mut poll: F,
    ) where
        F: FnMut(&mut Context<'_>) -> DriveOutcome,
    {
        loop {
            let mut made_progress = false;
            let mut cx = Context::from_waker(&self.waker);

            match rpc_system.poll_unpin(&mut cx) {
                Poll::Ready(Ok(())) | Poll::Ready(Err(_)) => {
                    made_progress = true;
                }
                Poll::Pending => {}
            }

            let outcome = poll(&mut cx);
            made_progress |= outcome.progressed;

            if outcome.done {
                break;
            }

            // Flush any writes queued by the poll closure before blocking.
            match rpc_system.poll_unpin(&mut cx) {
                Poll::Ready(Ok(())) | Poll::Ready(Err(_)) => {
                    made_progress = true;
                }
                Poll::Pending => {}
            }

            if !made_progress {
                if pollables.writer.ready() {
                    wasi_poll::poll(&[&pollables.reader]);
                } else {
                    wasi_poll::poll(&[&pollables.reader, &pollables.writer]);
                }
            }
        }
    }
}

/// Drive the RPC system and poll a single future to completion.
///
/// Returns `Some(value)` if the future resolves, or `None` if the
/// RPC system terminates before the future completes.
pub fn block_on<C, F>(session: &mut RpcSession<C>, mut future: F) -> Option<F::Output>
where
    C: FromClientHook,
    F: Future + Unpin,
{
    let waker = noop_waker();
    let mut rpc_done = false;
    loop {
        let mut cx = Context::from_waker(&waker);
        let mut made_progress = false;

        if !rpc_done {
            match session.rpc_system.poll_unpin(&mut cx) {
                Poll::Ready(_) => {
                    rpc_done = true;
                    made_progress = true;
                }
                Poll::Pending => {}
            }
        }

        match Pin::new(&mut future).poll(&mut cx) {
            Poll::Ready(value) => return Some(value),
            Poll::Pending => {}
        }

        // Flush any writes queued by the future before blocking in wasi_poll.
        if !rpc_done {
            match session.rpc_system.poll_unpin(&mut cx) {
                Poll::Ready(_) => {
                    rpc_done = true;
                    made_progress = true;
                }
                Poll::Pending => {}
            }
        }

        if rpc_done {
            return None;
        }

        if !made_progress {
            if session.pollables.writer.ready() {
                wasi_poll::poll(&[&session.pollables.reader]);
            } else {
                wasi_poll::poll(&[&session.pollables.reader, &session.pollables.writer]);
            }
        }
    }
}

/// Export a bootstrap capability over WASI stdin/stdout.
///
/// This is for handler processes spawned by `Server.serve()`. The host wires
/// the handler's stdin/stdout to a libp2p stream. This function sets up a
/// Cap'n Proto RPC VatNetwork over stdin/stdout and exports the given
/// bootstrap capability. The remote peer bootstraps it to obtain the service.
///
/// Unlike [`serve`], this function does NOT use the wetware:streams connection.
/// It reads/writes directly from WASI stdin/stdout and drives the RPC system
/// until the connection closes. No host capabilities are available — if the
/// handler needs IPFS/routing, it should use `system::run()` over data_streams
/// instead.
///
/// # Example
///
/// ```no_run
/// let engine: chess_capnp::chess_engine::Client =
///     capnp_rpc::new_client(ChessEngineImpl::new());
/// wetware_guest::serve_stdio(engine.client);
/// ```
pub fn serve_stdio(bootstrap: capnp::capability::Client) {
    let stdin = wasip2::cli::stdin::get_stdin();
    let stdout = wasip2::cli::stdout::get_stdout();

    let reader = StreamReader::new(stdin);
    let writer = StreamWriter::new(stdout);
    let pollables = StreamPollables {
        reader: reader.pollable(),
        writer: writer.pollable(),
    };

    let network = VatNetwork::new(reader, writer, Side::Server, Default::default());
    let mut rpc_system = RpcSystem::new(Box::new(network), Some(bootstrap));

    drive_rpc_only(&mut rpc_system, &pollables);

    // Forget resources to avoid WASI cleanup errors.
    std::mem::forget(rpc_system);
    std::mem::forget(pollables);
}

/// Drive an RPC system until the connection closes (no user future).
fn drive_rpc_only(rpc_system: &mut RpcSystem<Side>, pollables: &StreamPollables) {
    let waker = noop_waker();
    loop {
        let mut cx = Context::from_waker(&waker);
        match rpc_system.poll_unpin(&mut cx) {
            Poll::Ready(_) => break,
            Poll::Pending => {}
        }

        if pollables.writer.ready() {
            wasi_poll::poll(&[&pollables.reader]);
        } else {
            wasi_poll::poll(&[&pollables.reader, &pollables.writer]);
        }
    }
}

/// Drive an RPC system alongside a user future until the future completes
/// or the RPC connection closes.
fn drive_rpc_with_future(
    rpc_system: &mut RpcSystem<Side>,
    pollables: &StreamPollables,
    future: &mut Pin<Box<impl Future<Output = Result<(), capnp::Error>> + ?Sized>>,
) {
    let waker = noop_waker();
    let mut rpc_done = false;
    loop {
        let mut cx = Context::from_waker(&waker);
        let mut made_progress = false;

        if !rpc_done {
            match rpc_system.poll_unpin(&mut cx) {
                Poll::Ready(_) => {
                    rpc_done = true;
                    made_progress = true;
                }
                Poll::Pending => {}
            }
        }

        match future.as_mut().poll(&mut cx) {
            Poll::Ready(result) => {
                if let Err(e) = result {
                    let stderr = wasip2::cli::stderr::get_stderr();
                    let _ = stderr
                        .blocking_write_and_flush(format!("[FATAL] guest error: {e}\n").as_bytes());
                }
                return;
            }
            Poll::Pending => {}
        }

        // Flush any writes queued by the future before blocking in wasi_poll.
        if !rpc_done {
            match rpc_system.poll_unpin(&mut cx) {
                Poll::Ready(_) => {
                    rpc_done = true;
                    made_progress = true;
                }
                Poll::Pending => {}
            }
        }

        if rpc_done {
            return;
        }

        if !made_progress {
            if pollables.writer.ready() {
                wasi_poll::poll(&[&pollables.reader]);
            } else {
                wasi_poll::poll(&[&pollables.reader, &pollables.writer]);
            }
        }
    }
}

/// Run a guest program with an async entry point, exporting a bootstrap capability.
///
/// Like [`run`], but the guest also provides `bootstrap` as its own bootstrap
/// capability on the RPC connection.  The host can retrieve it via
/// `rpc_system.bootstrap(Side::Client)`.
///
/// Use this when the guest needs to export a capability back to the host —
/// for example, a kernel that wraps and attenuates the host's Membrane before
/// re-exporting it to external peers.
///
/// # Example
///
/// ```no_run
/// let my_membrane: capnp::capability::Client = capnp_rpc::new_client(MyMembraneImpl).client;
/// wetware_guest::serve(my_membrane, |host| async move {
///     // ... use host capabilities, export my_membrane to host ...
///     Ok(())
/// });
/// ```
pub fn serve<C, F, Fut>(bootstrap: capnp::capability::Client, f: F)
where
    C: FromClientHook + Clone,
    F: FnOnce(C) -> Fut,
    Fut: Future<Output = Result<(), capnp::Error>>,
{
    run_with_session(RpcSession::<C>::connect_with_export(Some(bootstrap)), f)
}

/// Run a guest program with an async entry point.
///
/// Sets up the RPC session, bootstraps the host capability, and drives
/// the provided async closure to completion alongside the RPC system.
/// Handles all resource cleanup automatically.
///
/// # Example
///
/// ```no_run
/// wetware_guest::run(|host| async move {
///     let executor = host.executor_request().send().pipeline.get_executor();
///     let resp = executor.echo_request().send().promise.await?;
///     let text = resp.get()?.get_response()?.to_str()?;
///     Ok(())
/// });
/// ```
pub fn run<C, F, Fut>(f: F)
where
    C: FromClientHook + Clone,
    F: FnOnce(C) -> Fut,
    Fut: Future<Output = Result<(), capnp::Error>>,
{
    run_with_session(RpcSession::<C>::connect(), f)
}

fn run_with_session<C, F, Fut>(mut session: RpcSession<C>, f: F)
where
    C: FromClientHook + Clone,
    F: FnOnce(C) -> Fut,
    Fut: Future<Output = Result<(), capnp::Error>>,
{
    let client = session.client.clone();
    let future = f(client);
    let mut future = Box::pin(future);

    drive_rpc_with_future(&mut session.rpc_system, &session.pollables, &mut future);

    // Forget to avoid dropping Cap'n Proto objects which would trigger
    // WASI resource cleanup errors.
    std::mem::forget(future);
    session.forget();
}
