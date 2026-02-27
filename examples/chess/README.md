# Chess Example

Two-node cross-network chess over libp2p ByteStreams.

Each node registers a `/ww/0.1.0/chess` listener, announces itself on
the Kademlia DHT, discovers peers, and plays random UCI moves over a
bidirectional stream.

## Prerequisites

A running Kubo node for DHT bootstrap:

```sh
ipfs daemon
```

## Running

Open two terminals and start each node on a different port:

```sh
# Terminal 1
cargo run --bin ww -- run --port=2025 examples/chess

# Terminal 2
cargo run --bin ww -- run --port=2026 examples/chess
```

Both nodes bootstrap into the DHT, exchange provider records, discover
each other, and play a game of random chess over the stream.

## Tests

```sh
cargo test -p chess --lib
```
