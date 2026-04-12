# Kernel Shell

The wetware kernel (`pid0`) provides two runtime modes, selected
automatically based on whether standard input is a terminal.

## Interactive mode (TTY)

When `ww run` detects a TTY on stdin it sets `WW_TTY=1` in the guest
environment. The kernel starts a Clojure-inspired Lisp REPL:

```
/ > (perform host :id)
"12D3KooWExample..."
/ > (perform host :addrs)
("/ip4/127.0.0.1/tcp/2025" "/ip4/192.168.1.5/tcp/2025")
/ > (exit)
```

### Connecting remotely

```sh
ww shell                                    # auto-discover local node
ww shell /dnsaddr/master.wetware.run        # connect via DNS
ww shell /ip4/127.0.0.1/tcp/2025/p2p/12D3KooW...  # direct dial
```

When no address is given, `ww shell` discovers a local node via
Kubo's LAN DHT. See [cli.md](cli.md) for details.

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

After grafting, the shell session holds references to all capabilities
the membrane provides. See [capabilities.md](capabilities.md) for the
full list and schemas.

#### host

| Method | Example | Description |
|--------|---------|-------------|
| `id` | `(perform host :id)` | Peer ID (hex-encoded) |
| `addrs` | `(perform host :addrs)` | Listen multiaddrs |
| `peers` | `(perform host :peers)` | Connected peers with addresses |
| `connect` | `(perform host :connect "/ip4/1.2.3.4/tcp/2025/p2p/12D3...")` | Dial a peer |
| `listen` | `(perform host :listen "/ip4/0.0.0.0/tcp/0")` | Listen on additional address |

#### runtime

Loading and running WASM binaries is a two-step process:

1. `runtime.load(bytes)` compiles the WASM and returns an `Executor`
2. `executor.spawn()` runs the process and captures stdout

The PATH lookup mechanism (see below) handles this automatically.

#### identity

| Method | Example | Description |
|--------|---------|-------------|
| `sign` | `(perform identity :sign data)` | Sign bytes with the node's Ed25519 key |
| `verify` | `(perform identity :verify data sig pubkey)` | Verify a signature |

#### routing

| Method | Example | Description |
|--------|---------|-------------|
| `provide` | `(perform routing :provide cid)` | Announce as provider for a CID |
| `findProviders` | `(perform routing :findProviders cid)` | Find providers for a CID |

### Built-ins

| Command | Description |
|---------|-------------|
| `(cd "<path>")` | Change working directory |
| `(def name value)` | Bind a value in the session environment |
| `(help)` | Print available capabilities and methods |
| `(exit)` | Terminate the kernel |

### PATH lookup

Any command that is not a known capability or built-in triggers a PATH
lookup. The kernel scans each directory in `PATH` (default: `/bin`) for
two candidates in order:

1. `<dir>/<cmd>.wasm` -- flat single-file binary
2. `<dir>/<cmd>/main.wasm` -- image-style nested binary

The first match wins. The bytes are loaded via `runtime.load()` to obtain
an `Executor`, then `executor.spawn()` runs the process. Standard output
is captured and printed; the exit code is reported on error.

```
/ > (my-tool "arg1" "arg2")
```

This looks for `/bin/my-tool.wasm` or `/bin/my-tool/main.wasm` in the
image filesystem. The nested form lets a command be a full FHS image
directory with its own `boot/`, `etc/`, etc.

## Daemon mode (non-TTY)

When stdin is not a terminal, the kernel enters daemon mode:

1. Grafts onto the host membrane and obtains a session
2. Logs a JSON readiness message to stderr:
   ```json
   {"event":"ready","peer_id":"12D3KooW..."}
   ```
3. Blocks on stdin until the host closes it (signaling shutdown)
4. All stdin data is discarded; no interactive input is processed
