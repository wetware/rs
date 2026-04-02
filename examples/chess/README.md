# Chess Example

Two-node cross-network chess over libp2p RPC capabilities.

## How it works

The `ww run` command takes one or more **image layers** as positional args.
Each layer is a directory that gets merged into a single FHS root, left to
right. The kernel (PID 0) sees this merged root as its virtual filesystem.

```sh
ww run --port=2025 crates/kernel examples/chess
```

Here, `crates/kernel` is the base layer (the kernel WASM binary) and
`examples/chess` is stacked on top. After merging, the kernel's filesystem
contains everything from both directories. Concretely, the contents of
`examples/chess/` are available to PID 0 under `$WW_ROOT/`:

```
$WW_ROOT/
├── bin/
│   └── chess-demo.wasm    ← from examples/chess (built by `make chess`)
├── etc/
│   └── init.d/
│       └── chess.glia     ← from examples/chess
├── boot/
│   └── main.wasm          ← from crates/kernel
└── ...
```

The host publishes this merged directory to IPFS and sets `$WW_ROOT` to
`/ipfs/<cid>`. When the kernel's init system reads `etc/init.d/chess.glia`,
the `(perform :load "bin/chess-demo.wasm")` call resolves the path relative
to `$WW_ROOT`, fetching the bytes from the merged image via the WASI
filesystem interceptor.

This is the key idea: **any directory can be an image layer.** You build a
chess demo by putting the right files in the right FHS paths and stacking
the directory onto the kernel.

## Architecture

```
              kernel boot
                  │
          chess.glia evaluated
             ┌────┴────┐
   (host listen)     (executor run)
          │              │
      cell mode      service mode
    (per-connection) (discovery loop)
```

Two execution modes, selected by the init.d script:

- **Cell** (`WW_CELL`): per-connection RPC cell spawned by
  `VatListener`. Creates a `ChessEngineImpl` and exports it via
  `system::serve()`. The host bridges the capability to the connecting
  peer via Cap'n Proto RPC bootstrapping.
- **Service** (default): long-running discovery loop. Provides the
  schema CID on the DHT, discovers peers via `routing.find_providers()`,
  dials them with `VatClient` to get typed `ChessEngine` capabilities,
  and plays random games. Exponential backoff (2 s to 15 min).

Each node registers a `VatListener` for the ChessEngine schema,
announces the schema CID on the Kademlia DHT, discovers peers, and
plays random UCI moves via typed Cap'n Proto RPC. Every completed game
publishes a content-addressed replay log to IPFS (see
[doc/replay.md](doc/replay.md)).

## Init.d Script

`etc/init.d/chess.glia` is evaluated by the kernel at boot. Each form
is a capability invocation. The kernel binds `executor` as a capability
value in the Glia environment, and scripts pass it explicitly to
functions that need spawn authority.

```clojure
; Register RPC cell — schema loaded from boot/main.schema.
; The executor is passed explicitly (no ambient authority).
(host listen executor (load "bin/chess-demo.wasm"))

; Run the chess demo in service mode — blocks until exit.
(executor run (load "bin/chess-demo.wasm"))
```

`(load "bin/chess-demo.wasm")` reads bytes from the WASI virtual
filesystem, resolved relative to `$WW_ROOT` (the merged image root).
The executor capability is the spawn authority that `VatListener`
needs to create cell processes. The kernel reads `boot/main.schema`
to derive the protocol CID.

## Schema CID

The protocol address is derived at build time from the ChessEngine
Cap'n Proto schema: `CIDv1(raw, BLAKE3(canonical(schema.Node)))`.
This CID serves as both the DHT key and the subprotocol address
(`/ww/0.1.0/<cid>`). The canonical schema bytes are stored in
`boot/main.schema` alongside the WASM binary. The kernel reads
this file at boot to derive the CID. See `build.rs` and the
`schema-id` crate for the compilation step.

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

Stack the chess layer on top of the kernel:

```sh
# Terminal 1
ww run --port=2025 crates/kernel examples/chess

# Terminal 2
ww run --port=2026 crates/kernel examples/chess
```

Both nodes bootstrap into the DHT, exchange provider records, discover
each other, and play a game of random chess via typed RPC.

## Tests

```sh
cargo test -p chess --lib
```

## See also

- [doc/replay.md](doc/replay.md) — replay log structure
