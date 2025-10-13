use std::io::{self, Read, Write};
use rand::Rng;
use base58::ToBase58;

/// An endpoint represents a communication channel for WASM processes.
/// 
/// In sync mode, it wraps stdin/stdout for direct I/O.
/// In async mode, it wraps a network stream for P2P communication.
/// This matches the Go implementation's Endpoint struct.
pub struct Endpoint {
    /// Unique name for this endpoint (base58 encoded random bytes)
    pub name: String,
    /// I/O channel - either stdin/stdout or network stream
    pub read_write_closer: Option<Box<dyn ReadWriteCloser>>,
}

/// Trait for I/O operations that can be used by the endpoint
pub trait ReadWriteCloser: Read + Write + Send {
    fn close(&mut self) -> io::Result<()>;
}

impl Default for Endpoint {
    fn default() -> Self {
        Self::new()
    }
}

impl Endpoint {
    /// Create a new endpoint with a random name
    pub fn new() -> Self {
        let mut rng = rand::thread_rng();
        let mut buf = [0u8; 8];
        rng.fill(&mut buf);
        
        Self {
            name: buf.to_base58(),
            read_write_closer: None,
        }
    }

    /// Get the protocol ID for this endpoint
    pub fn protocol(&self) -> String {
        format!("/ww/0.1.0/{}", self.name)
    }

    /// Set the I/O channel for this endpoint
    pub fn set_read_write_closer(&mut self, rwc: Box<dyn ReadWriteCloser>) {
        self.read_write_closer = Some(rwc);
    }

    /// Clear the I/O channel (useful for cleanup)
    pub fn clear_read_write_closer(&mut self) {
        self.read_write_closer = None;
    }
}

impl Read for Endpoint {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match &mut self.read_write_closer {
            Some(rwc) => rwc.read(buf),
            None => {
                // If no stream is available, return EOF immediately
                // This allows the WASM module to complete its main() function
                Ok(0)
            }
        }
    }
}

impl Write for Endpoint {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match &mut self.read_write_closer {
            Some(rwc) => rwc.write(buf),
            None => {
                // If no stream is available, discard output
                Ok(buf.len())
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match &mut self.read_write_closer {
            Some(rwc) => rwc.flush(),
            None => Ok(())
        }
    }
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        if let Some(rwc) = &mut self.read_write_closer {
            let _ = rwc.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct MockReadWriteCloser {
        data: Cursor<Vec<u8>>,
    }

    impl Read for MockReadWriteCloser {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.data.read(buf)
        }
    }

    impl Write for MockReadWriteCloser {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.data.write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.data.flush()
        }
    }

    impl ReadWriteCloser for MockReadWriteCloser {
        fn close(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_endpoint_creation() {
        let endpoint = Endpoint::new();
        assert!(!endpoint.name.is_empty());
        assert!(endpoint.protocol().starts_with("/ww/0.1.0/"));
    }

    #[test]
    fn test_endpoint_protocol() {
        let endpoint = Endpoint::new();
        let protocol = endpoint.protocol();
        assert!(protocol.starts_with("/ww/0.1.0/"));
        assert_eq!(protocol.len(), "/ww/0.1.0/".len() + endpoint.name.len());
    }

    #[test]
    fn test_endpoint_io_without_stream() {
        let mut endpoint = Endpoint::new();
        
        // Read should return EOF immediately
        let mut buf = [0u8; 10];
        let result = endpoint.read(&mut buf).unwrap();
        assert_eq!(result, 0);
        
        // Write should succeed but discard data
        let result = endpoint.write(b"hello").unwrap();
        assert_eq!(result, 5);
    }

    #[test]
    fn test_endpoint_io_with_stream() {
        let mut endpoint = Endpoint::new();
        let mock_rwc = Box::new(MockReadWriteCloser {
            data: Cursor::new(b"hello world".to_vec()),
        });
        endpoint.set_read_write_closer(mock_rwc);
        
        // Read should work
        let mut buf = [0u8; 11];
        let result = endpoint.read(&mut buf).unwrap();
        assert_eq!(result, 11);
        assert_eq!(&buf, b"hello world");
        
        // Write should work
        let result = endpoint.write(b"test").unwrap();
        assert_eq!(result, 4);
    }
}
