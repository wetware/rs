# HTTP Capability Surface Design

## Status: Phase 0 (stdin/stdout re-wiring prototype) — tracking issue #241

## Overview

HTTP interop for wetware: outgoing HTTP calls via an HttpClient capability,
incoming HTTP serving via `--with-http host:port` with raw HTTP bytes piped
to handler processes on stdin/stdout.

All HTTP capabilities are Cap'n Proto native, epoch-scoped, and follow
wetware's zero-ambient-authority model.

## Motivation

Wetware aspires to be an OS. An OS without HTTP is incomplete. The external
bridge workaround (standalone HTTP server → libp2p → Terminal.login() →
Membrane methods) works but places the bridge outside the capability model.
This design brings HTTP inside the Membrane so it benefits from ocap
discipline, epoch-scoped lifecycle, and capability provisioning.

## Cap'n Proto Schema

```capnp
# http.capnp

struct HttpRequest {
  method @0 :Text;
  path @1 :Text;
  headers @2 :List(Header);
}

struct HttpResponse {
  status @0 :UInt16;
  headers @1 :List(Header);
}

struct Header {
  name @0 :Text;
  value @1 :Text;
}

struct HandlerSpec {
  union {
    path @0 :Text;            # UnixFS path (.wasm or .glia, extension-detected)
    wasmBinary @1 :Data;      # inline WASM bytes
    wasmCid @2 :Text;         # CID pointing to WASM binary
    gliaCid @3 :Text;         # CID pointing to Glia script
  }
  args @4 :List(Text);        # WASI process args (set at spawn)
  env @5 :List(Text);         # WASI process env vars (set at spawn)
}

# Reuse ByteStream from system.capnp for HTTP bodies.

interface HttpClient {
  request @0 (req :HttpRequest, body :ByteStream)
    -> (resp :HttpResponse, body :ByteStream);
}

interface HttpClientBuilder {
  build @0 () -> (client :HttpClient);
  # Future: build(config :HttpClientConfig) for domain/verb attenuation
}

interface HttpServer {
  router @0 (executor :Executor) -> (router :HttpRouter);
}

interface HttpRouter {
  handle @0 (pattern :Text, spec :HandlerSpec,
             args :List(Text), env :List(Text)) -> (route :UInt128);
  remove @1 (route :UInt128) -> ();
}
```

## Membrane Integration

```capnp
interface Membrane {
  graft @0 () -> (
    identity   :Identity,              # @0
    host       :Host,                  # @1
    executor   :Executor,              # @2
    ipfs       :Client,                # @3
    routing    :Routing,               # @4
    http       :HttpClientBuilder,     # @5 — always present (outgoing)
    httpServer :HttpServer             # @6 — null without --with-http
  );
}
```

Schema evolution: Cap'n Proto struct fields default to null, so adding @5
and @6 is backward-compatible. HttpClientBuilder defined in `http.capnp`,
imported in `stem.capnp`.

## Architecture

### Outgoing HTTP (HttpClient)

```
WASI Guest ──Cap'n Proto RPC──> EpochGuardedHttpClient (reqwest) ──> Internet
```

- HttpClientBuilder.build() returns an epoch-guarded HttpClient
- Follows the IPFS HttpClient pattern (wraps reqwest::Client)
- Every RPC method calls EpochGuard::check() — stale epoch = error
- Glia stdlib: `(http/get url)`, `(http/post url body)`
- Membrane attenuation: httpClient can be withheld from remote peer sessions

### Incoming HTTP (--with-http)

```
Internet ──> hyper listener ──> route table ──> handler stdin  (raw HTTP request)
                                            <── handler stdout (raw HTTP response)
```

- `--with-http host:port` — operator opt-in. No flag = no listener = httpServer is null.
- Port binding is an operator decision at launch time, converted to a capability
  inside the sandbox. Same pattern as `--stem` for on-chain coordination.
- Single port. Route by longest prefix match. Conflict on registration = error.

### Executor Attenuation

HttpServer.router(executor) binds the Executor into the Router. PID0 binds,
delegates the bound Router to children. Children can register HTTP handlers
but cannot access the raw Executor for arbitrary process spawning. Handler
lifecycle is constrained to route lifetime.

```
PID0: httpServer.router(executor) → bound Router
  │
  └── child A: router.handle("/api/*", spec) → registers handler
      │
      └── child A has Router but NOT raw Executor
          Can register HTTP handlers, cannot spawn arbitrary processes
```

### CGI Model — Raw HTTP on stdin/stdout

The host pipes complete HTTP request bytes to the handler's stdin. The handler
reads a full HTTP request (method line, headers, body), processes it, and
writes a full HTTP response (status line, headers, body) to stdout.

No sideband protocol. No environment variable metadata. No RPC for request
metadata. HTTP is self-describing — method, path, headers are all in the
byte stream.

The handler needs an HTTP parser (small library in any language). For Glia
handlers, the Glia evaluator WASM includes one.

HTTP requests are self-delimiting (Content-Length / chunked encoding), which
solves the per-request framing question for long-lived handlers.

### Handler Lifecycle

- Handler processes are long-lived (gen_server model). Not per-request spawning.
- Default serial: requests to a handler are queued and processed one at a time.
- Developer opts into concurrency by spawning sub-workers via Executor.
- Handler process owns its route. Handler dies → Router auto-removes route.
- Route token (u128, CSPRNG) is an external kill switch for manual removal.
- Registering process can exit freely — route persists as long as handler lives.

### HandlerSpec Dispatch

```
HandlerSpec.path       → detect extension:
                           .wasm → spawn WASM binary directly
                           .glia → spawn Glia evaluator WASM with script as arg
HandlerSpec.wasmBinary → spawn from inline bytes
HandlerSpec.wasmCid    → fetch from IPFS, then spawn
HandlerSpec.gliaCid    → fetch Glia script from IPFS, spawn evaluator with script
```

CIDs are safe designators — content-addressed, immutable, unforgeable. Passing
a CID doesn't grant authority, only designation. The Executor bound in the
Router provides execution authority. Router has internal IPFS access.

Glia handlers are schema-transparent: the Glia evaluator is "just another
WASM binary" in the FHS image. Schema is language-agnostic.

### Epoch Transitions

Epoch advance kills all HTTP handlers. Clean break:
1. All handler processes terminated
2. All routes cleared from table
3. All HTTP capabilities go stale (EpochGuard)
4. After re-graft, PID0 re-registers routes via init.d

This matches how all other epoch-scoped capabilities work.

### Error Handling

- Handler crashes mid-request → host returns HTTP 502 Bad Gateway
- Handler dead on request arrival → 503 Service Unavailable
- Handler timeout → 504 Gateway Timeout
- Unmatched route → 404 Not Found
- Queue overflow → 503 Service Unavailable
- Request body exceeds max size → 413 Payload Too Large
- Handler restart managed by init.d supervision (no HTTP-specific supervisor)

### Backpressure & Limits

- Byte-bounded per-handler request queue (default 1 MiB). 503 when exceeded.
- Max request body size (default 10 MiB, configurable).
- Per-request timeout (default 30s).
- TCP backpressure: if handler's stdin buffer is full, host pauses socket read.

## Implementation Phases

### Phase 0: stdin/stdout Re-Wiring Prototype (#242)
HARD GATE. Validate that Wasmtime allows swapping stdin/stdout streams on a
running WASI process. If not, evaluate fallbacks (WASI p2 resources,
length-prefix framing).

### Phase 1: Schema + HttpClient (#243)
- Create `capnp/http.capnp`
- Implement EpochGuardedHttpClient, HttpClientBuilder
- Extend graft() with @5 (HttpClientBuilder), @6 (HttpServer, null for now)
- Glia stdlib: `(http/get url)`, `(http/post url body)`
- Membrane attenuation for remote peers

### Phase 2: --with-http + CGI Mode (#244)
- `--with-http host:port` CLI flag
- hyper HTTP server, route table, path-prefix matching
- HttpServer.router(executor) → HttpRouter
- HttpRouter.handle(pattern, spec) → route token
- stdin/stdout re-wiring, raw HTTP bytes
- Handler lifecycle, epoch transitions

### Phase 3: Full HttpHandler RPC Mode (#245)
- HttpHandler.handle(req, body) → (resp, body) interface
- Streaming bodies via ByteStream
- Alternative to CGI for handlers needing structured access

### Phase 4: WebSocket (#246)
Requires separate design doc. HTTP upgrade at host, bidirectional stream
to guest, frame semantics TBD.

## Open Questions

1. **TLS termination** — defer to reverse proxy for v1
2. **Static file serving** — FHS `pub/` directory convention?
3. **HttpClient attenuation** — HttpClientBuilder.build(config) for domain/verb restrictions (future, not MVP)

## Prior Art

- **Sandstorm**: C++ HTTP proxy forwards to grain sandbox. Grain never listens.
- **Deno**: `--allow-net=host:port` grants network access at launch.
- **BEAM/Cowboy**: Process-per-request via Ranch acceptor pool.
- **WASI HTTP**: `wasi:http/incoming-handler` — component exports handle(). We chose Cap'n Proto native instead.
- **CGI**: stdin = request body, stdout = response. We extend this to raw HTTP bytes (headers included).
