# Reference

Deep-dive into schemas, CLI, and shell commands.  Present the user
with a sub-menu and let them choose:

> 1. **Cell types** — The Cell union: raw, http, capnp, and pid0 mode
>    (`capnp/cell.capnp`)
> 2. **System capabilities** — Host, Executor, Process, ByteStream,
>    StreamListener, StreamDialer, VatListener, VatClient
>    (`capnp/system.capnp`)
> 3. **Membrane & auth** — Terminal, Membrane, Epoch, Signer, Identity
>    (`capnp/stem.capnp`)
> 4. **IPFS** — UnixFS, Block, Dag, Pin
>    (`capnp/ipfs.capnp`)
> 5. **Routing / DHT** — provide, findProviders, hash
>    (`capnp/routing.capnp`)
> 6. **CLI** — flags, subcommands, environment variables
>    (`doc/cli.md`)
> 7. **Shell** — Glia REPL syntax, built-ins, PATH lookup
>    (`doc/shell.md`)
> 8. **RPC transport** — duplex streams, scheduling, deadlock analysis
>    (`doc/rpc-transport.md`)
> 9. **schema-inject** — post-build tool for embedding cell type tags
>    (`crates/schema-id/src/bin/schema-inject.rs`)

When walking through a `.capnp` file, explain each interface and
method in plain language, then show the schema definition.

For `schema-inject`, show all three modes:
```
schema-inject wasm.wasm --raw /protocol/id
schema-inject wasm.wasm --http /api/v1
schema-inject wasm.wasm --capnp schema.bytes [--no-ipfs]
```

When the user finishes exploring a subsystem, re-present this
sub-menu or offer to return to the main menu from PROMPT.md.
