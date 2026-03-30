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
> 9. **schema-inject** — post-build cell type injection
>    (`crates/schema-id/src/bin/schema-inject.rs`)
> 10. **Effects** — `perform`, `with-handler`, `with-cap-handler`,
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

For `schema-inject`, run `cargo run -p schema-id --bin schema-inject -- --help`
yourself and show the user the actual CLI output.  Then walk through
the three modes with examples:
- `--raw bitswap` — raw libp2p streams
- `--http /api/v1` — HTTP/FastCGI routing
- `--capnp schema.bytes [--no-ipfs]` — typed Cap'n Proto RPC

Note: `--no-ipfs` (capnp only) skips pushing canonical schema bytes
to IPFS via Kubo.  Useful offline or when Kubo isn't running.
Protocol IDs for raw cells must not contain `/` (host prefixes
`/ww/0.1.0/stream/` automatically).

For effects, walk through the four forms:
- `(perform :keyword data)` — raise a keyword effect
- `(perform cap :method args...)` — raise a cap-scoped effect
- `(with-handler {:keyword (fn [data resume] ...)} body)` — install
  keyword handlers
- `(with-cap-handler cap (fn [data resume] ...) body)` — install a
  cap handler

Explain the handler stack (dynamic scope, newest-first matching)
and one-shot `resume`.  Read `crates/glia/src/effect.rs` for the
implementation.  Emphasize: effects are the *only* way to cross
the process boundary — anything not wrapped in an effect is local.

## After each topic

> Found what you needed?  Want to look at something else, or
> head back to the main menu?

Don't assume they want the next topic in sequence — ask.
