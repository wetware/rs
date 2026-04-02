# Discovery Example

Two-agent Greeter demo showing schema-keyed peer discovery over the DHT.

Agent A publishes a Greeter service. Agent B discovers it by schema CID
alone, dials it via Cap'n Proto RPC, and gets a typed greeting back.
No configuration, no service registry, no hardcoded addresses.

## Build

```bash
make discovery
```

This compiles the WASM guest and produces `boot/main.schema` (canonical
schema bytes for the Greeter interface) alongside `boot/main.wasm`.

## Run

Boot two nodes, each stacking the discovery layer on the kernel.
The init.d script registers the Greeter RPC cell automatically.
Then start the discovery service from the Glia shell:

```bash
# Terminal A — boot node, then start discovery service
cargo run -- run --port=2025 crates/kernel examples/discovery
/ > (executor run (load "bin/discovery.wasm"))

# Terminal B — boot node, then start discovery service
cargo run -- run --port=2026 crates/kernel examples/discovery
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
  greeter.capnp --> capnpc --> greeter_schema.bin --> boot/main.schema
  src/lib.rs    --> cargo  --> discovery.wasm      --> boot/main.wasm

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

The schema CID is derived deterministically from the Greeter interface
definition: `CIDv1(raw, BLAKE3(canonical(schema.Node)))`. Two nodes
with the same schema automatically find each other on the Kademlia DHT.

## Without Kubo

The demo works without Kubo. Schema push to IPFS is best-effort at
build time. Discovery happens via DHT `provide/findProviders` regardless.

## Schema

```capnp
interface Greeter {
  greet @0 (name :Text) -> (greeting :Text);
}
```

## Tests

```bash
cargo test -p discovery
```

Runs unit tests for the Greeter implementation and RPC round-trip tests
over in-memory Cap'n Proto duplex.
