use anyhow::Result;
use bytes::{Buf, BytesMut};
use futures::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use libp2p::swarm::{ConnectionId, StreamProtocol};
use std::collections::HashMap;
use tokio_util::codec::{Decoder, Encoder};
use tracing::{debug, info};
use std::sync::{Arc, Mutex};

use crate::service_manager::ServiceManager;

// Protocol identifier for wetware
pub const WW_PROTOCOL: &str = "/ww/0.1.0";

// TODO: Implement proper swarm capabilities
// For now, we'll use placeholder values that will be properly implemented later

/// Wetware protocol upgrade info (simplified)
#[derive(Debug, Clone)]
pub struct DefaultProtocolUpgrade {
    pub protocol: StreamProtocol,
}

impl DefaultProtocolUpgrade {
    pub fn new() -> Self {
        Self {
            protocol: StreamProtocol::new(WW_PROTOCOL),
        }
    }
}

/// Wetware stream that handles Cap'n Proto RPC over libp2p
#[derive(Debug)]
pub struct DefaultStream<T> {
    io: T,
    read_buffer: BytesMut,
    write_buffer: BytesMut,
}

impl<T> DefaultStream<T>
where
    T: AsyncRead + AsyncWrite + Send + Unpin,
{
    pub fn new(io: T) -> Self {
        Self {
            io,
            read_buffer: BytesMut::new(),
            write_buffer: BytesMut::new(),
        }
    }

    /// Send a Cap'n Proto message over the stream
    pub async fn send_capnp_message(&mut self, message: &[u8]) -> Result<()> {
        let length = message.len() as u32;

        // Write length prefix (4 bytes, big-endian for Cap'n Proto compatibility)
        bytes::BufMut::put_u32_le(&mut self.write_buffer, length);
        // Write message content
        self.write_buffer.extend_from_slice(message);

        // Flush to underlying stream
        self.io.write_all(&self.write_buffer).await?;
        self.io.flush().await?;

        // Clear write buffer
        self.write_buffer.clear();

        debug!("Sent Cap'n Proto message: {} bytes", length);
        Ok(())
    }

    /// Receive a Cap'n Proto message from the stream
    pub async fn receive_capnp_message(&mut self) -> Result<Option<Vec<u8>>> {
        // Read length prefix if we don't have enough bytes
        if self.read_buffer.len() < 4 {
            let mut temp_buffer = [0u8; 1024];
            let bytes_read = self.io.read(&mut temp_buffer).await?;
            if bytes_read == 0 {
                return Ok(None); // EOF
            }
            self.read_buffer
                .extend_from_slice(&temp_buffer[..bytes_read]);
        }

        // Check if we have enough bytes for the length prefix
        if self.read_buffer.len() < 4 {
            return Ok(None);
        }

        // Read length prefix (big-endian)
        let length = bytes::Buf::get_u32_le(&mut self.read_buffer) as usize;

        // Check if we have the full message
        if self.read_buffer.len() < length {
            return Ok(None);
        }

        // Extract message
        let message_bytes = self.read_buffer.split_to(length);

        debug!("Received Cap'n Proto message: {} bytes", length);
        Ok(Some(message_bytes.to_vec()))
    }
}

/// Cap'n Proto message codec for framing
#[derive(Debug)]
pub struct DefaultCodec;

impl Decoder for DefaultCodec {
    type Item = Vec<u8>;
    type Error = anyhow::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() < 4 {
            return Ok(None);
        }

        // Extract the message length from the first 4 bytes in little-endian order
        // This matches the length prefix encoding used in Cap'n Proto RPC protocol
        let length = u32::from_le_bytes([src[0], src[1], src[2], src[3]]) as usize;

        if src.len() < 4 + length {
            return Ok(None);
        }

        let message = src[4..4 + length].to_vec();
        src.advance(4 + length);

        Ok(Some(message))
    }
}

impl Encoder<Vec<u8>> for DefaultCodec {
    type Error = anyhow::Error;

    fn encode(&mut self, item: Vec<u8>, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let length = item.len() as u32;
        bytes::BufMut::put_u32_le(dst, length);
        dst.extend_from_slice(&item);
        Ok(())
    }
}

/// Network capability implementation
#[derive(Debug)]
pub struct NetworkCapability;

impl NetworkCapability {
    pub fn new() -> Self {
        Self
    }

    /// Connect to a host
    pub async fn connect(&self, host: &str, port: u16) -> Result<ConnectionHandle> {
        // TODO: Implement actual network connection
        debug!("Network connect: {}:{}", host, port);
        Ok(ConnectionHandle::new())
    }

    /// Listen on a port
    pub async fn listen(&self, port: u16) -> Result<ListenerHandle> {
        // TODO: Implement actual network listening
        debug!("Network listen: {}", port);
        Ok(ListenerHandle::new())
    }
}

/// Connection handle
#[derive(Debug)]
pub struct ConnectionHandle;

impl ConnectionHandle {
    pub fn new() -> Self {
        Self
    }

    /// Read from connection
    pub async fn read(&self, max_bytes: u32) -> Result<(Vec<u8>, bool)> {
        // TODO: Implement actual connection reading
        debug!("Connection read: {} bytes", max_bytes);
        Ok((Vec::new(), true)) // EOF for now
    }

    /// Write to connection
    pub async fn write(&self, data: &[u8]) -> Result<u32> {
        // TODO: Implement actual connection writing
        debug!("Connection write: {} bytes", data.len());
        Ok(data.len() as u32)
    }

    /// Close connection
    pub async fn close(&self) -> Result<()> {
        // TODO: Implement actual connection closing
        debug!("Connection close requested");
        Ok(())
    }
}

/// Listener handle
#[derive(Debug)]
pub struct ListenerHandle;

impl ListenerHandle {
    pub fn new() -> Self {
        Self
    }

    /// Accept a connection
    pub async fn accept(&self) -> Result<ConnectionHandle> {
        // TODO: Implement actual connection acceptance
        debug!("Listener accept requested");
        Ok(ConnectionHandle::new())
    }

    /// Close listener
    pub async fn close(&self) -> Result<()> {
        // TODO: Implement actual listener closing
        debug!("Listener close requested");
        Ok(())
    }
}

/// Swarm capabilities structure
pub struct SwarmCapabilities {
    pub service_manager: ServiceManager,
}

/// Default RPC server implementation using Cap'n Proto RPC
pub struct DefaultRpcServer {
    /// The service manager for handling import/export requests
    service_manager: Arc<Mutex<ServiceManager>>,
    /// The underlying stream
    stream: DefaultStream<libp2p::Stream>,
}

impl DefaultRpcServer {
    pub fn new(
        stream: DefaultStream<libp2p::Stream>,
        service_manager: Arc<Mutex<ServiceManager>>,
    ) -> Result<Self> {
        info!("Creating new DefaultRpcServer with shared ServiceManager");

        Ok(Self {
            service_manager,
            stream,
        })
    }

    /// Get a reference to the service manager
    pub fn get_service_manager(&self) -> Arc<Mutex<ServiceManager>> {
        Arc::clone(&self.service_manager)
    }

    /// Get a mutable reference to the service manager
    pub fn get_service_manager_mut(&self) -> Arc<Mutex<ServiceManager>> {
        Arc::clone(&self.service_manager)
    }

    /// Get a reference to the underlying stream
    pub fn get_stream(&self) -> &DefaultStream<libp2p::Stream> {
        &self.stream
    }

    /// Get a mutable reference to the underlying stream
    pub fn get_stream_mut(&mut self) -> &mut DefaultStream<libp2p::Stream> {
        &mut self.stream
    }

    /// Process RPC requests using the Cap'n Proto RPC system
    pub async fn process_rpc_request(&mut self, _request_data: &[u8]) -> Result<Vec<u8>> {
        // TODO: Implement proper Cap'n Proto RPC handling
        // For now, return empty response
        Ok(Vec::new())
    }
}

/// Wetware protocol behaviour that manages RPC connections
pub struct DefaultProtocolBehaviour {
    /// Active RPC connections for each connection
    rpc_connections: HashMap<ConnectionId, DefaultRpcConnection>,
    /// Protocol upgrade info
    upgrade: DefaultProtocolUpgrade,
    /// Shared service manager for all connections
    shared_service_manager: ServiceManager,
}

// TODO: Implement NetworkBehaviour properly when ready
// For now, we'll use a simpler approach

/// Event type for DefaultProtocolBehaviour
#[derive(Debug)]
pub enum DefaultProtocolEvent {
    /// New RPC stream established
    StreamEstablished(ConnectionId),
    /// RPC request received
    RpcRequest(ConnectionId, Vec<u8>),
}

impl DefaultProtocolBehaviour {
    pub fn new() -> Self {
        Self {
            rpc_connections: HashMap::new(),
            upgrade: DefaultProtocolUpgrade::new(),
            shared_service_manager: ServiceManager::new(),
        }
    }

    /// Get the protocol upgrade info
    pub fn upgrade_info(&self) -> &DefaultProtocolUpgrade {
        &self.upgrade
    }

    /// Handle incoming RPC stream
    pub fn handle_incoming_stream(
        &mut self,
        connection_id: ConnectionId,
        _stream: DefaultStream<libp2p::Stream>,
    ) -> Result<()> {
        // Use the shared ServiceManager for this connection
        // This allows services exported by one peer to be imported by others
        let rpc_server = DefaultRpcServer::new(_stream, Arc::new(Mutex::new(self.shared_service_manager.clone())))?;
        let rpc_connection = DefaultRpcConnection::new(rpc_server);
        self.rpc_connections.insert(connection_id, rpc_connection);
        
        info!(
            "New wetware RPC stream established on connection {} with shared ServiceManager",
            connection_id
        );
        
        Ok(())
    }

    /// Process RPC request for a specific connection
    pub async fn process_rpc_request(
        &mut self,
        connection_id: ConnectionId,
        request_data: &[u8],
    ) -> Result<Option<Vec<u8>>> {
        if let Some(connection) = self.rpc_connections.get_mut(&connection_id) {
            let response = connection
                .get_server_mut()
                .process_rpc_request(request_data)
                .await?;
            Ok(Some(response))
        } else {
            Ok(None) // Connection not found
        }
    }

    /// Get RPC connection for a specific connection ID
    pub fn get_rpc_connection(
        &mut self,
        connection_id: ConnectionId,
    ) -> Option<&mut DefaultRpcConnection> {
        self.rpc_connections.get_mut(&connection_id)
    }

    /// Remove RPC connection for a closed connection
    pub fn remove_connection(&mut self, connection_id: ConnectionId) {
        if self.rpc_connections.remove(&connection_id).is_some() {
            debug!("Removed RPC connection for connection {}", connection_id);
        }
    }

    /// Get count of active RPC connections
    pub fn get_active_connection_count(&self) -> usize {
        self.rpc_connections.len()
    }
}

impl Default for DefaultProtocolBehaviour {
    fn default() -> Self {
        Self::new()
    }
}

/// RPC connection wrapper
pub struct DefaultRpcConnection {
    server: DefaultRpcServer,
}

impl DefaultRpcConnection {
    pub fn new(server: DefaultRpcServer) -> Self {
        Self { server }
    }

    /// Get mutable reference to the RPC server
    pub fn get_server_mut(&mut self) -> &mut DefaultRpcServer {
        &mut self.server
    }
}

/// Stream handler for processing incoming default streams
#[derive(Debug)]
pub struct DefaultStreamHandler {
    /// Protocol upgrade info
    upgrade: DefaultProtocolUpgrade,
}

impl DefaultStreamHandler {
    pub fn new() -> Self {
        Self {
            upgrade: DefaultProtocolUpgrade::new(),
        }
    }

    /// Get the protocol upgrade info
    pub fn upgrade_info(&self) -> &DefaultProtocolUpgrade {
        &self.upgrade
    }

    /// Handle an incoming stream
    pub async fn handle_stream(
        &self,
        stream: libp2p::Stream,
    ) -> Result<DefaultStream<libp2p::Stream>> {
        let default_stream = DefaultStream::new(stream);
        Ok(default_stream)
    }
}

impl Default for DefaultStreamHandler {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protocol_identifier() {
        assert_eq!(WW_PROTOCOL, "/ww/0.1.0");
    }

    #[test]
    fn test_codec_encoding_decoding() {
        let mut codec = DefaultCodec;
        let mut buffer = BytesMut::new();

        let test_data = b"Hello, World!";
        codec.encode(test_data.to_vec(), &mut buffer).unwrap();

        let decoded = codec.decode(&mut buffer).unwrap().unwrap();
        assert_eq!(decoded, test_data);
    }
}
