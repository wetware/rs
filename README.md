# Wetware

The peer-to-peer agentic OS.

Sandboxed WASM processes with capability security, content-addressed
code, and P2P networking.

## Why

Agentic frameworks give you a platform for running agents. Wetware
gives your agents an **operating system**.

Wetware provides primitives (processes, networking, storage, identity)
and gets out of the way. Its processes are network-addressable,
capability-secured, and peer-to-peer by default. They work equally
well as autonomous agents, long-running services, or participants
in distributed protocols. An interactive shell exposed over MCP lets
agents drive the OS directly: discover services, spawn processes,
and manage capabilities.

Agentic frameworks rely on ambient authority: any code in the process
can call any API, read any secret, spend any resource, because
authority is determined by context rather than by explicit grant.
Wetware replaces this with capabilities. A process can only do what
it's been handed a capability to do.

## Quick start

```bash
# Build
cargo build                    # host binary
ww build std/kernel            # kernel agent

# Run (drops into an interactive Glia shell)
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

`ww run <mount>...` boots an agent:

1. **Starts a libp2p swarm** on the configured port (default 2025)
2. **Loads `boot/main.wasm`** from the merged image
3. **Spawns the agent** with WASI stdio and a **Membrane**: the
   capability hub that grants access to host, network, IPFS, and
   identity services via Cap'n Proto RPC

The agent authenticates to the Membrane (`graft()`) and receives
epoch-scoped capabilities. When the on-chain epoch advances (new code
deployed, configuration changed), all capabilities are revoked and the
agent must re-graft, picking up the new state automatically.

### Capabilities

After grafting, an agent holds references to:

| Capability | What it does |
|------------|-------------|
| **Host** | Peer identity, listen addresses, connected peers, network access |
| **Executor** | Spawn child WASM processes |
| **IPFS** | Content-addressed storage (cat, ls, add) |
| **Routing** | Kademlia DHT: publish and discover content/services |
| **Identity** | Host-side signing (private key never enters WASM) |
| **Listener / Dialer** | Open and accept P2P streams for custom subprotocols |

Each capability is epoch-guarded: it fails with `staleEpoch` once the
on-chain head advances, forcing a re-graft.

## The shell

The Glia shell is a Clojure-inspired REPL where capabilities are
first-class values. The language design blends three traditions:

- **E-lang**: capabilities as values you can pass, compose, and attenuate
- **Clojure**: s-expression syntax, immutable data, functional composition
- **Unix**: processes, PATH lookup, stdin/stdout, init.d scripts

```clojure
/ ❯ (host id)
"00240801122025c7ea..."

/ ❯ (host peers)
[{:peer-id "..." :addrs [...]} ...]

/ ❯ (ipfs cat "/ipfs/QmFoo...")
"hello world"
```

Init scripts in `etc/init.d/*.glia` run at boot, configuring services
and capabilities for the agent.

## Image layout

Each wetware image follows a minimal FHS convention:

```
<image>/
  boot/
    main.wasm          # agent entrypoint (required)
  bin/                 # executables on the kernel's PATH
  svc/                 # nested service images (spawned by pid0)
  etc/                 # configuration (consumed by pid0)
    init.d/            # boot scripts evaluated by the kernel
```

Only `boot/main.wasm` is required. Everything else is convention
between the image author and the kernel (pid0).

Mounts can be local paths or IPFS paths. Multiple mounts are merged
as layers (later mounts override earlier ones):

| Form | Example |
|------|---------|
| Local path | `std/kernel` |
| IPFS path | `/ipfs/QmAbc123...` |
| Targeted | `~/.ww/identity:/etc/identity` |
| Layered | `ww run /ipfs/QmBase my-overlay` |

## On-chain coordination

The `--stem` flag connects to an Atom contract on an EVM chain.
The contract holds a monotonic head pointer (an IPFS CID). When the
head is updated:

1. The off-chain indexer detects the `HeadUpdated` event
2. Waits for confirmation depth (reorg safety)
3. Advances the epoch, revoking all agent capabilities
4. Agents re-graft, receiving capabilities scoped to the new epoch

This provides a coordination primitive across trust boundaries:
multiple independent nodes watching the same contract will
synchronize their agent lifecycle to the same on-chain state.

## Standard library

`std/` contains the guest SDK and built-in agents:

```
std/
├── system/     Guest SDK: RPC session, async runtime, stream adapters
├── kernel/     Init agent (pid0): grafts onto the Membrane, runs init.d,
│               provides interactive shell
└── shell/      Interactive shell (in development)
```

Built with `ww build`, loadable from local paths or IPFS CIDs.

## Building

### Prerequisites

- Rust toolchain with `wasm32-wasip2` target
- (Optional) [Kubo](https://docs.ipfs.tech/install/) for IPFS resolution

```bash
rustup target add wasm32-wasip2
```

### Build

```bash
cargo build                    # host binary
ww build std/kernel            # kernel agent (→ std/kernel/boot/main.wasm)
ww build std/shell             # shell agent  (→ std/shell/boot/main.wasm)
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
# Local development
ww run std/kernel

# Load from IPFS
ww run /ipfs/QmSomeHash

# Layer images (base + overlay)
ww run /ipfs/QmStdCID my-app

# Mount identity into the image
ww run std/kernel ~/.ww/identity:/etc/identity

# Connect to on-chain epochs
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

## Architecture

Cap'n Proto schemas in `capnp/`:

- **`system.capnp`** — Host, Executor, Process, ByteStream, Listener, Dialer
- **`stem.capnp`** — Membrane, Epoch, Signer, Identity
- **`ipfs.capnp`** — IPFS CoreAPI (UnixFS, Block, Pin, …)
- **`routing.capnp`** — Kademlia DHT (provide, findProviders, hash)

For platform vision and roadmap, see
[`doc/designs/economic-agent-platform.md`](doc/designs/economic-agent-platform.md).
