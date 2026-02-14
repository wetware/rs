# Architecture

This document covers the design principles and capability flow of Wetware.
For transport plumbing (duplex streams, WASI bindings, scheduling, deadlocks),
see [rpc-transport.md](rpc-transport.md).

## Overview

Wetware is a peer-to-peer runtime that loads WASM guest images and executes
them inside sandboxed cells. `ww run <image>` does three things:

1. Starts a libp2p swarm
2. Loads `<image>/bin/main.wasm` via a chain loader (IPFS, then host filesystem)
3. Spawns the guest with a `Host` capability served over in-memory Cap'n Proto RPC

The runtime is deliberately simple. It loads a binary and hands it a capability.
Everything else — peer discovery, service management, access control — is the
guest's job.

## No ambient authority

Wetware follows capability-based security. All authority flows through
explicitly-passed Cap'n Proto capability objects. There is no ambient authority.

Traditional programs inherit authority from their environment: they can read
files, open sockets, inspect environment variables, and call any syscall the OS
allows. A Wetware guest has none of that. Its WASI sandbox provides stdio
(bound to the host terminal) and a data stream (bound to the Host RPC
connection). That's it. The guest's only connection to the outside world is the
`Host` capability the runtime hands it at boot.

```
Traditional process:        Wetware guest:
  env vars     -> yes         env vars     -> only if explicitly passed
  filesystem   -> yes         filesystem   -> no
  network      -> yes         network      -> no
  syscalls     -> yes         syscalls     -> WASI subset only
  ambient auth -> yes         ambient auth -> none
                              Host cap     -> the only authority
```

This is the foundation that makes untrusted code execution safe. A guest can
only do what the capabilities it holds allow. If you don't hand it the
`Executor` capability, it can't spawn children. If you don't hand it a
`connect` method, it can't dial peers.

## Layers

```
┌─────────────────────────────────────────────────────┐
│  Runtime (ww binary)                                │
│  - loads bin/main.wasm                              │
│  - starts libp2p swarm                              │
│  - serves Host capability to pid0                   │
│                                                     │
│  ┌───────────────────────────────────────────────┐  │
│  │  pid0 (guest)                                 │  │
│  │  - interprets boot/, svc/, etc/               │  │
│  │  - connects to bootstrap peers                │  │
│  │  - spawns services                            │  │
│  │  - defines the Membrane (what to export)      │  │
│  │                                               │  │
│  │  ┌─────────────┐  ┌─────────────┐            │  │
│  │  │ child-echo  │  │ metrics     │  ...        │  │
│  │  │ (service)   │  │ (service)   │             │  │
│  │  └─────────────┘  └─────────────┘            │  │
│  └───────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────┘
```

**Runtime** (`ww` binary) is the kernel. It knows how to load
`bin/main.wasm` from an image, start a libp2p swarm, and serve Host RPC.
It knows nothing about the rest of the image layout — `boot/`, `svc/`,
`etc/` are opaque directories as far as the runtime is concerned.

**pid0** (the guest loaded from `bin/main.wasm`) is init. It receives a
`Host` capability and uses it to interpret the image layout: read bootstrap
peers from `boot/`, spawn services from `svc/`, apply configuration from
`etc/`. pid0 is where policy lives.

**Children** are processes spawned by pid0 (or by other children) via
`host.executor().runBytes(wasm)`. Each child gets its own Host capability
over its own RPC connection. In the future, pid0 will be able to scope
these capabilities — giving a child a restricted view of the host.

## Capability flow

### Inbound: host to guest

The runtime creates a `Host` capability and bootstraps it to pid0 over
in-memory Cap'n Proto RPC. From pid0's perspective, it calls
`wetware_guest::run(|host| { ... })` and receives a `host::Client` —
a capability reference it can invoke:

```
Runtime                          pid0
───────                          ────
create HostImpl
  with ExecutorImpl
    with network state
serve via RpcSystem ──────────> bootstrap client
                                host.id()
                                host.addrs()
                                host.connect(peer, addrs)
                                host.executor() -> Executor
                                  executor.runBytes(wasm) -> Process
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

pid0 receives a `Host` capability — its view of the world. It can
wrap, filter, or extend that capability into a **Membrane**: an object
that controls what the outside world can do.

```
1. Runtime hands pid0 the Host capability
2. pid0 wraps Host into a Membrane (adds policy, filters methods, etc.)
3. pid0 exports the Membrane back to the runtime
4. Runtime serves the Membrane on a libp2p stream protocol
5. Remote peers interact with the Membrane, not with the raw Host
```

This is how pid0 controls access. The runtime doesn't decide what remote
peers can do — pid0 does, by choosing what to export. The runtime is just
the transport.

## Two-layer configuration

There are two distinct kinds of configuration, and they don't mix.

**Image config** (what to run) lives inside the image:

- `boot/` — which peers to connect to
- `svc/` — which services to start
- `etc/` — application configuration

Image config is content-addressed on IPFS. It's portable across nodes.
The same image CID produces the same behavior everywhere.

**Node config** (how to run it here) lives outside the image:

- `--port` — which port to listen on
- `--wasm-debug` — whether to enable debug info
- IPFS daemon address, log levels, resource limits

Node config is specific to this machine. It's set via CLI flags, env vars,
or (in the future) a local config file.

The runtime owns node config. pid0 owns image config. This separation
keeps images portable and reproducible.

## FHS image layout

Images follow a simplified Filesystem Hierarchy Standard. See the
[README](../README.md) for the full spec. In brief:

```
<image>/
  bin/main.wasm     # guest entrypoint (required) — consumed by runtime
  boot/<peerID>     # bootstrap peers (optional)  — consumed by pid0
  svc/<name>/       # nested service images        — consumed by pid0
  etc/              # configuration                — consumed by pid0
  usr/lib/          # shared WASM libraries        — reserved
```

Only `bin/main.wasm` is consumed by the runtime. Everything else is
convention between the image author and pid0.

## Network architecture (future)

Two nodes running Wetware can communicate via capability passing over
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

## See also

- [rpc-transport.md](rpc-transport.md) — transport plumbing, scheduling model, deadlock analysis
- [../capnp/peer.capnp](../capnp/peer.capnp) — Host, Executor, Process, ByteStream interfaces
- [../README.md](../README.md) — image layout, build instructions, usage
