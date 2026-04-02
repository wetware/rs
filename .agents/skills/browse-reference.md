# Browse Reference

Deep-dive into schemas, CLI, and shell commands.

## Start with what they need

Don't present a wall of options cold.  Ask:

> What are you looking up?  If you tell me what you're trying to
> do, I can point you to the right thing.

If they already know what they want, jump straight there.

If they want to browse, show the menu:

> Pick a topic, or tell me what you're trying to do:
>
> 1. **Cell types** — raw, http, capnp, pid0 (`capnp/cell.capnp`)
> 2. **System capabilities** — Host, Executor, Process, streams
>    (`capnp/system.capnp`)
> 3. **Membrane & auth** — Terminal, Membrane, Epoch, Identity
>    (`capnp/stem.capnp`)
> 4. **IPFS** — UnixFS, Block, Dag, Pin (`capnp/ipfs.capnp`)
> 5. **Routing / DHT** — provide, findProviders
>    (`capnp/routing.capnp`, `doc/routing.md`)
> 6. **CLI** — flags, subcommands, env vars (`doc/cli.md`)
> 7. **Shell** — Glia REPL syntax, built-ins (`doc/shell.md`)
> 8. **RPC transport** — duplex streams, scheduling (`doc/rpc-transport.md`)
> 9. **Typed bytecode layout** — `boot/main.wasm` + `boot/main.schema`
>    (`doc/architecture.md`, `capnp/cell.capnp`)
> 10. **Effects** — `perform`, `with-effect-handler`,
>     `resume` (`crates/glia/src/effect.rs`)
> 11. **Signing & keys** — Signer interface, key derivation
>     (`doc/keys.md`)
> 12. **Cross-crate schemas** — sharing Cap'n Proto definitions
>     across crates (`doc/capnp-cross-crate.md`)
> 13. **Guest API** — WASI bindings for guest WASM modules
>     (`doc/api/wasm-guest.md`)
> 14. **Design docs** — deep dives on effects, macros, HTTP surface,
>     economic platform (`doc/designs/`)

## How to present reference material

When walking through a `.capnp` file:
- Explain each interface and method in **plain language first**
- Then show the schema definition
- One interface at a time — don't dump the whole file

For the **typed bytecode layout**, explain how cell type is
determined by the FHS image structure:
- `boot/main.wasm` — the WASM component (all cell types)
- `boot/main.schema` — canonical `schema.Node` bytes (capnp cells)
- `boot/main.capnp` — symlink to source `.capnp` for human inspection

Walk through the tooling:
- `ww init <name>` — scaffolds a typed cell guest project
- `ww build` — compiles and produces all artifacts (main.wasm + main.schema)
- The kernel reads schema from `boot/main.schema` at load time

Note: protocol IDs for raw cells must not contain `/` (host prefixes
`/ww/0.1.0/stream/` automatically).

For effects, walk through the three forms:
- `(perform :keyword data)` — raise a keyword effect
- `(perform cap :method args...)` — raise a cap-scoped effect
- `(with-effect-handler target handler body)` — install a handler
  (keyword or cap target; use inline kwargs for multiple keyword
  handlers: `(with-effect-handler :k1 fn1 :k2 fn2 body...)`)

Explain the handler stack (dynamic scope, newest-first matching)
and one-shot `resume`.  Read `crates/glia/src/effect.rs` for the
implementation.  Emphasize: effects are the *only* way to cross
the process boundary — anything not wrapped in an effect is local.

## After each topic

> Found what you needed?  Want to look at something else, or
> head back to the main menu?

Don't assume they want the next topic in sequence — ask.
