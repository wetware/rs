# images

FHS-style image directories consumed by `ww run`.

Each image is a directory tree with at least `bin/main.wasm` as its entrypoint:

```
images/<name>/
  bin/main.wasm     # guest entrypoint — required
  boot/<peerID>     # bootstrap peer multiaddrs — read by kernel
  svc/<name>/       # nested service images — spawned by kernel
  etc/              # configuration — read by kernel
```

## Contents

| Image | Built from | Description |
|-------|------------|-------------|
| `images/kernel/` | `std/kernel/` | The init process. Daemon mode (non-TTY) or Lisp shell (TTY). |

## Usage

```sh
# Run a single image
ww run images/kernel

# Layer images: later layers override earlier ones per-file
ww run images/base images/overlay

# Use an on-chain base layer + local overlay
ww run --stem 0xABC... images/kernel
```

## Building

Images are assembled by `make images`. The `bin/main.wasm` artifact is copied from the compiled crate in `std/`. Source trees and build artifacts never live here — only the installed FHS roots.
