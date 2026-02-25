# std

Standard components for the wetware guest environment.

## Layout

| Path | Package | Role |
|------|---------|------|
| `std/runtime/` | `runtime` | Guest SDK — connects a WASM component to the host over WASI streams and drives the Cap'n Proto RPC event loop. All guests link against this. |
| `std/shell/`   | `shell`   | Interactive shell (in development). |

The kernel (`pid0`) has moved to `crates/kernel/` — see the workspace root README.

## Convention

Every component builds to `boot/main.wasm` inside its own directory:

```
std/shell/boot/main.wasm     <- ww build std/shell
```

Build artifacts are **not committed**. They are published to IPFS:

```bash
ww build std/shell
ww push std/
# -> /ipfs/<CID>
```

## Building

```bash
# Build a single component
ww build std/shell

# Or with cargo directly
cargo build -p shell --target wasm32-wasip2 --release
```
