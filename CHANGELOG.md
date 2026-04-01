# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

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
