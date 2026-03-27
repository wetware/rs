# Examples

Walk through the chess example as a real-world demonstration.
Read files from `examples/chess/`.

| Topic | Key files |
|-------|-----------|
| Overview | `examples/chess/README.md` |
| Game replay design | `examples/chess/doc/replay.md` |
| Source code | `examples/chess/src/lib.rs` |
| Cap'n Proto schema | `examples/chess/chess.capnp` |
| Init script | `examples/chess/etc/init.d/chess.glia` |

## Suggested walkthrough

1. **What it does**: two nodes play chess over libp2p.  One registers
   a listener, the other discovers it via DHT and connects.  Moves
   flow over a bidirectional Cap'n Proto stream.  The game replay is
   published to IPFS as a content-addressed linked list.
2. **How it's built**: walk through `src/lib.rs` — how it registers
   a protocol, accepts connections, manages game state.  Show the
   Cap'n Proto schema in `chess.capnp`.
3. **Key patterns**: listener registration, DHT discovery,
   bidirectional streams, IPFS publishing.
4. **Image layout**: show how the chess example is structured as
   an FHS image with `bin/chess-demo.wasm` and `etc/init.d/`.

After each section, ask if the user wants to dig deeper into a
specific pattern, or present the main menu from PROMPT.md.
