# Wetware Protocol (/ww/0.1.0)

The Wetware Protocol is a peer-to-peer RPC protocol that provides an **exporter** as its bootstrap capability. This exporter enables secure, capability-based access to system resources and services over libp2p networks using Cap'n Proto RPC.

## Protocol Overview

The wetware protocol (`/ww/0.1.0`) implements a capability-based security model where the **exporter** serves as the root bootstrap capability. This exporter provides authenticated peers with access to a controlled set of system capabilities, enabling secure remote execution and resource management.

### Core Design Principles

- **Exporter-First Architecture**: The exporter is the primary bootstrap capability that all authenticated connections receive
- **Capability-Based Security**: Granular permissions through the exporter's capability system
- **Peer-to-Peer RPC**: Direct communication between peers using Cap'n Proto over libp2p streams
- **Secure by Default**: No implicit trust - all access must be explicitly granted through capabilities

## Exporter as Bootstrap Capability

### What is the Exporter?

The exporter is the fundamental bootstrap capability that:

1. **Provides Access Control**: Acts as the gatekeeper for all system resources
2. **Manages Capabilities**: Grants, revokes, and delegates specific capabilities to authenticated peers
3. **Enables Service Discovery**: Allows peers to discover and request available services
4. **Maintains Security Context**: Tracks authentication state and capability permissions

### Exporter Capabilities

Through the exporter, authenticated peers can access:

- **Terminal Capability**: Remote terminal I/O operations
- **FileSystem Capability**: File and directory operations
- **Network Capability**: Network interface and connection management
- **Process Capability**: Process creation, monitoring, and control

### Authentication Flow

```
Peer Connection → Protocol Negotiation → Exporter Bootstrap → Capability Request → Access Grant
```

1. **Connection**: Peer establishes libp2p connection
2. **Protocol**: Both peers agree on `/ww/0.1.0`
3. **Bootstrap**: Exporter capability is automatically provided
4. **Authentication**: Peer authenticates through the exporter
5. **Access**: Specific capabilities are granted based on policy

## Protocol Architecture

### Core Components

1. **`WetwareProtocolUpgrade`**: Handles protocol negotiation during connection establishment
2. **`WetwareStream<T>`**: Generic stream wrapper with Cap'n Proto message framing
3. **`WetwareProtocolBehaviour`**: Network behaviour that manages RPC connections
4. **`Exporter`**: The bootstrap capability that provides access to all other capabilities
5. **`AuthService`**: Handles authentication and capability granting through the exporter

### Exporter Implementation

The exporter is implemented as a Cap'n Proto RPC service that:

```rust
// The exporter provides these core methods:
trait Exporter {
    // Bootstrap method - provides initial capability access
    fn bootstrap(&self) -> Result<BootstrapCapability, Error>;
    
    // Capability management
    fn request_capability(&self, name: &str) -> Result<Capability, Error>;
    fn list_capabilities(&self) -> Result<Vec<String>, Error>;
    
    // Authentication
    fn authenticate(&self, credentials: &Credentials) -> Result<AuthResult, Error>;
}
```

## Usage

### Basic Integration

```rust
use ww::rpc::{WetwareProtocolBehaviour, WetwareStreamHandler};

#[derive(NetworkBehaviour)]
struct MyBehaviour {
    kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
    identify: libp2p::identify::Behaviour,
    wetware: WetwareProtocolBehaviour, // Adds wetware protocol with exporter
}

// Create behaviour
let behaviour = MyBehaviour {
    kad: kademlia,
    identify: identify,
    wetware: WetwareProtocolBehaviour::new(), // Automatically provides exporter
};
```

### Exporter Initialization

The wetware protocol automatically:

1. **Negotiates Protocol**: Uses `/ww/0.1.0` for protocol upgrade
2. **Establishes Streams**: Creates Cap'n Proto RPC streams over libp2p
3. **Exports Exporter**: Provides the exporter as the bootstrap capability
4. **Manages Connections**: Tracks active RPC connections per peer

### Using the Exporter

```rust
// When a peer connects and negotiates the wetware protocol:
// 1. Stream is established
// 2. Exporter capability is automatically provided
// 3. Peer can use exporter to request specific capabilities
// 4. Exporter grants capabilities based on authentication policy

// Example: Requesting terminal capability through exporter
let terminal_cap = exporter.request_capability("terminal")?;
terminal_cap.execute_command("ls -la")?;
```

## Protocol Details

### Message Format

Each Cap'n Proto message follows this structure:

```
[Length: 4 bytes][Cap'n Proto Message: N bytes]
```

- **Length**: 32-bit big-endian length of the Cap'n Proto message
- **Message**: Standard Cap'n Proto serialized message containing exporter RPC calls

### Protocol Upgrade

The protocol upgrade process:

1. **Negotiation**: Both peers agree on `/ww/0.1.0`
2. **Stream Creation**: `WetwareStream` is created with the connection
3. **RPC Setup**: Cap'n Proto RPC system is initialized
4. **Exporter Export**: The exporter capability is automatically made available

## Example: Peer-to-Peer RPC with Exporter

See `examples/wetware_protocol_demo.rs` for a complete example of:

- Setting up a wetware protocol node with exporter
- Listening for connections
- Dialing other peers
- Using the exporter to access system capabilities

### Running the Demo

```bash
# Terminal 1: Start first node
cargo run --example wetware_protocol_demo

# Terminal 2: Start second node and connect to first
cargo run --example wetware_protocol_demo /ip4/127.0.0.1/tcp/XXXXX/p2p/PEER_ID
```

## Integration with Existing Code

The wetware protocol with exporter is designed to work alongside existing libp2p protocols:

- **Kademlia DHT**: For peer discovery and routing
- **Identify**: For peer information exchange
- **Wetware Protocol**: For application-specific RPC communication with exporter bootstrap

## Current Status

### Implemented

- ✅ Protocol negotiation (`/ww/0.1.0`)
- ✅ Stream management and framing
- ✅ Cap'n Proto message handling
- ✅ Exporter capability structure
- ✅ Basic capability implementations (Terminal, Network)
- ✅ RPC server setup with exporter export

### TODO

- 🔄 Complete exporter RPC implementation
- 🔄 Implement actual Terminal operations through exporter
- 🔄 Implement actual Network operations through exporter
- 🔄 Add FileSystem and Process capabilities to exporter
- 🔄 Implement proper authentication logic in exporter
- 🔄 Add capability revocation through exporter
- 🔄 Add connection event handling

## Next Steps

1. **Complete Exporter Implementation**: Finish the exporter RPC service implementation
2. **Implement Terminal Operations**: Connect the Terminal capability to actual terminal I/O through exporter
3. **Add Authentication**: Implement proper user authentication and capability granting in exporter
4. **Test Exporter RPC Calls**: Verify that clients can successfully use the exporter to access capabilities

## Testing

Run the test suite to verify protocol functionality:

```bash
cargo test --package ww --lib rpc
```

## Performance Considerations

- **Async I/O**: All operations are asynchronous and non-blocking
- **Message Framing**: Efficient length-prefixed message handling
- **Connection Management**: Automatic cleanup of closed connections
- **Exporter Caching**: Exporter capabilities are created once per connection

## Troubleshooting

### Common Issues

1. **Protocol Negotiation Fails**: Ensure both peers support `/ww/0.1.0`
2. **Exporter Not Available**: Check that the exporter capability is being properly exported
3. **RPC Connection Issues**: Check that Cap'n Proto dependencies are properly configured
4. **Capability Not Available**: Verify that the exporter is granting the requested capabilities

### Debug Logging

Enable debug logging to see protocol details:

```bash
RUST_LOG=debug cargo run
```

## Future Enhancements

Potential improvements for the wetware protocol exporter:

- **Reliable Delivery**: Add acknowledgment and retry mechanisms
- **Message Compression**: Implement compression for large messages
- **End-to-End Encryption**: Add encryption support for sensitive operations
- **Stream Multiplexing**: Support multiple logical streams per connection
- **Rate Limiting**: Add flow control and rate limiting
- **Metrics Collection**: Collect performance and usage metrics
- **Capability Delegation**: Allow the exporter to delegate capabilities to other peers
- **Exporter Clustering**: Support multiple exporter instances for load balancing
