#![feature(wasip2)]

use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use futures::future::FutureExt;
use futures::task::noop_waker;
use wasip2::cli::stderr::get_stderr;
use wasip2::exports::cli::run::Guest;

// Generate bindings for wetware streams (guest imports from host)
wit_bindgen::generate!({
    path: "../../wit",
    world: "guest-streams",
});

use wetware::streams::streams::{
    create_connection, InputStream as WetwareInputStream, OutputStream as WetwareOutputStream,
    Pollable as WetwarePollable, StreamError as WetwareStreamError,
};

mod peer_capnp;

struct StderrLogger;

impl log::Log for StderrLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= log::Level::Trace
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let stderr = get_stderr();
        let _ = stderr.blocking_write_and_flush(
            format!("[{}] {}\n", record.level(), record.args()).as_bytes(),
        );
    }

    fn flush(&self) {}
}

static LOGGER: StderrLogger = StderrLogger;

fn init_logging() {
    if log::set_logger(&LOGGER).is_ok() {
        log::set_max_level(log::LevelFilter::Trace);
    }
}

struct StreamReader {
    stream: WetwareInputStream,
    buffer: Vec<u8>,
    offset: usize,
}

impl StreamReader {
    fn new(stream: WetwareInputStream) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
            offset: 0,
        }
    }
}

impl StreamReader {
    fn pollable(&self) -> WetwarePollable {
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
            Err(WetwareStreamError::Closed) => std::task::Poll::Ready(Ok(0)),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("stream read error: {:?}", err),
            ))),
        }
    }
}

struct StreamWriter {
    stream: WetwareOutputStream,
}

impl StreamWriter {
    fn new(stream: WetwareOutputStream) -> Self {
        Self { stream }
    }

    fn pollable(&self) -> WetwarePollable {
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
                // Only write up to the budget
                let to_write = buf.len().min(budget as usize);
                match self.stream.write(&buf[..to_write]) {
                    Ok(_written) => std::task::Poll::Ready(Ok(to_write)),
                    Err(WetwareStreamError::Closed) => std::task::Poll::Ready(Ok(0)),
                    Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("stream write error: {:?}", err),
                    ))),
                }
            }
            Err(WetwareStreamError::Closed) => std::task::Poll::Ready(Ok(0)),
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
            Err(WetwareStreamError::Closed) => std::task::Poll::Ready(Ok(())),
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
            Err(WetwareStreamError::Closed) => std::task::Poll::Ready(Ok(())),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("stream close error: {:?}", err),
            ))),
        }
    }
}

struct Pid0;

impl Guest for Pid0 {
    fn run() -> Result<(), ()> {
        run_impl();
        Ok(())
    }
}

fn run_impl() {
    init_logging();
    log::trace!("pid0: start");
    const CHILD_WASM: &[u8] =
        include_bytes!("../../child-echo/target/wasm32-wasip2/release/child_echo.wasm");

    // Create connection and get streams from wetware interface
    let connection = create_connection();
    let input_stream = connection.get_input_stream();
    let output_stream = connection.get_output_stream();
    log::trace!("pid0: got wetware streams");

    let reader = StreamReader::new(input_stream);
    let writer = StreamWriter::new(output_stream);
    let reader_pollable = reader.pollable();
    let writer_pollable = writer.pollable();

    let network = VatNetwork::new(reader, writer, Side::Client, Default::default());
    let mut rpc_system = RpcSystem::new(Box::new(network), None);
    let executor: peer_capnp::executor::Client = rpc_system.bootstrap(Side::Server);
    log::trace!("pid0: rpc bootstrapped");

    let mut request = executor.run_bytes_request();
    {
        let mut params = request.get();
        params.set_wasm(CHILD_WASM);
        params.reborrow().init_args(0);
        params.reborrow().init_env(0);
    }
    let mut response = request.send().promise;
    log::trace!("pid0: runBytes sent");
    let mut wait_promise = None;

    let waker = noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);

    loop {
        let mut made_progress = false;

        match rpc_system.poll_unpin(&mut cx) {
            std::task::Poll::Ready(Ok(())) | std::task::Poll::Ready(Err(_)) => {
                made_progress = true;
            }
            std::task::Poll::Pending => {}
        }

        if wait_promise.is_none() {
            match response.poll_unpin(&mut cx) {
                std::task::Poll::Ready(Ok(resp)) => {
                    let process = resp
                        .get()
                        .expect("missing executor response")
                        .get_process()
                        .expect("missing process");
                    wait_promise = Some(process.wait_request().send().promise);
                    log::trace!("pid0: got process");
                    made_progress = true;
                }
                std::task::Poll::Ready(Err(err)) => {
                    panic!("executor call failed: {err}");
                }
                std::task::Poll::Pending => {}
            }
        } else if let Some(wait) = wait_promise.as_mut() {
            match wait.poll_unpin(&mut cx) {
                std::task::Poll::Ready(Ok(_)) => {
                    log::trace!("pid0: child exited");
                    // Leak Cap'n Proto resources to avoid "resource has children" error
                    // This is a workaround for complex resource cleanup dependencies
                    std::mem::forget(reader_pollable);
                    std::mem::forget(writer_pollable);
                    std::mem::forget(wait_promise);
                    std::mem::forget(response);
                    std::mem::forget(executor);
                    std::mem::forget(rpc_system);
                    log::trace!("pid0: cleanup complete");
                    break;
                }
                std::task::Poll::Ready(Err(err)) => {
                    panic!("wait failed: {err}");
                }
                std::task::Poll::Pending => {}
            }
        }

        if !made_progress {
            // Note: We can't use wasip2::io::poll::poll with wetware pollables
            // since they are different resource types. For now, just spin.
            // The host's channel-based streams should respond quickly.
            std::hint::spin_loop();
        }
    }
}

wasip2::cli::command::export!(Pid0);
