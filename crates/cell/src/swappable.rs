//! Swappable stdio streams for long-lived WASI processes.
//!
//! WASI binds file descriptors at process instantiation — you can't re-wire
//! fd 0/1 in a running process. SwappableReader/Writer solve this by providing
//! an AsyncRead/AsyncWrite impl that delegates to a swappable inner stream.
//! The host swaps the inner DuplexStream between requests while the guest
//! reads/writes normally through fd 0/1.
//!
//! Overhead: ~1.6us per swap (vs ~1-5ms for process respawn).
//!
//! ```text
//! Host side                          Guest side (WASI)
//! ┌─────────────────┐                ┌──────────────┐
//! │ SwapHandle       │──swap()──►    │              │
//! │ (holds Arc)      │               │ SwappableReader ──► fd 0 (stdin)
//! │                  │               │              │
//! │ DuplexStream₁   │◄──────────────│              │
//! │ DuplexStream₂   │  (current)    │ SwappableWriter ──► fd 1 (stdout)
//! │ DuplexStream₃   │               │              │
//! └─────────────────┘                └──────────────┘
//! ```
//!
//! Satisfies `AsyncRead + Send + Sync + Unpin + 'static` — the trait bounds
//! required by Wasmtime's WasiCtxBuilder.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{self, AsyncRead, AsyncWrite, ReadBuf};

// =========================================================================
// SwappableReader
// =========================================================================

/// An AsyncRead wrapper that delegates to a swappable inner stream.
///
/// Give this to `ProcBuilder::with_stdin()`. Between requests, call
/// `SwapHandle::swap()` to replace the inner DuplexStream.
pub struct SwappableReader {
    inner: Arc<Mutex<Option<Box<dyn AsyncRead + Send + Sync + Unpin + 'static>>>>,
}

/// Host-side handle for swapping the inner read stream.
#[derive(Clone)]
pub struct ReadSwapHandle {
    inner: Arc<Mutex<Option<Box<dyn AsyncRead + Send + Sync + Unpin + 'static>>>>,
}

/// Create a paired (reader, handle). Give the reader to WasiCtxBuilder,
/// keep the handle for swapping between requests.
pub fn swappable_reader() -> (SwappableReader, ReadSwapHandle) {
    let inner: Arc<Mutex<Option<Box<dyn AsyncRead + Send + Sync + Unpin + 'static>>>> =
        Arc::new(Mutex::new(None));
    (
        SwappableReader {
            inner: inner.clone(),
        },
        ReadSwapHandle { inner },
    )
}

impl ReadSwapHandle {
    /// Replace the inner stream. Returns the previous stream (if any).
    /// Call this between requests to wire in a fresh DuplexStream.
    pub fn swap(
        &self,
        new_stream: impl AsyncRead + Send + Sync + Unpin + 'static,
    ) -> Option<Box<dyn AsyncRead + Send + Sync + Unpin + 'static>> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(Box::new(new_stream))
    }

    /// Remove the inner stream. Subsequent reads return EOF.
    pub fn clear(&self) {
        *self.inner.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

impl AsyncRead for SwappableReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(reader) => Pin::new(reader.as_mut()).poll_read(cx, buf),
            None => Poll::Ready(Ok(())), // EOF
        }
    }
}

// =========================================================================
// SwappableWriter
// =========================================================================

/// An AsyncWrite wrapper that delegates to a swappable inner stream.
///
/// Give this to `ProcBuilder::with_stdout()`. Between requests, call
/// `WriteSwapHandle::swap()` to replace the inner DuplexStream.
pub struct SwappableWriter {
    inner: Arc<Mutex<Option<Box<dyn AsyncWrite + Send + Sync + Unpin + 'static>>>>,
}

/// Host-side handle for swapping the inner write stream.
#[derive(Clone)]
pub struct WriteSwapHandle {
    inner: Arc<Mutex<Option<Box<dyn AsyncWrite + Send + Sync + Unpin + 'static>>>>,
}

/// Create a paired (writer, handle). Give the writer to WasiCtxBuilder,
/// keep the handle for swapping between requests.
pub fn swappable_writer() -> (SwappableWriter, WriteSwapHandle) {
    let inner: Arc<Mutex<Option<Box<dyn AsyncWrite + Send + Sync + Unpin + 'static>>>> =
        Arc::new(Mutex::new(None));
    (
        SwappableWriter {
            inner: inner.clone(),
        },
        WriteSwapHandle { inner },
    )
}

impl WriteSwapHandle {
    /// Replace the inner stream. Returns the previous stream (if any).
    pub fn swap(
        &self,
        new_stream: impl AsyncWrite + Send + Sync + Unpin + 'static,
    ) -> Option<Box<dyn AsyncWrite + Send + Sync + Unpin + 'static>> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(Box::new(new_stream))
    }

    /// Remove the inner stream. Subsequent writes return BrokenPipe.
    pub fn clear(&self) {
        *self.inner.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }
}

impl AsyncWrite for SwappableWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(writer) => Pin::new(writer.as_mut()).poll_write(cx, buf),
            None => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "no inner writer — stream not connected",
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(writer) => Pin::new(writer.as_mut()).poll_flush(cx),
            None => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut guard = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_mut() {
            Some(writer) => Pin::new(writer.as_mut()).poll_shutdown(cx),
            None => Poll::Ready(Ok(())),
        }
    }
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn reader_empty_returns_eof() {
        let (mut reader, _handle) = swappable_reader();
        let mut buf = vec![0u8; 10];
        let n = reader.read(&mut buf).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn reader_swap_and_read() {
        let (mut reader, handle) = swappable_reader();

        handle.swap(std::io::Cursor::new(b"hello".to_vec()));
        let mut result = String::new();
        reader.read_to_string(&mut result).await.unwrap();
        assert_eq!(result, "hello");
    }

    #[tokio::test]
    async fn reader_multiple_swaps() {
        let (mut reader, handle) = swappable_reader();

        for i in 0..3 {
            let data = format!("request {i}");
            handle.swap(std::io::Cursor::new(data.clone().into_bytes()));
            let mut result = String::new();
            reader.read_to_string(&mut result).await.unwrap();
            assert_eq!(result, data);
        }
    }

    #[tokio::test]
    async fn reader_clear_returns_eof() {
        let (mut reader, handle) = swappable_reader();

        handle.swap(std::io::Cursor::new(b"data".to_vec()));
        handle.clear();
        let mut buf = vec![0u8; 10];
        let n = reader.read(&mut buf).await.unwrap();
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn writer_empty_returns_broken_pipe() {
        let (mut writer, _handle) = swappable_writer();
        let err = writer.write(b"data").await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[tokio::test]
    async fn writer_swap_and_write() {
        let (mut writer, handle) = swappable_writer();

        let (mut host_read, guest_write) = io::duplex(1024);
        handle.swap(guest_write);

        writer.write_all(b"response").await.unwrap();
        writer.flush().await.unwrap();

        // Replace writer to close the guest_write side
        handle.swap(io::sink());

        let mut result = String::new();
        host_read.read_to_string(&mut result).await.unwrap();
        assert_eq!(result, "response");
    }

    #[tokio::test]
    async fn duplex_roundtrip() {
        let (mut reader, read_handle) = swappable_reader();
        let (mut writer, write_handle) = swappable_writer();

        for i in 0..3 {
            // Create fresh duplex pair for this "request"
            let (mut host_write, guest_read) = io::duplex(1024);
            let (guest_write, mut host_read) = io::duplex(1024);

            read_handle.swap(guest_read);
            write_handle.swap(guest_write);

            // Host writes request
            let request = format!("request {i}");
            host_write.write_all(request.as_bytes()).await.unwrap();
            drop(host_write); // EOF

            // "Guest" reads via swappable reader
            let mut received = String::new();
            reader.read_to_string(&mut received).await.unwrap();
            assert_eq!(received, request);

            // "Guest" writes response via swappable writer
            let response = format!("response {i}");
            writer.write_all(response.as_bytes()).await.unwrap();
            writer.flush().await.unwrap();
            write_handle.swap(io::sink()); // close guest side

            // Host reads response
            let mut result = String::new();
            host_read.read_to_string(&mut result).await.unwrap();
            assert_eq!(result, response);
        }
    }

    #[tokio::test]
    async fn swap_handle_is_clone() {
        let (_reader, handle) = swappable_reader();
        let handle2 = handle.clone();
        handle2.swap(std::io::Cursor::new(b"from clone".to_vec()));
        // Both handles point to the same inner
        handle.clear();
    }
}
