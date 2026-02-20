#![feature(wasip2)]

use futures::io::{AsyncRead, AsyncWrite};
use futures::task::{noop_waker, Context};
use futures::Future;
use std::pin::Pin;
use std::task::Poll;
use wasip2::cli::stdin::get_stdin;
use wasip2::cli::stdout::get_stdout;
use wasip2::io::poll;
use wasip2::io::streams::{InputStream, OutputStream};

/// Wrapper around WASI Preview 2 stdin stream for async I/O
/// This enables implementing AsyncRead/AsyncWrite traits needed for Cap'n Proto transports.
/// Based on the approach from: https://mikel.xyz/posts/capnp-in-wasm/
struct AsyncStdin {
    stream: InputStream,
}

impl AsyncStdin {
    fn new(stream: InputStream) -> Self {
        Self { stream }
    }
}

impl AsyncRead for AsyncStdin {
    fn poll_read(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        // Non-blocking read: try to read available bytes.
        let len = buf.len() as u64;
        let bytes = match self.stream.read(len) {
            Ok(bytes) => bytes,
            Err(err) => {
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("stream read error: {:?}", err),
                )));
            }
        };

        let n = bytes.len();
        if n == 0 {
            // No data ready yet. Use WASI polling to wait for stream readiness.
            // This WASI call allows wasmtime to yield back to the host.
            let pollable = self.stream.subscribe();
            // poll will block the guest thread, but wasmtime can yield
            // at the WASI function call boundary, allowing the host to remain responsive.
            let _ = poll::poll(&[&pollable]);
            // After polling returns, the stream should be ready.
            // Try reading again immediately rather than returning Pending.
            let len = buf.len() as u64;
            let bytes = match self.stream.read(len) {
                Ok(bytes) => bytes,
                Err(err) => {
                    return Poll::Ready(Err(std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("stream read error: {:?}", err),
                    )));
                }
            };
            let n = bytes.len();
            if n == 0 {
                // EOF: stream has closed, no more data will be available.
                // Return Ready(Ok(0)) to signal EOF to the caller.
                return Poll::Ready(Ok(0));
            }
            buf[..n].copy_from_slice(&bytes);
            return Poll::Ready(Ok(n));
        }

        buf[..n].copy_from_slice(&bytes);
        Poll::Ready(Ok(n))
    }
}

/// Wrapper around WASI Preview 2 stdout stream for async I/O
/// The write implementation relies on the write immediately completing; either because
/// the other end read it or the data was buffered. Our specific case has buffered pipes
/// backed by a multi-threaded host on the other side.
struct AsyncStdout {
    stream: OutputStream,
}

impl AsyncStdout {
    fn new(stream: OutputStream) -> Self {
        Self { stream }
    }
}

impl AsyncWrite for AsyncStdout {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }
        Poll::Ready(
            self.stream
                .blocking_write_and_flush(buf)
                .map(|_| buf.len())
                .map_err(|err| {
                    std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("stream write error: {:?}", err),
                    )
                }),
        )
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // Ensure any pending output is committed before proceeding.
        Poll::Ready(self.stream.blocking_flush().map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("stream flush error: {:?}", err),
            )
        }))
    }

    fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // Ensure all pending output is committed before close.
        Poll::Ready(self.stream.blocking_flush().map_err(|err| {
            std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("stream close error: {:?}", err),
            )
        }))
    }
}

/// The default kernel pipes stdin to stdout asynchronously using WASI Preview 2 async streams.
/// This implementation uses WASI Preview 2's native async I/O APIs, enabling support for
/// async transports like Cap'n Proto. Based on: https://mikel.xyz/posts/capnp-in-wasm/
///
/// This implementation uses manual future polling with WASI polling for non-blocking execution.
/// When `poll_read` returns `Poll::Pending`, it uses `wasi:io/poll::poll` to wait for stream
/// readiness. This WASI call allows wasmtime to yield back to the host's Tokio runtime at the
/// WASI function call boundary, preventing OS thread blocking while maintaining AsyncRead/AsyncWrite
/// trait implementations for Cap'n Proto compatibility.
#[no_mangle]
pub extern "C" fn _start() {
    use futures::io::AsyncReadExt;
    use futures::io::AsyncWriteExt;

    // Get stdin and stdout as WASI Preview 2 async streams
    let stdin_stream = get_stdin();
    let stdout_stream = get_stdout();

    let mut stdin = AsyncStdin::new(stdin_stream);
    let mut stdout = AsyncStdout::new(stdout_stream);
    let mut buffer = [0u8; 8192];

    // Create the async echo future
    let echo_future = async move {
        loop {
            match stdin.read(&mut buffer).await {
                Ok(0) => break, // EOF
                Ok(n) => {
                    let _ = stdout.write_all(&buffer[..n]).await;
                    let _ = stdout.flush().await;
                }
                Err(_) => break, // Error
            }
        }
    };

    // Use manual future polling with noop waker.
    // Actual waiting happens in WASI polling calls within poll_read,
    // which allows wasmtime to yield at WASI function call boundaries.
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut pinned = std::pin::pin!(echo_future);

    loop {
        match pinned.as_mut().poll(&mut cx) {
            Poll::Ready(()) => break,
            Poll::Pending => {
                // When Pending, the WASI polling in poll_read handles actual waiting.
                // Continue polling the future, which will call poll_read again.
                // The poll_read implementation will use WASI poll to wait for readiness.
            }
        }
    }
}
