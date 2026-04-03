use capnp::capability::FromClientHook;
use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

// Tracks whether any data was written during the current poll cycle.
//
// The guest is single-threaded WASM, so a thread-local `Cell<bool>` is
// race-free.  This flag replaces the racy `pollables.writer.ready()`
// check that caused deadlocks during multi-cycle writes: the host-side
// `AsyncWriteStream` worker could replenish the write budget between
// the RPC poll and the readiness check, making the loop think it was
// idle and block on reader-only — while the RPC system still had
// pending writes.
thread_local! {
    static WRITE_OCCURRED: Cell<bool> = const { Cell::new(false) };
}

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
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::other(format!(
                "stream read error: {:?}",
                err
            )))),
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
                    Ok(_written) => {
                        WRITE_OCCURRED.with(|f| f.set(true));
                        std::task::Poll::Ready(Ok(to_write))
                    }
                    Err(WasiStreamError::Closed) => std::task::Poll::Ready(Ok(0)),
                    Err(err) => std::task::Poll::Ready(Err(std::io::Error::other(format!(
                        "stream write error: {:?}",
                        err
                    )))),
                }
            }
            Err(WasiStreamError::Closed) => std::task::Poll::Ready(Ok(0)),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::other(format!(
                "stream write error: {:?}",
                err
            )))),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.stream.flush() {
            Ok(()) => std::task::Poll::Ready(Ok(())),
            Err(WasiStreamError::Closed) => std::task::Poll::Ready(Ok(())),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::other(format!(
                "stream flush error: {:?}",
                err
            )))),
        }
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.stream.flush() {
            Ok(()) => std::task::Poll::Ready(Ok(())),
            Err(WasiStreamError::Closed) => std::task::Poll::Ready(Ok(())),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::other(format!(
                "stream close error: {:?}",
                err
            )))),
        }
    }
}

pub struct StreamPollables {
    pub reader: WasiPollable,
    pub writer: WasiPollable,
}

/// Safety-net timeout for idle poll cycles.
///
/// When the polling loop has no pending writes and made no progress, it blocks
/// on the reader pollable alone.  The host streams large responses (e.g. 1 MB
/// handler WASM) in chunks via wasmtime's `AsyncReadStream`, whose background
/// task can race with the foreground pollable check — causing a missed wakeup
/// that would block the guest indefinitely.
///
/// Adding a `wasi:clocks/monotonic-clock.subscribe-duration` pollable to the
/// poll set provides a guaranteed wakeup (per the WASI spec, this is the
/// canonical way to add a timeout to a poll).  In the common case the reader
/// fires first and latency is unaffected; if a wakeup is missed, the timeout
/// fires and the loop retries.
///
/// The pollable is created once before each loop and reused across iterations.
/// Because clock pollables are level-triggered (stay ready once elapsed), we
/// refresh only when the timeout actually fires.
const IDLE_POLL_TIMEOUT_NS: u64 = 100_000_000; // 100ms

fn new_idle_timeout() -> WasiPollable {
    wasip2::clocks::monotonic_clock::subscribe_duration(IDLE_POLL_TIMEOUT_NS)
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

    /// Leak resources to avoid WASI cleanup panics at process exit.
    ///
    /// Cap'n Proto destructors try to close handles that are already dead
    /// when the host tears down the RPC channel. This is a WASI-P2 wart;
    /// revisit when wasmtime/WASI stabilises resource cleanup ordering.
    pub fn forget(self) {
        std::mem::forget(self.client);
        std::mem::forget(self.rpc_system);
        std::mem::forget(self.pollables);
    }
}

/// Core poll loop: drive RPC alongside user work until one side finishes.
///
/// Each iteration: reset write flag → poll RPC → poll user work → flush
/// RPC writes → block on WASI I/O.
///
/// Returns `Some(value)` when `poll_work` returns `Poll::Ready(value)`.
/// Returns `None` if the RPC connection closes first.
fn poll_loop<T>(
    rpc_system: &mut RpcSystem<Side>,
    pollables: &StreamPollables,
    mut poll_work: impl FnMut(&mut Context<'_>) -> Poll<T>,
) -> Option<T> {
    let mut rpc_done = false;
    let mut idle_timeout = new_idle_timeout();
    loop {
        let mut cx = Context::from_waker(Waker::noop());
        let mut made_progress = false;
        WRITE_OCCURRED.with(|f| f.set(false));

        // Drive RPC state machine (process inbound messages).
        if !rpc_done {
            if let Poll::Ready(_) = Pin::new(&mut *rpc_system).poll(&mut cx) {
                rpc_done = true;
                made_progress = true;
            }
        }

        // Drive user work.
        if let Poll::Ready(val) = poll_work(&mut cx) {
            return Some(val);
        }

        // Flush any writes queued by user work before blocking on WASI poll.
        if !rpc_done {
            if let Poll::Ready(_) = Pin::new(&mut *rpc_system).poll(&mut cx) {
                rpc_done = true;
                made_progress = true;
            }
        }

        if rpc_done {
            return None;
        }

        let wrote = WRITE_OCCURRED.with(|f| f.get());

        if wrote || made_progress {
            if !pollables.writer.ready() {
                wasi_poll::poll(&[&pollables.reader, &pollables.writer]);
            }
        } else {
            wasi_poll::poll(&[&pollables.reader, &idle_timeout]);
            if idle_timeout.ready() {
                idle_timeout = new_idle_timeout();
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

    // Drive RPC only (no user future) — poll_loop returns None when RPC closes.
    let _: Option<()> = poll_loop(&mut rpc_system, &pollables, |_| Poll::Pending);

    // Forget resources to avoid WASI cleanup errors.
    std::mem::forget(rpc_system);
    std::mem::forget(pollables);
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
/// wetware_guest::run(|membrane| async move {
///     let graft = membrane.graft_request().send().promise.await?;
///     let runtime = graft.get()?.get_runtime()?;
///     let load_resp = runtime.load_request().send().promise.await?;
///     let executor = load_resp.get()?.get_executor()?;
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
    let mut future = Box::pin(f(client));

    let _ = poll_loop(&mut session.rpc_system, &session.pollables, |cx| {
        future.as_mut().poll(cx)
    });

    // Forget to avoid dropping Cap'n Proto objects which would trigger
    // WASI resource cleanup errors.
    std::mem::forget(future);
    session.forget();
}
