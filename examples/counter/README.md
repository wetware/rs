# Counter — HTTP/FastCGI Cell Demo

Stateful HTTP cell that counts requests. Proves the `Cell::http` pipeline
end-to-end: host receives HTTP, spawns a cell, pipes FastCGI over stdio,
returns the response.

## What it demonstrates

1. `Cell::http("/counter")` tag in the WASM custom section
2. Host-side `HttpListener` routes `GET /counter` and `POST /counter`
   to the counter cell
3. FastCGI framing over stdin/stdout between host and guest
4. Per-request cell spawning (Mode A: stateless from the host's view,
   state lives inside the cell's memory for its lifetime)

## Design

### Wire protocol: FastCGI (spec v1)

The cell speaks real FastCGI binary protocol (version 1) over stdin/stdout.
The host acts as the FastCGI client, the guest as the FastCGI server.

```
stdin  (host → guest):  FCGI_BEGIN_REQUEST → FCGI_PARAMS* → empty FCGI_PARAMS → FCGI_STDIN* → empty FCGI_STDIN
stdout (guest → host):  FCGI_STDOUT (CGI headers + body) → empty FCGI_STDOUT → FCGI_END_REQUEST
```

Each record has an 8-byte header: version (1), type, request ID (big-endian
u16), content length (big-endian u16), padding length, reserved. CGI params
(REQUEST_METHOD, REQUEST_URI, etc.) are encoded as FastCGI name-value pairs
with length-prefixed fields (1-byte length if <128, 4-byte with high bit set
otherwise).

The guest responds with a CGI-style response in FCGI_STDOUT:
`Status: 200 OK\r\nContent-Type: text/plain\r\n\r\nbody`

Using real FastCGI means any standard FastCGI library can test or interact
with cells. No bespoke framing to maintain.

**Why not raw HTTP on stdio?** Parsing HTTP/1.1 in the guest adds a
dependency (or a hand-rolled parser). FastCGI is a well-defined binary
protocol that's simpler to parse than HTTP. The host already has an HTTP
parser and translates between HTTP and FastCGI.

**Why not Cap'n Proto RPC?** HTTP cells are intentionally simpler than
capnp cells. They target developers who want to expose a REST endpoint
without learning Cap'n Proto. FastCGI is the simplest well-specified
binary protocol for this job.

### Cell lifecycle

```
HTTP request arrives at host
        │
        ▼
  ┌─────────────┐
  │ HttpListener │  routes by path prefix → finds "/counter"
  └──────┬──────┘
         │
         ▼
  ┌─────────────┐
  │  Executor   │  spawns counter.wasm as WASI process
  └──────┬──────┘
         │
         ▼
  ┌─────────────┐     stdin: FastCGI records (BEGIN_REQUEST, PARAMS, STDIN)
  │ Counter Cell│  ◄──────────────────────────
  │  (WASI P2)  │  ──────────────────────────►
  └─────────────┘     stdout: FastCGI records (STDOUT, END_REQUEST)
         │
         ▼
  Host translates stdout → HTTP response → client
```

**Mode A (v1):** One cell per request. Cell starts, handles request, exits.
No connection pooling, no keepalive. Simple, correct, easy to reason about.

**Mode B (future):** Cell pool with keepalive. Host reuses cells across
requests. Requires a "ready" signal from the cell (e.g., reading a second
request on stdin after responding). Deferred.

### Counter logic

The counter cell is intentionally trivial:

- `GET /counter` → responds with the current count (starts at 0)
- `POST /counter` → increments and responds with the new count
- Any other method → 405 Method Not Allowed

In Mode A (one cell per request), the counter resets every request (always
returns 0 for GET, 1 for POST). This is correct behavior — it proves the
pipeline works. Persistent state requires either:

- Mode B (keepalive): cell survives across requests, holds state in memory
- External state: cell reads/writes to IPFS or a capability-backed store

The demo starts with Mode A. The counter value is uninteresting. What
matters is that an HTTP request arrives, hits the cell, and a response
comes back through the full pipeline.

## Files

```
examples/counter/
├── Cargo.toml          # standalone WASI P2 crate
├── Makefile            # make -C examples/counter
├── README.md           # this file
├── bin/                # build output (gitignored)
│   └── counter.wasm
└── src/
    └── lib.rs          # guest implementation
```

## Building

```sh
make -C examples/counter
```

This compiles the WASI component and injects the `Cell::http("/counter")`
tag via `schema-inject --http /counter`.

## Dependencies (host-side, not yet implemented)

The counter cell is ready to build and test with a mock stdio harness, but
the full end-to-end flow requires host-side work tracked in TODOS.md:

1. **HttpListener capability** — accepts `Cell::http` binaries, starts an
   HTTP server, routes by path prefix, spawns cells per request
2. **FastCGI codec** — host-side codec that translates HTTP requests to
   FastCGI records on stdin and parses FCGI_STDOUT back to HTTP responses
3. **system.capnp** — `HttpListener` interface definition

The guest cell can be developed and unit-tested independently of the host.
The stdio framing is simple enough to test with a harness that writes
request bytes to the cell's stdin and reads the response from stdout.

## Testing without HttpListener

Until the host-side handler exists, test the cell directly by writing
FastCGI records to its stdin and reading the response from stdout:

```rust
// Pseudocode: spawn counter.wasm, send FastCGI request, read response
let process = executor.run_bytes(counter_wasm).await;

// Send FCGI_BEGIN_REQUEST (type=1, role=RESPONDER, flags=0)
process.stdin.write(&fcgi_header(1, 1, 8));  // type=BEGIN, id=1, len=8
process.stdin.write(&[0, 1, 0, 0, 0, 0, 0, 0]);  // role=1(responder), flags=0

// Send FCGI_PARAMS with REQUEST_METHOD=GET
process.stdin.write(&fcgi_params(1, &[("REQUEST_METHOD", "GET")]));
process.stdin.write(&fcgi_header(4, 1, 0));  // empty PARAMS = end

// Send empty FCGI_STDIN
process.stdin.write(&fcgi_header(5, 1, 0));  // empty STDIN = end

// Read FCGI_STDOUT records, parse CGI response
let response = read_fcgi_stdout(&mut process.stdout);
assert!(response.contains("Status: 200 OK"));
assert!(response.contains("0"));
```
