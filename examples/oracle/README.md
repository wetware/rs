# Oracle -- Gas Price Feed

Decentralized gas price oracle over schema-keyed RPC and HTTP.
Fetches live gas prices from Blocknative via `HttpClient`, serves
them over typed Cap'n Proto RPC **and** HTTP/JSON, and advertises
on the DHT for peer discovery.

## What it demonstrates

- **Dual transport** -- one binary, two transports (vat RPC + HTTP)
- **Cap'n Proto cell** (`WW_CELL_MODE=vat`) -- schema-keyed RPC
- **WAGI cell** (`WW_CELL_MODE=http`) -- CGI, curl-friendly JSON
- `HttpClient` capability for outbound HTTP (domain-scoped)
- `with`/`cell`/`listen` DX for capability-scoped cell definitions
- Schema-keyed DHT discovery via `routing.provide()` / `findProviders()`
- Four-mode binary: vat cell, WAGI cell, service (DHT), consumer (query)

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
registers the oracle cell on both transports.

```sh
# Terminal 1 -- oracle provider
ww run --http-listen 127.0.0.1:8080 --port=2025 std/kernel examples/oracle
```

This drops you into a Glia shell. The oracle is now serving on:
- **RPC** (libp2p, schema-keyed) -- for peer-to-peer typed queries
- **HTTP** at `http://localhost:8080/oracle` -- for curl/browser

### Step 2: Query via curl

```sh
# All pairs
curl http://localhost:8080/oracle

# Single pair
curl 'http://localhost:8080/oracle?pair=ETH%2Fgas'
```

Example response:

```json
{
  "pairs": {
    "ETH/gas": {
      "price": 30.12,
      "unit": "gwei",
      "confidence": 0.99,
      "timestamp": 1700000000
    },
    "POLYGON/gas": { ... },
    "BASE/gas": { ... }
  }
}
```

### Step 3: Start the DHT service (optional)

From the Glia shell, run the oracle in service mode. This provides
the schema CID on the DHT and re-provides periodically:

```clojure
/ > (perform runtime :run (load "bin/oracle.wasm") "serve")
```

### Step 4: Query from a consumer (optional)

Open a second terminal and boot a consumer node:

```sh
# Terminal 2 -- price consumer
WW_CONSUMER=1 ww run --port=2026 std/kernel examples/oracle
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
ORACLE NODE:                            CURL CLIENT:
  init.d registers cell on              curl http://localhost:8080/oracle
  two transports (vat + http)              |
                                           v
  HttpListener accepts     <---HTTP---  axum server
  spawns cell (http mode)               routes by prefix
    membrane.graft()                       |
    http_client.get(blocknative)           v
    build JSON response                 CGI response -> JSON
    write to stdout

                                        CONSUMER NODE:
  VatListener accepts      <--libp2p--  vat_client.dial(oracle, schema)
  spawns cell (cell mode)               bootstrap --> PriceOracle cap
    membrane.graft()                    oracle.get_pairs() -> ["ETH/gas", ...]
    http_client.get(blocknative)        oracle.get_price("ETH/gas")
    cache prices                           |
    serve PriceOracle       --RPC-->    display prices
    refresh loop (30s)
```

### Four execution modes

The same binary serves all modes. Detection:

| Mode | Trigger | Transport |
|------|---------|-----------|
| **Vat cell** | No args, no `REQUEST_METHOD` | Cap'n Proto RPC over libp2p |
| **WAGI cell** | `REQUEST_METHOD` env var present | CGI over stdin/stdout |
| **Service** | `serve` subcommand | DHT provide loop |
| **Consumer** | `consume` subcommand | DHT discover + RPC query |

- **Vat cell mode** (`WW_CELL_MODE=vat`): spawned by `VatListener`
  per incoming RPC connection. Creates a `PriceOracleImpl`, grafts
  to obtain `HttpClient`, fetches prices from Blocknative, and
  exports the oracle as the bootstrap capability. Refreshes prices
  every 30-60 seconds while the connection is alive.
- **WAGI cell mode** (`WW_CELL_MODE=http`): spawned by `HttpListener`
  per HTTP request. Grafts the membrane over `wetware:streams`
  (side-channel), fetches prices via `HttpClient`, writes a JSON
  response to stdout via CGI. Stateless -- one cell per request.
- **Service mode**: long-running DHT provider loop. Provides the
  schema CID on the DHT and re-provides periodically (records expire).
- **Consumer mode**: discovers oracle providers via DHT, dials them
  with `VatClient`, queries prices. Exponential backoff (2 s to 60 s).

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
;; Grant an HttpClient capability and define the cell.
(def oracle
  (with [(http (perform host :http-client))]
    (cell (load "bin/oracle.wasm")
          (load "bin/oracle.schema"))))

;; Mount on both transports.
(perform host :listen oracle)              ;; vat RPC
(perform host :listen oracle "/oracle")    ;; HTTP/WAGI
```

**`with`** creates local capability bindings (sugar over `let`).
**`cell`** bundles wasm + schema + all capabilities from scope.
**`(perform host :listen ...)`** registers the cell with the host:
- Cell alone → VatListener (schema-keyed RPC over libp2p)
- Cell + path → HttpListener (WAGI at the given prefix)

The same binary handles both transports. It detects HTTP mode via
the `REQUEST_METHOD` CGI env var (injected by HttpListener).

The service and consumer modes are started interactively from the
Glia shell -- not from the init.d script.

## Tests

```sh
cargo test --manifest-path examples/oracle/Cargo.toml
```

Runs unit tests for cache initialization, JSON parsing, JSON
response building, and RPC round-trip tests over in-memory Cap'n
Proto duplex (get_price, get_pairs, unknown pair error).

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
│       └── oracle.glia   # cell registration (dual transport)
└── src/
    └── lib.rs            # guest implementation
```
