# AI Context

Concise reference for AI agents working with Wetware.  Skills
read this on demand — it is NOT embedded in the system prompt.

For full details, read `doc/architecture.md` and the files
referenced below.

---

**Wetware** is a peer-to-peer operating system for autonomous agents.
It replaces ambient authority with capability-based security.  Agents
run as WASM processes called **Cells** with zero ambient authority —
they can only do what they've been explicitly granted capabilities
to do.

**Cells** are the unit of computation.  Each Cell is a WASM binary
whose stdio is wired to a transport by the host.  The `WW_CELL_MODE`
envvar tells the guest what plumbing it's running under:

| `WW_CELL_MODE` | stdio carries | Host wires up |
|----------------|--------------|---------------|
| `vat` | Cap'n Proto RPC | `/ww/0.1.0/vat/{cid}` listener |
| `raw` | raw libp2p stream bytes | `/ww/0.1.0/stream/{protocol}` listener |
| `http` | CGI env vars + stdin/stdout | WAGI (CGI for WASM) |
| absent | Cap'n Proto RPC (host channel) | pid0 — full Membrane graft |

The kernel (`boot/main.wasm`) is pid0 — a raw cell whose stdio
is the host's Cap'n Proto RPC channel, not a libp2p stream.  It
grafts the full Membrane and spawns all other cells.

Schema bytes for capnp cells are compiled at build time and passed
explicitly to `VatListener.listen()` via init.d scripts.  Guests
dispatch on subcommands: no args = cell mode, `"serve"` for the
service loop.  `ww init` scaffolds a complete cell project;
`ww build` produces all artifacts.

Architecture (three layers):
- **Host** (`ww` binary): boots a libp2p swarm, loads the kernel
  WASM, serves a Membrane over Cap'n Proto RPC.
- **Kernel** (pid0): calls `membrane.graft()` to obtain capabilities
  (Host, Runtime, Routing, Identity, HttpClient).  Interprets the FHS
  image layout.  All policy lives here.
- **Children**: spawned by pid0 with attenuated capabilities.

Key abstractions:
- **Cell type system**: schema bytes passed explicitly via RPC;
  `WW_CELL_MODE` envvar indicates transport (vat, raw, http).
- **Membrane**: the capability hub.  `graft()` returns epoch-scoped
  capabilities.  pid0 can wrap/filter capabilities and export an
  attenuated Membrane to the network.
- **Epoch lifecycle**: when `--stem` points to an on-chain Atom
  contract, capabilities are revoked on each epoch advance.  Agents
  re-graft automatically.
- **FHS images**: layers are stacked with per-file union.  Later
  layers override earlier ones.  `ww run --stem 0xABC /ipfs/QmX ./local`
- **Glia shell**: Clojure-inspired REPL where capabilities are
  first-class values.  `(perform host :id)`, `(perform ipfs :cat "/ipfs/Qm...")`.
- **Cap'n Proto RPC**: bidirectional — both host and guest can serve
  and consume capabilities.

AI integration -- drivetrain, not engine:
Wetware doesn't embed an LLM.  The LLM (Claude, Ollama, etc.)
connects *to* a Wetware node over MCP and gets a Glia shell.
Wetware is the drivetrain; the LLM is the driver.  "Agent" means
any autonomous process -- AI, human, script.  Wetware controls
what they're *allowed to do*, not what they *are*.  The MCP
endpoint itself is a Cell -- a sandboxed WASM process with scoped
capabilities, not special host code.

MCP tools -- how AI agents interact:
The MCP cell exposes per-capability tools derived from the
membrane graft.  Each capability becomes an MCP tool with an
`action` parameter:
- **host** -- `id`, `peers`, `addrs` (node identity + peers)
- **routing** -- `provide`, `find_providers` (DHT routing)
- **runtime** -- `run` (load + execute WASM binaries)
- **identity** -- `sign`, `verify` (Ed25519 operations)
- **http-client** -- `get`, `post` (outbound HTTP)
- **import** -- `import` (load Glia module by path)
- **eval** -- evaluate any Glia s-expression (primary interface)

`eval` is the primary power interface.  Per-cap tools are the
discovery layer -- they make Glia's eval surface legible to AI
agents without requiring Glia syntax knowledge.

~/.ww user layer:
Run `ww perform install` to bootstrap `~/.ww/` (boot, bin, lib,
etc/init.d).  Mount it: `ww run --mcp std/kernel ~/.ww`.
AI agents write files to `~/.ww/`; cells read from it.
Generate identity: `ww keygen > ~/.ww/etc/identity`.

The problem Wetware solves:

```
Traditional process:        Wetware Cell:
  env vars     -> yes         env vars     -> only if explicitly passed
  filesystem   -> yes         filesystem   -> none; content via IPFS capability
  network      -> yes         network      -> no
  syscalls     -> yes         syscalls     -> WASI subset only
  ambient auth -> yes         ambient auth -> none
                              graft caps   -> the only authority
```

Capabilities after grafting:

| Capability | Purpose |
|------------|---------|
| Host | Peer identity, addresses, peer management |
| Runtime | Load WASM binaries, obtain scoped Executors |
| Routing | Kademlia DHT (provide, findProviders) |
| Identity | Host-side signing (private key never enters WASM) |
| HttpClient | Outbound HTTP requests |
| StreamListener / StreamDialer | P2P byte streams for raw cells |
| VatListener / VatClient | Cap'n Proto RPC for capnp cells |

Quick start:
```
rustup target add wasm32-wasip2
make                             # builds everything (host + kernel + shell + examples)
cargo run -- run std/kernel   # drops into Glia shell
```

Once in the shell:
```clojure
/ > (perform host :id)           ;; peer identity
/ > (perform host :peers)        ;; connected peers
/ > (perform host :addrs)        ;; listen addresses
/ > (help)                       ;; available capabilities
/ > (exit)                       ;; quit
```

Concurrency model (E-ordering):
Method calls on a single Cap'n Proto object are serialized -- no
races within an object.  Calls across objects are independent and
concurrent.  Pipelining lets you chain calls on promises:
`ipfs.unixfs().cat(path)` resolves in one round-trip, not three.
No locks, no semaphores -- the object IS the synchronization
boundary.

Platform vision (from `doc/designs/economic-agent-platform.md`):
Wetware is the economic coordination layer for autonomous agents.
Agents hold assets, trade services, coordinate across trust
boundaries, and attest to their behavior.
