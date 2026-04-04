# Fuel Auction

Peer-to-peer compute marketplace via RFQ (request-for-quote) protocol.

## What it demonstrates

- **ComputeProvider as a vat cell** -- the auction provider is a regular WASM guest, not host-side code
- `FuelPolicy::Oneshot` for budget-tracked cell execution (cells trap at fuel exhaustion)
- `Identity.signer()` for quote signing + `Identity.verify()` for signature verification
- `VatHandler::Serve` for persistent capability export (one auction object serves all bidders)
- Schema-keyed DHT discovery via `routing.provide()` / `findProviders()`
- `Runtime.load()` + `Executor.spawn()` with fuel policy for spawning metered child cells
- Nonce-based replay prevention with TTL pruning

## How it works

```
Consumer                     DHT                     Provider
    |                         |                         |
    |-- find_providers(CP) -->|                         |
    |<-- [provider_A, ...] ---|                         |
    |                         |                         |
    |-- dial + login ---------------------------------->|
    |<-- ComputeProvider cap ----------------------------
    |                         |                         |
    |-- quote(wasm_cid, 5M) --------------------------->|
    |<-- Quote{sig, price=120, nonce=42} ---------------
    |                         |                         |
    |-- accept(Quote) --------------------------------->|
    |    [verify sig, check nonce+expiry, spawn cell]   |
    |<-- Process ----------------------------------------
    |                         |                         |
    |  ... cell runs, fuel ticks down ...               |
    |  ... remaining_budget -> 0 -> Trap::OutOfFuel ... |
```

Quotes are **defunctionalized** -- plain structs, not live capabilities. A coordinator
agent can solicit quotes from multiple providers, compare them, and hand the best
quote to a worker agent. No live RPC references flow through the graph.

## Prerequisites

- Rust toolchain with `wasm32-wasip2` target:
  ```sh
  rustup target add wasm32-wasip2
  ```

## Building

```sh
make auction
```

## Schema

`auction.capnp` defines:

- **`Quote`** -- signed price commitment (price, fuel, expiry, CID binding, nonce, Ed25519 signature)
- **`ComputeProvider`** -- four methods:
  - `quote(wasmCid, fuelRequested)` -- get a signed quote
  - `accept(quote)` -- redeem a quote, spawn a metered cell
  - `price()` -- spot rate for quick comparison
  - `status()` -- live auction state for metrics

## Pricing

Posted price with utilization multiplier:

```
price = base_price * (1 + committed / total_capacity)
```

At 0% utilization, price equals the base rate. At 50%, it's 1.5x. At 90%, 1.9x.
The operator sets `base_price` and `total_capacity`.

## Security

- **Replay prevention:** Each quote carries a random nonce. Redeemed nonces are tracked.
  Expired nonces are pruned on each `accept()`. Hard cap at 10K stored nonces.
- **CID binding:** `wasmCid` in the Quote binds the price commitment to a specific binary.
  Providers reject `accept()` if the CID doesn't match.
- **Quote expiry:** Quotes are valid for 5 minutes (`QUOTE_TTL_SECS`). Expired quotes
  are rejected on `accept()`.
- **Budget enforcement:** Spawned cells use `FuelPolicy::Oneshot`. The epoch callback
  in the host runtime deducts consumed fuel from `remaining_budget` and stops refueling
  at 0. The cell traps naturally via `Trap::OutOfFuel`.

## Running

### Step 1: Boot the node

Stack the auction layer on top of the kernel. The init.d script
registers the ComputeProvider cell with the host's `VatListener`.

```sh
ww run --port=2025 crates/kernel examples/auction
```

This drops you into a Glia shell.

### Step 2: Start the DHT service

From the Glia shell, run the auction in service mode. This provides
the schema CID on the DHT and re-provides periodically:

```clojure
/ > (perform runtime :run (load "bin/auction.wasm") "serve")
```

### Step 3: Query from a consumer (optional)

Open a second terminal and boot a consumer node:

```sh
ww run --port=2026 crates/kernel examples/auction
```

From the consumer's Glia shell, compare quotes across providers:

```clojure
/ > (perform auction :compare "QmWasmCid...")
```

## Init.d script

`etc/init.d/auction.glia`:

```clojure
;; Register the ComputeProvider vat cell.
;; VatListener spawns a cell per RPC connection; the cell exports
;; ComputeProvider via system::serve().
(def auction-wasm (load "bin/auction.wasm"))
(def auction-schema (load "bin/auction.schema"))

(perform host :listen runtime auction-wasm auction-schema)
```

The script registers the auction binary with the host's `VatListener`.
The schema is read from `bin/auction.schema` (adjacent to the WASM
binary). Each incoming RPC connection spawns a fresh cell that exports
a `ComputeProvider` capability.

The service mode (DHT provide) is started interactively from the
Glia shell -- not from the init.d script.

## Comparing from the shell

From a running Glia REPL:

```clojure
(perform auction :compare "QmWasmCid...")
;; => ({:provider "12D3Koo..." :price 120 :fuel 1000000 :expires 1743724800}
;;     {:provider "12D3Koo..." :price 150 :fuel 1000000 :expires 1743724800})
```

The `:compare` handler discovers providers via DHT, solicits quotes in parallel
(up to 10 providers, 5s timeout each), filters expired quotes, and sorts by price.

## Files

```
examples/auction/
├── Cargo.toml
├── Makefile              # make auction
├── README.md             # this file
├── auction.capnp         # ComputeProvider schema source
├── bin/                  # build output (gitignored)
│   ├── auction.wasm
│   └── auction.schema    # compiled schema bytes
├── etc/
│   └── init.d/
│       └── auction.glia  # cell registration
└── src/
    └── lib.rs            # guest implementation
```

## Phase 2 roadmap

- **Metered FuelPolicy** -- JIT refueling via `FuelSource` capability (pay-as-you-go)
- **Stake-weighted ordering** -- providers weighted by on-chain collateral
- **Membrane fuel attenuation** -- `GraftPolicy` maps identity to fuel budget
- **Operator market** -- aggregator vat routes bids to cheapest/closest provider
