# Wetware

A peer-to-peer runtime that loads WASM guest images and executes them inside
sandboxed cells, with host capabilities exposed over Cap'n Proto RPC.

## Quick start

```bash
# Build guests and assemble images
make guests images

# Boot pid0 from a local image
cargo run -- run examples/images/pid0
```

## How it works

`ww run <image>` does three things:

1. **Starts a libp2p swarm** on the configured port (default 2020)
2. **Loads `<image>/bin/main.wasm`** using a chain loader (tries IPFS, falls
   back to host filesystem)
3. **Spawns the guest** with WASI stdio bound to the host terminal and Cap'n
   Proto Host RPC served over in-memory data streams

The guest (pid0) bootstraps by calling methods on the `Host` capability — for
example, `host.executor().runBytes(wasm)` to spawn child processes.

## Image layout

Images follow a simplified
[Filesystem Hierarchy Standard](https://en.wikipedia.org/wiki/Filesystem_Hierarchy_Standard)
convention. This gives us a predictable, extensible structure whether the image
lives on local disk or is fetched from IPFS:

```
<image>/
  bin/
    main.wasm          # guest entrypoint (required)
  etc/                 # reserved — configuration files
  usr/
    lib/               # reserved — shared WASM libraries
```

Today only `bin/main.wasm` is required. The reserved directories are placeholders
for future capabilities:

- **`etc/`** — guest configuration (capability grants, resource limits, etc.)
- **`usr/lib/`** — shared WASM libraries that guests can link against

The `<image>` argument to `ww run` can be:

| Form | Example | Resolution |
|------|---------|------------|
| Local path | `examples/images/pid0` | Read directly from host filesystem |
| IPFS path | `/ipfs/QmAbc123...` | Fetched via local IPFS daemon (http://localhost:5001) |

## Building

### Prerequisites

- Rust nightly toolchain with `wasm32-wasip2` target
- (Optional) [Kubo](https://docs.ipfs.tech/install/) for IPFS image resolution

```bash
# One-time setup for guest compilation
rustup target add wasm32-wasip2 --toolchain nightly
```

### Build everything

```bash
make guests images    # build all WASM guests + assemble image dirs
cargo build           # build the host binary
```

### Build guests individually

Guest build order matters — pid0 embeds child-echo via `include_bytes!`, so
child-echo must build first:

```bash
make child-echo       # must come first
make shell
make pid0             # depends on child-echo
make images           # copy artifacts into examples/images/
```

## Usage

```
ww run <IMAGE> [OPTIONS]

Arguments:
  <IMAGE>    Image path: local directory or IPFS path containing bin/main.wasm

Options:
  --port <PORT>      libp2p swarm port [default: 2020]
  --wasm-debug       Enable WASM debug info for guest processes
  -h, --help         Print help
  -V, --version      Print version
```

### Examples

```bash
# Boot pid0 from local image directory
cargo run -- run examples/images/pid0

# With debug logging
RUST_LOG=ww=debug cargo run -- run examples/images/pid0

# Custom swarm port
cargo run -- run examples/images/pid0 --port 3000

# Boot from IPFS (requires running Kubo daemon)
cargo run -- run /ipfs/QmSomeHash
```

### Logging

Logging uses the standard `RUST_LOG` environment variable:

```bash
RUST_LOG=ww=info          # default — info-level wetware logs
RUST_LOG=ww=debug         # include debug output
RUST_LOG=ww=trace         # everything, including RPC message tracing
```

## Architecture

```
┌──────────────────────────────────────────────────────┐
│  ww run <image>                                      │
│                                                      │
│  ┌─────────────┐    ┌─────────────────────────────┐  │
│  │ libp2p      │    │ Cell                        │  │
│  │ swarm       │    │                             │  │
│  │ (port 2020) │    │  ┌───────┐  Cap'n Proto    │  │
│  │             │◄───┤  │ pid0  │◄──── RPC ──────►│  │
│  │             │    │  │ .wasm │  (in-memory)     │  │
│  │             │    │  └───────┘                  │  │
│  │             │    │      │                      │  │
│  │             │    │      │ host.executor()      │  │
│  │             │    │      │   .runBytes(wasm)    │  │
│  │             │    │      ▼                      │  │
│  │             │    │  ┌────────────┐             │  │
│  │             │    │  │ child-echo │             │  │
│  │             │    │  │ .wasm      │             │  │
│  │             │    │  └────────────┘             │  │
│  └─────────────┘    └─────────────────────────────┘  │
│                                                      │
│  stdin/stdout/stderr ◄──── WASI stdio ────► terminal │
└──────────────────────────────────────────────────────┘
```

- **Cell** loads `bin/main.wasm` from the image and spawns it as a WASI process
- **Host RPC** (Cap'n Proto) is served over in-memory duplex streams — no TCP
  listener, no external port
- **WASI stdio** is bound to the host terminal, so guest trace logs appear on
  stderr just like a normal process
- **ExecutorImpl** lets guests spawn child processes (pid0 → child-echo)

## Testing

```bash
cargo test --lib      # 32 unit tests (no guest builds needed)
```

## Cap'n Proto schema

The Host capability interface is defined in [`capnp/peer.capnp`](capnp/peer.capnp):

- **Host** — identity, addresses, peers, connect, executor
- **Executor** — `runBytes(wasm)` to spawn a child process, `echo` for testing
- **Process** — stdin/stdout/stderr streams + `wait()` for exit code
- **ByteStream** — read/write/close over duplex pipes
