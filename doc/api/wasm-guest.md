# WASM Guest API Reference

This document specifies the host-guest interface for Wetware WASM components.
A guest is a WASI P2 component (`wasm32-wasip2`) that communicates with
the host via two channels: standard WASI interfaces and a custom bidirectional
stream used for Cap'n Proto RPC.

## Component Model

Guests are WASI CLI commands. The host instantiates the guest as a
`wasi:cli/command` component and calls `wasi:cli/run#run` to start it.

**Target triple**: `wasm32-wasip2`

**Required export**:

| Export | Signature | Description |
|--------|-----------|-------------|
| `wasi:cli/run#run` | `() -> result` | Entry point. Called by host to start the guest. |

## WASI Host Functions

Standard WASI P2 interfaces provided by the host. Implemented via
`wasmtime_wasi::p2::add_to_linker_async`.

### wasi:io/streams@0.2.9

| Resource | Method | Signature | Description |
|----------|--------|-----------|-------------|
| `input-stream` | `read` | `(len: u64) -> result<list<u8>, stream-error>` | Non-blocking read up to `len` bytes. Empty list = no data yet. |
| `input-stream` | `blocking-read` | `(len: u64) -> result<list<u8>, stream-error>` | Blocking read up to `len` bytes. |
| `input-stream` | `skip` | `(len: u64) -> result<u64, stream-error>` | Skip up to `len` bytes, return count skipped. |
| `input-stream` | `blocking-skip` | `(len: u64) -> result<u64, stream-error>` | Blocking skip. |
| `input-stream` | `subscribe` | `() -> pollable` | Get pollable for read readiness. |
| `output-stream` | `check-write` | `() -> result<u64, stream-error>` | Return max bytes the next `write` may accept. Never blocks. |
| `output-stream` | `write` | `(contents: list<u8>) -> result<_, stream-error>` | Non-blocking write. Precondition: `len(contents) <= check-write()`. Traps otherwise. |
| `output-stream` | `blocking-write-and-flush` | `(contents: list<u8>) -> result<_, stream-error>` | Write up to 4096 bytes and flush. Blocks until complete. |
| `output-stream` | `flush` | `() -> result<_, stream-error>` | Request flush of buffered output. Non-blocking. |
| `output-stream` | `blocking-flush` | `() -> result<_, stream-error>` | Flush and block until complete. |
| `output-stream` | `subscribe` | `() -> pollable` | Get pollable for write readiness. |
| `output-stream` | `write-zeroes` | `(len: u64) -> result<_, stream-error>` | Write `len` zero bytes. Same preconditions as `write`. |
| `output-stream` | `splice` | `(src: borrow<input-stream>, len: u64) -> result<u64, stream-error>` | Pipe from input to this output. |

### wasi:io/poll@0.2.9

| Function | Signature | Description |
|----------|-----------|-------------|
| `poll` | `(in: list<borrow<pollable>>) -> list<u32>` | Wait until one or more pollables are ready. Returns indices of ready items. Traps if list is empty. |

| Resource | Method | Signature | Description |
|----------|--------|-----------|-------------|
| `pollable` | `ready` | `() -> bool` | Check readiness without blocking. |
| `pollable` | `block` | `()` | Block until ready. |

### wasi:io/error@0.2.9

| Resource | Method | Signature | Description |
|----------|--------|-----------|-------------|
| `error` | `to-debug-string` | `() -> string` | Human-readable error description. Not for machine parsing. |

### wasi:cli/stdin, wasi:cli/stdout, wasi:cli/stderr

| Function | Signature | Description |
|----------|-----------|-------------|
| `get-stdin` | `() -> input-stream` | Guest's standard input. Connected to host-provided pipe. |
| `get-stdout` | `() -> output-stream` | Guest's standard output. Connected to host-provided pipe. |
| `get-stderr` | `() -> output-stream` | Guest's standard error. Connected to host-provided pipe. |

**Stdio behavior**: The host provides explicit async pipes for each stream.
In byte-stream mode (`Listener`/`Dialer`), stdin/stdout are wired to the
libp2p stream. In RPC mode (`RpcListener`/`RpcDialer`), stdin/stdout are
used for direct RPC bootstrapping via `serve_stdio()`. Stderr is always
available for logging.

### wasi:clocks/monotonic-clock

| Function | Signature | Description |
|----------|-----------|-------------|
| `subscribe-duration` | `(ns: u64) -> pollable` | Create a pollable that resolves after `ns` nanoseconds. Used for idle poll timeouts. |

### wasi:filesystem/types (conditional)

Filesystem access is **read-only** and only available when an image root
is mounted. The host preopens the merged FHS image directory at `/` with
`DirPerms::READ` and `FilePerms::READ`.

When IPFS caching is active, filesystem operations are intercepted by
`fs_intercept` to resolve content from IPFS transparently.

**Constraint**: Guests cannot write to the filesystem. All writes must go
through capabilities (IPFS, ByteStream, etc.).

## Custom Interfaces

### wetware:streams/streams@0.1.0

Bidirectional data channel between host and guest, used as the transport
layer for Cap'n Proto RPC (Membrane bootstrap).

| Function | Signature | Description |
|----------|-----------|-------------|
| `create-connection` | `() -> connection` | Create a bidirectional stream pair. Can only be called **once** per process. |

| Resource | Method | Signature | Description |
|----------|--------|-----------|-------------|
| `connection` | `get-input-stream` | `() -> input-stream` | Get the read half. Can only be called **once**. |
| `connection` | `get-output-stream` | `() -> output-stream` | Get the write half. Can only be called **once**. |

**Transport**: Backed by a `tokio::io::DuplexStream` (64 KiB buffer).
The host holds the other end and runs Cap'n Proto RPC over it.

**Constraint**: Both `create-connection` and the `get-*-stream` methods
are one-shot. Second calls return an error. This enforces single-owner
semantics on the RPC channel.

**Availability**: Only present when the host enables data streams
(`Builder::with_data_streams()`). Guests spawned without data streams
(e.g., byte-pump handlers) will get an error on `create-connection`.

## Cap'n Proto RPC (over wetware:streams)

Once the guest obtains input/output streams from `wetware:streams`,
it bootstraps a Cap'n Proto RPC session over them. The host serves
the **Membrane** as the bootstrap capability.

### Connection Setup

1. Guest calls `create-connection()` → gets `connection` resource
2. Guest calls `connection.get-input-stream()` and `connection.get-output-stream()`
3. Guest creates `VatNetwork::new(reader, writer, Side::Client, ...)`
4. Guest creates `RpcSystem::new(network, bootstrap_export)`
5. Guest bootstraps host capability: `rpc_system.bootstrap(Side::Server)` → `Membrane`
6. Guest optionally exports its own bootstrap cap (for `system::serve()`)

### Guest Entry Points

The `system` crate (`std/system`) provides two entry points that handle
all connection setup automatically:

| Function | Signature | Description |
|----------|-----------|-------------|
| `system::run` | `(f: FnOnce(C) -> Future) -> ()` | Bootstrap host cap, run closure, drive RPC. |
| `system::serve` | `(bootstrap: Client, f: FnOnce(C) -> Future) -> ()` | Same as `run`, but also exports `bootstrap` to host. |
| `system::serve_stdio` | `(bootstrap: Client) -> ()` | Export cap over stdin/stdout (no Membrane). For byte-stream handlers. |
| `system::block_on` | `(session: &mut RpcSession, future: F) -> Option<T>` | Drive RPC alongside a future. Returns `None` if RPC closes first. |

### Membrane Capabilities

After bootstrapping, the guest calls `membrane.graft()` to obtain
session-scoped capabilities:

| Capability | Interface | Description |
|------------|-----------|-------------|
| Host | `system_capnp::host` | Node identity, network interfaces, executor access. |
| Executor | `system_capnp::executor` | Spawn WASM processes. |
| IPFS | `ipfs_capnp::client` | Content-addressed storage (add/cat/pin). |
| Routing | `routing_capnp::routing` | DHT operations (provide/find_providers). |
| Identity | `stem_capnp::identity` | Host-side signing (private key never leaves host). |

All capabilities are **epoch-guarded**: they become invalid when the
host advances its epoch. Calls on stale capabilities return a
`staleEpoch` error.

## Cap'n Proto RPC (system.capnp)

Full interface reference for the capabilities available to guests.

### Host

| Method | Signature | Description |
|--------|-----------|-------------|
| `id` | `() -> (peerId: Data)` | This node's libp2p peer ID. |
| `addrs` | `() -> (addrs: List(Data))` | Multiaddrs this node listens on. |
| `peers` | `() -> (peers: List(PeerInfo))` | Currently connected peers. |
| `executor` | `() -> (executor: Executor)` | Get an epoch-scoped Executor. |
| `network` | `() -> (listener, dialer, rpcListener, rpcDialer)` | Get network interfaces (byte-stream + RPC modes). |

### Executor

| Method | Signature | Description |
|--------|-----------|-------------|
| `runBytes` | `(wasm: Data, args: List(Text), env: List(Text)) -> (process: Process)` | Spawn a WASM component from raw bytes. |
| `echo` | `(message: Text) -> (response: Text)` | Diagnostic echo. Returns message unmodified. |
| `bind` | `(wasm: Data, args: List(Text), env: List(Text)) -> (bound: BoundExecutor)` | Create a reusable executor bound to specific WASM + args + env. |

### BoundExecutor

| Method | Signature | Description |
|--------|-----------|-------------|
| `spawn` | `() -> (process: Process)` | Spawn a new instance of the bound binary. |

### Process

| Method | Signature | Description |
|--------|-----------|-------------|
| `stdin` | `() -> (stream: ByteStream)` | Writable stream to guest's stdin. |
| `stdout` | `() -> (stream: ByteStream)` | Readable stream from guest's stdout. |
| `stderr` | `() -> (stream: ByteStream)` | Readable stream from guest's stderr. |
| `wait` | `() -> (exitCode: Int32)` | Block until process exits. |
| `bootstrap` | `() -> (cap: AnyPointer)` | Get the capability exported by the guest via `system::serve()`. Type-erased. |

### ByteStream

| Method | Signature | Description |
|--------|-----------|-------------|
| `read` | `(maxBytes: UInt32) -> (data: Data)` | Read up to `maxBytes`. Empty data = EOF. |
| `write` | `(data: Data) -> ()` | Write data to stream. |
| `close` | `() -> ()` | Close stream. Further reads return EOF, writes fail. |

### Listener (byte-stream mode)

| Method | Signature | Description |
|--------|-----------|-------------|
| `listen` | `(executor: Executor, protocol: Text, handler: Data) -> ()` | Accept streams on `/ww/0.1.0/{protocol}`. Per-stream: spawn handler, wire stdin/stdout. |

### Dialer (byte-stream mode)

| Method | Signature | Description |
|--------|-----------|-------------|
| `dial` | `(peer: Data, protocol: Text) -> (stream: ByteStream)` | Open stream to peer on `/ww/0.1.0/{protocol}`. Returns bidirectional ByteStream. |

### RpcListener (capability mode)

| Method | Signature | Description |
|--------|-----------|-------------|
| `listen` | `(executor: Executor, handler: Data) -> ()` | Accept streams on `/ww/0.1.0/{cid}`. Schema extracted from handler WASM's `schema.capnp` custom section. Per-stream: spawn handler, bridge bootstrap cap to peer. |

### RpcDialer (capability mode)

| Method | Signature | Description |
|--------|-----------|-------------|
| `dial` | `(peer: Data, schema: Data) -> (cap: AnyPointer)` | Open stream to peer on `/ww/0.1.0/{cid}`. Bootstrap RPC, return remote's capability. Type-erased. |

## WASM Custom Sections

Custom sections embedded in the WASM binary carry metadata that the host
inspects before instantiation.

| Section Name | Contents | Purpose |
|--------------|----------|---------|
| `schema.capnp` | Canonical Cap'n Proto encoding of a `schema.Node` | Identifies the binary as an RPC handler. Host extracts schema, derives CID for protocol addressing. |

**Schema CID derivation**: `CIDv1(raw, BLAKE3(section_bytes))` → protocol
`/ww/0.1.0/{cid}`. The host re-derives the CID from the section bytes;
it never trusts a guest-supplied CID.

**Absence**: If `schema.capnp` is not present or is empty, the binary is
not an RPC handler. Passing it to `rpcListen` is an error.

## Implementation Constraints

### Single-threaded guest execution

Guests run on a single WASM thread. The `system` crate uses cooperative
polling (`noop_waker` + manual `wasi:io/poll`) instead of a real async
runtime. There is no `tokio` or `async-std` inside the guest.

### Write tracking

The guest tracks whether writes occurred during a poll cycle via a
thread-local `WRITE_OCCURRED` flag. This prevents a deadlock where:
1. RPC system queues a write
2. Guest blocks on reader-only poll
3. Host never receives the write → both sides wait forever

When writes occurred, the guest polls both reader and writer. When idle
(no writes, no progress), it polls reader + a 100ms timeout to handle
missed wakeups from the host's `AsyncReadStream` background task.

### Resource cleanup

Cap'n Proto destructors attempt to close WASI handles that may already
be torn down by the host. The `system` crate calls `std::mem::forget()`
on RPC resources at exit to avoid panics. This is a WASI P2 wart —
revisit when wasmtime stabilizes resource cleanup ordering.

### Epoch guards

All Membrane-provided capabilities are wrapped in epoch guards. When
the host advances its epoch (e.g., on-chain state change), all outstanding
capabilities become invalid. Calls return `staleEpoch` errors. Guests
must re-graft to obtain fresh capabilities.

### Pipe buffer sizes

| Buffer | Size | Location |
|--------|------|----------|
| stdio (stdout, stderr) | 1024 bytes | `proc.rs:BUFFER_SIZE` |
| data stream (RPC transport) | 64 KiB | `proc.rs:PIPE_BUFFER_SIZE` |

### Idle poll timeout

100ms (`IDLE_POLL_TIMEOUT_NS`). Created via `wasi:clocks/monotonic-clock.subscribe-duration`.
Fires when no writes occurred and no progress was made, preventing indefinite
blocking on missed wakeups.
