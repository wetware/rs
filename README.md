# Wetware Runtime (`ww`)

Wetware is a peer-to-peer WASM sandbox. The `ww` CLI runs WASM components
under WASI Preview 2, connecting them to a libp2p swarm and exposing host
capabilities via Cap'n Proto RPC.

## Architecture

```
                        ┌──────────────────────────┐
  operator ──stdio──>   │  ww run <path>           │
                        │                          │
                        │  libp2p swarm            │
                        │    ├─ Kademlia DHT       │
                        │    ├─ Identify            │
                        │    └─ TCP/Noise/Yamux    │
                        │                          │
                        │  Cap'n Proto RPC         │
                        │    └─ Membrane bootstrap │
                        │        ├─ Host           │
                        │        ├─ Executor       │
                        │        └─ StatusPoller   │
                        │                          │
                        │  Wasmtime (WASI P2)      │
                        │    └─ guest.wasm         │
                        └──────────────────────────┘
```

**Membrane** (from [stem](https://github.com/wetware/stem)) is the RPC
bootstrap capability. Guests graft onto it and receive an epoch-scoped
**Session** containing **Host**, **Executor**, and **StatusPoller**. When the
on-chain epoch advances, all capabilities from the previous session fail with
`staleEpoch`.

### Key Modules

| Module | Description |
|--------|-------------|
| `cell`   | WASM component loading and execution via Wasmtime |
| `host`   | libp2p swarm lifecycle and peer management |
| `rpc`    | Cap'n Proto RPC servers (Host, Executor, Membrane) |
| `ipfs`   | Kubo API client for IPFS peer discovery |
| `loaders`| WASM bytecode loaders (filesystem, IPFS) |
| `config` | Runtime configuration and presets |

### Cap'n Proto Schemas

Schemas live in `capnp/`:

- **peer.capnp** — Host, Executor, Process, ByteStream interfaces
- **membrane.capnp** — WetwareSession (Host + Executor extension for stem's generic Session)
- **stem.capnp** — Vendored from stem for import resolution (no code generated; see [capnp-cross-crate.md](doc/capnp-cross-crate.md))

### Workspace

The workspace includes host-side code and several WASM guest crates:

- `.` — the `ww` host binary and library
- `guests/pid0` — init process (bootstrap guest)
- `guests/shell` — interactive shell guest
- `guests/child-echo` — echo child process for testing
- `guests/guest-runtime` — shared guest-side runtime utilities

## Prerequisites

1. **Rust toolchain** (stable + nightly for guest builds)
   ```bash
   rustup install stable
   rustup target add wasm32-wasip2 --toolchain nightly
   ```

2. **Kubo (IPFS) daemon** for peer discovery
   ```bash
   kubo daemon
   ```

3. **Cap'n Proto compiler** (`capnp`) for schema compilation

## Quick Start

```bash
# Build everything
make

# Run with a local WASM volume
cargo run -- run /boot \
  --volume examples/default-kernel/target/wasm32-wasip2/release:/boot
```

## Usage

```
ww <COMMAND>

Commands:
  run     Run a wetware node
  help    Print help
```

### `ww run` Options

- `--ipfs <URL>` — Kubo HTTP API endpoint (default: `http://localhost:5001`)
- `--preset <PRESET>` — Configuration preset (`minimal`, `development`, `production`)
- `--env-config` — Load configuration from environment variables

### Environment Variables

- **`WW_IPFS`** — Kubo HTTP API endpoint
- **`RUST_LOG`** — Tracing filter (e.g. `ww=debug`, `ww=info,libp2p=debug`)

## Building WASM Guests

Guest crates target `wasm32-wasip2` (nightly-only):

```bash
# Build all guests
make

# Build a specific guest
make -C examples/default-kernel build
```

The `ww run` command expects `{path}/main.wasm` in the mounted directory.

## Docker

```bash
make podman-build    # Build image
make podman-run      # Run container
make podman-clean    # Clean up
```

## Documentation

Design docs live in [`doc/`](doc/):

- [**capnp-cross-crate.md**](doc/capnp-cross-crate.md) — Cross-crate Cap'n Proto type sharing via `crate_provides`
- [**rpc-transport.md**](doc/rpc-transport.md) — RPC transport architecture
