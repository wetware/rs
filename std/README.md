# std

Standard crates for the wetware guest environment.

## Layout

| Crate | Package | Role |
|-------|---------|------|
| `std/runtime/` | `runtime` | Guest SDK — connects a WASM component to the host over WASI streams and drives the Cap'n Proto RPC event loop. All guest processes link against this. |
| `std/kernel/` | `kernel` | Init process (pid0) — grafts onto the host Membrane, obtains a Session, and enters either **daemon mode** (non-TTY) or **shell mode** (TTY Lisp REPL). Builds to `images/kernel/`. |

## Naming convention

Each executable crate in `std/` has a corresponding directory in `images/`:

```
std/kernel/  →  images/kernel/bin/main.wasm
```

Library crates (`std/runtime/`) have no `images/` counterpart.

## Building

```sh
make guests    # compile all std/ guest crates to wasm32-wasip2
make images    # install artifacts into images/
make kernel    # shorthand: build std/kernel + install to images/kernel
```
