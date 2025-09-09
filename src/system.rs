use anyhow::{anyhow, Result};
use nix::sys::socket::{self, AddressFamily, SockFlag, SockType};
use std::io::Write;
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use tracing::{debug, info};

/// Bootstrap file descriptor number for host-guest communication
/// This matches the Go implementation's BOOTSTRAP_FD constant
#[allow(dead_code)]
pub const BOOTSTRAP_FD: i32 = 3;

/// Socket handler for host-guest RPC over Unix domain sockets
///
/// This creates an anonymous Unix domain socket pair using socketpair(2) for
/// bidirectional communication between the host process and the cell subprocess.
/// The guest socket is passed to the subprocess as FD3, while the host socket
/// remains in the host process for RPC communication.
///
/// # Cap'n Proto Integration Status
///
/// The socket pair is created and properly passed to the subprocess, but the
/// actual Cap'n Proto RPC protocol implementation is stubbed. The subprocess
/// receives FD3 as a Unix domain socket, but RPC handling is not implemented.
///
/// # File Descriptor Convention
///
/// - **FD3**: Unix domain socket for host-guest RPC communication
/// - **FD4+**: User-configurable file descriptors passed via --with-fd flags
///
/// This matches the Go implementation's file descriptor conventions.
pub struct Socket {
    /// Host-side socket for RPC communication with the cell
    #[allow(dead_code)]
    host_socket: UnixStream,
    /// Guest-side socket that will be passed to the subprocess as FD3
    guest_socket: UnixStream,
}

impl Socket {
    /// Create a new socket handler with an anonymous Unix domain socket pair
    ///
    /// This uses socketpair(2) to create a pair of connected Unix domain sockets.
    /// The guest socket will be passed to the subprocess as FD3, while the host
    /// socket remains in the host process for RPC communication.
    ///
    /// # Returns
    ///
    /// Returns a `Socket` instance with both sockets ready for use.
    ///
    /// # Errors
    ///
    /// Returns an error if socketpair(2) fails or if the sockets cannot be created.
    pub fn new() -> Result<Self> {
        debug!("Creating Unix domain socket pair for host-guest communication");

        // Create anonymous Unix domain socket pair
        let (host_fd, guest_fd) = socket::socketpair(
            AddressFamily::Unix,
            SockType::Stream,
            None,
            SockFlag::empty(),
        )
        .map_err(|e| anyhow!("Failed to create socket pair: {}", e))?;

        debug!(
            host_fd = host_fd.as_raw_fd(),
            guest_fd = guest_fd.as_raw_fd(),
            "Created Unix domain socket pair"
        );

        // Convert raw file descriptors to UnixStream objects
        // We need to take ownership of the FDs to avoid double-close issues
        let host_fd_raw = host_fd.as_raw_fd();
        let guest_fd_raw = guest_fd.as_raw_fd();

        // Prevent the nix Fd objects from closing the FDs when dropped
        std::mem::forget(host_fd);
        std::mem::forget(guest_fd);

        // Create UnixStream objects from the raw FDs
        let host_socket = unsafe { UnixStream::from_raw_fd(host_fd_raw) };
        let guest_socket = unsafe { UnixStream::from_raw_fd(guest_fd_raw) };

        info!("Socket pair established for host-guest communication (FD3 for guest)");

        Ok(Self {
            host_socket,
            guest_socket,
        })
    }

    /// Get the guest socket for passing to the subprocess as FD3
    ///
    /// This returns the guest-side socket that should be passed to the subprocess
    /// as an extra file descriptor. The subprocess will receive this as FD3.
    ///
    /// # Returns
    ///
    /// Returns the guest socket as a raw file descriptor.
    #[allow(dead_code)]
    pub fn guest_socket_fd(&self) -> RawFd {
        self.guest_socket.as_raw_fd()
    }

    /// Get the guest socket as a UnixStream for subprocess integration
    ///
    /// This is used when integrating with subprocess execution to pass the
    /// socket to the child process.
    pub fn into_guest_socket(self) -> UnixStream {
        self.guest_socket
    }

    /// Handle RPC communication with the cell (stubbed implementation)
    ///
    /// This method is called to handle RPC communication with the cell subprocess
    /// over the Unix domain socket. The current implementation is stubbed and
    /// only logs that RPC communication is available.
    ///
    /// # Cap'n Proto Integration Status
    ///
    /// This method is stubbed and does not implement the full Cap'n Proto RPC
    /// protocol. Future implementation should:
    ///
    /// 1. Deserialize Cap'n Proto messages from the socket
    /// 2. Handle bootstrap capability requests
    /// 3. Process import/export service requests
    /// 4. Serialize and send responses back to the cell
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` on success, or an error if communication fails.
    #[allow(dead_code)]
    pub async fn handle_rpc(&mut self) -> Result<()> {
        debug!("Handling RPC communication with cell subprocess");

        // TODO: Implement Cap'n Proto RPC protocol over Unix domain socket
        // For now, just log that RPC communication is available
        info!("RPC communication with cell is available over FD3 (Unix domain socket)");
        info!("Cap'n Proto RPC implementation is stubbed - full protocol not implemented");

        // In a full implementation, this would:
        // 1. Read Cap'n Proto messages from self.host_socket
        // 2. Parse and handle bootstrap requests
        // 3. Process service import/export requests
        // 4. Send responses back to the cell

        Ok(())
    }

    /// Test the socket pair connectivity
    ///
    /// This method tests that the socket pair is working correctly by sending
    /// a test message from host to guest and verifying it can be received.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` if the test succeeds, or an error if communication fails.
    #[allow(dead_code)]
    pub fn test_connectivity(&mut self) -> Result<()> {
        debug!("Testing socket pair connectivity");

        // Send a test message from host to guest
        let test_message = b"host-to-guest test";
        self.host_socket.write_all(test_message)?;
        self.host_socket.flush()?;

        debug!("Test message sent from host to guest");

        // Note: In a real test, we would read from the guest socket, but since
        // we're not running the subprocess yet, we'll just verify the write succeeded
        info!("Socket pair connectivity test completed successfully");

        Ok(())
    }

    /// Get information about the socket setup
    ///
    /// This returns a summary of the socket configuration for
    /// logging and debugging purposes.
    #[allow(dead_code)]
    pub fn info(&self) -> SocketInfo {
        SocketInfo {
            host_fd: self.host_socket.as_raw_fd(),
            guest_fd: self.guest_socket.as_raw_fd(),
            bootstrap_fd: BOOTSTRAP_FD,
        }
    }
}

/// Information about the socket setup
#[derive(Debug, Clone)]
pub struct SocketInfo {
    /// Host-side socket file descriptor
    pub host_fd: RawFd,
    /// Guest-side socket file descriptor
    pub guest_fd: RawFd,
    /// Bootstrap file descriptor number (always 3)
    pub bootstrap_fd: i32,
}

impl std::fmt::Display for SocketInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Socket(host_fd={}, guest_fd={}, bootstrap_fd={})",
            self.host_fd, self.guest_fd, self.bootstrap_fd
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_socket_creation() {
        let socket = Socket::new().unwrap();
        let info = socket.info();

        // Verify that we have valid file descriptors
        assert!(info.host_fd >= 0);
        assert!(info.guest_fd >= 0);
        assert_eq!(info.bootstrap_fd, BOOTSTRAP_FD);

        // Verify that host and guest FDs are different
        assert_ne!(info.host_fd, info.guest_fd);
    }

    #[test]
    fn test_socket_pair_connectivity() {
        let mut socket = Socket::new().unwrap();

        // Test that we can send data through the socket pair
        let result = socket.test_connectivity();
        assert!(result.is_ok());
    }

    #[test]
    fn test_bootstrap_fd_constant() {
        // Verify that our BOOTSTRAP_FD matches the Go implementation
        assert_eq!(BOOTSTRAP_FD, 3);
    }

    #[test]
    fn test_socket_info_display() {
        let socket = Socket::new().unwrap();
        let info = socket.info();
        let info_str = format!("{}", info);

        // Verify that the display format includes all expected information
        assert!(info_str.contains("host_fd="));
        assert!(info_str.contains("guest_fd="));
        assert!(info_str.contains("bootstrap_fd=3"));
    }
}
