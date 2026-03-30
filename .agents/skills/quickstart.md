# Quickstart

Build and run Wetware in five minutes.  For first-time setup and
orientation, see `onboard-new-user.md` instead.

Three steps.  ~5 minutes total.

## Step 1 of 3: Build (~2 min)

```
rustup target add wasm32-wasip2   # one-time, skip if already done
make
```

Builds host binary, kernel, shell, and examples.  First build is
slower — that's normal.

## Step 2 of 3: Run (~30 sec)

```
cargo run -- run crates/kernel
```

Boots a libp2p swarm, loads the kernel WASM, drops into the Glia
shell.

## Step 3 of 3: Try it (~1 min)

```clojure
(host id)              ;; your peer identity
(host peers)           ;; connected peers
(executor echo "hello") ;; round-trip RPC — if this works, the stack is live
(exit)                 ;; done
```

**That's it.**  You just booted a p2p capability-secured OS: host,
kernel, Membrane, shell.

## What happened (optional — ask first)

`ww run` did three things:

1. Started a **libp2p swarm** on the configured port
2. Loaded `crates/kernel/bin/main.wasm` — the kernel Cell (pid0)
3. Spawned it with a **Membrane** — the capability hub that grants
   Host, Executor, IPFS, Routing, and Identity via Cap'n Proto RPC

The kernel grafted onto the Membrane, received epoch-scoped
capabilities, and launched the Glia shell.

## Next

> Ready to go deeper?  We can explore concepts, study an example,
> or start building something.

Present the main menu from `.agents/prompt.md`.
