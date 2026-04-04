# TODOs

## Shell session memory limits / TTL
**What:** Bound memory growth in long-lived shell cell sessions. Each `def` grows the Glia `Env`, each `load` caches bytes in a `thread_local! HashMap` with no eviction. A long-lived session or malicious client can grow WASM linear memory until the host OOMs.
**Why:** Per-session isolation via VatListener spawn mode means each shell connection is a separate WASM process. But there's no ceiling on how large that process can grow.
**Context:** Options: (a) WASM linear memory limit via wasmtime config, (b) session TTL (kill after N minutes), (c) Env size limit in glia, (d) `load` cache eviction. The VatListener connection rate limiting TODO (P2) provides the multiplier backstop (max concurrent sessions). This TODO handles per-session growth. Design doc: `~/.gstack/projects/wetware-ww/lthibault-master-design-20260402-192805.md`.
**Effort:** S
**Priority:** P2
**Depends on:** Shell cell (ww shell)

## Glia-level finally / resource cleanup via effects
**What:** `with-resource` or `finally` pattern — cleanup handlers that run on scope exit.
**Why:** Rust Drop handles Rust-side cleanup, but Glia code can't hook into scope exit.
**Context:** Design doc notes this as follow-up. Key question: does `finally` run if effect handler resumes?
**Depends on:** #247 (needs with-handler resume infrastructure)

## `glia lint` — static analysis for effect type consistency
**What:** A lint pass that checks effect type keywords used in `perform` against those handled in `match`/`with-effect-handler`. Catches typos like `:typo` vs `:fail` at dev time.
**Why:** Runtime can't warn without noise — retry handlers that succeed on first try would warn every time. Static analysis catches the real bugs before production.
**Context:** Erlang has Dialyzer for this. Glia's dynamic typing limits what's statically checkable, but keyword constants in perform/effect clauses are low-hanging fruit. Start with: collect all `perform :X` and `(effect :X ...)` in a file/module, flag mismatches.
**Depends on:** match + pattern matching

## Guard clauses for `match`
**What:** `(pattern :when guard-expr) body` — conditional pattern matching.
**Why:** Completes the pattern matching story. Clojure's core.match and Erlang both have guards.
**Context:** Guards evaluate in the scope of the pattern's bindings. If guard is falsy, fall through to next clause. The pattern module's `match_pattern` is currently pure (no eval dependency); guards would require threading the evaluator through pattern matching. Design doc has full syntax spec in Deferred Work section.
**Depends on:** match + pattern matching

## Cache: bloom filter for mutex contention reduction
**What:** Add a lock-free bloom filter in front of `Mutex<ArcInner>` in `PinsetCache`. Definite-miss CIDs skip the mutex entirely.
**Why:** Under adversarial guest load, many concurrent `ensure()` calls for uncached CIDs contend on the mutex. Bloom absorbs misses without touching the lock.
**Context:** Size generously (100K entries at 0.001% FPR = ~244KB, ~20 hash functions, ~40ns per check). Never rebuild — stale bits just mean spurious lock acquisitions, not correctness issues. Study `quick_cache` source for concurrent bloom patterns.
**Depends on:** `crates/cache` (weighted ARC)

## Cache: metrics and observability
**What:** Hit rate, eviction count, weight utilization, inflight count. Expose via `tracing` spans or a `CacheStats` struct.
**Why:** Can't tune `budget` or `inline_threshold` without visibility into cache behavior.
**Context:** Pure additive — no runtime impact on existing code paths. Add counters to `ensure()` hot path.
**Depends on:** `crates/cache` (weighted ARC)

## Cache: mutable path caching (`/ipns/`, `/p2p/`)
**What:** Support caching mutable paths with TTL-based invalidation.
**Why:** v1 only caches content-addressed paths (`/ipfs/`). Mutable paths need TTL and re-resolution.
**Context:** IPNS records have a TTL field. `/p2p/` paths resolve via DHT with its own caching semantics. Needs design work around invalidation strategy.
**Depends on:** `crates/cache` (weighted ARC)

## Import caching (idempotent require)
**What:** Make `(import "foo")` idempotent. Second call returns cached bindings instead of re-evaluating the file. Like Clojure's `require`.
**Why:** Without caching, every `(import "utils")` re-reads `/lib/utils.glia`, re-evals it in a fresh Env, and re-binds all prefixed names. Wasteful if called from multiple modules. Also a correctness question: if `utils.glia` has side effects, re-import runs them again.
**Context:** Cache key options: module name (simple), or CID of the underlying file (content-addressed, survives layer changes). Start with module name. For .glia: cache the resulting bindings map. For .wasm: cache the capability reference (if the process is still alive). Need to decide what happens if the underlying file changes between imports (hot reload?). v1 re-evals every time.
**Effort:** S
**Priority:** P2
**Depends on:** import system (#166)

## ~~RPC handshake timeout for VatClient.dial()~~ ✅
**RESOLVED:** `VatClient::dial()` now wraps `remote_cap.when_resolved()` in a 30s `tokio::time::timeout` after `rpc_system.bootstrap()`. If the remote accepts the libp2p stream but never speaks Cap'n Proto, the dial fails with a clear timeout error instead of hanging indefinitely.

## ~~Epoch-watching in accept loops (VatListener + StreamListener)~~ ✅
**RESOLVED:** Both accept loops now use `tokio::select!` to watch the epoch guard's `watch::Receiver` for changes. When the epoch sequence advances past the issued sequence, the loop breaks with a log warning. Same pattern in both `vat_listener.rs` and `stream_listener.rs`.

## ~~Protocol namespace collision between StreamListener and VatListener~~
**RESOLVED:** Stream and vat protocols now use distinct prefixes:
`/ww/0.1.0/stream/{name}` vs `/ww/0.1.0/vat/{cid}`.

## Connection rate limiting for VatListener
**What:** Every incoming connection in `VatListenerImpl` spawns a new WASI cell process with no concurrency limit. A malicious peer (or many peers) can flood connections, causing unbounded process spawning.
**Why:** Each cell holds WASM memory, an RPC system, and a libp2p stream. Unbounded spawning is a resource exhaustion vector.
**Context:** Add a semaphore or max-connections limit to the accept loop. Consider making the limit configurable via the `listen()` params or a sensible default (e.g., 64 concurrent cells per protocol).
**Effort:** S
**Priority:** P2

## ~~Bootstrap timeout in handle_vat_connection~~ ✅
**RESOLVED:** `handle_vat_connection()` now wraps `bootstrap_request()` in a 10s `tokio::time::timeout`. Produces a clear error referencing `system::serve()`.

## ~~Dual DHT — LAN + WAN content routing~~ ✅
**RESOLVED:** `kad_lan` field added to `WetwareBehaviour` running `/ipfs/lan/kad/1.0.0` in server mode. Dual-dispatch provide/findProviders with cross-DHT PeerId dedup via `FindRequest`. Kubo peers classified by `is_lan_addr()` into WAN/LAN routing tables. 10 unit tests for extracted helpers. Design doc at `~/.gstack/projects/wetware-ww/lthibault-feat-local-routing-design-20260329-131709.md`.

## Thread-per-subsystem runtime (Pingora-inspired) (#302)
**What:** Replace the single shared multi-threaded tokio runtime with a thread-per-subsystem topology. Each subsystem (libp2p swarm, HTTP/WAGI, executor, epoch) gets its own OS thread with its own `current_thread` tokio runtime. The executor gets N worker threads for M:N cell scheduling via fuel-based cooperative yielding.
**Why:** Isolation (cells can't starve the swarm), predictable latency (no cross-subsystem task stealing), foundation for hot restart and per-cell resource accounting.
**Context:** Inspired by Cloudflare Pingora's threading model, but without the Pingora dependency. `wasmtime::Store` is `!Send`, so executor threads must use `current_thread` + `LocalSet`. AIMD fuel scheduler yields every 10K instructions, enabling cooperative M:N scheduling within each thread. Cross-thread communication stays as tokio channels. Design doc at `~/.gstack/projects/wetware-ww/lthibault-master-design-20260331-182015.md`.
**Effort:** M-L
**Priority:** P1
**Depends on:** AIMD fuel scheduler (done)

## Metrics-over-WAGI cell
**What:** A `Cell::http("/metrics")` that exposes executor pool stats (cell counts per worker, spawn channel depth, compilation cache hit rate) as Prometheus-format metrics over the WAGI HTTP path.
**Why:** Operators need visibility into runtime health without attaching a debugger. Standard Prometheus scraping works with existing monitoring stacks.
**Context:** Deferred from thread-per-subsystem scope (#302). The executor pool exposes `cell_counts` and `worker_count()` already. CompilationService (when built) will track cache hits/misses. Wire these into a metrics cell that formats as Prometheus text exposition.
**Effort:** S
**Priority:** P2
**Depends on:** WAGI host implementation (Phase 2), CompilationService

## Worker health monitoring / heartbeats
**What:** Each executor worker thread emits periodic heartbeat timestamps. A monitor checks for stale workers (no heartbeat in N seconds) and logs warnings.
**Why:** A stuck WASM cell (infinite loop that doesn't yield fuel) silently blocks its worker thread. Without heartbeats, the operator can't tell which worker is stuck or that capacity is degraded.
**Context:** Deferred from thread-per-subsystem scope (#302). Implementation: each worker updates an `AtomicU64` timestamp after each fuel yield. A lightweight monitor thread (or the Host supervisor) periodically scans timestamps. Stale = no update in 5s. Log warning with worker ID and last-known cell name.
**Effort:** S
**Priority:** P2
**Depends on:** Thread-per-subsystem runtime (#302)

## ~~Nested LocalSet cleanup in spawn_rpc_inner~~ ✅
**RESOLVED:** `spawn_rpc_inner()` in `src/cell/executor.rs` and both spawn paths in `src/rpc/mod.rs` now use `tokio::task::spawn_local()` targeting the ambient worker `LocalSet` instead of creating nested `LocalSet`s. RPC systems and stderr drains run as sibling tasks on the worker, enabling proper M:N cooperative scheduling.

## WAGI host-side implementation (axum + route table, Phase 2)
**What:** Implement `--with-http host:port` flag that spawns an axum router. Routes by path prefix from `Cell::http(prefix)` custom section. Each request dispatches a cell spawn to the `ExecutorPool` via `SpawnRequest` channel, pipes CGI env vars + body to stdin, reads CGI response from stdout.
**Why:** `Cell::http` variant exists in the type system. Phase 1 delivers WAGI adapter, lightweight spawn, and `ww test http` CLI. Phase 2 adds the production HTTP server.
**Context:** WagiAdapter (`src/dispatcher/wagi.rs`) provides `build_cgi_env()` and `parse_cgi_response()`. All cells now get data_streams + membrane RPC (lightweight flag removed); WAGI cells use stdin/stdout for CGI I/O while accessing host capabilities via the WIT side-channel. axum runs on the dedicated WagiService thread (`current_thread` runtime). WASM execution happens on executor threads. ~100-150 lines for the server + route table.
**Effort:** M
**Priority:** P2
**Depends on:** WAGI adapter (done), lightweight spawn (done), AIMD fuel scheduler (done), thread-per-subsystem runtime (#302)

## HTTP-to-capnp bridge module
**What:** A capnp cell that translates HTTP requests into capability invocations. This is an application-level module, not a runtime feature. An HTTP/WAGI cell (Cell::http) that reads CGI env vars from the host, dials a capnp service via VatClient, invokes a method, and returns the result as a CGI response on stdout.
**Why:** Enables HTTP clients to interact with typed capabilities without speaking capnp-rpc. The bridge is a regular cell, not special runtime machinery.
**Context:** This is intentionally application-level. The bridge cell would be a WASM binary with `Cell::http` that uses the guest Membrane to dial capnp services. It translates REST-style routes to capability method calls. Could be generic (schema-driven routing) or hand-written per service. Uses wagi-guest crate for CGI env var reading and response formatting.
**Effort:** M
**Priority:** P3
**Depends on:** WAGI host implementation (Phase 2), VatClient guest-side

## mDNS for Kubo-less LAN peer discovery
**What:** Add `libp2p::mdns::tokio::Behaviour` to `WetwareBehaviour` to discover LAN peers without Kubo. mDNS is a **peer discovery source** that feeds the LAN DHT routing table — not a routing primitive. It does not touch Cap'n Proto or the guest API.
**Why:** The dual DHT bootstraps the LAN routing table from Kubo's swarm peers. Without Kubo (or in environments where Kubo has no private-address peers), the LAN DHT starts empty. mDNS enables zero-config LAN discovery. Note: mDNS does NOT work in cloud/container environments (no multicast). Kubo bootstrap is the fallback/primary for those environments. Dual DHT and mDNS are orthogonal — can be built and merged independently.
**Context:** mDNS adds ~25-40 lines (config, event handling, address reconciliation). CI consideration: GitHub Actions runners may not support mDNS multicast, so mDNS-dependent tests should be `#[ignore]` or gated behind an env check. All critical logic remains testable via `LocalRouting` and mock swarm channels.
**Effort:** S (CC: ~30 min)
**Priority:** P3
**Depends on:** Dual DHT (architecturally orthogonal but LAN DHT should exist first so mDNS has a routing table to feed)

## Multi-language WAGI examples (Go, Python)
**What:** WAGI cell examples in Go (via TinyGo) and Python (via componentize-py). Proves that any language compiling to wasm32-wasip2 can serve HTTP through Wetware.
**Why:** The WAGI model's main selling point is language-agnostic WAGI cells. Rust-only examples don't demonstrate this.
**Context:** TinyGo targets wasm32-wasip2 natively. componentize-py wraps CPython into a WASI component. Both toolchains are maturing but have sharp edges. Defer until toolchains stabilize and the Rust WAGI path is proven in production.
**Effort:** M
**Priority:** P3
**Depends on:** WAGI host implementation (Phase 2)

## CidTree: concurrent directory listing cache
**What:** Replace `Mutex<LruCache>` in `CidTree` with a concurrent cache (`dashmap` or `quick_cache`) to reduce contention under high concurrent cell load.
**Why:** Every path resolution for every guest call acquires the dir_cache mutex for each directory level. With many cells sharing a CidTree, this serializes all FS operations at the lock.
**Context:** CID-keyed entries are immutable, making this a read-mostly workload. `dashmap` or `quick_cache` would allow concurrent reads without lock contention. Profile first to confirm this is actually a bottleneck before migrating.
**Effort:** S
**Priority:** P3
**Depends on:** CidTree virtual filesystem (src/vfs.rs)

## CidTree: streaming reads for large files
**What:** Add a streaming read path for CidTree-backed files that pipes IPFS content directly to the WASI read buffer instead of materializing the entire file to staging first.
**Why:** Current approach fetches full file content to `staging_dir/CID` on `open_at`. For large files (ML models, datasets), this blocks the open call until the entire file is downloaded.
**Context:** Requires implementing custom `read_via_stream` in `fs_intercept.rs` instead of delegating to wasmtime-wasi's standard impl. This breaks the "delegate everything" pattern which is the current design's main simplicity win. Only worth doing when large-file workloads exist.
**Effort:** M
**Priority:** P3
**Depends on:** CidTree virtual filesystem (src/vfs.rs)

## MCP server over HTTP+SSE (Mode 2)
**What:** Run the McpAdapter as an HTTP+SSE endpoint on the node, so remote MCP clients can connect without a local `ww mcp` process. Same adapter code, different transport.
**Why:** Enables web-based MCP clients and remote LLM-to-node connections without requiring the LLM to run on the same machine as the node.
**Context:** Mode 1 (`ww mcp` over stdio) is the primary interface. Mode 2 reuses the same `ProtocolAdapter` trait with an HTTP+SSE transport instead of stdio. The design doc (`~/.gstack/projects/wetware-ww/lthibault-master-design-20260326-223714.md`) already accounts for this. Not needed for the initial demo since Claude connects to a local host and dials remote nodes via capabilities.
**Effort:** M
**Priority:** P3
**Depends on:** `ww mcp` (Mode 1, stdio transport)
