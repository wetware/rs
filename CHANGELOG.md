# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- Release pipeline: 4-platform binary matrix (linux x86_64/aarch64, macOS x86_64/aarch64) with GitHub Actions
- Multi-arch container images (linux/amd64 + linux/arm64) pushed to ghcr.io/wetware/ww
- Cosign keyless container image signing via GitHub OIDC
- CHECKSUMS.txt with multihash (BLAKE3 + SHA2-256) and raw sha256sum-compatible section
- Install script (`scripts/install.sh`): one-liner installer with OS/arch detection and checksum verification
- `Cross.toml` for Cap'n Proto cross-compilation on aarch64-unknown-linux-gnu
- `Containerfile.release` for fast container builds from pre-built binaries
- Install script test suite (`tests/test_install.sh`)

### Fixed
- **Security:** Identity private key no longer exposed to WASM guests. `resolve_identity()` reads identity directly from `--identity` path, never from the merged FHS tree (which is preopened to guests via WASI and published to IPFS).
- MCP wiring uses absolute binary path via `current_exe()`, fixing PATH ambiguity.
- `claude mcp add/remove` now handles idempotent exit codes (server already exists / not found) without erroring.

### Changed
- Moved release builds from `rust.yml` to dedicated `release.yml` workflow
- `ww perform install` suppresses internal daemon_install noise, shows clean checklist output.
- Daemon plist/systemd unit passes `--identity` as a CLI flag instead of a `path:/etc/identity` mount.
- `~/.ww/{kernel,shell,mcp}/bin/` image roots created and populated with embedded WASM on install.

## [0.0.5.0] - 2026-04-06

### Added
- `crates/stem/` crate: `StemSource` async trait abstracting epoch sources, enabling both on-chain (Atom contract) and off-chain (IPNS) epoch anchors behind a common interface.
- `AtomSource`: wraps existing `AtomIndexer` + `Finalizer` behind the `StemSource` trait.
- `IpnsSource`: polls IPNS names via IPFS HTTP API, emitting `Provenance::Timestamp` epochs for off-chain deployments.
- `StemEvent`: backend-agnostic epoch event type for the shared pin/swap/broadcast pipeline.
- `HttpClient.name_resolve()` and `name_publish()` for IPNS operations via IPFS HTTP API.

### Changed
- **Breaking:** `Epoch.adopted_block` replaced with `Epoch.provenance: Provenance` enum (variants: `Block(u64)` for on-chain, `Timestamp(u64)` for off-chain). Cap'n Proto schema updated with union-based provenance and literate documentation.
- `src/epoch.rs` refactored: `handle_epoch_advance()` is now source-agnostic, shared by all epoch backends. Legacy `run_epoch_pipeline()` preserved for backward compatibility.

## [Unreleased]

### Added
- `ww perform install`: full bootstrap (dirs, identity, daemon, MCP wiring, summary).
- `ww perform uninstall`: clean removal of daemon, MCP config, optional ~/.ww deletion.
- `EmbeddedLoader`: WASM images embedded in binary via `include_bytes!()` for pre-built distribution. ChainLoader priority: HostPath > Embedded > IPFS (local files override embedded for hot-patching).
- MCP tool responses now include `[CID: ...]` content hash for provenance tracking. CID computed from WASM bytecode via blake3, passed to guest via `WW_CELL_CID` env var.
- `ww doctor` install state checks: identity, daemon running, Claude Code MCP configured.
- CI: WASM images built before host binary (embedded via include_bytes). Container pushed to ghcr.io.
- `.agents/skills/mcp-quickstart.md`: 3-minute quickstart demonstrating provenance and capability security.
- build.rs: clear compile error when WASM files missing in release mode, empty stubs in debug mode.

### Changed
- `ww perform install` now generates identity at `~/.ww/identity` (was `~/.ww/etc/identity`).
- CI container registry switched from private registry to ghcr.io (uses GITHUB_TOKEN).
- `.agents/skills/onboard-new-user.md` rewritten for binary-first install flow.

### Removed
- `VERSION` file (version source of truth is now `Cargo.toml` only).
- `.agents/skills/{explain-concepts,browse-reference,study-examples}.md` moved to archive.

### Changed
- Val::Map now backed by persistent CHAMP trie (im::HashMap) via ValMap wrapper. O(1) clone, O(log N) insert, O(1) lookup for maps with 32+ entries (77-92% improvement). ValMap is the future seam for IPLD-backed persistent maps.
- `extract_method` moved to glia crate (was duplicated in kernel and caps), returns `(&str, &[Val])` instead of `(String, Vec<Val>)` for zero-alloc capability dispatch.
- Fn/Macro equality: `Rc::ptr_eq` identity semantics (was always-false). Functions are now equal to themselves.
- Hash + Eq implemented for Val (enables use as map keys, set elements).

### Added
- `AtomicBloom`: lock-free bloom filter for ARC cache (100K capacity, 0.001% FPR, ~244KB). `PinsetCache::probably_cached()` public API for lock-free presence checks.
- `doc/designs/fuel-scheduling.md`: EWMA ratio estimator design doc.
- Criterion benchmarks for streams, glia map, ARC cache, kernel dispatch.
- ARC cache hit/miss/eviction Prometheus counters.
- Tracing spans on RPC listeners.

### Added
- Epoch-bound Terminal login: challenge-response now signs `nonce || epoch_seq` (16 bytes), preventing both same-epoch and cross-epoch replay attacks
- Graceful epoch shutdown: configurable drain delay before epoch broadcast (SIGTERM/SIGKILL model for in-flight operations)
- `doc/replay-protection.md`: four-layer defence model documentation (domain separation, epoch binding, epoch guards, on-chain finality)

### Changed
- `Signer.sign()` now takes `(nonce, epochSeq)` instead of just `(nonce)` — old clients will fail auth (correct behavior)
- TerminalServer requires `epoch_rx: watch::Receiver<Epoch>` at construction
- EpochService accepts `drain_duration: Duration` (default 1s via `--epoch-drain-secs`)

## [0.0.5.0] - 2026-04-06

### Added
- Terminal login unit tests: matching epoch, wrong epoch_seq, epoch-advance race condition
- Epoch drain delay unit tests: deferred broadcast timing and zero-drain regression
- `EpochAdvancingSigner` test helper for simulating epoch races during auth

### Changed
- SigningDomain payload_type renamed from `/{domain}/nonce` to `/{domain}/challenge` (reflects 16-byte `nonce || epoch_seq` payload)

### Added
- `PollSet` for multiplexing extra WASI pollables (stdin, listeners) alongside RPC in guest poll_loop
- `system::run_with(poll_set, f)` entry point for guests needing concurrent async I/O
- `PollLoopExit` with cycle counter for diagnosing RPC connection drops
- 51 new tests: MCP tool dispatch, caps effect handlers, ByteStream I/O, HttpClient allowlist, membrane E2E
- `ww perform install` — bootstrap ~/.ww user layer (boot, bin, lib, etc/init.d). Idempotent.
- `ww doctor` — environment health check (rustc, cargo, wasm32-wasip2, optional Kubo/Ollama)
- MCP dynamic tools: per-capability MCP tools generated from membrane graft (host, routing, runtime, identity, http-client, import). Each tool has per-action parameter schemas. eval remains primary interface.
- MCP security: input escaping (glia_escape), action allowlisting, no generic tools for unknown capabilities
- TODOS.md: Export.schema population, MCP resources, MCP prompts, eval error improvements

### Changed
- MCP cell: async stdin via StreamReader + PollSet (fixes host:peers connection drop)
- MCP cell: tools/list returns per-cap tools dynamically instead of single static eval tool
- .agents/prompt.md: document MCP tools and ~/.ww workflow for AI agents
- Extract shared effect handlers into std/caps crate (shell + MCP share, no duplication)
- Rename NamedCap to Export in stem.capnp (membrane exports capabilities)
- Export: use Capability + Schema.Node types instead of AnyPointer + Data
- Membrane.graft() returns `List(Export)` instead of named typed fields; capabilities looked up by name
- Guest runtime: unify three duplicate poll loops (drive_rpc_only, drive_rpc_with_future, block_on) into a single generic `poll_loop<T>()`
- Guest runtime: replace `futures::noop_waker`/`poll_unpin` with `std::task::Waker::noop()`/`Pin::new().poll()`
- Glia effect handler: simplify state machine (factor out repeated handler stack push, remove no-op match)

### Fixed
- Shell cell: missing import handler (broke all eval with "target must be a keyword or cap")
- Shell E2E tests: WASM path mismatch (tests were silently skipping)

### Removed
- Dead code: `RpcDriver`, `DriveOutcome`, `drive_until`, `block_on` (zero callers)

### Added
- Glia: `(def m (perform import "path"))` loads and caches modules as a capability-gated effect
- `ww run --mcp`: MCP server cell (std/mcp/) with shared caps crate, Claude Code integration
- Auction example: HTTP/WAGI endpoint at /auction (curl-able JSON status)
- `HttpClient.post()`: outbound HTTP POST capability for WASM guests (domain-scoped, epoch-guarded)
- Mindshare schema + project scaffold: symmetric p2p context sharing for LLMs (`examples/mindshare/`)
- Glia shell: `(perform auction :compare)` discovers providers and compares fuel prices
- `--metrics-addr` flag: optional Prometheus metrics endpoint for fuel observability
- Fuel auction example: ComputeProvider vat cell with RFQ protocol
- `doc/guest-runtime.md`: design spec for the hand-rolled single-threaded async runtime
- `FuelPolicy` schema: `Executor.spawn()` accepts a fuel allocation policy (scheduled or oneshot)
- `FuelEstimator::new_oneshot()`: spawn cells with fixed fuel budgets that trap at exhaustion
- `Identity.verify()`: Ed25519 signature verification on the membrane (symmetric with sign)
- Init.d scripts for auction, echo, counter, and mindshare examples (all 7 examples now bootable)

### Fixed
- Example Makefiles: `make -C examples/foo` works from project root (CARGO variable)
- Oracle init.d: replace invalid `(with ...)` syntax with `(def http ...)` cap binding
- Counter example: remove stale schema-inject step (removed in #313)
- Shell cell: zero warnings (fix unused mut, duplicate build_dispatch call, allow dead_code on scaffolding)

## [0.0.4.1] - 2026-04-03

### Fixed
- Chess and discovery examples: add missing `http.capnp` to build (required by stem.capnp import)
- Chess example: remove stale IPFS graft dependency, replay logging is now local
- Remove unused `ipfs_capnp` module from chess and discovery (stem.capnp doesn't import it)

## [0.0.4.0] - 2026-04-03

### Added
- `ww shell` CLI: connect to a running node and evaluate Glia expressions remotely
- Shell cell (`std/shell/`): WASM guest that evaluates Glia over Cap'n Proto RPC
- `Shell.eval()` interface in `shell.capnp`: send text, get result + error flag
- Client-mode libp2p swarm (`ClientSwarm`): identify + stream only, no listeners
- Shell init.d registration via VatListener spawn mode
- Prelude loaded at shell cell startup (when, and, or, defn, cond, with)
- Cap handlers for host (:id, :addrs, :peers), routing (:provide, :hash), ipfs (:cat, :ls)
- rustyline REPL with 30s eval timeout and Ctrl-D/exit support

## [0.0.3.0] - 2026-04-03

### Added
- Capability threading: `with` block grants in init.d scripts now flow into spawned cells' membranes as `extras` in the graft response
- `NamedCap` schema type for forwarding type-erased named capabilities across the spawn pipeline
- `Membrane.graft()` returns an `extras` field containing init.d-scoped capability grants
- `VatListener.listen()` and `Executor.spawn()` accept optional `caps` parameter for capability forwarding
- Dual-transport cell registration: one binary can serve both vat RPC (libp2p) and HTTP/WAGI from a single init.d script
- `with` prelude macro for capability grant bindings in glia scripts
- `Val::Cell` type: bundles wasm + schema + captured capabilities from lexical scope
- `cell` builtin: constructs Cell values, scanning the environment for `Val::Cap` bindings
- `(perform host :new-http-client)` returns an HttpClient capability to glia scripts
- `(perform host :listen <cell>)` for VatListener and `(perform host :listen <cell> "/path")` for HttpListener
- Oracle example HTTP mode: stateless per-request JSON endpoint via `curl`

## [0.0.2.0] - 2026-04-02

### Added
- Ratio-based EWMA fuel estimator replacing the binary AIMD scheduler. Tracks consumed/budget ratio via exponential moving average, sizes budgets inversely: I/O-bound cells get large budgets, compute-heavy cells get small ones.
- `src/sched.rs` module with shared scheduling constants (fuel limits, yield interval, epoch tick rate)
- Epoch-based refueling via `epoch_deadline_callback`. Compute-bound cells that don't make host calls get refueled every 10ms, preventing `Trap::OutOfFuel`. The epoch callback only updates the EWMA for cells with zero host calls that epoch, avoiding false observations for I/O cells that straddle epoch boundaries.
- Epoch tick task on executor worker 0 (calls `Engine::increment_epoch()` every 10ms on the shared Engine)
- Shared `Arc<Engine>` in `ExecutorPool` with `engine()` accessor for callers

### Changed
- AIMD fuel scheduler (`FuelScheduler`) replaced by `FuelEstimator` in `ComponentRunStates`
- Fuel budget is now the scheduling quantum: larger budget = higher effective priority
- Wasmtime engine config now enables `epoch_interruption(true)` alongside existing fuel support
- Call hook logs EWMA ratio alongside budget for observability

### Removed
- AIMD constants (`ADDITIVE_INCREMENT`, `DECREASE_FACTOR_NUM/DEN`)
- Binary 50% threshold classification (replaced by continuous ratio tracking)

## [0.0.5.0] - 2026-04-02

### Added
- `Runtime` capability with system-wide WASM compilation caching (BLAKE3-keyed, shared across all cells)
- `--runtime-cache-policy` CLI flag (`shared`/`isolated`, default `shared`, env `WW_RUNTIME_CACHE_POLICY`)
- `Executor.spawn(args, env)` now accepts per-request arguments and environment variables
- WAGI cells receive proper CGI env vars (`REQUEST_METHOD`, `PATH_INFO`, etc.) at spawn time

### Removed
- Old `Executor` interface (runBytes, echo, bind)
- `BoundExecutor` interface (collapsed into new `Executor`)
- `Host.executor` method (Runtime comes from membrane graft, not Host)
- Glia shell `(perform executor :echo ...)` command

### Changed
- Membrane graft returns `runtime :Runtime` instead of `executor :Executor`
- `Runtime.load(wasm)` is the OCAP attenuation boundary: returns a scoped `Executor` bound to one binary
- One RuntimeImpl per worker thread, shared across all cells via client cloning
- Listeners (StreamListener, VatListener, HttpListener) take `Executor` instead of `BoundExecutor`
- Kernel `:run` and `:listen` handlers use two-step `runtime.load()` → `executor.spawn()` pattern
- Cap'n Proto pipelining resolves `load()` → `spawn()` in one round-trip
- All documentation, agent prompts, example READMEs, and init.d scripts updated for Runtime API

## [0.0.4.0] - 2026-04-02

### Added
- Lazy virtual filesystem (`CidTree`) resolves guest paths through IPFS directory DAGs on demand
- 3-tier directory listing cache: in-memory LRU → staging disk → IPFS daemon
- `resolve_mounts_virtual()` produces a merged root CID without materializing files
- Atomic root-CID swap via `ArcSwap` for epoch updates (FS swap happens-before capability death)
- Pre-warm root directory listing before epoch swap

### Removed
- IPFS capability (`ipfs` field) from membrane graft response (stem.capnp)
- `EpochGuardedIpfsClient` and `EpochGuardedUnixFS` from host RPC layer
- 7 IPFS capability tests (replaced by VFS tests in fs_intercept and vfs modules)

### Changed
- Kernel ipfs handler reads through WASI virtual FS instead of Cap'n Proto RPC
- `(perform ipfs :cat path)` and `(perform ipfs :ls path)` now use `std::fs`
- `(perform ipfs :add)` returns error (deferred to stem contract)
- Kernel boot sequence (`run_initd`) uses WASI `read_dir` + `read` instead of IPFS ls/cat
- One filesystem surface: all guest content access goes through WASI virtual FS

## [0.0.3.3] - 2026-04-02

### Fixed
- All documentation uses correct `(perform cap :method ...)` Glia syntax
- Removed last stale references to schema-inject and custom sections from embedded context
- Example READMEs match actual init.d scripts (schema arg, subcommands)

## [0.0.3.2] - 2026-04-02

### Changed
- Cell guests dispatch on subcommands instead of envvars: no args = cell mode, `serve` / `consume` for application roles
- Init.d scripts only register cells; user starts services from the Glia shell with `(perform executor :run wasm "serve")`
- `WW_CELL_MODE` envvar is now informational only (set by kernel, not used for dispatch)
- `WW_CELL=1` envvar removed entirely

## [0.0.3.1] - 2026-04-02

### Changed
- All example READMEs rewritten with consistent hand-holding structure

## [0.0.3.0] - 2026-04-02

### Added
- `ipfs :add` handler: `(perform ipfs :add <bytes>)` returns CID
- `ww init <name>` scaffolds typed cell guest projects
- `ww build` places artifacts in bin/ (wasm + schema)
- Oracle example README
- Init.d scripts for all examples (chess, discovery, oracle, echo, counter)

### Changed
- Kernel reads schema from explicit RPC params (not WASM custom sections)
- Example Makefiles simplified (no schema-inject step)

### Removed
- `schema-inject` binary and `inject` feature from schema-id crate
- WASM custom section extraction from kernel
- Cell-building functions from schema-id (build_cell_capnp_message, etc.)
- ~900 lines of custom section infrastructure and tests

## [0.0.2.0] - 2026-04-01

### Added
- Price oracle demo — end-to-end multi-agent example with capability-scoped HTTP, Cap'n Proto RPC, and DHT discovery (#171)
  - `HttpClient` capability for domain-scoped outbound HTTP
  - `WagiService` — axum HTTP server on dedicated OS thread with channel-based CGI dispatch
  - `VatListener.serve()` for persistent capability export (no per-connection cell spawning)
  - `VatHandler` union: `spawn` (BoundExecutor) vs `serve` (AnyPointer) in system.capnp
  - `HttpListener` with `RouteRegistry` bridging axum threads to Cap'n Proto event loops
  - `--http-listen` CLI flag for enabling the HTTP server
  - Oracle example guest: dual-mode WASM binary (service + consumer), Blocknative gas price feed, schema CID pipeline, 7 unit tests
  - `AuthPolicy` trait stub for pluggable authentication (Terminal challenge-response)

## [0.0.1.1] - 2026-04-01

### Changed
- AIMD fuel scheduler uses classic `budget * 3/4` decrease instead of `consumed * 3/4`. Smoother convergence, no oscillation for guests alternating between I/O and compute.

## [0.0.1.0] - 2026-04-01

### Changed
- Every cell type now gets membrane RPC and WIT data_streams. HTTP/WAGI cells can access host capabilities (IPFS, routing, identity) through the WIT side-channel while using stdin/stdout for CGI I/O. No more "lightweight" cells that miss out on the capability system.
- One spawn path for all cell types. The `lightweight` flag and `new_lightweight()` are gone. Cell types are differentiated by stdin/stdout semantics, not by which host plumbing they get.
- Vat cells use stdin as a shutdown signal: closing stdin tells the cell to drain gracefully. No bytes are ever written (equivalent to Go's `<-chan struct{}`). `handle_vat_connection` closes stdin on all exit paths (peer disconnect, bootstrap timeout, capability extraction failure) to prevent orphaned processes.

## [Unreleased]

### Added
- Thread-per-subsystem runtime inspired by Cloudflare Pingora (#302)
  - Each subsystem (libp2p swarm, epoch pipeline, WASM executor) runs on its own OS thread with its own single-threaded tokio runtime
  - `Service` trait + `Host` supervisor for lifecycle management and coordinated shutdown
  - `ExecutorPool` with M:N cell scheduling: N worker threads, each `current_thread` + `LocalSet`, least-loaded assignment with round-robin fallback
  - `SwarmService` and `EpochService` run on dedicated threads, isolated from cell execution
  - `--executor-threads` CLI flag (0 = auto-detect CPU cores)
  - Kernel cell runs inside ExecutorPool instead of on the CLI thread
  - `SpawnRequest` struct with cell name, factory, and optional result channel for exit code piping
  - Per-cell tracing spans for readable multi-cell `RUST_LOG` output
  - Cell panic detection and logging via JoinHandle monitoring
  - `CompilationService` stub for off-thread WASM compilation with blake3-keyed cache
  - Bounded spawn channel (depth 64) with `try_send` to prevent self-deadlock
  - 14 unit tests covering host lifecycle, executor pool scheduling, round-robin distribution, panic handling, exit code piping, and bounded channel backpressure

### Changed
- `spawn_rpc_inner` and child cell spawn paths use ambient `LocalSet` instead of nested `LocalSet`, enabling proper M:N cooperative scheduling across cells on the same worker thread
- `SwarmService` and `EpochService` now respect shutdown signal via `tokio::select!`
- `ExecutorPool` stores worker `JoinHandle`s and joins them on drop for clean shutdown
- Process.kill() RPC for cell termination (#305)
  - Kill signal via watch channel, exit code 137 (SIGKILL convention)
  - Both lightweight and full spawn paths support kill via `tokio::select!`
- Lightweight spawn path for ephemeral cells (#305)
  - Skips membrane/RPC setup for WAGI/CGI cells, reducing per-request overhead
- Prerequisite spikes for thread-per-subsystem runtime (#306, refs #302)
  - Spike 1: Two cells interleave on shared LocalSet via fuel yields
  - Spike 2: Two Cap'n Proto RPC systems coexist on shared LocalSet
  - Spike 3: Off-thread WASM compilation (267x speedup via serialize/deserialize)
  - Bonus: current_thread runtime in std::thread (worker thread topology)
- WAGI adapter and guest crate for WAGI cells (#304)
  - `WagiAdapter` with `build_cgi_env()` and `parse_cgi_response()` (16 unit tests)
  - `wagi-guest` crate: zero-dependency helper library for WAGI cells
  - Counter example rewritten from 305 lines of FastCGI to 32 lines using `wagi-guest`
- AIMD fuel scheduler for cooperative M:N cell scheduling (#303)
  - Additive-increase multiplicative-decrease fuel budgeting at wasmtime host call boundaries
  - Cells yield every 10K instructions; I/O-efficient cells converge to 10M fuel, compute-heavy to 10K
  - 10 unit tests for FuelScheduler convergence and clamping behavior
