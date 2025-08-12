# Basic P2P

A minimal Rust libp2p application that connects to a local Kubo (IPFS) node and publishes signed peer records to the DHT.

## Features

- **Auto-detects Kubo API**: Reads `$IPFS_PATH` or defaults to `~/.ipfs/api`
- **Creates libp2p Host**: Generates Ed25519 identity and listens on TCP
- **Publishes Peer Records**: Signs and publishes peer records to the DHT via Kubo
- **DHT Queries**: Queries the DHT for published peer records

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

2. **Run the application**:
   ```bash
   cargo run
   ```

## Expected Output

```
Starting basic-p2p application...
Connected to Kubo at http://127.0.0.1:5001
Kubo PeerId: QmX...
Local PeerId: 12D3KooW...
Listening on: /ip4/127.0.0.1/tcp/12345
Kubo API: http://127.0.0.1:5001
Published signed peer record at key: /p2p/peer-record/12D3KooW...
Querying DHT for our peer record...
Peer record not found yet. This is normal - DHT propagation takes time.
Raw response: 
Try running the query again in a few seconds.
```

## How It Works

1. **API Detection**: Reads the Kubo API endpoint from `~/.ipfs/api` file
2. **Host Creation**: Generates Ed25519 keypair and creates a libp2p swarm
3. **Peer Record**: Builds a signed peer record with listen addresses
4. **DHT Publishing**: Publishes the record to Kubo's DHT
5. **DHT Query**: Queries the DHT to verify the record was published

## Troubleshooting

- **"IPFS API file not found"**: Make sure Kubo is running (`kubo daemon`)
- **Connection errors**: Check if Kubo is listening on the expected port
- **DHT not found**: DHT propagation takes time; wait a few seconds and retry

## Dependencies

- `libp2p`: P2P networking stack
- `reqwest`: HTTP client for Kubo API
- `tokio`: Async runtime
- `anyhow`: Error handling
- `base64`: Encoding/decoding
- `serde`: JSON handling
