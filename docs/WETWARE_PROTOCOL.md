# Wetware Protocol (/ww/0.1.0)

This document explains how to use the wetware protocol that implements Cap'n Proto RPC over libp2p streams.

## Overview

The wetware protocol provides a foundation for building secure, peer-to-peer RPC communication using:

- **Protocol Identifier**: `/ww/0.1.0`
- **Transport**: libp2p streams with Cap'n Proto RPC
- **Authentication**: Bootstrap capability system with granular permissions
- **Capabilities**: Terminal, FileSystem, Network, Process management

## Architecture

### Core Components

1. **`WetwareProtocolUpgrade`**: Handles protocol negotiation during connection establishment
2. **`WetwareStream<T>`**: Generic stream wrapper with Cap'n Proto message framing
3. **`WetwareProtocolBehaviour`**: Network behaviour that manages RPC connections
4. **`BootstrapCapability`**: Root capability that provides access to authenticated services
5. **`AuthService`**: Handles authentication and capability granting

### Capability System

The protocol implements a capability-based security model where:

- **Bootstrap Capability**: Root capability available to all authenticated connections
- **Terminal Capability**: Access to terminal I/O operations
- **FileSystem Capability**: Access to file system operations
- **Network Capability**: Access to network operations
- **Process Capability**: Access to process management

Capabilities can be granted or denied based on authentication policy, allowing fine-grained access control.

## Usage

### Basic Integration

```rust
use ww::rpc::{WetwareProtocolBehaviour, WetwareStreamHandler};

#[derive(NetworkBehaviour)]
struct MyBehaviour {
    kad: libp2p::kad::Behaviour<libp2p::kad::store::MemoryStore>,
    identify: libp2p::identify::Behaviour,
    wetware: WetwareProtocolBehaviour, // Add wetware protocol
}

// Create behaviour
let behaviour = MyBehaviour {
    kad: kademlia,
    identify: identify,
    wetware: WetwareProtocolBehaviour::new(),
};
```

### Protocol Initialization

The wetware protocol automatically:

1. **Negotiates Protocol**: Uses `/ww/0.1.0` for protocol upgrade
2. **Establishes Streams**: Creates Cap'n Proto RPC streams over libp2p
3. **Exports Bootstrap Capability**: Provides root access to authenticated services
4. **Manages Connections**: Tracks active RPC connections per peer

### Authentication Flow

```rust
// When a peer connects and negotiates the wetware protocol:
// 1. Stream is established
// 2. Bootstrap capability is exported
// 3. Peer can authenticate and request specific capabilities
// 4. Capabilities are granted based on auth policy
```

## Protocol Details

### Message Format

Each Cap'n Proto message follows this structure:

```
[Length: 4 bytes][Cap'n Proto Message: N bytes]
```

- **Length**: 32-bit big-endian length of the Cap'n Proto message
- **Message**: Standard Cap'n Proto serialized message

### Protocol Upgrade

The protocol upgrade process:

1. **Negotiation**: Both peers agree on `/ww/0.1.0`
2. **Stream Creation**: `WetwareStream` is created with the connection
3. **RPC Setup**: Cap'n Proto RPC system is initialized
4. **Capability Export**: Bootstrap capability is made available

## Example: Peer-to-Peer RPC

See `examples/wetware_protocol_demo.rs` for a complete example of:

- Setting up a wetware protocol node
- Listening for connections
- Dialing other peers
- Establishing RPC connections

### Running the Demo

```bash
# Terminal 1: Start first node
cargo run --example wetware_protocol_demo

# Terminal 2: Start second node and connect to first
cargo run --example wetware_protocol_demo /ip4/127.0.0.1/tcp/XXXXX/p2p/PEER_ID
```

## Integration with Existing Code

The wetware protocol is designed to work alongside existing libp2p protocols:

- **Kademlia DHT**: For peer discovery and routing
- **Identify**: For peer information exchange
- **Wetware Protocol**: For application-specific RPC communication

## Current Status

### Implemented

- ✅ Protocol negotiation (`/ww/0.1.0`)
- ✅ Stream management and framing
- ✅ Cap'n Proto message handling
- ✅ Bootstrap capability structure
- ✅ Basic capability implementations (Terminal, Network)
- ✅ RPC server setup (capability export commented out)

### TODO

- 🔄 Export bootstrap capability in RPC server
- 🔄 Implement actual Terminal operations
- 🔄 Implement actual Network operations
- 🔄 Add FileSystem and Process capabilities
- 🔄 Implement proper authentication logic
- 🔄 Add capability revocation
- 🔄 Add connection event handling

## Next Steps

1. **Enable Capability Export**: Uncomment and implement the bootstrap capability export in `WetwareRpcServer::new`
2. **Implement Terminal Operations**: Connect the Terminal capability to actual terminal I/O
3. **Add Authentication**: Implement proper user authentication and capability granting
4. **Test RPC Calls**: Verify that clients can successfully call RPC methods

## Testing

Run the test suite to verify protocol functionality:

```bash
cargo test --package ww --lib rpc
```

## Performance Considerations

- **Async I/O**: All operations are asynchronous and non-blocking
- **Message Framing**: Efficient length-prefixed message handling
- **Connection Management**: Automatic cleanup of closed connections
- **Capability Caching**: Capabilities are created once per connection

## Troubleshooting

### Common Issues

1. **Protocol Negotiation Fails**: Ensure both peers support `/ww/0.1.0`
2. **RPC Connection Issues**: Check that Cap'n Proto dependencies are properly configured
3. **Capability Not Available**: Verify that the bootstrap capability is being exported

### Debug Logging

Enable debug logging to see protocol details:

```bash
RUST_LOG=debug cargo run
```

## Future Enhancements

Potential improvements for the wetware protocol:

- **Reliable Delivery**: Add acknowledgment and retry mechanisms
- **Message Compression**: Implement compression for large messages
- **End-to-End Encryption**: Add encryption support for sensitive operations
- **Stream Multiplexing**: Support multiple logical streams per connection
- **Rate Limiting**: Add flow control and rate limiting
- **Metrics Collection**: Collect performance and usage metrics
- **Capability Delegation**: Allow capabilities to be delegated to other peers
