# Wetware

A peer-to-peer agentic OS.

Sandboxed WASM agents with zero ambient authority, capability-based
security, and on-chain coordination.

## Quick start

```bash
# Build
cargo build                    # host binary
ww build std/kernel            # kernel agent

# Run — drops into an interactive Glia shell
ww run std/kernel
```

```
/ ❯ (help)
Capabilities:
  (host id)                      Peer ID
  (host addrs)                   Listen addresses
  (host peers)                   Connected peers
  (host connect "<multiaddr>")   Dial a peer
  (executor echo "<msg>")        Diagnostic echo
  (ipfs cat "<path>")            Fetch IPFS content
  (ipfs ls "<path>")             List IPFS directory

Built-ins:
  (cd "<path>")                  Change working directory
  (help)                         This message
  (exit)                         Quit

Unrecognized commands are looked up in PATH (default /bin).

/ ❯ (host id)
"00240801122025c7ea..."
/ ❯ (exit)
```

For CLI help:

```bash
ww --help
ww run --help
```

## How it works

`ww run <mount>...` does three things:

1. **Starts a libp2p swarm** on the configured port (default 2025)
2. **Loads `boot/main.wasm`** from the merged image
3. **Spawns the agent** with WASI stdio bound to the host terminal and
   Cap'n Proto RPC served over in-memory data streams

## Image layout

Each wetware image follows a minimal FHS convention:

```
<image>/
  boot/
    main.wasm          # agent entrypoint (required)
  bin/                 # executables on the kernel's PATH
  svc/                 # nested service images (spawned by pid0)
  etc/                 # configuration (consumed by pid0)
```

Only `boot/main.wasm` is required. Everything else is convention
between the image author and pid0.

Mounts can be local paths or IPFS paths:

| Form | Example |
|------|---------|
| Local path | `std/kernel` |
| IPFS path  | `/ipfs/QmAbc123...` |
| Targeted   | `~/.ww/identity:/etc/identity` |

## Standard library (`std/`)

`std/` contains the guest SDK and built-in agents. Built with `ww build`,
published to IPFS with `ww push`.

```
std/
├── system/     Guest SDK — RPC session, async runtime, stream adapters
├── kernel/     Init agent (pid0) — grafts onto the host Membrane
└── shell/      Interactive shell (in development)
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
ww run [OPTIONS] [MOUNT]...

Arguments:
  [MOUNT]...    source or source:/guest/path (default: .)

Options:
  --port <PORT>                libp2p swarm port [default: 2025]
  --wasm-debug                 Enable WASM debug info
  --identity <PATH>            secp256k1 identity file [env: WW_IDENTITY]
  --stem <ADDR>                Atom contract address (enables epoch pipeline)
  --rpc-url <URL>              HTTP JSON-RPC URL [default: http://127.0.0.1:8545]
  --ws-url <URL>               WebSocket JSON-RPC URL [default: ws://127.0.0.1:8545]
  --confirmation-depth <N>     Confirmations before finalizing [default: 6]
```

### Examples

```bash
# Run the kernel locally
ww run std/kernel

# Run from IPFS
ww run /ipfs/QmSomeHash

# Merge image layers (std base + user app overlay)
ww run /ipfs/QmStdCID my-app

# Targeted mount (identity file at /etc/identity)
ww run std/kernel ~/.ww/identity:/etc/identity

# With on-chain epoch tracking
ww run std/kernel --stem 0xABCD...
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
