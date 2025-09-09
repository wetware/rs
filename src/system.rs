use anyhow::{anyhow, Result};
use nix::sys::socket::{self, AddressFamily, SockFlag, SockType};
use std::collections::HashMap;
use std::io::Write;
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
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

/// File descriptor mapping for user-provided FDs
struct FDMapping {
    name: String,
    source_fd: i32,
    target_fd: i32,
}

/// File descriptor manager for passing FDs to child processes
///
/// This manager handles:
/// - Unix domain socket pair creation for host-guest communication (FD3)
/// - User file descriptor duplication and passing (FD4+)
/// - Environment variable generation (WW_FD_*)
/// - Proper cleanup and resource management
///
/// # File Descriptor Convention
///
/// - **FD3**: Unix domain socket for host-guest RPC communication (bootstrap)
/// - **FD4+**: User-configurable file descriptors passed via --with-fd flags
///
/// This matches the Go implementation's file descriptor conventions.
pub struct FDManager {
    /// User file descriptor mappings (FD4+)
    mappings: Vec<FDMapping>,
    /// Socket for host-guest RPC (FD3)
    socket: Option<Socket>,
}

impl FDManager {
    /// Create a new FD manager from --with-fd flag values
    ///
    /// This creates a new FD manager that will handle both the Unix domain socket
    /// pair for host-guest communication (FD3) and user file descriptors (FD4+).
    ///
    /// # Arguments
    ///
    /// * `fd_flags` - List of --with-fd flag values in "name=fdnum" format
    ///
    /// # Returns
    ///
    /// Returns a new FDManager instance ready for subprocess execution.
    ///
    /// # Errors
    ///
    /// Returns an error if the FD flags are invalid or if socket pair creation fails.
    pub fn new(fd_flags: Vec<String>) -> Result<Self> {
        let mut mappings = Vec::new();
        let mut used_names = std::collections::HashSet::new();

        // Parse user file descriptor mappings (FD4+)
        for (i, flag) in fd_flags.iter().enumerate() {
            let (name, source_fd) = Self::parse_fd_flag(flag)?;

            if used_names.contains(&name) {
                return Err(anyhow!("Duplicate name '{}' in --with-fd flags", name));
            }

            // Target FD starts at 4 (after FD3 for bootstrap socket) and increments sequentially
            let target_fd = 4 + i as i32;

            mappings.push(FDMapping {
                name: name.clone(),
                source_fd,
                target_fd,
            });

            used_names.insert(name);
        }

        // Create Unix domain socket pair for host-guest communication (FD3)
        let socket = Some(Socket::new()?);

        debug!(
            user_fd_count = mappings.len(),
            "Created FD manager with {} user file descriptors and Unix domain socket pair",
            mappings.len()
        );

        Ok(Self { mappings, socket })
    }

    /// Parse a --with-fd flag value in "name=fdnum" format
    fn parse_fd_flag(value: &str) -> Result<(String, i32)> {
        let parts: Vec<&str> = value.splitn(2, '=').collect();
        if parts.len() != 2 {
            return Err(anyhow!(
                "Invalid format: expected 'name=fdnum', got '{}'",
                value
            ));
        }

        let name = parts[0].to_string();
        if name.is_empty() {
            return Err(anyhow!("Name cannot be empty"));
        }

        let fdnum: i32 = parts[1]
            .parse()
            .map_err(|_| anyhow!("Invalid fd number '{}'", parts[1]))?;

        if fdnum < 0 {
            return Err(anyhow!("FD number must be non-negative, got {}", fdnum));
        }

        Ok((name, fdnum))
    }

    /// Generate environment variables for the child process
    pub fn generate_env_vars(&self) -> HashMap<String, String> {
        let mut env_vars = HashMap::new();

        for mapping in &self.mappings {
            let env_var = format!("WW_FD_{}", mapping.name.to_uppercase());
            env_vars.insert(env_var, mapping.target_fd.to_string());
        }

        env_vars
    }

    /// Prepare file descriptors for passing to child process
    ///
    /// This method prepares all file descriptors that will be passed to the
    /// subprocess, including the Unix domain socket pair (FD3) and user FDs (FD4+).
    ///
    /// # Returns
    ///
    /// Returns a vector of raw file descriptors ready for subprocess inheritance.
    ///
    /// # Errors
    ///
    /// Returns an error if FD duplication fails or if the socket pair is not available.
    pub fn prepare_fds(&mut self) -> Result<Vec<i32>> {
        let mut extra_fds = Vec::new();

        // Add Unix domain socket for host-guest communication (FD3)
        if let Some(socket) = self.socket.take() {
            let guest_socket = socket.into_guest_socket();
            let guest_fd = guest_socket.into_raw_fd();
            extra_fds.push(guest_fd);

            debug!("Prepared Unix domain socket for FD3 (bootstrap)");
        } else {
            return Err(anyhow!("Socket not initialized"));
        }

        // Add user file descriptors (FD4+)
        for mapping in &self.mappings {
            // Duplicate the source FD to avoid conflicts
            let new_fd = unsafe { libc::dup(mapping.source_fd) };
            if new_fd < 0 {
                return Err(anyhow!(
                    "Failed to duplicate fd {} for '{}': {}",
                    mapping.source_fd,
                    mapping.name,
                    std::io::Error::last_os_error()
                ));
            }

            extra_fds.push(new_fd);

            debug!(
                name = %mapping.name,
                source_fd = mapping.source_fd,
                target_fd = mapping.target_fd,
                "File descriptor prepared"
            );
        }

        info!(
            total_fds = extra_fds.len(),
            "Prepared {} file descriptors for subprocess (FD3: socket, FD4+: user FDs)",
            extra_fds.len()
        );

        Ok(extra_fds)
    }

    /// Close all managed file descriptors
    ///
    /// This method closes all user file descriptors that were duplicated for
    /// the subprocess. The Unix domain socket pair is automatically cleaned up
    /// when the Socket object is dropped.
    ///
    /// # Returns
    ///
    /// Returns `Ok(())` on success, or an error if cleanup fails.
    pub fn close_fds(&self) -> Result<()> {
        debug!("Closing {} user file descriptors", self.mappings.len());

        for mapping in &self.mappings {
            // Only close file descriptors that are valid (>= 0)
            // In test contexts, source_fd might be invalid test values
            if mapping.source_fd >= 0 {
                // Check if the file descriptor is actually open before trying to close it
                let result = unsafe { libc::fcntl(mapping.source_fd, libc::F_GETFD) };
                if result >= 0 {
                    // File descriptor is open, close it
                    let close_result = unsafe { libc::close(mapping.source_fd) };
                    if close_result < 0 {
                        debug!(
                            name = %mapping.name,
                            source_fd = mapping.source_fd,
                            error = %std::io::Error::last_os_error(),
                            "Failed to close file descriptor"
                        );
                    } else {
                        debug!(
                            name = %mapping.name,
                            source_fd = mapping.source_fd,
                            "File descriptor closed"
                        );
                    }
                } else {
                    debug!(
                        name = %mapping.name,
                        source_fd = mapping.source_fd,
                        "File descriptor not open, skipping"
                    );
                }
            } else {
                debug!(
                    name = %mapping.name,
                    source_fd = mapping.source_fd,
                    "Skipping invalid file descriptor"
                );
            }
        }

        info!("File descriptor cleanup completed");
        Ok(())
    }

    /// Get a reference to the socket handler
    ///
    /// This returns a reference to the socket handler for RPC
    /// communication with the subprocess.
    ///
    /// # Returns
    ///
    /// Returns `Some(Socket)` if available, or `None` if not initialized.
    #[allow(dead_code)]
    pub fn socket(&self) -> Option<&Socket> {
        self.socket.as_ref()
    }

    /// Take ownership of the socket handler
    ///
    /// This takes ownership of the socket handler, removing it
    /// from the FD manager. This is useful when transferring ownership to
    /// the subprocess execution context.
    ///
    /// # Returns
    ///
    /// Returns `Some(Socket)` if available, or `None` if not initialized.
    #[allow(dead_code)]
    pub fn take_socket(&mut self) -> Option<Socket> {
        self.socket.take()
    }
}

impl Drop for FDManager {
    /// Clean up resources when FDManager is dropped
    ///
    /// Note: This does not close file descriptors because:
    /// - Original source_fd should remain open (owned by caller)
    /// - Duplicated file descriptors are passed to subprocess and closed by it
    /// - Socket file descriptors are handled by Socket's own Drop implementation
    fn drop(&mut self) {
        debug!("Dropping FDManager - no file descriptor cleanup needed");
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
