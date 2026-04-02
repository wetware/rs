# Discovery -- Greeter RPC

Two-agent Greeter demo showing schema-keyed peer discovery over
the DHT. Agent A publishes a Greeter service. Agent B discovers
it by schema CID alone, dials it via Cap'n Proto RPC, and gets a
typed greeting back. No configuration, no service registry, no
hardcoded addresses.

## What it demonstrates

- **Cap'n Proto cell** (`WW_CELL_MODE=vat`) -- schema-keyed RPC
- `VatListener` for per-connection capability cells
- `VatClient` for typed RPC dialing
- Schema-keyed DHT discovery via `routing.provide()` / `findProviders()`
- Dual-mode binary: cell mode (RPC server) + service mode (discovery loop)
- Exponential backoff with jitter for peer discovery

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
make discovery
```

This compiles the WASM guest and copies the compiled schema bytes
(`discovery.schema`) next to the binary. The schema is passed
explicitly via RPC at runtime -- no custom sections.

## Running

### Step 1: Boot the nodes

Stack the discovery layer on top of the kernel. The init.d script
registers the greeter cell with the host's `VatListener`.

```sh
# Terminal A -- boots Agent A, provides Greeter on DHT
ww run --port=2025 crates/kernel examples/discovery

# Terminal B -- boots Agent B, discovers A, calls greet()
ww run --port=2026 crates/kernel examples/discovery
```

Each terminal drops you into a Glia shell.

### Step 2: Start the service

From each Glia shell, run the discovery demo in service mode:

```clojure
/ > (executor run (load "bin/discovery.wasm"))
```

Expected output on Agent B:

```
[INFO] service: peer ..a1b2c3d4
[INFO] service: schema CID bafy...
[INFO] service: looking for peers...
[INFO] service: found 1 peer(s)
[INFO] ..a1b2c3d4 -> ..e5f6g7h8: Hello, peer ..a1b2c3d4! I'm ..e5f6g7h8
```

## How it works

```
BUILD TIME:
  greeter.capnp --> capnpc --> greeter_schema.bin --> discovery.wasm + discovery.schema

AGENT A (service mode):                    AGENT B (service mode):
  membrane.graft()                           membrane.graft()
  routing.provide(CID)  --DHT-->            routing.find_providers(CID)
                                             |
                         <--libp2p stream--  vat_client.dial(A, schema)
  VatListener accepts                        |
  spawns cell (cell mode)                    bootstrap --> Greeter cap
  cell serves Greeter                        greeter.greet("peer B")
                         --RPC response-->   "Hello, peer B! I'm A"
```

The schema CID is derived deterministically from the Greeter
interface definition: `CIDv1(raw, BLAKE3(canonical(schema.Node)))`.
Two nodes with the same schema automatically find each other on
the Kademlia DHT.

### Schema

```capnp
interface Greeter {
  greet @0 (name :Text) -> (greeting :Text);
}
```

### Cell mode vs service mode

The same binary serves both roles:

- **Cell mode** (`WW_CELL_MODE=vat`): spawned by `VatListener`
  per incoming RPC connection. Creates a `GreeterImpl` and exports
  it via `system::serve()`. The host bridges the capability to the
  connecting peer.
- **Service mode** (default): long-running discovery loop. Provides
  the schema CID on the DHT, discovers peers via
  `routing.find_providers()`, dials them with `VatClient`, and calls
  `greet()`. Exponential backoff (2 s to 15 min).

## Init.d script

`etc/init.d/discovery.glia`:

```clojure
; Register RPC cell for the Greeter capability.
; VatListener spawns a cell per connection; the cell exports
; a Greeter capability via system::serve().
(host :listen executor (load "bin/discovery.wasm"))
```

The script registers the discovery binary with the host's
`VatListener`. The schema is read from `bin/discovery.schema`
(adjacent to the WASM binary). Each incoming RPC connection spawns
a fresh cell that exports a `Greeter` capability.

The service mode is started interactively from the Glia shell --
not from the init.d script.

## Without Kubo

The demo works without Kubo. Schema push to IPFS is best-effort
at build time. Discovery happens via DHT `provide/findProviders`
regardless.

## Tests

```sh
cargo test -p discovery
```

Runs unit tests for the Greeter implementation and RPC round-trip
tests over in-memory Cap'n Proto duplex.

## Files

```
examples/discovery/
├── Cargo.toml
├── Makefile               # make discovery
├── README.md              # this file
├── greeter.capnp          # Greeter schema source
├── bin/                   # build output (gitignored)
│   ├── discovery.wasm
│   └── discovery.schema   # compiled schema bytes
├── etc/
│   └── init.d/
│       └── discovery.glia # cell registration
└── src/
    └── lib.rs             # guest implementation
```
