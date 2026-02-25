# std

Guest SDK and built-in agents for the wetware runtime.

## Layout

| Path | Package | Role |
|------|---------|------|
| `std/system/` | `system` | Guest SDK — connects a WASM agent to the host over WASI streams and drives the Cap'n Proto RPC event loop. All guests link against this. |
| `std/kernel/`  | `kernel`    | Init agent (pid0) — grafts onto the host Membrane, re-exports an attenuated capability to peers. |
| `std/shell/`   | `shell`   | Interactive shell (in development). |

## Convention

Every component builds to `boot/main.wasm` inside its own directory:

```
std/kernel/boot/main.wasm    ← ww build std/kernel
std/shell/boot/main.wasm     ← ww build std/shell
```

Build artifacts are **not committed**. They are published to IPFS:

```bash
ww build std/kernel
ww push std/
# → /ipfs/<CID>
```

## Building

```bash
# Build a single component
ww build std/kernel

# Or with cargo directly
cargo build -p kernel --target wasm32-wasip2 --release
```
