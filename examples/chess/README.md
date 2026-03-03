# Chess Example

Two-node cross-network chess over libp2p ByteStreams.

Each node registers a `/ww/0.1.0/chess` listener, announces itself on
the Kademlia DHT, discovers peers, and plays random UCI moves over a
bidirectional stream. Every completed game publishes a content-addressed
replay log to IPFS (see [doc/replay.md](doc/replay.md)).

## Architecture

```
              kernel boot
                  │
          chess.glia evaluated
             ┌────┴────┐
     (host listen)  (executor run)
          │              │
     handler mode    service mode
    (per-connection) (discovery loop)
```

Two execution modes, selected by the init.d script:

- **Handler** (`WW_HANDLER`): per-connection bytestream handler spawned
  by the Listener. Reads/writes newline-delimited UCI moves on
  stdin/stdout.
- **Service** (default): long-running discovery loop. DHT
  provide/findProviders with exponential backoff (2 s → 15 min).

## Init.d Script

`etc/init.d/chess.glia` is evaluated by the kernel at boot. Each form
is a capability invocation — inner expressions resolve first.

```clojure
; Register handler on chess protocol.
(host listen "chess" (ipfs cat "bin/chess-demo.wasm"))

; Run the chess demo in service mode — blocks until exit.
(executor run (ipfs cat "bin/chess-demo.wasm")
  :env {"WW_NS" "ww.chess.v1"})
```

`(ipfs cat "bin/chess-demo.wasm")` is resolved relative to `$WW_ROOT`
(the merged IPFS image root set by the host at kernel spawn time).

## Prerequisites

A running Kubo node for DHT bootstrap:

```sh
ipfs daemon
```

## Building

```sh
make chess
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

## See also

- [doc/replay.md](doc/replay.md) — replay log structure
