# Study Examples

Walk through real examples together to see how Cells work in
practice.

## Start with what they want to learn

Don't just present a list — ask what they're after:

> We have three examples that show different cell types.  What
> sounds most useful to you?
>
> 1. **Echo** — simplest possible cell.  Good if you want to see
>    the bare minimum.  *(~5 min read)*
> 2. **Counter** — HTTP cell with FastCGI.  Good if you're building
>    a web service.  *(~10 min read)*
> 3. **Chess** — full Cap'n Proto RPC over libp2p.  Good if you want
>    to see a real multi-node app.  *(~15 min read)*
>
> Or tell me what you're trying to build and I'll pick the most
> relevant one.

---

## 1. Echo (raw cell) — ~5 min

Read files from `examples/echo/`.

| What to read | Path |
|------|------|
| Source | `examples/echo/src/lib.rs` |
| Build | `examples/echo/Makefile` |
| README | `examples/echo/README.md` |

Walk through together:

1. **What it does**: reads stdin, writes it back to stdout.  That's it.
2. **Why it matters**: this is the stdin/stdout convention that *all*
   cell types share.  Everything else builds on this.
3. **Build it**: `make -C examples/echo` → `examples/echo/bin/echo.wasm`.
   No `schema-inject` needed (no custom section).
4. **See it tested**: `examples/echo_handler_e2e.rs` shows how the
   host spawns and exercises it.

**Name the win**: "That's a complete cell.  Everything else is
just fancier plumbing on top of this pattern."

Check in before moving on.

---

## 2. Counter (HTTP/FastCGI cell) — ~10 min

Read files from `examples/counter/`.

| What to read | Path |
|------|------|
| Source | `examples/counter/src/lib.rs` |
| Build | `examples/counter/Makefile` |
| README | `examples/counter/README.md` |

Walk through together:

1. **What it does**: serves `GET /counter` (returns count) and
   `POST /counter` (increments).  405 for everything else.
2. **The key difference**: this cell has a *type tag*.  Show the
   Makefile's `schema-inject bin/counter.wasm --http /counter` step.
   "This is what tells the host to route HTTP traffic here."
3. **FastCGI protocol**: the cell speaks binary FastCGI over stdio.
   The host translates HTTP ↔ FastCGI.  Simpler than parsing HTTP/1.1.
4. **Per-request spawn**: each request gets a fresh instance.  Counter
   resets — that's expected for the demo.
5. **Build it**: `make -C examples/counter` — note the two-step
   pipeline (compile, then inject).

**Name the win**: "You've seen the full build pipeline: compile WASM,
inject cell type, host routes traffic.  That's how HTTP cells work."

Check in before moving on.

---

## 3. Chess (Cap'n Proto RPC cell) — ~15 min

Read files from `examples/chess/`.

| What to read | Path |
|------|------|
| Overview | `examples/chess/README.md` |
| Source | `examples/chess/src/lib.rs` |
| Schema | `examples/chess/chess.capnp` |
| Init script | `examples/chess/etc/init.d/chess.glia` |
| Replay design | `examples/chess/doc/replay.md` |

This is the big one.  Walk through in layers — don't dump
everything at once:

1. **The pitch** (~2 min): Two nodes play chess over libp2p.  Moves
   flow over Cap'n Proto.  Game replay published to IPFS.
   "This shows what a real multi-node Wetware app looks like."
2. **The schema** (~3 min): Read `chess.capnp`.  Show the interface.
   "This is the contract between the two nodes."
3. **The code** (~5 min): Walk through `src/lib.rs`.  Focus on: how
   it registers a listener, accepts connections, manages game state.
   Don't read every line — hit the interesting parts.
4. **The image layout** (~2 min): FHS structure with `bin/` and
   `etc/init.d/`.  "This is how it'd be deployed."

**Name the win**: "That's a full peer-to-peer application: typed RPC,
DHT discovery, IPFS publishing, image packaging."

---

## After each example

> Want to dig into a specific pattern?  Try another example?
> Or move on to building something of your own?

Always offer the exit back to the main menu.
