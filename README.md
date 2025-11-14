# Wetware Protocol Node

A Rust libp2p application that implements the Wetware Protocol (`/ww/0.1.0`) for peer-to-peer RPC communication with capability-based security. The node connects to IPFS DHT networks and provides secure remote execution capabilities through Cap'n Proto RPC.

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

2. **Cap'n Proto compiler (version 0.5.2 or higher)**
   ```bash
   # Ubuntu/Debian
   sudo apt-get install capnproto
   
   # macOS
   brew install capnp
   
   # Or build from source
   # See https://capnproto.org/install.html
   ```

3. **Rust toolchain (nightly required)**
   ```bash
   rustup install nightly
   rustup default nightly
   ```
   
   **Note**: This project requires Rust nightly due to dependencies that use `edition2024` features. The nightly toolchain provides access to these experimental features.

## Usage

The application now uses a subcommand structure. The main command is `ww` with a `run` subcommand for starting a wetware node.

### Command Structure

```bash
ww <COMMAND>

Commands:
  run     Run a wetware node
  help    Print this message or the help of the given subcommand(s)
```

### Running a Wetware Node

1. **Start Kubo daemon** (in a separate terminal):
   ```bash
   kubo daemon
   ```

2. **Run the application** using the `run` subcommand:
   ```bash
   # Use defaults (http://localhost:5001, info log level)
   cargo run -- run
   
   # Custom IPFS endpoint
   cargo run -- run --ipfs http://127.0.0.1:5001
   cargo run -- run --ipfs http://192.168.1.100:5001
   
   # Custom log level
   cargo run -- run --loglvl debug
   cargo run -- run --loglvl trace
   
   # Combine both
   cargo run -- run --ipfs http://192.168.1.100:5001 --loglvl debug
   
   # Or use environment variables
   export WW_IPFS=http://192.168.1.100:5001
   export WW_LOGLVL=debug
   cargo run -- run
   ```

### Command Options

The `run` subcommand supports the following options:

- `--ipfs <IPFS>`: IPFS node HTTP API endpoint (e.g., http://127.0.0.1:5001)
- `--loglvl <LEVEL>`: Log level (trace, debug, info, warn, error)
- `--preset <PRESET>`: Use preset configuration (minimal, development, production)
- `--env-config`: Use configuration from environment variables

### Building and Running WASM Examples

WASM examples are built automatically when you run `make` or `make examples`. The `default-kernel` example is included by default.

**Quick start**:
```bash
# Build everything (including examples)
make

# Run the default-kernel example
./target/release/ww run /boot --volume examples/default-kernel/target/wasm32-wasip2/release:/boot
```

To test `ww run` with the `default-kernel` example:

1. **Build the WASM file** (already done if you ran `make`):
   ```bash
   make example-default-kernel
   ```
   
   Or run directly from the example directory:
   ```bash
   make -C examples/default-kernel build
   ```
   
   The example Makefile handles building and ensuring the output is named `main.wasm`.

2. **Run with local filesystem mount**:
   ```bash
   cargo run -- run /app \
     --volume examples/default-kernel/target/wasm32-wasip2/release:/app
   ```

3. **Export to IPFS and run from IPFS** (optional):
   ```bash
   # Build and export to IPFS
   make example-default-kernel-ipfs
   
   # Or run directly:
   make -C examples/default-kernel ipfs
   
   # This will output an IPFS hash like: /ipfs/QmHash...
   # Then run with:
   cargo run -- run /ipfs/QmHash...
   ```

**Note**: The `ww run` command expects `{path}/main.wasm`, so when using volume mounts, ensure `main.wasm` exists in the mounted directory. The example Makefiles handle this automatically.

## Docker Support

The project includes a multi-stage Docker build for containerized deployment and distribution.

### Building with Podman

```bash
# Build the container image
make podman-build
# or
podman build -t wetware:latest .

# Run the container
make podman-run
# or
podman run --rm -it wetware:latest

# Clean up container images
make podman-clean
```

### Container Features

- **Multi-stage build**: Optimizes image size by separating build and runtime stages
- **Security**: Runs as non-root user (`wetware`)
- **Efficient caching**: Leverages container layer caching for faster builds
- **Minimal runtime**: Based on Debian Bookworm slim for smaller footprint

**Note**: When running the container, you'll need to use the `run` subcommand:
```bash
# Run the container with the run subcommand
podman run --rm -it wetware:latest run

# With custom options
podman run --rm -it wetware:latest run --ipfs http://host.docker.internal:5001 --loglvl debug
```

### Podman Compose (Optional)

Create a `docker-compose.yml` for easy development (works with both Docker and Podman):

```yaml
version: '3.8'
services:
  wetware:
    build: .
    ports:
      - "8080:8080"
    environment:
      - WW_IPFS=http://host.docker.internal:5001
      - WW_LOGLVL=info
    volumes:
      - ./config:/app/config
    command: ["run"]  # Use the run subcommand
```

## CI/CD Pipeline

The project includes GitHub Actions workflows for automated testing, building, and publishing.

### Workflow Features

- **Automated Testing**: Runs on every push and pull request
- **Code Quality**: Includes formatting checks and clippy linting
- **Release Automation**: Automatically builds and publishes artifacts on releases
- **Docker Integration**: Builds and pushes Docker images to registry
- **Artifact Publishing**: Creates distributable binaries and archives

### Triggering Releases

1. **Create a GitHub release** with a semantic version tag (e.g., `v1.0.0`)
2. **Workflow automatically**:
   - Builds the Rust application
   - Creates release artifacts (binary + tarball)
   - Builds and pushes Docker images
   - Uploads artifacts to GitHub releases

### Required Secrets

For Docker publishing, set these repository secrets:
- `DOCKER_USERNAME`: Your Docker Hub username
- `DOCKER_PASSWORD`: Your Docker Hub access token

### Manual Workflow Triggers

```bash
# Test only
gh workflow run rust.yml --ref main

# Build Docker image (on main branch)
gh workflow run rust.yml --ref main
```

## Logging Configuration

The application uses structured logging with the `tracing` crate. You can configure log levels using environment variables:

### Environment Variables

- **`WW_IPFS`**: IPFS node HTTP API endpoint (defaults to http://localhost:5001)
  ```bash
  # Use default localhost endpoint
  export WW_IPFS=http://localhost:5001
  
  # Use custom IPFS node
  export WW_IPFS=http://192.168.1.100:5001
  
  # Use remote IPFS node
  export WW_IPFS=https://ipfs.example.com:5001
  ```

- **`WW_LOGLVL`**: Controls the log level (trace, debug, info, warn, error)
  ```bash
  # Set log level for all components
  export WW_LOGLVL=info
  
  # More verbose logging
  export WW_LOGLVL=debug
  export WW_LOGLVL=trace
  
  # Only show warnings and errors
  export WW_LOGLVL=warn
  export WW_LOGLVL=error
  ```

### Log Levels

- **`error`**: Errors that need immediate attention
- **`warn`**: Warnings about potential issues
- **`info`**: General information about application flow
- **`debug`**: Detailed debugging information
- **`trace`**: Very detailed tracing (very verbose)

## How It Works

1. **Configuration**: Determines IPFS endpoint from command line, environment variable, or default
2. **Peer Discovery**: Queries the configured IPFS node's HTTP API to discover connected peers
3. **Host Creation**: Generates Ed25519 keypair and creates libp2p swarm with IPFS-compatible protocols
4. **DHT Bootstrap**: Adds discovered peers to Kademlia routing table and establishes connections
5. **Network Integration**: Joins the IPFS DHT network and participates in DHT operations
6. **DHT Operations**: Can provide content and query for providers in the IPFS network

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
- **Connection errors**: Check if Kubo is listening on the expected port and endpoint
- **DHT bootstrap failures**: Ensure Kubo has peers and the API endpoint is correct
- **Protocol compatibility**: The application uses standard IPFS protocols for full compatibility
- **RSA connection errors**: RSA support is included for legacy IPFS peers
- **Configuration issues**: Check `WW_IPFS` environment variable for correct IPFS endpoint
- **Logging issues**: Check `WW_LOGLVL` environment variable and ensure tracing is properly initialized

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
