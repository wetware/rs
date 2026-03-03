# Chess Example

Two-node cross-network chess over libp2p ByteStreams.

Each node registers a `/ww/0.1.0/chess` listener, announces itself on
the Kademlia DHT, discovers peers, and plays random UCI moves over a
bidirectional stream. Every completed game publishes a content-addressed
replay log to IPFS (see [doc/replay.md](doc/replay.md)).

## Prerequisites

A running Kubo node for DHT bootstrap:

```sh
ipfs daemon
```

## Running

Stack the chess layer on top of the kernel. The kernel reads
`etc/init.d/chess.glia` and handles listener registration, DHT
announcement, and peer discovery automatically.

```sh
# Terminal 1
cargo run --bin ww -- run --port=2025 crates/kernel examples/chess

# Terminal 2
cargo run --bin ww -- run --port=2026 crates/kernel examples/chess
```

Both nodes bootstrap into the DHT, exchange provider records, discover
each other, and play a game of random chess over the stream.

## Tests

```sh
cargo test -p chess --lib
```
