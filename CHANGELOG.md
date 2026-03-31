# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- Process.kill() RPC for cell termination (#305)
  - Kill signal via watch channel, exit code 137 (SIGKILL convention)
  - Both lightweight and full spawn paths support kill via `tokio::select!`
- Lightweight spawn path for ephemeral cells (#305)
  - Skips membrane/RPC setup for WAGI/CGI cells, reducing per-request overhead
- WAGI adapter and guest crate for HTTP cells (#304)
  - `WagiAdapter` with `build_cgi_env()` and `parse_cgi_response()` (16 unit tests)
  - `wagi-guest` crate: zero-dependency helper library for WAGI cells
  - Counter example rewritten from 305 lines of FastCGI to 32 lines using `wagi-guest`
- AIMD fuel scheduler for cooperative M:N cell scheduling (#303)
  - Additive-increase multiplicative-decrease fuel budgeting at wasmtime host call boundaries
  - Cells yield every 10K instructions; I/O-efficient cells converge to 10M fuel, compute-heavy to 10K
  - 10 unit tests for FuelScheduler convergence and clamping behavior
