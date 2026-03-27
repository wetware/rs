# Examples

Walk through the chess example as a real-world demonstration.
Read files from `examples/chess/`.

| Topic | Key files |
|-------|-----------|
| Overview | `examples/chess/README.md` |
| Game replay design | `examples/chess/doc/replay.md` |
| Handler source | `examples/chess/handler/` |
| Service source | `examples/chess/service/` |

## Suggested walkthrough

1. **What it does**: two nodes play chess over libp2p.  One registers
   a listener, the other discovers it via DHT and connects.  Moves
   flow over a bidirectional Cap'n Proto stream.  The game replay is
   published to IPFS as a content-addressed linked list.
2. **How it's built**: walk through the handler code — how it
   registers a protocol, accepts connections, manages game state.
3. **Key patterns**: listener registration, DHT discovery,
   bidirectional streams, IPFS publishing.
4. **Image layout**: show how the chess example is structured as
   an FHS image with `bin/main.wasm`.

After each section, ask if the user wants to dig deeper into a
specific pattern or move on.
