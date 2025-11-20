# Default Kernel Example

The default kernel is intentionally tiny: it targets `wasm32-wasip2`, prints
`Hello, Wetware!` on stdout, and then exits. Its primary goal is to demonstrate
that `ww run` can execute a WASI component and surface its stdio directly to the
operator.

## Prerequisites

- Rust nightly toolchain
- `wasm32-wasip2` target (`rustup target add wasm32-wasip2`)

## Building

From the repository root:

```bash
make example-default-kernel     # or simply `make`
```

From this directory:

```bash
cd examples/default-kernel
make build
```

The build copies the produced artifact to
`target/wasm32-wasip2/release/main.wasm`, which is the layout that `ww run`
expects.

## Running with `ww`

1. Build the guest as described above.
2. Execute it with `ww run`, pointing at the directory that contains
   `main.wasm`:

   ```bash
   ./target/release/ww run examples/default-kernel/target/wasm32-wasip2/release
   ```

`ww run` now wires host stdio directly to the guest, so you should see:

```
Hello, Wetware!
```

## Cleaning

Run `make clean` (in this directory or via the repo-level `make clean`) to wipe
`target/` and any generated `.cid` files.

## Customizing

Feel free to edit `src/lib.rs` to experiment with other WASI Preview 2 APIs.
Because the guest no longer depends on Cap'n Proto or asynchronous runtimes,
adding basic functionality is as simple as using `println!`, reading stdin, etc.
