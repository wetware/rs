# Kernel Shell

The wetware kernel (`pid0`) provides two runtime modes, selected
automatically based on whether standard input is a terminal.

## Interactive mode (TTY)

When `ww run` detects a TTY on stdin it sets `WW_TTY=1` in the guest
environment. The kernel starts a Clojure-inspired Lisp REPL:

```
/ ❯ (perform host :id)
"00240801122025c7ea..."
/ ❯ (perform runtime :run (load "bin/my-tool.wasm"))
...
/ ❯ (exit)
```

### Syntax

Every expression is an S-expression. Effects use the `perform` form:
the first argument is the capability, the second is a keyword naming
the method, and the rest are arguments:

```
(perform capability :method [args...])
```

Strings are double-quoted. Symbols are bare words. Comments start with
`;` and run to end of line.

### Capabilities

#### host

| Method | Example | Description |
|--------|---------|-------------|
| `id` | `(perform host :id)` | Peer ID (hex-encoded) |
| `addrs` | `(perform host :addrs)` | Listen multiaddrs |
| `peers` | `(perform host :peers)` | Connected peers with addresses |
| `connect` | `(perform host :connect "/ip4/1.2.3.4/tcp/2025/p2p/12D3...")` | Dial a peer |

#### runtime

| Method | Example | Description |
|--------|---------|-------------|
| `run` | `(perform runtime :run (load "bin/my-tool.wasm"))` | Load WASM, spawn a process, capture stdout |

#### ipfs

| Method | Example | Description |
|--------|---------|-------------|
| `cat` | `(perform ipfs :cat "/ipfs/QmFoo...")` | Fetch IPFS content (UTF-8 or byte count) |
| `ls` | `(perform ipfs :ls "/ipfs/QmFoo...")` | List directory entries |

IPFS methods pipeline through the `UnixFS` sub-API of the
`Ipfs.Client` capability on the session.

### Built-ins

| Command | Description |
|---------|-------------|
| `(cd "<path>")` | Change working directory |
| `(help)` | Print available capabilities and methods |
| `(exit)` | Terminate the kernel |

### PATH lookup

Any command that is not a known capability or built-in triggers a PATH
lookup. The kernel scans each directory in `PATH` (default: `/bin`) for
two candidates in order:

1. `<dir>/<cmd>.wasm` — flat single-file binary
2. `<dir>/<cmd>/main.wasm` — image-style nested binary

The first match wins. The bytes are loaded via `runtime.load()` to obtain
an `Executor`, then `executor.spawn()` runs the process. Standard output
is captured and printed; the exit code is reported on error.

```
/ ❯ (my-tool "arg1" "arg2")
```

This looks for `/bin/my-tool.wasm` or `/bin/my-tool/main.wasm` in the
image filesystem. The nested form lets a command be a full FHS image
directory with its own `boot/`, `etc/`, etc.

## Daemon mode (non-TTY)

When stdin is not a terminal, the kernel enters daemon mode:

1. Grafts onto the host membrane and obtains a session
2. Logs a JSON readiness message to stderr:
   ```json
   {"event":"ready","peer_id":"0024080112..."}
   ```
3. Blocks on stdin until the host closes it (signaling shutdown)
4. All stdin data is discarded; no interactive input is processed
