# Default Kernel Example

The default kernel is intentionally tiny: it targets `wasm32-wasip2` and pipes
stdin to stdout asynchronously using WASI Preview 2 async streams. It implements
`AsyncRead` and `AsyncWrite` traits needed for Cap'n Proto transports, providing
a foundation for async I/O in Wasm guests. Its primary goal is to demonstrate
that `ww run` can execute a WASI component and surface its stdio directly to the
operator, leveraging wasip2's native async features.

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

`ww run` now wires host stdio directly to the guest. The kernel will echo
whatever you type:

```
$ echo "Hello, Wetware!" | ./target/release/ww run examples/default-kernel/target/wasm32-wasip2/release
Hello, Wetware!
```

## Cleaning

Run `make clean` (in this directory or via the repo-level `make clean`) to wipe
`target/` and any generated `.cid` files.

## Customizing

Feel free to edit `src/lib.rs` to experiment with other WASI Preview 2 APIs.
The kernel uses the `wasip2` crate to access WASI Preview 2 streams directly,
and manual future polling with `futures::task::noop_waker` for async runtime
support. The `AsyncStdin` and `AsyncStdout` wrappers implement `futures::io::AsyncRead`
and `AsyncWrite` traits, enabling asynchronous I/O operations that leverage WASI
Preview 2's native async stream capabilities. This pattern supports Cap'n Proto
transports and other async I/O use cases. See https://mikel.xyz/posts/capnp-in-wasm/
for more details.
