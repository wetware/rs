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

A minimal WASM component that demonstrates the basic setup for a Wetware Protocol guest. It initializes Cap'n Proto RPC over WASI streams and runs a simple event loop.

See [default-kernel/README.md](default-kernel/README.md) for detailed documentation.

## Requirements

- **Rust nightly toolchain**: Examples use `wasm32-wasip2` target which requires unstable features
- **WASI Preview 2**: Examples use WASI Preview 2 APIs via the `wasip2` feature
- **ww crate with guest feature**: Examples depend on `ww` with the `guest` feature enabled

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
   - Dependency on `ww` with `features = ["guest"]`
   - WASM-compatible dependencies (e.g., `tokio` with limited features)
3. Add `#![feature(wasip2)]` to your `lib.rs`
4. Create a `Makefile` similar to `default-kernel/Makefile`
5. Update the top-level `Makefile` to include your example

For more details, see the `default-kernel` example as a reference implementation.

