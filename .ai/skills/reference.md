# Reference

Deep-dive into schemas, CLI, and shell commands.  Present the user
with a sub-menu and let them choose:

> 1. **System capabilities** — Host, Executor, Process, ByteStream
>    (`capnp/system.capnp`)
> 2. **Membrane & auth** — Terminal, Membrane, Epoch, Signer
>    (`capnp/stem.capnp`)
> 3. **IPFS** — UnixFS, Block, Dag, Pin
>    (`capnp/ipfs.capnp`)
> 4. **Routing / DHT** — provide, findProviders, hash
>    (`capnp/routing.capnp`)
> 5. **CLI** — flags, subcommands, environment variables
>    (`doc/cli.md`)
> 6. **Shell** — Glia REPL syntax, built-ins, PATH lookup
>    (`doc/shell.md`)
> 7. **RPC transport** — duplex streams, scheduling, deadlock analysis
>    (`doc/rpc-transport.md`)

When walking through a `.capnp` file, explain each interface and
method in plain language, then show the schema definition.

When the user finishes exploring a subsystem, re-present this
sub-menu or offer to return to the main menu from PROMPT.md.
