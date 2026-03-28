# TODOs

## Async NativeFn variant
**What:** Add `Val::AsyncNativeFn` — a Rust-side closure returning a Future.
**Why:** Current NativeFn is sync-only. Future stdlib (http, file I/O) needs async native fns.
**Context:** Resume function is sync (channel send is instant), so not needed for #247. But NativeFn will be used beyond resume. The async gap surfaces when Glia stdlib expands.
**Depends on:** #247 (NativeFn ships there)

## Glia-level finally / resource cleanup via effects
**What:** `with-resource` or `finally` pattern — cleanup handlers that run on scope exit.
**Why:** Rust Drop handles Rust-side cleanup, but Glia code can't hook into scope exit.
**Context:** Design doc notes this as follow-up. Key question: does `finally` run if handler resumes?
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

## RPC handshake timeout for RpcDialer.dial()
**What:** Add an RPC-level timeout to `RpcDialerImpl::dial()` so it doesn't silently hang when the remote peer accepts the libp2p stream but never speaks Cap'n Proto.
**Why:** The 30s `DIAL_TIMEOUT` only covers stream establishment. If the remote accepts the stream but doesn't run an RPC server, `rpc_system.bootstrap()` returns a proxy immediately, but method calls on that proxy hang until the TCP-level timeout (minutes). Guests see a silent hang.
**Context:** The fix is to wrap the first RPC operation (or the bootstrap itself) in a timeout. E.g., send a lightweight probe after bootstrap and fail if no response within 30s. Or use `tokio::time::timeout` around the bootstrap cap retrieval.
**Effort:** S
**Priority:** P1
**Depends on:** PR #270 (RpcListener/RpcDialer)

## Epoch-watching in accept loops (RpcListener + Listener)
**What:** The accept loops in `RpcListenerImpl::listen()` and `ListenerImpl::listen()` check the epoch guard once at entry but never recheck. A guest whose epoch goes stale continues accepting connections and spawning handlers indefinitely.
**Why:** This is a trust boundary violation. The membrane's epoch-based revocation is supposed to invalidate capabilities, but the long-lived accept loop keeps serving after revocation.
**Context:** Fix by adding a `tokio::select!` inside the accept loop that watches the epoch guard's `receiver` for changes and breaks when stale. Same pattern needed in both `src/rpc/rpc_listener.rs:70-92` and `src/rpc/listener.rs:75-92`. The dialer has a shorter TOCTOU window but should also recheck epoch after stream establishment.
**Effort:** S
**Priority:** P1
**Depends on:** PR #270 (RpcListener/RpcDialer)

## Protocol namespace collision between Listener and RpcListener
**What:** Both `Listener` (byte-stream) and `RpcListener` (capability) use the same protocol prefix `/ww/0.1.0/{suffix}`. Byte-stream Listener takes an arbitrary user string; RpcListener uses a CID. A guest could register a byte-stream listener with a name that matches a CID, causing a cross-mode collision.
**Why:** The `control.accept()` call will fail for whichever registers second, but the error won't explain the cross-mode collision. Silent misrouting if a stream-mode handler accidentally claims an RPC protocol address.
**Context:** Fix by using distinct prefixes, e.g., `/ww/0.1.0/stream/{name}` vs `/ww/0.1.0/rpc/{cid}`. This is a wire protocol change so it should be done before any stable release.
**Effort:** S
**Priority:** P2
**Depends on:** PR #270 (RpcListener/RpcDialer)

## Connection rate limiting for RpcListener
**What:** Every incoming connection in `RpcListenerImpl` spawns a new WASI handler process with no concurrency limit. A malicious peer (or many peers) can flood connections, causing unbounded process spawning.
**Why:** Each handler holds WASM memory, an RPC system, and a libp2p stream. Unbounded spawning is a resource exhaustion vector.
**Context:** Add a semaphore or max-connections limit to the accept loop. Consider making the limit configurable via the `listen()` params or a sensible default (e.g., 64 concurrent handlers per protocol).
**Effort:** S
**Priority:** P2
**Depends on:** PR #270 (RpcListener/RpcDialer)
