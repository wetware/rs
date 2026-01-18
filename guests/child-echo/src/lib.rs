#![feature(wasip2)]

use capnp_rpc::rpc_twoparty_capnp::Side;
use capnp_rpc::twoparty::VatNetwork;
use capnp_rpc::RpcSystem;
use futures::future::FutureExt;
use futures::task::noop_waker;
use wasip2::cli::stderr::get_stderr;
use wasip2::cli::stdin::get_stdin;
use wasip2::cli::stdout::get_stdout;
use wasip2::io::poll;
use wasip2::io::streams::{InputStream, OutputStream, StreamError};

#[allow(dead_code)]
mod peer_capnp {
    include!(concat!(
        env!("OUT_DIR"),
        "/Users/mikel/Code/github.com/wetware/rs/capnp/peer_capnp.rs"
    ));
}

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
    stream: InputStream,
    buffer: Vec<u8>,
    offset: usize,
}

impl StreamReader {
    fn new(stream: InputStream) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
            offset: 0,
        }
    }

    fn pollable(&self) -> wasip2::io::poll::Pollable {
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
            Err(StreamError::Closed) => std::task::Poll::Ready(Ok(0)),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("stream read error: {:?}", err),
            ))),
        }
    }
}

struct StreamWriter {
    stream: OutputStream,
}

impl StreamWriter {
    fn new(stream: OutputStream) -> Self {
        Self { stream }
    }

    fn pollable(&self) -> wasip2::io::poll::Pollable {
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
                    Ok(()) => std::task::Poll::Ready(Ok(to_write)),
                    Err(StreamError::Closed) => std::task::Poll::Ready(Ok(0)),
                    Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("stream write error: {:?}", err),
                    ))),
                }
            }
            Err(StreamError::Closed) => std::task::Poll::Ready(Ok(0)),
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
            Err(StreamError::Closed) => std::task::Poll::Ready(Ok(())),
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
            Err(StreamError::Closed) => std::task::Poll::Ready(Ok(())),
            Err(err) => std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("stream close error: {:?}", err),
            ))),
        }
    }
}

#[no_mangle]
pub extern "C" fn _start() {
    init_logging();
    log::trace!("child-echo: start");

    // Get stdin/stdout for RPC
    let stdin = get_stdin();
    let stdout = get_stdout();

    let reader = StreamReader::new(stdin);
    let writer = StreamWriter::new(stdout);
    let reader_pollable = reader.pollable();
    let writer_pollable = writer.pollable();

    // Bootstrap RPC connection
    let network = VatNetwork::new(reader, writer, Side::Client, Default::default());
    let mut rpc_system = RpcSystem::new(Box::new(network), None);
    let executor: peer_capnp::executor::Client = rpc_system.bootstrap(Side::Server);
    log::trace!("child-echo: rpc bootstrapped");

    // Make TWO concurrent echo calls
    let mut request1 = executor.echo_request();
    request1.get().set_message("Hello from call 1");
    let mut promise1 = request1.send().promise;

    let mut request2 = executor.echo_request();
    request2.get().set_message("Hello from call 2");
    let mut promise2 = request2.send().promise;

    log::trace!("child-echo: sent both echo requests");

    // Poll until both responses received
    let waker = noop_waker();
    let mut cx = std::task::Context::from_waker(&waker);
    let mut response1_done = false;
    let mut response2_done = false;

    loop {
        let mut made_progress = false;

        // Poll RPC system
        match rpc_system.poll_unpin(&mut cx) {
            std::task::Poll::Ready(Ok(())) => {
                log::trace!("child-echo: rpc_system Ready(Ok)");
                made_progress = true;
            }
            std::task::Poll::Ready(Err(e)) => {
                log::error!("child-echo: rpc_system Ready(Err): {:?}", e);
                made_progress = true;
            }
            std::task::Poll::Pending => {}
        }

        // Poll response 1
        if !response1_done {
            log::trace!("child-echo: polling promise1");
            match promise1.poll_unpin(&mut cx) {
                std::task::Poll::Ready(Ok(resp)) => {
                    log::trace!("child-echo: promise1 Ready(Ok)");
                    match resp.get() {
                        Ok(reader) => match reader.get_response() {
                            Ok(text) => match text.to_str() {
                                Ok(s) => {
                                    log::trace!("child-echo: call 1 response: {}", s);
                                    response1_done = true;
                                    made_progress = true;
                                }
                                Err(e) => {
                                    log::error!("child-echo: call 1 to_str failed: {:?}", e);
                                    return;
                                }
                            },
                            Err(e) => {
                                log::error!("child-echo: call 1 get_response failed: {:?}", e);
                                return;
                            }
                        },
                        Err(e) => {
                            log::error!("child-echo: call 1 resp.get() failed: {:?}", e);
                            return;
                        }
                    }
                }
                std::task::Poll::Ready(Err(err)) => {
                    log::error!("child-echo: call 1 failed: {}", err);
                    return;
                }
                std::task::Poll::Pending => {
                    log::trace!("child-echo: promise1 Pending");
                }
            }
        }

        // Poll response 2
        if !response2_done {
            log::trace!("child-echo: polling promise2");
            match promise2.poll_unpin(&mut cx) {
                std::task::Poll::Ready(Ok(resp)) => {
                    log::trace!("child-echo: promise2 Ready(Ok)");
                    match resp.get() {
                        Ok(reader) => match reader.get_response() {
                            Ok(text) => match text.to_str() {
                                Ok(s) => {
                                    log::trace!("child-echo: call 2 response: {}", s);
                                    response2_done = true;
                                    made_progress = true;
                                }
                                Err(e) => {
                                    log::error!("child-echo: call 2 to_str failed: {:?}", e);
                                    return;
                                }
                            },
                            Err(e) => {
                                log::error!("child-echo: call 2 get_response failed: {:?}", e);
                                return;
                            }
                        },
                        Err(e) => {
                            log::error!("child-echo: call 2 resp.get() failed: {:?}", e);
                            return;
                        }
                    }
                }
                std::task::Poll::Ready(Err(err)) => {
                    log::error!("child-echo: call 2 failed: {}", err);
                    return;
                }
                std::task::Poll::Pending => {
                    log::trace!("child-echo: promise2 Pending");
                }
            }
        }

        // Exit when both done
        if response1_done && response2_done {
            log::trace!("child-echo: both calls complete, exiting");
            break;
        }

        if !made_progress {
            let _ = poll::poll(&[&reader_pollable, &writer_pollable]);
        }
    }

    // Cleanup - forget resources to avoid "resource has children" errors
    std::mem::forget(promise1);
    std::mem::forget(promise2);
    std::mem::forget(rpc_system);

    log::trace!("child-echo: cleanup complete");
}
