//! Spike: SwappableReader + per-request spawn benchmark.
//!
//! Validates two things:
//! 1. Can we give Wasmtime a custom AsyncRead impl with a swappable inner?
//! 2. What's the latency difference between per-request spawn and stream swap?
//!
//! Run: cargo run --example swappable_stdio_spike

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Instant;
use tokio::io::{self, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

// =========================================================================
// SwappableReader — the core idea
// =========================================================================

/// An AsyncRead wrapper that delegates to a swappable inner stream.
/// The inner stream can be replaced between requests. Reads from whatever
/// is currently inside.
///
/// Uses std::sync::Mutex because poll_read is synchronous (returns Poll,
/// can't await). Send + Sync + Unpin + 'static as required by Wasmtime's
/// WasiCtxBuilder.
struct SwappableReader {
    inner: Arc<Mutex<Option<Box<dyn AsyncRead + Send + Sync + Unpin + 'static>>>>,
}

impl SwappableReader {
    fn new() -> (Self, SwapHandle) {
        let inner: Arc<Mutex<Option<Box<dyn AsyncRead + Send + Sync + Unpin + 'static>>>> =
            Arc::new(Mutex::new(None));
        let handle = SwapHandle {
            inner: inner.clone(),
        };
        (Self { inner }, handle)
    }
}

/// Handle held by the host to swap the inner stream between requests.
#[derive(Clone)]
struct SwapHandle {
    inner: Arc<Mutex<Option<Box<dyn AsyncRead + Send + Sync + Unpin + 'static>>>>,
}

impl SwapHandle {
    /// Replace the inner stream. Returns the old stream (if any).
    fn swap(
        &self,
        new_stream: Box<dyn AsyncRead + Send + Sync + Unpin + 'static>,
    ) -> Option<Box<dyn AsyncRead + Send + Sync + Unpin + 'static>> {
        let mut guard = self.inner.lock().unwrap();
        guard.replace(new_stream)
    }

    /// Remove the inner stream (causes reads to return EOF).
    fn clear(&self) {
        let mut guard = self.inner.lock().unwrap();
        *guard = None;
    }
}

impl AsyncRead for SwappableReader {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut guard = self.inner.lock().unwrap();
        match guard.as_mut() {
            Some(reader) => {
                // Delegate to the inner reader.
                // SAFETY: We hold the mutex, inner is pinned-in-place inside the Box.
                let pinned = Pin::new(reader.as_mut());
                pinned.poll_read(cx, buf)
            }
            None => {
                // No inner stream — return EOF (0 bytes read).
                Poll::Ready(Ok(()))
            }
        }
    }
}

// Same pattern for stdout
struct SwappableWriter {
    inner: Arc<Mutex<Option<Box<dyn AsyncWrite + Send + Sync + Unpin + 'static>>>>,
}

impl SwappableWriter {
    fn new() -> (Self, WriteSwapHandle) {
        let inner: Arc<Mutex<Option<Box<dyn AsyncWrite + Send + Sync + Unpin + 'static>>>> =
            Arc::new(Mutex::new(None));
        let handle = WriteSwapHandle {
            inner: inner.clone(),
        };
        (Self { inner }, handle)
    }
}

#[derive(Clone)]
struct WriteSwapHandle {
    inner: Arc<Mutex<Option<Box<dyn AsyncWrite + Send + Sync + Unpin + 'static>>>>,
}

impl WriteSwapHandle {
    fn swap(
        &self,
        new_stream: Box<dyn AsyncWrite + Send + Sync + Unpin + 'static>,
    ) -> Option<Box<dyn AsyncWrite + Send + Sync + Unpin + 'static>> {
        let mut guard = self.inner.lock().unwrap();
        guard.replace(new_stream)
    }
}

impl AsyncWrite for SwappableWriter {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let mut guard = self.inner.lock().unwrap();
        match guard.as_mut() {
            Some(writer) => Pin::new(writer.as_mut()).poll_write(cx, buf),
            None => Poll::Ready(Ok(0)), // no writer — discard
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut guard = self.inner.lock().unwrap();
        match guard.as_mut() {
            Some(writer) => Pin::new(writer.as_mut()).poll_flush(cx),
            None => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let mut guard = self.inner.lock().unwrap();
        match guard.as_mut() {
            Some(writer) => Pin::new(writer.as_mut()).poll_shutdown(cx),
            None => Poll::Ready(Ok(())),
        }
    }
}

// =========================================================================
// Test: basic swap works
// =========================================================================

async fn test_basic_swap() {
    println!("--- Test: basic swap ---");

    let (reader, handle) = SwappableReader::new();
    let mut reader = reader;

    // No inner stream — should return EOF
    let mut buf = vec![0u8; 10];
    let n = reader.read(&mut buf).await.unwrap();
    assert_eq!(n, 0, "empty reader should return EOF");
    println!("  [OK] Empty reader returns EOF");

    // Swap in a stream with data
    let data = std::io::Cursor::new(b"hello world".to_vec());
    handle.swap(Box::new(data));

    let mut result = String::new();
    reader.read_to_string(&mut result).await.unwrap();
    assert_eq!(result, "hello world");
    println!("  [OK] First swap: read 'hello world'");

    // Swap in a new stream
    let data2 = std::io::Cursor::new(b"second request".to_vec());
    handle.swap(Box::new(data2));

    let mut result2 = String::new();
    reader.read_to_string(&mut result2).await.unwrap();
    assert_eq!(result2, "second request");
    println!("  [OK] Second swap: read 'second request'");

    // Clear — should return EOF again
    handle.clear();
    let mut buf2 = vec![0u8; 10];
    let n = reader.read(&mut buf2).await.unwrap();
    assert_eq!(n, 0);
    println!("  [OK] Clear returns EOF");
}

// =========================================================================
// Test: DuplexStream swap (simulates real WASI usage)
// =========================================================================

async fn test_duplex_swap() {
    println!("\n--- Test: DuplexStream swap (simulates WASI stdio) ---");

    let (reader, read_handle) = SwappableReader::new();
    let (writer, write_handle) = SwappableWriter::new();
    let mut reader = reader;
    let mut writer = writer;

    // Simulate 3 serial requests
    for i in 0..3 {
        // Create fresh DuplexStream pair for this request
        let (mut host_write, guest_read) = io::duplex(1024);
        let (guest_write, mut host_read) = io::duplex(1024);

        // Swap in the guest sides
        read_handle.swap(Box::new(guest_read));
        write_handle.swap(Box::new(guest_write));

        // Host writes request
        let request = format!("request {i}");
        host_write.write_all(request.as_bytes()).await.unwrap();
        drop(host_write); // signal EOF

        // "Guest" reads stdin (via SwappableReader)
        let mut received = String::new();
        reader.read_to_string(&mut received).await.unwrap();
        assert_eq!(received, request);

        // "Guest" writes response to stdout (via SwappableWriter)
        let response = format!("response {i}");
        writer.write_all(response.as_bytes()).await.unwrap();
        // Flush the writer to ensure data goes through
        writer.flush().await.unwrap();
        // Drop the guest write side to signal EOF
        write_handle.swap(Box::new(io::sink())); // replace with sink to close

        // Host reads response
        let mut result = String::new();
        host_read.read_to_string(&mut result).await.unwrap();
        assert_eq!(result, response);

        println!("  [OK] Request {i}: '{request}' → '{response}'");
    }
}

// =========================================================================
// Benchmark: swap latency
// =========================================================================

async fn benchmark_swap_latency() {
    println!("\n--- Benchmark: DuplexStream swap overhead ---");

    let iterations = 1000;
    let (reader, read_handle) = SwappableReader::new();
    let (_writer, write_handle) = SwappableWriter::new();

    // Measure swap + read cycle
    let start = Instant::now();
    for _ in 0..iterations {
        let (mut host_write, guest_read) = io::duplex(1024);
        let (_guest_write, _host_read) = io::duplex(1024);

        read_handle.swap(Box::new(guest_read));
        write_handle.swap(Box::new(_guest_write));

        host_write.write_all(b"(+ 1 2)").await.unwrap();
        drop(host_write);

        // We can't read from `reader` here because it was moved.
        // Just measure swap + duplex creation overhead.
    }
    let elapsed = start.elapsed();
    let _ = reader; // keep alive

    println!(
        "  {iterations} swap cycles: {:.2}ms total, {:.2}us per swap",
        elapsed.as_secs_f64() * 1000.0,
        elapsed.as_secs_f64() * 1_000_000.0 / iterations as f64
    );

    // Compare: just DuplexStream creation (no swap)
    let start2 = Instant::now();
    for _ in 0..iterations {
        let (_a, _b) = io::duplex(1024);
        let (_c, _d) = io::duplex(1024);
    }
    let elapsed2 = start2.elapsed();

    println!(
        "  {iterations} duplex-only: {:.2}ms total, {:.2}us per pair",
        elapsed2.as_secs_f64() * 1000.0,
        elapsed2.as_secs_f64() * 1_000_000.0 / iterations as f64
    );

    let swap_overhead_us =
        (elapsed.as_secs_f64() - elapsed2.as_secs_f64()) * 1_000_000.0 / iterations as f64;
    println!("  Swap overhead: ~{:.2}us per request", swap_overhead_us);
}

// =========================================================================
// Main
// =========================================================================

#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("SwappableStdin Spike");
    println!("====================\n");

    test_basic_swap().await;
    test_duplex_swap().await;
    benchmark_swap_latency().await;

    println!("\n====================");
    println!("RESULT: SwappableReader works. DuplexStream swap is viable.");
    println!("Next: integrate with Wasmtime WasiCtxBuilder to validate end-to-end.");
}
