use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};

/// A thread-safe pipe that can have its underlying stream swapped at runtime.
/// This allows WASI stdin/stdout to be redirected to different streams per-message.
///
/// Matches the Go implementation's approach where the Endpoint's ReadWriteCloser
/// is set to the network stream for each message.
#[derive(Clone)]
pub struct Pipe {
    inner: Arc<Mutex<Option<Box<dyn super::endpoint::ReadWriteCloser>>>>,
}

impl Default for Pipe {
    fn default() -> Self {
        Self::new()
    }
}

impl Pipe {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
        }
    }

    /// Set the underlying stream for this pipe
    pub fn set(&self, stream: Box<dyn super::endpoint::ReadWriteCloser>) {
        *self.inner.lock().unwrap() = Some(stream);
    }

    /// Clear the underlying stream
    pub fn clear(&self) {
        *self.inner.lock().unwrap() = None;
    }
}

impl Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        match &mut *inner {
            Some(stream) => stream.read(buf),
            None => Ok(0), // EOF when no stream is set
        }
    }
}

impl Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut inner = self.inner.lock().unwrap();
        match &mut *inner {
            Some(stream) => stream.write(buf),
            None => Ok(buf.len()), // Discard when no stream is set
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        match &mut *inner {
            Some(stream) => stream.flush(),
            None => Ok(()),
        }
    }
}

