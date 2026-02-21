# Wetware

A peer-to-peer runtime that loads WASM guest images and executes them inside
sandboxed cells, with host capabilities exposed over Cap'n Proto RPC.

## Quick start

```bash
# Scaffold a new guest
ww init my-app --template rust
cd my-app

# Build
ww build

# Run locally
ww run

# Publish to IPFS and run from anywhere
ww push
ww run /ipfs/<CID>
```

## Developer CLI

`ww` is the developer CLI for building and publishing wetware applications.

```
ww init [<subdir>] [--template rust]   Scaffold a new environment
ww build [<path>]                      Compile to boot/main.wasm
ww run [<path>|<ipfs-path>]            Boot a local or IPFS environment
ww push [<path>]                       Publish to IPFS, print CID
```

## How it works

`ww run <path>` does three things:

1. **Starts a libp2p swarm** on the configured port (default 2025)
2. **Loads `boot/main.wasm`** from the environment (local or IPFS)
3. **Spawns the guest** with WASI stdio bound to the host terminal and Cap'n
   Proto RPC served over in-memory data streams

## Environment layout

Each wetware environment follows a minimal FHS convention:

```
<env>/
  boot/
    main.wasm          # guest entrypoint (required)
  bin/                 # additional executables (optional)
  etc/                 # configuration (optional, lazy)
  usr/                 # shared libraries (optional, lazy)
  var/                 # runtime state (optional, lazy)
```

Only `boot/main.wasm` is required. Other directories are created on demand.

The `<path>` argument to `ww run` can be:

| Form | Example |
|------|---------|
| Local path | `std/kernel` |
| IPFS path  | `/ipfs/QmAbc123...` |

## Standard library (`std/`)

`std/` is the base image layer — default programs available in every wetware
environment. Built with `ww build`, published to IPFS with `ww push`.

```
std/
├── runtime/    Rust guest SDK (library crate)
├── kernel/     Init process — grafts onto the host Membrane
└── shell/      Interactive shell (in development)
```

Build and publish the std layer:

```bash
ww build std/kernel
ww push std/
```

## Building

### Prerequisites

- Rust toolchain with `wasm32-wasip2` target
- (Optional) [Kubo](https://docs.ipfs.tech/install/) for IPFS resolution

```bash
rustup target add wasm32-wasip2
```

### Build the host binary

```bash
cargo build
```

### Build a std component

```bash
ww build std/kernel    # produces std/kernel/boot/main.wasm
ww build std/shell     # produces std/shell/boot/main.wasm
```

## Usage

```
ww run <PATH> [OPTIONS]

Arguments:
  <PATH>    Local directory or IPFS path (default: .)

Options:
  --port <PORT>                libp2p swarm port [default: 2025]
  --wasm-debug                 Enable WASM debug info
  --stem <ADDR>                Atom contract address (enables epoch pipeline)
  --rpc-url <URL>              HTTP JSON-RPC URL [default: http://127.0.0.1:8545]
  --ws-url <URL>               WebSocket JSON-RPC URL [default: ws://127.0.0.1:8545]
  --confirmation-depth <N>     Confirmations before finalizing [default: 6]
```

### Examples

```bash
# Run the kernel locally
ww run std/kernel

# Run a user app
ww run my-app

# Run from IPFS
ww run /ipfs/QmSomeHash

# Merge image layers (std base + user app)
ww run /ipfs/QmStdCID my-app

# With on-chain epoch tracking
ww run my-app --stem 0xABCD... --rpc-url http://127.0.0.1:8545
```

### Logging

```bash
RUST_LOG=ww=info     # default
RUST_LOG=ww=debug    # verbose
RUST_LOG=ww=trace    # everything
```

## Testing

```bash
cargo test --lib
cargo test -p membrane
cargo test -p atom --lib
```

## Cap'n Proto schemas

Defined in `capnp/`:

- **`system.capnp`** — Host, Executor, Process, ByteStream
- **`stem.capnp`** — Membrane, Session (host + executor + ipfs)
- **`ipfs.capnp`** — IPFS CoreAPI (UnixFS, Block, Pin, …)
