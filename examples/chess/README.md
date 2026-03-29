# Chess Example

Two-node cross-network chess over libp2p RPC capabilities.

Each node registers an `RpcListener` for the ChessEngine schema,
announces the schema CID on the Kademlia DHT, discovers peers, and
plays random UCI moves via typed Cap'n Proto RPC. Every completed game
publishes a content-addressed replay log to IPFS (see
[doc/replay.md](doc/replay.md)).

## Architecture

```
              kernel boot
                  │
          chess.glia evaluated
             ┌────┴────┐
   (host rpcListen)  (executor run)
          │              │
     handler mode    service mode
    (per-connection) (discovery loop)
```

Two execution modes, selected by the init.d script:

- **Handler** (`WW_HANDLER`): per-connection RPC handler spawned by
  `RpcListener`. Creates a `ChessEngineImpl` and exports it via
  `system::serve()`. The host bridges the capability to the connecting
  peer via Cap'n Proto RPC bootstrapping.
- **Service** (default): long-running discovery loop. Provides the
  schema CID on the DHT, discovers peers via `routing.find_providers()`,
  dials them with `RpcDialer` to get typed `ChessEngine` capabilities,
  and plays random games. Exponential backoff (2 s to 15 min).

## Schema CID

The protocol address is derived at build time from the ChessEngine
Cap'n Proto schema: `CIDv1(raw, BLAKE3(canonical(schema.Node)))`.
This CID serves as both the DHT key and the subprotocol address
(`/ww/0.1.0/<cid>`). The canonical schema bytes are embedded in the
WASM binary as a custom section named `schema.capnp`. The host
extracts the section and derives the CID at runtime. See `build.rs`,
the `schema-id` crate, and `make chess` for the injection step.

## Init.d Script

`etc/init.d/chess.glia` is evaluated by the kernel at boot. Each form
is a capability invocation — inner expressions resolve first.

```clojure
; Register RPC handler — schema extracted from WASM custom section.
(host rpcListen (ipfs cat "bin/chess-demo.wasm"))

; Run the chess demo in service mode — blocks until exit.
(executor run (ipfs cat "bin/chess-demo.wasm"))
```

`(ipfs cat "bin/chess-demo.wasm")` is resolved relative to `$WW_ROOT`
(the merged IPFS image root set by the host at kernel spawn time).
`rpcListen` takes a single argument: the handler WASM binary. The host
inspects the `schema.capnp` custom section to determine the protocol.

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
`etc/init.d/chess.glia` and handles RPC handler registration, DHT
announcement, and peer discovery automatically.

```sh
# Terminal 1
cargo run --bin ww -- run --port=2025 crates/kernel examples/chess

# Terminal 2
cargo run --bin ww -- run --port=2026 crates/kernel examples/chess
```

Both nodes bootstrap into the DHT, exchange provider records, discover
each other, and play a game of random chess via typed RPC.

## Tests

```sh
cargo test -p chess --lib
```

## See also

- [doc/replay.md](doc/replay.md) — replay log structure
