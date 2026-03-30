# Review an App

Audit a Wetware application for capability hygiene, security, and
correctness.  The user may point you at their own code or at an
example in this repo.

## Start with their concerns

Don't jump straight into a checklist.  Ask:

> What are you most worried about?  Or should I do a full sweep?

If they have a specific concern, start there — it shows you're
listening and gives them a quick win.  Then offer to check the
rest.

If they want a full sweep, tell them what to expect:

> I'll check seven things: cell type correctness, least authority,
> trust boundaries, image layout, protocol correctness, effect
> hygiene, and epoch safety.  I'll flag anything I find as
> critical / warning / suggestion.  Should take a few minutes.

## What to check

Work through these in order of impact.  **Report findings as you
go** — don't save everything for the end.  Each finding is a small
deliverable that keeps the review feeling productive.

### 1. Cell type correctness

- Right custom section for the use case?
- Cell type appropriate? (`raw` for streams, `http` for REST,
  `capnp` for typed RPC, absent for pid0)
- For `capnp`: embedded schema matches actual interface?
- For `http`: path prefix matches host routing?
- For `raw`: protocol ID valid (non-empty, no `/`)?
- `schema-inject` idempotent in build pipeline?

### 2. Principle of least authority

For each agent:
- What capabilities does it hold?
- Does it need all of them?
- Could any be attenuated further?

Read `doc/capabilities.md` and `doc/architecture.md` (Membrane
pattern section).

### 3. Trust boundaries

- Does pid0 give children more authority than needed?
- Could a compromised child escalate?
- Are network-exported Membranes restricted?
- Terminal authentication used where needed?

### 4. Image layout

- FHS conventions followed?  Read `doc/images.md`.
- Layers composed correctly (override, not duplicate)?
- `bin/main.wasm` present in the union?

### 5. Protocol correctness

- Schemas match implementation?
- Streams registered and discovered correctly?
- RPC bidirectionality used appropriately?
- HTTP cells handle all expected methods?  Error codes correct?

### 6. Effect hygiene

Read the app's source and trace every operation that reaches
beyond the process.  Do this yourself — don't ask the user.

- **Trace boundary crossings**: grep for `perform`, handler
  installations, and any direct I/O.  Every network/swarm
  interaction MUST go through an effect.  If something crosses
  the boundary without `perform`, flag it as critical.
- **Check handler scope**: are handlers installed too high
  (over-privileged) or too low (effects propagate unhandled)?
- **Blast radius**: could any handler be narrowed?  A handler
  that handles more effect types than needed is a risk.
- **Unhandled effects**: trace which effects propagate to the
  top without being caught.  Flag any that should be handled.

### 7. Epoch safety

- Agents handle re-grafting correctly?
- Stale capabilities caught and retried?
- State that doesn't survive epoch transitions?

## Output

After each area, share what you found.  Then compile a summary:

1. **Summary** — overall assessment (1-2 sentences)
2. **Findings** — numbered, each with severity
   (critical / warning / suggestion) and a concrete fix
3. **Cell type audit** — confirm type is appropriate and correctly
   injected
4. **Capability map** — table: current capabilities vs. recommended
   minimum

**Start with quick wins** — if there's a one-line fix, surface
it first.  Momentum matters.

When done:

> ⚗️ That's the review.  Want to dig into any of these findings?
> Or head back to the main menu?
