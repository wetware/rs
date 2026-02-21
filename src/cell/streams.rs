//! Stream adapters for bridging tokio channels to WASI streams.
//!
//! This module provides adapters that convert tokio channels into AsyncRead/AsyncWrite
//! implementations, which can then be wrapped by wasmtime-wasi's stream types.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

/// An adapter that converts a tokio mpsc receiver into an AsyncRead implementation.
///
/// This allows data from a channel to be read asynchronously, suitable for use
/// as a WASI input stream.
pub struct Reader {
    receiver: mpsc::UnboundedReceiver<Vec<u8>>,
    buffer: Vec<u8>,
    buffer_pos: usize,
}

impl Reader {
    /// Create a new ChannelReader from a receiver.
    pub fn new(receiver: mpsc::UnboundedReceiver<Vec<u8>>) -> Self {
        Self {
            receiver,
            buffer: Vec::new(),
            buffer_pos: 0,
        }
    }
}

impl AsyncRead for Reader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        // First, try to read from our internal buffer if we have data
        if self.buffer_pos < self.buffer.len() {
            let available = &self.buffer[self.buffer_pos..];
            let to_copy = available.len().min(buf.remaining());
            buf.put_slice(&available[..to_copy]);
            self.buffer_pos += to_copy;

            // If we've consumed the entire buffer, clear it
            if self.buffer_pos >= self.buffer.len() {
                self.buffer.clear();
                self.buffer_pos = 0;
            }

            return Poll::Ready(Ok(()));
        }

        // Try to receive more data from the channel
        match self.receiver.poll_recv(cx) {
            Poll::Ready(Some(data)) => {
                if data.is_empty() {
                    // Empty vec might indicate EOF, but we'll treat it as no data
                    // and continue polling
                    return Poll::Pending;
                }
                self.buffer = data;
                self.buffer_pos = 0;

                // Now read from the new buffer
                let available = &self.buffer[self.buffer_pos..];
                let to_copy = available.len().min(buf.remaining());
                buf.put_slice(&available[..to_copy]);
                self.buffer_pos += to_copy;

                Poll::Ready(Ok(()))
            }
            Poll::Ready(None) => {
                // Channel closed - EOF
                Poll::Ready(Ok(()))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

/// An adapter that converts a tokio mpsc sender into an AsyncWrite implementation.
///
/// This allows data to be written asynchronously to a channel, suitable for use
/// as a WASI output stream.
pub struct Writer {
    sender: mpsc::UnboundedSender<Vec<u8>>,
}

impl Writer {
    /// Create a new ChannelWriter from a sender.
    pub fn new(sender: mpsc::UnboundedSender<Vec<u8>>) -> Self {
        Self { sender }
    }
}

impl AsyncWrite for Writer {
    fn poll_write(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if buf.is_empty() {
            return Poll::Ready(Ok(0));
        }

        // For unbounded channels, we can always send immediately
        // We send the data as a single chunk
        let data = buf.to_vec();
        match self.sender.send(data) {
            Ok(()) => Poll::Ready(Ok(buf.len())),
            Err(_) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "channel receiver dropped",
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Unbounded channels don't need flushing
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        // Dropping the sender will signal shutdown to the receiver
        // We don't need to do anything special here
        Poll::Ready(Ok(()))
    }
}

/// Channel pair type for bidirectional host-guest communication.
pub type ChannelPair = (
    mpsc::UnboundedSender<Vec<u8>>,
    mpsc::UnboundedReceiver<Vec<u8>>,
    mpsc::UnboundedSender<Vec<u8>>,
    mpsc::UnboundedReceiver<Vec<u8>>,
);

/// Create a bidirectional channel pair for host-guest communication.
///
/// Returns (host_sender, host_receiver, guest_sender, guest_receiver) where:
/// - host_sender: Host can write to this, guest reads from guest_receiver
/// - host_receiver: Host reads from this, guest writes to guest_sender
pub fn create_channel_pair() -> ChannelPair {
    let (host_to_guest_tx, host_to_guest_rx) = mpsc::unbounded_channel();
    let (guest_to_host_tx, guest_to_host_rx) = mpsc::unbounded_channel();

    (
        host_to_guest_tx,
        host_to_guest_rx,
        guest_to_host_tx,
        guest_to_host_rx,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn test_channel_reader() {
        let (tx, rx) = mpsc::unbounded_channel();
        let mut reader = Reader::new(rx);

        // Send some data
        tx.send(b"hello".to_vec()).unwrap();
        tx.send(b"world".to_vec()).unwrap();

        // Read it back
        let mut buf = vec![0u8; 10];
        let n = reader.read(&mut buf).await.unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..5], b"hello");

        let n = reader.read(&mut buf).await.unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..5], b"world");
    }

    #[tokio::test]
    async fn test_channel_writer() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let mut writer = Writer::new(tx);

        // Write some data
        writer.write_all(b"test").await.unwrap();
        writer.flush().await.unwrap();

        // Read it back
        let data = rx.recv().await.unwrap();
        assert_eq!(data, b"test");
    }
}
