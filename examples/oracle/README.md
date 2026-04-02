# Oracle -- Gas Price Feed

Decentralized gas price oracle over schema-keyed RPC. Fetches live
gas prices from Blocknative via `HttpClient`, serves them over typed
Cap'n Proto RPC, and advertises on the DHT for peer discovery.

## What it demonstrates

- **Cap'n Proto cell** (`WW_CELL_MODE=vat`) -- schema-keyed RPC
- `VatListener.serve()` for persistent capability cells
- `HttpClient` capability for outbound HTTP (domain-scoped)
- Schema-keyed DHT discovery via `routing.provide()` / `findProviders()`
- Three-mode binary: cell mode (RPC server), service mode (DHT provider), consumer mode (price querier)
- Periodic background price refresh within a long-lived cell

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
make oracle
```

This compiles the WASM guest and copies the compiled schema bytes
(`oracle.schema`) next to the binary. The schema is passed
explicitly via RPC at runtime -- no custom sections.

## Running

### Step 1: Boot the oracle node

Stack the oracle layer on top of the kernel. The init.d script
registers the oracle cell with the host's `VatListener`.

```sh
# Terminal 1 -- oracle provider
ww run --port=2025 crates/kernel examples/oracle
```

This drops you into a Glia shell.

### Step 2: Start the oracle service

From the Glia shell, run the oracle in service mode. This provides
the schema CID on the DHT and re-provides periodically:

```clojure
/ > (perform runtime :run (load "bin/oracle.wasm") "serve")
```

### Step 3: Query from a consumer (optional)

Open a second terminal and boot a consumer node:

```sh
# Terminal 2 -- price consumer
WW_CONSUMER=1 ww run --port=2026 crates/kernel examples/oracle
```

From the consumer's Glia shell:

```clojure
/ > (perform runtime :run (load "bin/oracle.wasm") "consume")
```

The consumer discovers the oracle provider via DHT, dials it with
`VatClient`, and queries gas prices:

```
[INFO] consumer: peer ..a1b2c3d4
[INFO] consumer: looking for oracle providers...
[INFO] consumer: found 1 oracle provider(s)
[INFO] ..e5f6g7h8: ETH/gas = 30.12 gwei (confidence 99%)
```

## How it works

### Architecture

```
ORACLE NODE:                          CONSUMER NODE:
  init.d registers cell                 init.d registers cell
  service mode:                         consumer mode:
    membrane.graft()                      membrane.graft()
    routing.provide(CID)  --DHT-->       routing.find_providers(CID)
    re-provide loop                       |
                                         vat_client.dial(oracle, schema)
  VatListener accepts   <--libp2p--      |
  spawns cell (cell mode)                bootstrap --> PriceOracle cap
    membrane.graft()                     oracle.get_pairs() -> ["ETH/gas", ...]
    http_client.get(blocknative)         oracle.get_price("ETH/gas")
    cache prices                          |
    serve PriceOracle     --RPC-->       display prices
    refresh loop (30s)
```

### Three execution modes

- **Cell mode** (`WW_CELL_MODE=vat`): spawned by `VatListener`
  per incoming RPC connection. Creates a `PriceOracleImpl`, grafts
  to obtain `HttpClient`, fetches prices from Blocknative, and
  exports the oracle as the bootstrap capability. Refreshes prices
  every 30-60 seconds while the connection is alive.
- **Service mode** (default): long-running DHT provider loop.
  Provides the schema CID on the DHT and re-provides periodically
  (DHT records expire).
- **Consumer mode** (`WW_CONSUMER=1`): discovers oracle providers
  via DHT, dials them with `VatClient`, queries available pairs
  and prices. Exponential backoff (2 s to 60 s).

### Schema

```capnp
interface PriceOracle {
  getPrice @0 (pair :Text) -> (price :Int64, decimals :UInt8,
                                timestamp :Int64, confidence :Float64);
  getPairs @1 () -> (pairs :List(Text));
}
```

Supported pairs: `ETH/gas`, `POLYGON/gas`, `BASE/gas`.

### Price fetching

The cell uses the `HttpClient` capability (obtained via
`membrane.graft()`) to call the Blocknative gas price API.
`HttpClient` is domain-scoped -- the host controls which domains
the cell can reach. Prices are cached in-process and served via
RPC. Confidence decays toward 0.0 if data goes stale.

## Init.d script

`etc/init.d/oracle.glia`:

```clojure
; Register RPC cell for the PriceOracle capability.
; VatListener spawns a cell per connection; the cell exports
; a PriceOracle capability via system::serve().
(def oracle-wasm (load "bin/oracle.wasm"))
(def oracle-schema (load "bin/oracle.schema"))

(perform host :listen executor oracle-wasm oracle-schema)
```

The script registers the oracle binary with the host's
`VatListener`. The schema is read from `bin/oracle.schema`
(adjacent to the WASM binary). Each incoming RPC connection spawns
a fresh cell that exports a `PriceOracle` capability, fetches
prices via `HttpClient`, and refreshes them in the background.

The service and consumer modes are started interactively from the
Glia shell -- not from the init.d script.

## Tests

```sh
cargo test -p oracle
```

Runs unit tests for cache initialization, JSON parsing, and RPC
round-trip tests over in-memory Cap'n Proto duplex (get_price,
get_pairs, unknown pair error).

## Files

```
examples/oracle/
├── Cargo.toml
├── Makefile              # make oracle
├── README.md             # this file
├── oracle.capnp          # PriceOracle schema source
├── bin/                  # build output (gitignored)
│   ├── oracle.wasm
│   └── oracle.schema     # compiled schema bytes
├── etc/
│   └── init.d/
│       └── oracle.glia   # cell registration
└── src/
    └── lib.rs            # guest implementation
```
