# Explain Concepts

Walk the user through *why* Wetware exists and the mental model
behind it.

## Start with their question

Don't follow a fixed order.  Ask:

> What are you most curious about?  Some starting points:
>
> 1. **Cells** — the unit of computation (what makes Wetware different)
> 2. **Capabilities** — why no ambient authority, and what replaces it
> 3. **Architecture** — the three layers (host, kernel, children)
> 4. **The Membrane** — how capabilities flow and get attenuated
> 5. **Effects** — the boundary between local and global
> 6. **Epochs** — on-chain coordination and capability lifecycle
> 7. **Images** — how code is packaged and layered
>
> Or just ask a question and I'll find the right thread.

If they pick one, cover that topic (see below), then check in.
Don't automatically proceed to the next topic — ask what they
want.

## Topic guide

For each topic, read the referenced files and explain in plain
language.  **Lead with the problem it solves**, then show how
Wetware addresses it.  One concept at a time.

### Cells

Key files: `capnp/cell.capnp`, `doc/architecture.md` section
"Cell types"

A Cell is a WASM binary with a type tag stored in a **WASM custom
section** named `"cell.capnp"`.  The tag is a Cap'n Proto union
injected post-build by `schema-inject` — the WASM itself doesn't
know about it.  When the host loads a Cell, it reads the custom
section to decide what plumbing to wire up (network listeners,
protocol bridges, etc.).  No section = pid0.

Read `capnp/cell.capnp` and show the actual schema.  Then walk
through the four variants **one at a time**, checking in between
each:

1. **`raw`** — raw libp2p stream bytes.  Think: custom binary
   protocols.  "Make sense?  Next one's more familiar."
2. **`http`** — FastCGI.  Think: REST APIs with familiar tooling.
3. **`capnp`** — Cap'n Proto RPC.  Think: typed, schema-addressed
   capabilities.
4. **absent** — pid0 mode.  The kernel itself.  "This one's rare —
   it's only for when you ARE the kernel."

Emphasize: the cell type determines how a process talks to the
*network*, not what it can do internally.

### Capabilities (ambient authority problem)

Key files: `doc/architecture.md` (section "No ambient authority"),
`doc/capabilities.md`

Start with the problem: agentic frameworks give agents ambient
authority — any code can call any API, read any secret.  Then
show the comparison table and explain ocap: having a reference
IS authorization.

### Architecture (three layers)

Key files: `doc/architecture.md` (section "Layers")

Host → Kernel → Children.  The host is deliberately simple (it's
the sandbox).  The kernel is the policy engine.  Children get only
what pid0 hands them.

### The Membrane

Key files: `doc/architecture.md` (section "The Membrane pattern")

How pid0 receives capabilities, wraps/attenuates them, and exports
them.  This is the key mechanism for security composition.

### Effects

Key files: `crates/glia/src/effect.rs`, `crates/glia/src/eval.rs`

**Lead with the why:** anything that isn't wrapped in an effect is
process-local.  It can't reach the network, can't mutate shared
state, can't affect anything "out there" in the swarm.  Effects
are the boundary between local computation and global action.

This is the flip side of "no ambient authority."  In a normal OS,
any code can call any syscall.  In Wetware, the only way to cross
the process boundary is to `perform` an effect — and an effect
only does something if a handler is installed for it.  No handler,
no action.

Walk through the two flavors:

1. **Keyword effects** — environmental, matched by name.
   `(perform :log "msg")`.  Think: "I want something from the
   environment."  The handler decides what `:log` means.
2. **Capability effects** — object-scoped, matched by identity.
   `(perform my-cap :method arg)`.  Think: "I want this specific
   capability to do something."  Only the handler holding that
   exact cap object responds.

Then show the handler side:

- `with-handler` installs keyword handlers (a map).
- `with-cap-handler` installs a handler for one specific cap.
- Handlers receive `(data, resume)`.  `resume` is a one-shot
  continuation — call it to send a value back to the performer.

**The key insight:** because effects propagate up the handler
stack, pid0 can intercept, wrap, attenuate, or deny any effect
a child performs.  This is how the Membrane composes — it's
handlers all the way up.

### Epochs

Key files: `doc/capabilities.md`, `doc/architecture.md`
(section "Epoch lifecycle")

On-chain coordination: when the epoch advances, all capabilities
are revoked.  Agents re-graft and pick up new state automatically.

### Images

Key files: `doc/images.md`

FHS convention, layer stacking, per-file union.  How code gets
packaged and deployed.

## After each topic

Check in:

> Make sense?  Want to go deeper on this, try a different topic,
> or move on to something else?

Always offer the escape hatch back to the main menu.
