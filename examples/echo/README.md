# Echo Cell

Minimal stdin/stdout echo cell for integration testing.

## What it demonstrates

- **Raw cell** (`WW_CELL_MODE=raw`) -- no schema, no Cap'n Proto
- `StreamListener` for byte-stream protocol handling
- WASI P2 `cli::run` guest interface
- Full spawn-pipe-collect pipeline validation

## Prerequisites

- Rust toolchain with `wasm32-wasip2` target:
  ```sh
  rustup target add wasm32-wasip2
  ```

## Building

```sh
make echo
```

Or manually:

```sh
cargo build -p echo --target wasm32-wasip2 --release
mkdir -p examples/echo/bin
cp target/wasm32-wasip2/release/echo.wasm examples/echo/bin/echo.wasm
```

## Running

### Step 1: Boot the node

Stack the echo layer on top of the kernel. The init.d script
registers the echo cell with the host's `StreamListener`.

```sh
ww run std/kernel examples/echo
```

This drops you into the Glia shell.

### Step 2: Start the service

From the Glia shell, run the echo cell interactively:

```clojure
/ > (perform runtime :run (load "bin/echo.wasm"))
```

The cell reads all of stdin, writes it to stdout unchanged, then
exits. It validates the full spawn-pipe-collect pipeline without
any protocol-specific logic.

## How it works

```
  Caller
    │
    ▼
┌──────────┐     stdin: raw bytes
│ Runtime  │ ──────────────────────► ┌───────────┐
│ +Executor│ ◄────────────────────── │ Echo Cell │
└──────────┘     stdout: same bytes  │ (WASI P2) │
                                     └───────────┘
```

The echo cell implements the WASI `cli::run` guest interface.
On start, it polls stdin in a loop, copying each chunk to stdout
verbatim. On EOF it flushes and exits. No dependencies beyond
`wasip2` and `wit-bindgen`.

This is the simplest possible cell: no schema, no capability
negotiation, no RPC. It exists to validate that the host can
spawn a WASM process, pipe bytes through it, and collect the
output.

## Init.d script

`etc/init.d/echo.glia`:

```clojure
; Register the echo cell as a raw stream handler.
; StreamListener spawns a cell per connection.
(perform host :listen runtime "echo" (load "bin/echo.wasm"))
```

The script uses `runtime.load()` to compile the binary, then
registers the resulting `Executor` with the host's
`StreamListener` under the protocol name `"echo"`. Each
incoming stream connection spawns a fresh echo cell via
`executor.spawn()`. The capability is passed explicitly -- no
ambient authority.

## Tests

The echo cell is used by the end-to-end integration test:

```sh
cargo run --example echo_handler_e2e
```

This exercises `Runtime.load()` -> `Executor.spawn()` ->
stdin/stdout round-trip.

## Files

```
examples/echo/
├── Cargo.toml          # standalone WASI P2 crate
├── Makefile            # make echo
├── README.md           # this file
├── bin/                # build output (gitignored)
│   └── echo.wasm
├── etc/
│   └── init.d/
│       └── echo.glia   # cell registration
└── src/
    └── lib.rs          # guest implementation
```
