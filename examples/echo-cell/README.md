# Echo Cell

Minimal WASI echo cell for integration testing. Reads all of stdin, writes
it to stdout unchanged, then exits.

## How it works

The echo cell implements the WASI `cli::run` guest interface. On start, it
polls stdin in a loop, copying each chunk to stdout verbatim. On EOF it
flushes and exits. No dependencies beyond `wasip2` and `wit-bindgen`.

This is a `Cell::raw` cell (no Cap'n Proto schema, no custom section). The
host spawns it as a WASI process and pipes stdin/stdout via `ByteStream`
capabilities. It's the simplest possible cell, useful for validating the
full spawn-pipe-collect pipeline without protocol-specific logic.

## Used by

- `examples/echo_handler_e2e.rs` — end-to-end test that exercises
  `Executor.bind()` → `BoundExecutor.spawn()` → stdin/stdout round-trip,
  and `HttpServer::handle()` (Mode A: per-request spawn).

## Building

```sh
cargo build -p echo-cell --target wasm32-wasip2 --release
cp target/wasm32-wasip2/release/echo_handler.wasm examples/echo-cell/bin/echo_cell.wasm
```

Or, if the pre-built binary in `bin/` is current, the e2e test uses it
directly via `include_bytes!`.

## Running the e2e test

```sh
cargo run --example echo_handler_e2e
```
