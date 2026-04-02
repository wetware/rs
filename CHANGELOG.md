# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.0.3.0] - 2026-04-02

### Added
- `fs` capability for capability-gated filesystem access: `(perform fs :read "path")`
- `ipfs :add` handler: `(perform ipfs :add <bytes>)` returns CID
- `ww init <name>` scaffolds typed cell guest projects (capnp schema, build.rs, boot/main.capnp symlink)
- `ww build` produces `boot/main.schema` alongside `boot/main.wasm`
- `capnp/fs.capnp` schema for the Fs capability interface
- Oracle example README

### Changed
- Kernel loads schema from `boot/main.schema` instead of WASM custom sections
- Example Makefiles produce `boot/main.schema` instead of running `schema-inject`

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
- RPC cells use stdin as a shutdown signal: closing stdin tells the cell to drain gracefully. No bytes are ever written (equivalent to Go's `<-chan struct{}`). `handle_vat_connection` closes stdin on all exit paths (peer disconnect, bootstrap timeout, capability extraction failure) to prevent orphaned processes.

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
- WAGI adapter and guest crate for HTTP cells (#304)
  - `WagiAdapter` with `build_cgi_env()` and `parse_cgi_response()` (16 unit tests)
  - `wagi-guest` crate: zero-dependency helper library for WAGI cells
  - Counter example rewritten from 305 lines of FastCGI to 32 lines using `wagi-guest`
- AIMD fuel scheduler for cooperative M:N cell scheduling (#303)
  - Additive-increase multiplicative-decrease fuel budgeting at wasmtime host call boundaries
  - Cells yield every 10K instructions; I/O-efficient cells converge to 10M fuel, compute-heavy to 10K
  - 10 unit tests for FuelScheduler convergence and clamping behavior
