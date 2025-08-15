# Wetware Protocol (/ww/0.1.0)

The Wetware Protocol is a peer-to-peer RPC protocol that provides both **exporter** and **importer** capabilities for secure, capability-based access to system resources and services over libp2p networks using Cap'n Proto RPC.

## Protocol Overview

The wetware protocol (`/ww/0.1.0`) implements a capability-based security model where peers can both export and import capabilities through a shared membrane. This dual capability system enables secure remote execution and resource management with fine-grained access control.

### Core Design Principles

- **Dual Capability Architecture**: Both exporter and importer capabilities work together for secure capability management
- **Capability-Based Security**: Granular permissions through the membrane's capability system
- **Peer-to-Peer RPC**: Direct communication between peers using Cap'n Proto over libp2p streams
- **Secure by Default**: No implicit trust - all access must be explicitly granted through capabilities

## Exporter and Importer Capabilities

### What are the Exporter and Importer?

The wetware protocol provides two complementary capabilities:

1. **Exporter**: Allows peers to export capabilities to the shared membrane
   - Provides access control by managing what capabilities are available
   - Generates secure tokens for capability access
   - Maintains the capability registry

2. **Importer**: Allows peers to import capabilities from the shared membrane
   - Enables discovery and access to available capabilities
   - Uses secure tokens for capability retrieval
   - Provides controlled access to system resources

### Available Capabilities

Through the membrane, authenticated peers can access:

- **Terminal Capability**: Remote terminal I/O operations
- **FileSystem Capability**: File and directory operations
- **Network Capability**: Network interface and connection management
- **Process Capability**: Process creation, monitoring, and control

### Authentication Flow

```
Peer Connection → Protocol Negotiation → Membrane Access → Capability Export/Import → Access Grant
```

1. **Connection**: Peer establishes libp2p connection
2. **Protocol**: Both peers agree on `/ww/0.1.0`
3. **Membrane Access**: Peer gains access to exporter and importer capabilities
4. **Capability Management**: Peer can export new capabilities or import existing ones
5. **Access**: Specific capabilities are granted based on policy and tokens

## Protocol Architecture

### Core Components

1. **`WetwareProtocolUpgrade`**: Handles protocol negotiation during connection establishment
2. **`WetwareStream<T>`**: Generic stream wrapper with Cap'n Proto message framing
3. **`WetwareProtocolBehaviour`**: Network behaviour that manages RPC connections
4. **`Membrane`**: The shared capability registry that manages both exporter and importer operations
5. **`Exporter`**: Interface for exporting capabilities to the membrane
6. **`Importer`**: Interface for importing capabilities from the membrane

### Membrane Implementation

The membrane is implemented as a shared registry that provides both exporter and importer capabilities:

```rust
// The membrane provides these core interfaces:
trait Exporter {
    // Export a capability to the membrane
    fn export(&self, service: Capability) -> Result<ServiceToken, Error>;
}

trait Importer {
    // Import a capability from the membrane using a token
    fn import(&self, token: ServiceToken) -> Result<Capability, Error>;
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
    wetware: WetwareProtocolBehaviour, // Adds wetware protocol with membrane access
}

// Create behaviour
let behaviour = MyBehaviour {
    kad: kademlia,
    identify: identify,
    wetware: WetwareProtocolBehaviour::new(), // Automatically provides membrane access
};
```

### Membrane Initialization

The wetware protocol automatically:

1. **Negotiates Protocol**: Uses `/ww/0.1.0` for protocol upgrade
2. **Establishes Streams**: Creates Cap'n Proto RPC streams over libp2p
3. **Provides Membrane Access**: Gives peers access to both exporter and importer capabilities
4. **Manages Connections**: Tracks active RPC connections per peer

### Using the Membrane

```rust
// When a peer connects and negotiates the wetware protocol:
// 1. Stream is established
// 2. Membrane access is automatically provided
// 3. Peer can use exporter to provide capabilities or importer to access them
// 4. Membrane manages capability tokens and access control

// Example: Exporting a terminal capability
let terminal_cap = TerminalCapability::new();
let token = exporter.export(terminal_cap)?;

// Example: Importing a capability using a token
let imported_cap = importer.import(token)?;
imported_cap.execute_command("ls -la")?;
```

## Protocol Details

### Message Format

Each Cap'n Proto message follows this structure:

```
[Length: 4 bytes][Cap'n Proto Message: N bytes]
```

- **Length**: 32-bit big-endian length of the Cap'n Proto message
- **Message**: Standard Cap'n Proto serialized message containing importer RPC calls

### Protocol Upgrade

The protocol upgrade process:

1. **Negotiation**: Both peers agree on `/ww/0.1.0`
2. **Stream Creation**: `WetwareStream` is created with the connection
3. **RPC Setup**: Cap'n Proto RPC system is initialized
4. **Importer Export**: The importer capability is automatically made available

## Example: Peer-to-Peer RPC with Importer

See `examples/wetware_protocol_demo.rs` for a complete example of:

- Setting up a wetware protocol node with importer
- Listening for connections
- Dialing other peers
- Using the importer to access system capabilities

### Running the Demo

```bash
# Terminal 1: Start first node
cargo run --example wetware_protocol_demo

# Terminal 2: Start second node and connect to first
cargo run --example wetware_protocol_demo /ip4/127.0.0.1/tcp/XXXXX/p2p/PEER_ID
```

## Integration with Existing Code

The wetware protocol with importer is designed to work alongside existing libp2p protocols:

- **Kademlia DHT**: For peer discovery and routing
- **Identify**: For peer information exchange
- **Wetware Protocol**: For application-specific RPC communication with importer bootstrap

## Current Status

### Implemented

- ✅ Protocol negotiation (`/ww/0.1.0`)
- ✅ Stream management and framing
- ✅ Cap'n Proto message handling
- ✅ Importer capability structure
- ✅ Basic capability implementations (Terminal, Network)
- ✅ RPC server setup with importer export

### TODO

- 🔄 Complete importer RPC implementation
- 🔄 Implement actual Terminal operations through importer
- 🔄 Implement actual Network operations through importer
- 🔄 Add FileSystem and Process capabilities to importer
- 🔄 Implement proper authentication logic in importer
- 🔄 Add capability revocation through importer
- 🔄 Add connection event handling

## Next Steps

1. **Complete Importer Implementation**: Finish the importer RPC service implementation
2. **Implement Terminal Operations**: Connect the Terminal capability to actual terminal I/O through importer
3. **Add Authentication**: Implement proper user authentication and capability granting in importer
4. **Test Importer RPC Calls**: Verify that clients can successfully use the importer to access capabilities

## Testing

Run the test suite to verify protocol functionality:

```bash
cargo test --package ww --lib rpc
```

## Performance Considerations

- **Async I/O**: All operations are asynchronous and non-blocking
- **Message Framing**: Efficient length-prefixed message handling
- **Connection Management**: Automatic cleanup of closed connections
- **Importer Caching**: Importer capabilities are created once per connection

## Troubleshooting

### Common Issues

1. **Protocol Negotiation Fails**: Ensure both peers support `/ww/0.1.0`
2. **Importer Not Available**: Check that the importer capability is being properly exported
3. **RPC Connection Issues**: Check that Cap'n Proto dependencies are properly configured
4. **Capability Not Available**: Verify that the importer is granting the requested capabilities

### Debug Logging

Enable debug logging to see protocol details:

```bash
RUST_LOG=debug cargo run
```

## Future Enhancements

Potential improvements for the wetware protocol importer:

- **Reliable Delivery**: Add acknowledgment and retry mechanisms
- **Message Compression**: Implement compression for large messages
- **End-to-End Encryption**: Add encryption support for sensitive operations
- **Stream Multiplexing**: Support multiple logical streams per connection
- **Rate Limiting**: Add flow control and rate limiting
- **Metrics Collection**: Collect performance and usage metrics
- **Capability Delegation**: Allow the importer to delegate capabilities to other peers
- **Importer Clustering**: Support multiple importer instances for load balancing
