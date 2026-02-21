# CLI Reference

## ww run

Start the wetware daemon and boot an image.

```
ww run [OPTIONS] <IMAGE>...
```

### Arguments

| Argument | Description |
|----------|-------------|
| `<IMAGE>...` | One or more image layers (local paths or `/ipfs/Qm...`). Later layers override earlier ones via per-file union. The merged result must contain `bin/main.wasm`. |

### Options

| Flag | Default | Description |
|------|---------|-------------|
| `--port <PORT>` | `2025` | libp2p swarm listen port |
| `--wasm-debug` | off | Enable WASM debug info for guest processes |
| `--stem <ADDR>` | none | Atom contract address (hex, 0x-prefixed). Enables epoch lifecycle: reads on-chain HEAD, prepends as base image layer, watches for updates. |
| `--rpc-url <URL>` | `http://127.0.0.1:8545` | HTTP JSON-RPC endpoint for `eth_call` / `eth_getLogs` |
| `--ws-url <URL>` | `ws://127.0.0.1:8545` | WebSocket JSON-RPC endpoint for `eth_subscribe` |
| `--confirmation-depth <N>` | `6` | Blocks to wait before finalizing a `HeadUpdated` event |

### Examples

```sh
# Run a local image
ww run images/kernel

# Run with a custom port
ww run --port 3030 images/kernel

# Run with Stem contract (epoch lifecycle)
ww run --stem 0x1234...abcd images/kernel

# Stack image layers: on-chain base + local overlay
ww run --stem 0x1234...abcd ./my-app

# Custom RPC endpoints
ww run --stem 0x1234...abcd \
  --rpc-url http://rpc.example.com:8545 \
  --ws-url ws://rpc.example.com:8546 \
  --confirmation-depth 12 \
  images/kernel
```

### TTY detection

When stdin is a terminal, the host sets `WW_TTY=1` in the guest
environment. The default kernel uses this to choose between interactive
shell mode (TTY) and daemon mode (non-TTY). See [shell.md](shell.md).

### Environment variables

| Variable | Set by | Description |
|----------|--------|-------------|
| `WW_TTY` | host (if stdin is a TTY) | Signals interactive mode to the guest |
| `PATH` | host (default: `/bin`) | Search path for `.wasm` executables |
| `RUST_LOG` | user | Controls host-side tracing verbosity |
