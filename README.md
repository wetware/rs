# Basic P2P with IPFS DHT Bootstrap

A minimal Rust libp2p application that connects to a local Kubo (IPFS) node and bootstraps into the IPFS DHT network using peers discovered from Kubo.

## Features

- **IPFS DHT Bootstrap**: Automatically discovers and connects to IPFS peers from local Kubo node
- **Protocol Compatibility**: Uses standard IPFS protocols (`/ipfs/kad/1.0.0`, `/ipfs/id/1.0.0`) for full network compatibility
- **RSA Key Support**: Includes RSA support for connecting to legacy IPFS peers
- **Creates libp2p Host**: Generates Ed25519 identity and listens on TCP with IPFS-compatible protocols
- **DHT Operations**: Participates in IPFS DHT operations (provide/query) after bootstrap
- **Structured Logging**: Comprehensive logging with configurable levels and performance metrics

## Prerequisites

1. **Kubo (IPFS) daemon running locally**
   ```bash
   kubo daemon
   ```

2. **Rust toolchain**
   ```bash
   rustup install stable
   ```

## Usage

1. **Start Kubo daemon** (in a separate terminal):
   ```bash
   kubo daemon
   ```

2. **Run the application** with Kubo API endpoint:
   ```bash
   # Connect to default Kubo API
   cargo run -- http://127.0.0.1:5001
   
   # Or specify custom endpoint
   cargo run -- http://192.168.1.100:5001
   ```

## Logging Configuration

The application uses structured logging with the `tracing` crate. You can configure log levels using environment variables:

### Environment Variables

- **`WW_LOG`**: Controls the log level for different components
  ```bash
  # Set default log level
  export WW_LOG=ww=info,libp2p=debug
  
  # More verbose logging
  export WW_LOG=ww=debug,libp2p=trace
  
  # Only show warnings and errors
  export WW_LOG=ww=warn
  ```

### Log Levels

- **`error`**: Errors that need immediate attention
- **`warn`**: Warnings about potential issues
- **`info`**: General information about application flow
- **`debug`**: Detailed debugging information
- **`trace`**: Very detailed tracing (very verbose)

### Performance Metrics

The application logs performance metrics for key operations:
- Kubo peer discovery duration
- Host setup time
- DHT bootstrap duration
- Provider announcement time
- Provider query time
- Total application runtime

### Example Log Output

```
2024-01-15T10:30:00.123Z INFO  ww::main{thread_id=1 thread_name="tokio-runtime-worker"}: Starting basic-p2p application
2024-01-15T10:30:00.124Z INFO  ww::main{thread_id=1 thread_name="tokio-runtime-worker"}: Bootstrap Kubo node kubo_url=http://127.0.0.1:5001
2024-01-15T15T10:30:00.125Z INFO  ww::get_kubo_peers{thread_id=1 thread_name="tokio-runtime-worker"}: Querying Kubo node for peers url=http://127.0.0.1:5001/api/v0/swarm/peers
2024-01-15T10:30:00.200Z INFO  ww::get_kubo_peers{thread_id=1 thread_name="tokio-runtime-worker"}: Found peer addresses from Kubo node peer_count=5 parse_errors=0
2024-01-15T10:30:00.201Z INFO  ww::main{thread_id=1 thread_name="tokio-runtime-worker"}: Kubo peer discovery completed duration_ms=76
```

## Expected Output

```
Starting basic-p2p application...
Bootstrap Kubo node kubo_url=http://127.0.0.1:5001
Querying Kubo node for peers url=http://127.0.0.1:5001/api/v0/swarm/peers
Found peer addresses from Kubo node peer_count=86 parse_errors=0
Found peers from Kubo node peer_count=86
Kubo peer discovery completed duration_ms=18
Generated Ed25519 keypair peer_id=12D3KooWDmTwwTyjY7kwFvY3qPPJMLaZYrs62a4xqPRByu8rczoX
Created Kademlia configuration
Set Kademlia to client mode
Built libp2p swarm
Started listening on address listen_addr=/ip4/0.0.0.0/tcp/0
Local PeerId peer_id=12D3KooWDmTwwTyjY7kwFvY3qPPJMLaZYrs62a4xqPRByu8rczoX
Adding 86 IPFS peers to Kademlia routing table
Bootstrapping DHT with IPFS peers
DHT bootstrap completed
Provider announcement completed
Provider query completed
Starting DHT event loop
Application ready! Successfully joined the IPFS DHT network
```

## How It Works

1. **Peer Discovery**: Queries local Kubo node's HTTP API to discover connected peers
2. **Host Creation**: Generates Ed25519 keypair and creates libp2p swarm with IPFS-compatible protocols
3. **DHT Bootstrap**: Adds discovered peers to Kademlia routing table and establishes connections
4. **Network Integration**: Joins the IPFS DHT network and participates in DHT operations
5. **DHT Operations**: Can provide content and query for providers in the IPFS network

## DHT Bootstrap Process

The application implements a sophisticated DHT bootstrap process:

1. **Peer Discovery**: Queries the local Kubo node's `/api/v0/swarm/peers` endpoint to discover connected peers
2. **Routing Table Population**: Adds discovered peers to the Kademlia routing table before establishing connections
3. **Connection Establishment**: Dials discovered peers to establish TCP connections
4. **Protocol Handshake**: Performs identify and Kademlia protocol handshakes using standard IPFS protocols
5. **Bootstrap Trigger**: Triggers the Kademlia bootstrap process to populate the routing table
6. **Network Participation**: Begins participating in DHT operations (provide/query)

This approach ensures rapid integration into the IPFS network by leveraging the local Kubo node's peer knowledge.

## Protocol Compatibility

The application is designed for full IPFS network compatibility:

- **Kademlia DHT**: Uses `/ipfs/kad/1.0.0` protocol for DHT operations
- **Identify**: Uses `/ipfs/id/1.0.0` protocol for peer identification
- **Transport**: Supports TCP with Noise encryption and Yamux multiplexing
- **Key Types**: Supports both Ed25519 (modern) and RSA (legacy) key types
- **Multiaddr**: Handles standard IPFS multiaddresses with peer IDs

This ensures the application can communicate with any IPFS node in the network, regardless of their specific configuration.

## Troubleshooting

- **"IPFS API file not found"**: Make sure Kubo is running (`kubo daemon`)
- **Connection errors**: Check if Kubo is listening on the expected port
- **DHT bootstrap failures**: Ensure Kubo has peers and the API endpoint is correct
- **Protocol compatibility**: The application uses standard IPFS protocols for full compatibility
- **RSA connection errors**: RSA support is included for legacy IPFS peers
- **Logging issues**: Check `WW_LOG` environment variable and ensure tracing is properly initialized

## Dependencies

- `libp2p`: P2P networking stack with IPFS protocol support
- `libp2p-kad`: Kademlia DHT implementation for IPFS compatibility
- `libp2p-identify`: Peer identification protocol for IPFS compatibility
- `reqwest`: HTTP client for Kubo API integration
- `tokio`: Async runtime for concurrent operations
- `anyhow`: Error handling and propagation
- `serde`: JSON serialization/deserialization for API responses
- `tracing`: Structured logging framework with performance metrics
- `tracing-subscriber`: Logging subscriber with environment-based configuration
