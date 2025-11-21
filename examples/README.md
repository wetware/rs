# Wetware Protocol Examples

This directory contains example WASM components that demonstrate how to build and run guest programs for the Wetware Protocol.

## Overview

Examples in this directory are WASM (WebAssembly) components that can be executed by the `ww run` command. They use WASI Preview 2 (wasip2) and the `ww::guest` module to establish Cap'n Proto RPC communication over WASI streams.

## Building Examples

Examples are built automatically when you run `make` from the project root:

```bash
# Build all examples
make examples

# Or build everything (including main binary and examples)
make
```

You can also build individual examples:

```bash
# Build default-kernel example
make example-default-kernel

# Or build from the example directory
make -C examples/default-kernel build
```

## Running Examples

Once built, examples can be run using `ww run` with a volume mount:

```bash
# Run default-kernel example
./target/release/ww run /boot \
  --volume examples/default-kernel/target/wasm32-wasip2/release:/boot
```

The `--volume` flag mounts the example's build directory to a path inside the WASM environment. The example expects to find `main.wasm` at the mounted path.

## Available Examples

### default-kernel

A minimal WASM component that demonstrates async I/O using WASI Preview 2 streams.
It pipes stdin to stdout asynchronously, implementing `AsyncRead` and `AsyncWrite`
traits needed for Cap'n Proto transports and other async I/O use cases.

See [default-kernel/README.md](default-kernel/README.md) for detailed documentation.

## Requirements

- **Rust nightly toolchain**: Examples use `wasm32-wasip2` target which requires unstable features
- **WASI Preview 2**: Examples use WASI Preview 2 APIs via the `wasip2` feature
- **Example-specific dependencies**: Each example may have different dependencies (e.g., `default-kernel` uses `wasip2` and `futures`, but not `ww`)

## Project Structure

Each example follows this structure:

```
examples/
  <example-name>/
    ├── Cargo.toml      # Example dependencies and configuration
    ├── Makefile         # Build automation
    ├── src/
    │   └── lib.rs       # Example source code
    └── target/          # Build output (generated)
        └── wasm32-wasip2/
            └── release/
                └── main.wasm  # Final WASM binary
```

## Development

To create a new example:

1. Create a new directory under `examples/`
2. Add a `Cargo.toml` with:
   - `crate-type = ["cdylib"]`
   - WASM-compatible dependencies (e.g., `wasip2`, `futures`, or `ww` with `features = ["guest"]` if needed)
3. Add `#![feature(wasip2)]` to your `lib.rs` if using WASI Preview 2 APIs
4. Create a `Makefile` similar to `default-kernel/Makefile`
5. Update the top-level `Makefile` to include your example

For more details, see the `default-kernel` example as a reference implementation.

