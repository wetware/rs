# Oracle Example

Gas price feed over schema-keyed Cap'n Proto RPC with Terminal auth.

## How it works

The oracle is a single WASM binary that runs in two modes:

- **Service mode** (default): Fetches live gas prices from the
  Blocknative API via `HttpClient`, caches them in memory, and exports
  a `PriceOracle` capability via `VatListener.serve()`. Provides the
  schema CID on the DHT so consumers can discover it.

- **Consumer mode** (`WW_CONSUMER=1`): Discovers oracle providers via
  DHT `findProviders()`, dials them with `VatClient`, and queries
  prices.

## Architecture

```
              membrane.graft()
                    │
            ┌───────┴───────┐
      service mode      consumer mode
     (WW_CONSUMER        (WW_CONSUMER=1)
        absent)                │
            │           findProviders()
     HttpClient.get()     ┌───┴───┐
     (Blocknative)     VatClient.dial()
            │              │
     PriceOracle       PriceOracle
     VatListener.serve()  .getPrice()
            │
     routing.provide()
```

## Capabilities used

| Capability | Purpose |
|-----------|---------|
| `HttpClient` | Outbound HTTP to Blocknative API (domain-scoped) |
| `VatListener` | Export PriceOracle as persistent RPC capability |
| `VatClient` | Dial oracle peers for typed RPC |
| `Routing` | DHT provide/findProviders for discovery |
| `Host` | Peer identity and network access |

## Cap'n Proto interface

```capnp
interface PriceOracle {
  getPrice @0 (pair :Text) -> (price :Int64, decimals :UInt8,
                                timestamp :Int64, confidence :Float64);
  getPairs @1 () -> (pairs :List(Text));
}
```

Supported pairs: `ETH/gas`, `POLYGON/gas`, `BASE/gas`.

Prices are in the smallest unit (e.g. wei for ETH/gas), with `decimals`
indicating how many places to shift for human-readable output. Confidence
decays toward 0.0 if data is stale.

## Prerequisites

A running Kubo node for DHT bootstrap:

```sh
ipfs daemon
```

## Building

```sh
make oracle
```

## Running

Boot two nodes, each stacking the oracle layer on the kernel.
The init.d script registers the PriceOracle RPC cell automatically.
Then start the service/consumer from the Glia shell:

```sh
# Terminal 1 — boot node, then start the oracle service
ww run --port=2025 crates/kernel examples/oracle
/ > (executor run (load "bin/oracle.wasm"))

# Terminal 2 — boot node, then start a consumer
ww run --port=2026 crates/kernel examples/oracle
/ > (executor run (load "bin/oracle.wasm") :env {"WW_CONSUMER" "1"})
```

The service node fetches gas prices, exports PriceOracle via RPC cells,
and provides the schema CID on the DHT. The consumer discovers the
service, dials it via VatClient, and logs prices to stderr.

## Tests

```sh
cargo test -p oracle --lib
```
