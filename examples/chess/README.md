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

### Layered (kernel + chess overlay)

Stack the chess layer on top of the kernel. The kernel reads
`etc/init.d/chess.glia` and handles listener registration, DHT
announcement, and peer discovery automatically.

```sh
# Terminal 1
cargo run --bin ww -- run --port=2025 crates/kernel examples/chess

# Terminal 2
cargo run --bin ww -- run --port=2026 crates/kernel examples/chess
```

### Standalone (legacy)

The chess binary also works as a standalone image for backward
compatibility:

```sh
# Terminal 1
cargo run --bin ww -- run --port=2025 examples/chess

# Terminal 2
cargo run --bin ww -- run --port=2026 examples/chess
```

Both nodes bootstrap into the DHT, exchange provider records, discover
each other, and play a game of random chess over the stream.

At the end of each game the root CID of the replay log is printed:

```
game ..a1b2 vs ..c3d4: replay -> bafkrei...
```

Fetch it with `ipfs cat <cid>` and follow the `prev` links to walk the
full move history. See [doc/replay.md](doc/replay.md) for the data
structure and schema.

## Tests

```sh
cargo test -p chess --lib
```
