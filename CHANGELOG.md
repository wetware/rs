# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- **HTTP cells via WAGI** — serve HTTP from any language that compiles to wasm32-wasip2 (#304)
  - `WagiAdapter`: host-side CGI env var injection and response parsing (16 unit tests)
  - `wagi-guest` crate: zero-dependency helpers for reading requests and writing responses
  - Counter example rewritten from 305 lines of binary framing to 32 lines with `wagi-guest`
- **AIMD fuel scheduler** — fair cooperative scheduling for multiple cells on one thread (#303)
  - Cells yield every 10K WASM instructions, I/O-efficient cells get more fuel, compute-heavy cells get less
  - Enables M:N cell scheduling without tokio work stealing
- **Process.kill() RPC** — terminate a running cell from the host (#305)
  - Sends kill signal via watch channel, cell exits with code 137
  - Both lightweight and full spawn paths support kill via `tokio::select!`
- **Lightweight spawn path** — faster cell startup for ephemeral HTTP cells (#305)
  - Skips membrane/RPC setup for WAGI cells, reducing per-request overhead
- **Thread-per-subsystem validation spikes** — Phase 0 of the Pingora-inspired runtime (#306, refs #302)
  - Two cells interleave on shared LocalSet via fuel yields
  - Two Cap'n Proto RPC systems coexist on shared LocalSet
  - Off-thread WASM compilation achieves 267x speedup via serialize/deserialize
  - Worker thread topology validated with explicit `current_thread` runtime
