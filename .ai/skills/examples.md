# Examples

Walk through real examples to show how Cells work in practice.
Present a sub-menu:

> **Which example would you like to explore?**
>
> 1. **Echo** — minimal raw-stream cell (stdin/stdout echo).
>    *(recommended first example)*
> 2. **Counter** — HTTP/FastCGI cell that counts POST requests.
> 3. **Chess** — full Cap'n Proto RPC application: two nodes play
>    chess over libp2p with IPFS-published game replays.
>
> Pick a number, or tell me what you're curious about.

---

## 1. Echo (raw cell, no custom section)

Read files from `examples/echo/`.

| Topic | Key files |
|-------|-----------|
| Source | `examples/echo/src/lib.rs` |
| Build | `examples/echo/Makefile` |
| README | `examples/echo/README.md` |

### Walkthrough

1. **What it does**: a WASI component that reads stdin and writes it
   back to stdout.  No custom section — it runs in pid0/WIT mode
   or as a raw stream cell spawned by the kernel.
2. **Why it matters**: this is the simplest possible cell.  It
   demonstrates the stdin/stdout convention that all cell types share.
3. **Build**: `make -C examples/echo` compiles to
   `examples/echo/bin/echo.wasm`.  No `schema-inject` step needed
   (no custom section).
4. **Integration test**: `examples/echo_handler_e2e.rs` shows how
   the host spawns and exercises an echo cell.

---

## 2. Counter (HTTP/FastCGI cell)

Read files from `examples/counter/`.

| Topic | Key files |
|-------|-----------|
| Source | `examples/counter/src/lib.rs` |
| Build | `examples/counter/Makefile` |
| README | `examples/counter/README.md` |

### Walkthrough

1. **What it does**: an HTTP cell that serves `GET /counter` (returns
   the count) and `POST /counter` (increments and returns it).
   Other methods get 405.
2. **Cell type**: `Cell::http("/counter")`.  The Makefile compiles the
   WASM component, then runs `schema-inject bin/counter.wasm --http /counter`
   to embed the custom section.
3. **FastCGI protocol**: the cell speaks real FastCGI v1 binary
   protocol over stdio.  The host translates HTTP <-> FastCGI.
   This is simpler to parse than HTTP/1.1 and keeps the cell
   IPFS-agnostic.
4. **Per-request spawn (Mode A)**: each HTTP request spawns a fresh
   cell instance.  Counter resets every request — correct for the demo
   (persistent state requires Mode B / keepalive, not yet implemented).
5. **Key patterns**: show the `schema-inject` post-build step and
   explain idempotent injection (safe for repeated builds).

---

## 3. Chess (Cap'n Proto RPC cell)

Read files from `examples/chess/`.

| Topic | Key files |
|-------|-----------|
| Overview | `examples/chess/README.md` |
| Game replay design | `examples/chess/doc/replay.md` |
| Source code | `examples/chess/src/lib.rs` |
| Cap'n Proto schema | `examples/chess/chess.capnp` |
| Init script | `examples/chess/etc/init.d/chess.glia` |

### Walkthrough

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

---

After each section, ask if the user wants to dig deeper into a
specific pattern, try another example, or return to the main menu
from PROMPT.md.
