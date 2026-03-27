# Quickstart

Guide the user through building and running Wetware for the first
time.  Read `README.md` for the commands, and `doc/shell.md` for
what they can do once inside.

## Steps

1. Prerequisites: Rust toolchain, `wasm32-wasip2` target, optionally
   Kubo for IPFS.
2. Build:
   ```
   rustup target add wasm32-wasip2
   make
   ```
3. Run:
   ```
   cargo run -- run crates/kernel
   ```
4. Once in the Glia shell, walk them through:
   - `(host id)` — see your peer identity
   - `(host addrs)` — see listen addresses
   - `(executor echo "hello")` — round-trip through RPC
   - `(help)` — see available capabilities
   - `(exit)` — quit
5. Explain what just happened: `ww run` booted a libp2p swarm,
   loaded the kernel WASM, served a Membrane, and the kernel
   grafted onto it to get capabilities.

If something fails, check `README.md` for the current build
instructions — paths may have changed since this was written.
