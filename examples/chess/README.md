# Chess Engine

Two-node cross-network chess over libp2p RPC capabilities.

## What it demonstrates

- **Cap'n Proto cell** (`WW_CELL_MODE=vat`) -- schema-keyed RPC
- `VatListener` for per-connection capability cells
- `VatClient` for typed RPC dialing
- Schema-keyed DHT discovery via `routing.provide()` / `findProviders()`
- IPFS replay log publishing
- Dual-mode binary: cell mode (RPC server) + service mode (discovery loop)

## Prerequisites

- Rust toolchain with `wasm32-wasip2` target:
  ```sh
  rustup target add wasm32-wasip2
  ```
- A running Kubo node for DHT bootstrap:
  ```sh
  ipfs daemon
  ```

## Building

```sh
make chess
```

This compiles the WASM guest and copies the compiled schema bytes
(`chess-demo.schema`) next to the binary. The schema is passed
explicitly via RPC at runtime -- no custom sections.

## Running

### Step 1: Boot the nodes

Stack the chess layer on top of the kernel. The init.d script
registers the chess cell with the host's `VatListener`.

```sh
# Terminal 1
ww run --port=2025 crates/kernel examples/chess

# Terminal 2
ww run --port=2026 crates/kernel examples/chess
```

Each terminal drops you into a Glia shell.

### Step 2: Start the service

From each Glia shell, run the chess demo in service mode:

```clojure
/ > (perform runtime :run (load "bin/chess-demo.wasm") "serve")
```

Both nodes bootstrap into the DHT, exchange provider records,
discover each other, and play a game of random chess via typed RPC.

## How it works

### Image layers

The `ww run` command takes one or more **image layers** as positional
args. Each layer is a directory that gets merged into a single FHS
root, left to right. The kernel (PID 0) sees this merged root as
its virtual filesystem.

```
$WW_ROOT/
├── bin/
│   └── chess-demo.wasm    <- from examples/chess (built by make chess)
├── etc/
│   └── init.d/
│       └── chess.glia     <- from examples/chess
├── boot/
│   └── main.wasm          <- from crates/kernel
└── ...
```

The host publishes this merged directory to IPFS and sets `$WW_ROOT`
to `/ipfs/<cid>`. When the kernel's init system reads
`etc/init.d/chess.glia`, the `(load "bin/chess-demo.wasm")` call
resolves the path relative to `$WW_ROOT`, fetching the bytes from
the merged image via the WASI filesystem interceptor.

### Architecture

```
                 kernel boot
                      |
             chess.glia evaluated
                      |
         (perform host :listen ...)
                      |
                  cell mode
               (per-connection)
```

Two execution modes, selected by the init.d script:

- **Cell mode** (`WW_CELL_MODE=vat`): per-connection vat cell
  spawned by `VatListener`. Creates a `ChessEngineImpl` and exports
  it via `system::serve()`. The host bridges the capability to the
  connecting peer via Cap'n Proto RPC bootstrapping.
- **Service mode** (default): long-running discovery loop. Provides
  the schema CID on the DHT, discovers peers via
  `routing.find_providers()`, dials them with `VatClient` to get
  typed `ChessEngine` capabilities, and plays random games.
  Exponential backoff (2 s to 15 min).

### Schema CID

The protocol address is derived at build time from the ChessEngine
Cap'n Proto schema: `CIDv1(raw, BLAKE3(canonical(schema.Node)))`.
This CID serves as both the DHT key and the subprotocol address
(`/ww/0.1.0/vat/{cid}`). Schema bytes are compiled at build time
and passed explicitly via RPC -- the host reads `bin/chess-demo.schema`
from the image to derive the CID.

### Schema

```capnp
interface ChessEngine {
  getState      @0 () -> (fen :Text);
  applyMove     @1 (uci :Text) -> (ok :Bool, reason :Text);
  getLegalMoves @2 () -> (moves :List(Text));
  getStatus     @3 () -> (status :GameStatus);

  enum GameStatus {
    ongoing   @0;
    checkmate @1;
    stalemate @2;
    draw      @3;
  }
}
```

## Init.d script

`etc/init.d/chess.glia`:

```clojure
; Register vat cell for the ChessEngine capability.
; VatListener spawns a cell process per connection; the cell exports
; a ChessEngine capability via system::serve().
(def chess-wasm (load "bin/chess-demo.wasm"))
(def chess-schema (load "bin/chess-demo.schema"))

(perform host :listen executor chess-wasm chess-schema)
```

The script registers the chess binary with the host's `VatListener`.
The schema is read from `bin/chess-demo.schema` (adjacent to the
WASM binary). Each incoming RPC connection spawns a fresh cell that
exports a `ChessEngine` capability.

The service mode is started interactively from the Glia shell --
not from the init.d script. The executor capability is passed
explicitly -- no ambient authority.

## Tests

```sh
cargo test -p chess --lib
```

## See also

- [doc/replay.md](doc/replay.md) -- replay log structure

## Files

```
examples/chess/
├── Cargo.toml
├── Makefile              # make chess
├── README.md             # this file
├── chess.capnp           # ChessEngine schema source
├── bin/                  # build output (gitignored)
│   ├── chess-demo.wasm
│   └── chess-demo.schema # compiled schema bytes
├── doc/
│   └── replay.md         # replay log format
├── etc/
│   └── init.d/
│       └── chess.glia    # cell registration + service launch
└── src/
    └── lib.rs            # guest implementation
```
