# RPC Transport: Wasmtime Host <-> pid0 <-> child-echo

This document explains how the Wasmtime host, the pid0 guest, and the child-echo
guest communicate over Cap'n Proto RPC, with a focus on the transport plumbing
and scheduling model.

Primary code references:
- `src/rpc/mod.rs`
- `src/cell/proc.rs`
- `guests/guest-runtime/src/lib.rs`
- `guests/pid0/src/lib.rs`
- `guests/child-echo/src/lib.rs`

## High-level layout

At runtime there are two RPC links:
- Host <-> pid0 (guest)
- Host <-> child-echo (guest) created by pid0 via `run_bytes`

Both links use the same transport mechanism: Cap'n Proto RPC over a bidirectional
in-memory duplex stream that is exposed to guests as WASI io/streams resources.

ASCII overview:

```
          (Cap'n Proto RPC over wetware:streams)
┌───────────────────────────┐           ┌───────────────────────────┐
│        Wasmtime Host      │           │           pid0            │
│  - ExecutorImpl server    │<=========>│  - RpcSession client       │
│  - VatNetwork + RpcSystem │           │  - RpcDriver loop          │
└───────────────────────────┘           └───────────────────────────┘
           ^                                            |
           | run_bytes (spawn child)                    | Cap'n Proto RPC
           |                                            v
┌───────────────────────────┐           ┌───────────────────────────┐
│        Wasmtime Host      │           │        child-echo         │
│  - ExecutorImpl server    │<=========>│  - RpcSession client       │
│  - VatNetwork + RpcSystem │           │  - RpcDriver loop          │
└───────────────────────────┘           └───────────────────────────┘
```

## Transport plumbing: host side

### 1) Creating the transport channel

`ProcBuilder::with_data_streams()` allocates an in-memory duplex stream:

- `tokio::io::duplex(PIPE_BUFFER_SIZE)` yields `(host_stream, guest_stream)`.
- `host_stream` stays on the host; `guest_stream` is injected into the guest
  runtime state (`ComponentRunStates.data_stream`).

Reference: `src/cell/proc.rs:208`

```
tokio::io::duplex()  ->  host_stream  <----->  guest_stream
```

### 2) Exposing to the guest as WASI io/streams

When the guest calls `wetware:streams/streams#create-connection`, the host
replaces the `guest_stream` with WASI stream resources:

- Split guest stream into read/write halves.
- Wrap them as `AsyncReadStream` and `AsyncWriteStream`.
- Store them in a `ConnectionState` resource.

Reference: `src/cell/proc.rs:435`

```
guest_stream
  -> split (guest_read, guest_write)
  -> DynInputStream (AsyncReadStream)
  -> DynOutputStream (AsyncWriteStream)
```

### 3) Wiring Cap'n Proto RPC over the host side

`run_bytes` in the host `ExecutorImpl` sets up the child process and builds a
Cap'n Proto `VatNetwork` over the host's stream halves:

- `handles.take_host_split()` yields `(reader, writer)`.
- `build_peer_rpc(reader, writer, wasm_debug)` wraps them in a `VatNetwork`.
- A `RpcSystem` is spawned in a local task set alongside the guest process.

References: `src/rpc/mod.rs:296`, `src/rpc/mod.rs:390`

```
host_stream -> split -> (AsyncRead, AsyncWrite) -> VatNetwork -> RpcSystem
```

## Transport plumbing: guest side

### 1) Guest stream connection

`RpcSession::connect()` in the guest runtime calls the WIT binding
`create_connection()` and obtains a WASI input and output stream:

- `create_connection()` -> `connection.get_input_stream()` and
  `connection.get_output_stream()`.
- These are wrapped as `StreamReader` and `StreamWriter`.

Reference: `guests/guest-runtime/src/lib.rs:177`

```
create_connection()
  -> WASI InputStream  -> StreamReader (AsyncRead)
  -> WASI OutputStream -> StreamWriter (AsyncWrite)
```

### 2) Cap'n Proto RPC over WASI streams

The guest constructs a Cap'n Proto `VatNetwork` over those stream adapters and
bootstraps a client:

Reference: `guests/guest-runtime/src/lib.rs:202`

```
StreamReader/StreamWriter -> VatNetwork -> RpcSystem -> client
```

## Scheduling model and CPU behavior

Guests are not busy-spinning when idle. They run a cooperative loop in
`RpcDriver::drive_until`:

1) Poll the RPC system and any application futures/promises.
2) If no progress is made, block in `wasi_poll::poll` on stream readiness
   (pollable handles from the WASI streams).

Reference: `guests/guest-runtime/src/lib.rs:268`

Key points:
- **No constant CPU polling**: when nothing makes progress, the guest blocks on
  `wasi_poll::poll`, yielding to the runtime.
- **Asynchronous on the host**: the host side uses Tokio async I/O and a spawned
  `RpcSystem` to process messages without blocking the host thread.

ASCII loop detail:

```
loop:
  poll rpc_system + app promises
  if done -> exit
  if no progress -> wasi_poll::poll([reader, writer])
```

## End-to-end flows

### Flow A: pid0 spawns child-echo

Sequence (simplified):

```
pid0 guest                  host                        child-echo guest
-----------                ------                      -----------------
RpcSession::connect()
run_bytes RPC  ----------> ExecutorImpl::run_bytes
                           spawn child Proc + RpcSystem
                           return Process cap
wait RPC     <-----------> ProcessImpl::wait
```

References:
- `guests/pid0/src/lib.rs:55`
- `src/rpc/mod.rs:296`

### Flow B: child-echo calls echo twice

Sequence (simplified):

```
child-echo guest            host
----------------           ------
RpcSession::connect()
echo RPC #1  -------------> ExecutorImpl::echo
echo RPC #2  -------------> ExecutorImpl::echo
responses    <------------- results
```

References:
- `guests/child-echo/src/lib.rs:50`
- `src/rpc/mod.rs:276`

## Transport diagram (host/guest boundary)

```
Guest code (WASM)                           Host (Tokio + Wasmtime)
-----------------                          ------------------------
RpcSession::connect()                      ProcBuilder::with_data_streams()
  create_connection()  <WIT>              create duplex (host/guest)
  get_input_stream()   <WIT>              map to AsyncReadStream
  get_output_stream()  <WIT>              map to AsyncWriteStream
  StreamReader/Writer                      host_stream split
        |                                        |
        v                                        v
  VatNetwork + RpcSystem                VatNetwork + RpcSystem
        |                                        |
        +------------- Cap'n Proto RPC ----------+
```

## Notes and implications

- The transport is **async** on the host and **poll-driven** on the guest with
  explicit blocking on WASI pollables.
- RPC messages traverse memory-only duplex streams; there is no OS socket I/O.
- Backpressure is mediated by the WASI output stream budget (`check_write`) and
  the duplex buffer size (`PIPE_BUFFER_SIZE`).

## Deadlock analysis and mitigations

The guest can deadlock if it blocks without a peer making progress, or if both
ends wait for each other without driving their RPC systems. The transport itself
is cooperative: it requires one side to be actively polled to move data.

### Deadlock causes

1) Host stops driving the child RPC system
- In `run_bytes`, the host spawns a local task to run the child's `RpcSystem`.
  If that task is never scheduled (or exits early), the guest will block in
  `wasi_poll::poll` waiting for read/write readiness that never comes.
- References: `src/rpc/mod.rs:340`, `guests/guest-runtime/src/lib.rs:268`

2) Guest waits on RPC promises without calling `drive_until`
- Cap'n Proto RPC futures only make progress when `rpc_system.poll_unpin` is
  driven. If user code blocks or returns without continuing the `drive_until`
  loop, responses will never be delivered.
- References: `guests/pid0/src/lib.rs:71`, `guests/child-echo/src/lib.rs:69`

3) Backpressure deadlock: both sides waiting on writable/readable state
- If the guest's writer is not ready and the host's reader isn't draining (or
  vice versa), both sides can become stuck waiting for readiness.
- The guest-side `StreamWriter` reports readiness via `check_write`; if it
  repeatedly returns 0, the driver waits for readiness with `wasi_poll::poll`.
- References: `guests/guest-runtime/src/lib.rs:85`, `guests/guest-runtime/src/lib.rs:268`

4) Application-level wait cycles
- A guest can block awaiting a host response that itself depends on a guest
  callback or further guest progress (capability-based cycles).
- This shows up when the guest stops polling while the host expects a follow-up
  message from the same guest.

### Mitigations

1) Ensure both RPC systems are continuously driven
- Host: keep the child's `RpcSystem` alive for the lifetime of the guest process.
- Guest: keep `drive_until` running whenever promises are outstanding.

2) Use timeouts on long-lived RPC calls
- On the guest, wrap `drive_until` or higher-level workflows with timeouts and
  error if no progress is made for a window.
- On the host, bound guest process execution (`tokio::time::timeout` is already
  used for instantiation in `src/cell/proc.rs:399`).

3) Add explicit yield points in guests
- If a guest performs long CPU work, ensure it periodically drives the RPC
  system and yields to pollables. Without this, inbound RPC traffic will stall.

4) Consider buffering or flow-control strategy
- Increase duplex buffer size (`PIPE_BUFFER_SIZE`) or chunk writes to reduce
  long stalls when one side is briefly slow to drain.

5) Observability
- Keep trace logging enabled during development to detect stalled states and
  verify that the RPC system is being polled.
