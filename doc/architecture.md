# Architecture

This document covers the design principles and capability flow of Wetware.
For transport plumbing (duplex streams, WASI bindings, scheduling, deadlocks),
see [rpc-transport.md](rpc-transport.md).  For the guest-side async runtime
(poll loop, effect system, oneshot channels), see
[guest-runtime.md](guest-runtime.md).

## Overview

Wetware is a decentralized operating system for autonomous agents. It runs
WASM guests in sandboxed cells with zero ambient authority: all capabilities
are explicitly granted over Cap'n Proto RPC, and object lifetimes are managed
through on-chain epoch boundaries.

`ww run` resolves one or more image layers into a unified FHS root, loads
`boot/main.wasm` from the result, and spawns the agent with epoch-scoped
capabilities served over in-memory Cap'n Proto RPC.

The host is deliberately simple. It merges layers, loads a binary, and
hands it capabilities. Everything else — peer discovery, service management,
access control — is the agent's job. The host is the sandbox; the agent
is the policy engine.

## No ambient authority

Wetware follows capability-based security. All authority flows through
explicitly-passed Cap'n Proto capability objects. There is no ambient authority.

Traditional programs inherit authority from their environment: they can read
files, open sockets, inspect environment variables, and call any syscall the OS
allows. A Wetware agent has none of that. Its WASI sandbox provides stdio
(bound to the host terminal) and a data stream (bound to the RPC connection).
That's it. The agent's only connection to the outside world is the `Membrane`
the host hands it at boot — and it calls `graft()` to obtain actual
capabilities. (Having a Membrane reference IS authorization — ocap model.
Authentication, if needed, is handled by wrapping the Membrane in a
`Terminal(Membrane)` challenge-response layer.)

```
Traditional process:        Wetware guest:
  env vars     -> yes         env vars     -> only if explicitly passed
  filesystem   -> yes         filesystem   -> none; content via IPFS capability
  network      -> yes         network      -> no
  syscalls     -> yes         syscalls     -> WASI subset only
  ambient auth -> yes         ambient auth -> none
                              graft caps   -> the only authority (Host + Runtime + Routing + Identity)
```

**There is no native filesystem on the guest side.** The WASI sandbox does
not provide filesystem access. All content loading goes through capabilities
— specifically the IPFS capability obtained via `ipfs.cat(path)`.

The host publishes the merged FHS image to IPFS and sets `$WW_ROOT` to the
resulting IPFS path (e.g. `/ipfs/QmHash...`). Guests resolve content relative
to this root: `ipfs.cat("$WW_ROOT/bin/main.wasm")`.

This is the foundation that makes untrusted code execution safe. An agent can
only do what the capabilities it holds allow. If you don't hand it the
`Executor` capability, it can't spawn children. If you don't hand it a
`connect` method, it can't dial peers.

## Comparison: Cloudflare Workers

Cloudflare Workers is the closest prior art for sandboxed, instruction-metered
guest execution at the edge.  The table below maps the two models side by side.

| Dimension | Cloudflare Workers | Wetware |
|---|---|---|
| Isolation unit | V8 Isolate | WASM Component (Cell) |
| Runtime | JavaScript / V8 | WASM / Wasmtime |
| OS threads | Shared pool across isolates | Dedicated OS thread per executor worker |
| Task scheduling | V8 event loop per isolate | `tokio::task::spawn_local` on a `LocalSet` per worker |
| Multiplexing | Many isolates per thread (V8-managed) | M:N — many cells per worker (EWMA fuel scheduler) |
| Preemption mechanism | V8 interrupt API (time-based) | Wasmtime fuel counter (instruction-based, deterministic) |
| CPU-bound guest behavior | Isolate interrupted after CPU time budget | Cell yields every `YIELD_INTERVAL` instructions via `fuel_async_yield_interval` |
| Cold start | ~0ms (isolate reuse within process) | Per-cell WASM compilation; `Engine` is shared across cells |
| Authority model | Ambient — `fetch`, KV, R2 via binding config | Zero ambient — all capabilities granted via `membrane.graft()` |
| Inter-cell communication | Service bindings (HTTP), Durable Object RPC | Cap'n Proto RPC over in-process or libp2p transport |
| Shared state | Durable Objects (single-writer actor) | Capabilities are the unit of shared state; epoch lifecycle for revocation |
| Memory isolation | Separate V8 heap per isolate | Separate Wasmtime `Store` per cell |
| Send-safety | N/A (JavaScript) | `Store` is `!Send` — cells are pinned to their worker's `LocalSet` |

The sharpest differences:

**Preemption.** Workers uses time-based interrupts external to V8.  Wetware's
preemption is instruction-count-based and baked into Wasmtime — the same binary
consumes the same fuel regardless of host CPU speed, making scheduling behavior
deterministic and independently verifiable.

**Authority.** Workers still grants ambient authority: you configure which
services a Worker can reach, but inside those bounds it calls `fetch()` freely.
Wetware has no ambient authority at all.  Every capability is an unforgeable
object reference.  If the `Executor` capability is not in scope, a cell cannot
spawn children — there is no configuration flag to bypass this.

## Layers

```
┌─────────────────────────────────────────────────────┐
│  Host (ww binary)                                   │
│  - loads kernel from boot/main.wasm                 │
│  - starts libp2p swarm                              │
│  - serves Membrane to kernel                        │
│                                                     │
│  ┌───────────────────────────────────────────────┐  │
│  │  kernel (pid0)                                │  │
│  │  - grafts onto Membrane, obtains capabilities  │  │
│  │  - interprets bin/, svc/, etc/                │  │
│  │  - connects to bootstrap peers                │  │
│  │  - spawns services                            │  │
│  │  - defines what to export to the network      │  │
│  │                                               │  │
│  │  ┌─────────────┐  ┌─────────────┐             │  │
│  │  │ child-echo  │  │ metrics     │  ...        │  │
│  │  │ (service)   │  │ (service)   │             │  │
│  │  └─────────────┘  └─────────────┘             │  │
│  └───────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────┘
```

**Host** (`ww` binary) is the supervisor. It loads `boot/main.wasm` from an
image, starts a libp2p swarm, and serves a `Membrane` to pid0 over Cap'n
Proto RPC. It knows nothing about the rest of the image layout — `bin/`,
`svc/`, `etc/` are opaque directories as far as the host is concerned.

**pid0** (the kernel agent loaded from `boot/main.wasm`) is init. It
receives a `Membrane`, calls `graft()`, and uses the resulting capabilities
to interpret the image layout: look up executables from `bin/`, spawn
services from `svc/`, apply configuration from `etc/`. pid0 is where
policy lives.

**Children** are agents spawned by pid0 (or by other children) via
`runtime.load(wasm)` followed by `executor.spawn()`. Each child gets
its own set of capabilities over its own RPC connection. pid0 can scope
these capabilities, giving a child a restricted view of the host.

## Capability flow

### Inbound: host to guest

The host creates a Membrane and bootstraps it to pid0 over in-memory
Cap'n Proto RPC. pid0 calls `membrane.graft()` to obtain epoch-guarded
capabilities as flat return fields:

- **identity** (`Identity`) — host-side signing (maps domains → Signers)
- **host** (`Host`) — network identity and peer management
- **runtime** (`Runtime`) — load WASM binaries, obtain scoped Executors
- **routing** (`Routing`) — Kademlia DHT (provide, findProviders)
- **httpClient** (`HttpClient`) — outbound HTTP requests

All capabilities are epoch-guarded: they become stale when the on-chain
head advances. The guest must re-graft to obtain fresh capabilities.

Having a Membrane reference IS authorization (ocap model). `graft()` is
parameterless — no signer needed. To gate access for remote peers, wrap
the Membrane in `Terminal(Membrane)`, which requires challenge-response
authentication before handing out the Membrane reference.

```
Host                             pid0
────                             ────
create Membrane
  with GraftBuilder
    Host (network state)
    Runtime (engine, cache)
    Routing (DHT)
serve via RpcSystem ──────────> membrane.graft() -> (identity, host, runtime, routing, httpClient)
                                  host.id()
                                  host.addrs()
                                  runtime.load(wasm) -> executor
                                  executor.spawn(args, env) -> process
```

### Outbound: guest to host

Cap'n Proto RPC is bidirectional. The guest can export capabilities *back*
to the host — the host just bootstraps from the guest's side of the
connection. The host doesn't need to know in advance what the guest will
export.

This is the key insight: the RPC connection is symmetric. Both sides can
serve capabilities. Both sides can hold references to the other's objects.

### Network: host to remote peers

The host can take whatever capability it bootstrapped from the guest and
serve it over a libp2p stream protocol. Remote peers get a Cap'n Proto
client stub pointing at the guest's exported capability, proxied through
the host.

```
Node A                                     Node B
──────                                     ──────
pid0 exports Membrane ──> host             host ──> pid0 imports Membrane
                          serves on                  as a client stub
                          libp2p stream
                            <═══════════════>
                          Cap'n Proto RPC over libp2p
```

### The Membrane pattern

pid0 receives a `Membrane` from the host, calls `graft()`, and obtains
capabilities. It can then wrap, filter, or extend those capabilities into
a new **Membrane**: an object that controls what the outside world can do.

```
1. Host hands pid0 a Membrane reference
2. pid0 calls graft(), receives capabilities (identity, host, runtime, routing, httpClient)
3. pid0 wraps capabilities into an attenuated Membrane (adds policy, filters methods)
4. pid0 exports the attenuated Membrane back to the host (optionally wrapped in Terminal)
5. Host serves the exported capability on a libp2p stream protocol
6. Remote peers authenticate via Terminal (if present), then interact with the Membrane
```

This is how pid0 controls access. The host doesn't decide what remote
peers can do — pid0 does, by choosing what to export. The host is just
the transport.

## Configuration

There is one configuration model: FHS. An image is an FHS directory tree.
pid0 interprets it. The host only reads `boot/main.wasm`; everything
else is between the image author and pid0.

See the [README](../README.md) for the image layout. In brief:

```
<image>/
  boot/main.wasm    # agent entrypoint — consumed by host
  bin/              # executables on the kernel's PATH — consumed by pid0
  svc/<name>/       # nested service images — consumed by pid0
  etc/              # configuration — consumed by pid0
```

The FHS root that pid0 sees can be assembled from multiple **layers**
via per-file union:

```
ww run [--stem <contract>] [<path> ...]
```

The Stem contract's head CID (if provided) forms the base layer.
Positional arguments are stacked on top in order. Later layers override
earlier layers at the file level. There are no deletes — you can add
and override, but not remove.

```
ww run --stem 0xABC... /ipfs/QmOverlay ./local-tweaks
        │                │                │
        ▼                ▼                ▼
   base layer       middle layer      top layer
   (from chain)     (from IPFS)       (local fs)
```

No single layer needs to be complete. A Stem CID might provide `etc/`
and `bin/` but no `boot/main.wasm`, expecting an overlay to supply the
entrypoint. The only requirement is that the **union** contains
`boot/main.wasm`.

```sh
# Standalone: fully self-contained local image
ww run ./my-image

# Cluster provides everything, run as-is
ww run --stem 0xABC...

# Cluster provides authority + bootstrap, you provide the code
ww run --stem 0xABC... ./my-app

# Cluster base, IPFS plugin, local dev config
ww run --stem 0xABC... /ipfs/QmPlugin ./local-config
```

### Layer resolution

- **Per-file union.** Each layer contributes files. If two layers provide
  the same path, the later layer wins.
- **No deletes.** To remove something from a lower layer, publish a new
  version of that layer without it.
- **Directories merge, files replace.** If layer A has `boot/QmPeerA` and
  layer B has `boot/QmPeerB`, the result has both. If layer B also has
  `boot/QmPeerA`, layer B's version wins.

### Stem integration

When `--stem` is provided, the host reads the head CID from the
contract, fetches it from IPFS as the base layer, and boots pid0 with
an epoch-scoped Membrane. When the on-chain head advances, capabilities
are revoked and the host reloads with the new base.

Without `--stem`, pid0 gets a Membrane with no epoch lifecycle.
The process exits when pid0 exits.

### Node config

Orthogonal to the image, **node config** controls how the host
behaves on this particular machine: `--port`, `--wasm-debug`, IPFS
daemon address, log levels, resource limits. Node config is set via CLI
flags or env vars — it never lives inside image layers.

## Network architecture

Two nodes running Wetware communicate via capability passing over
libp2p:

```
┌─────────────────────┐              ┌─────────────────────┐
│  Node A (server)    │              │  Node B (client)     │
│                     │              │                      │
│  pid0 exports       │   libp2p    │  pid0 receives       │
│  Membrane ─────────>│<═══════════>│──────> Membrane stub  │
│                     │  Cap'n Proto │                      │
│  ww run <server>    │     RPC     │  ww run <client>     │
└─────────────────────┘              └─────────────────────┘
```

`ww run <server-image>` boots pid0, which exports a Membrane on the
network. `ww run <client-image>` connects to the server's peer ID and
receives a capability stub for the Membrane. The client can then call
methods on the Membrane as if it were local — Cap'n Proto handles the
serialization and transport.

All network communication is capability-mediated. A guest can only talk
to peers it has a capability reference for. There is no "broadcast to
the network" or "listen for connections" — only explicit capability
passing.

## Epoch lifecycle

When `--stem` points to an Atom smart contract, the host starts an
epoch pipeline that watches for `HeadUpdated` events on-chain:

```
AtomIndexer (WebSocket + HTTP backfill)
    |  HeadUpdatedObserved events
    v
Finalizer (K-confirmation strategy)
    |  FinalizedEvent
    v
pin new CID / unpin old CID on IPFS
    |
    v
epoch_tx.send(Epoch { seq, head, adopted_block })
    |
    v
EpochGuard invalidation → stale capabilities fail → guest re-grafts
```

The epoch channel is created before the guest spawns, so the pipeline
runs concurrently with the guest via `CellBuilder::with_epoch_rx()`.

## IPFS capability

`Ipfs.Client` (`capnp/ipfs.capnp`) mirrors Go's CoreAPI. Currently
implemented sub-APIs:

- **UnixFS**: `cat(path) -> Data`, `ls(path) -> List(Entry)`,
  `add(data) -> cid`

The chess example uses `add()` to publish game replays as an IPLD-like
linked list — one JSON node per move pair, each linking to the previous
via CID (see `examples/chess/doc/replay.md`).

Stub interfaces declared for future work: Block, Dag, Name, Key, Pin,
Object, Swarm, PubSub, Routing.

The host delegates to a local Kubo HTTP client (`http://localhost:5001`).
Cap'n Proto pipelining allows `ipfs.unixfs().cat(path)` to
resolve in a single round-trip.

## See also

- [routing.md](routing.md) — DHT design, capability model, bootstrap lifecycle
- [shell.md](shell.md) — kernel shell reference (interactive + daemon modes)
- [cli.md](cli.md) — CLI flags and usage
- [rpc-transport.md](rpc-transport.md) — transport plumbing, scheduling model, deadlock analysis
- [../capnp/system.capnp](../capnp/system.capnp) — Host, Runtime, Executor, Process, ByteStream interfaces
- [../capnp/ipfs.capnp](../capnp/ipfs.capnp) — IPFS Client capability schema
- [../README.md](../README.md) — image layout, build instructions, usage
