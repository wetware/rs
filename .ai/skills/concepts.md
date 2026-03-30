# Concepts

Walk the user through *why* Wetware exists and the mental model
behind it.  **Lead with Cells** — they are the central abstraction.
Read and reference these files:

| Topic | Key files |
|-------|-----------|
| Cell type system | `capnp/cell.capnp`, `doc/architecture.md` section "Cell types" |
| Ambient authority vs capabilities | `doc/architecture.md`, section "No ambient authority" |
| The three layers (host, kernel, children) | `doc/architecture.md`, section "Layers" |
| The Membrane pattern | `doc/architecture.md`, section "The Membrane pattern" |
| Capability lifecycle and epoch scoping | `doc/capabilities.md` |
| Image layers and FHS convention | `doc/images.md` |
| Network architecture | `doc/architecture.md`, section "Network architecture" |
| On-chain coordination | `doc/architecture.md`, section "Epoch lifecycle" |

## Suggested order

1. Start with **Cells as the unit of computation**.  A Cell is a WASM
   binary with a type tag (a Cap'n Proto union in a WASM custom
   section named `"cell.capnp"`).  The tag tells the host what
   plumbing to wire up.  Show `capnp/cell.capnp`:

   - **`raw`** — stdin/stdout carry raw libp2p stream bytes.  The
     cell names a protocol ID (e.g. `"bitswap"`).  Host registers
     `/ww/0.1.0/stream/{protocol}` and spawns the cell per connection.
   - **`http`** — stdin/stdout carry FastCGI records.  The cell names
     an HTTP path prefix (e.g. `"/api/v1"`).  Host routes matching
     HTTP requests to the cell.
   - **`capnp`** — stdin/stdout carry Cap'n Proto RPC.  The cell
     embeds a `Schema.Node`; the host derives a CID from the canonical
     schema bytes and registers `/ww/0.1.0/rpc/{cid}`.
   - **absent section** — pid0 / WIT mode.  The kernel itself: it
     grafts onto the Membrane and receives full capabilities.

   Emphasize: the cell type determines how a process talks to the
   network, not what it can do internally.

2. Explain **the problem**: agentic frameworks give agents ambient
   authority.  Any code can call any API, read any secret.  Show the
   comparison table from `doc/architecture.md`.

3. Introduce **capabilities as the alternative**.  A process can only
   do what it's been handed a capability to do.  Explain the ocap
   model: having a reference IS authorization.

4. Walk through the **three layers** (host, kernel, children) using
   the ASCII diagram.  Emphasize that the host is deliberately simple
   — it's the sandbox, the agent is the policy engine.

5. Explain the **Membrane pattern**: how pid0 receives capabilities,
   wraps/attenuates them, and exports them to the network.

6. Cover **epochs**: on-chain coordination, capability revocation,
   re-grafting.

After each topic, ask if the user wants to go deeper, move to
another topic, or return to the main menu from PROMPT.md.
