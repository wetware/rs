# Review

Audit a Wetware application for capability hygiene, security, and
correctness.  The user may point you at their own code or at an
example in this repo (try `examples/chess/`).

## What to check

### 1. Principle of least authority

For each agent, answer:

- What capabilities does it hold?
- Does it need all of them?
- Could any be attenuated further (e.g. read-only IPFS, scoped
  routing, restricted executor)?

Read `doc/capabilities.md` for the capability model.  Read
`doc/architecture.md`, section "The Membrane pattern" for how
attenuation works.

### 2. Trust boundaries

- Does pid0 give children more authority than they need?
- Could a compromised child escalate through the capabilities
  it holds?
- Are network-exported Membranes appropriately restricted?
- Is Terminal authentication used where needed?

### 3. Image layout

- Does the FHS structure follow conventions?  Read `doc/images.md`.
- Are layers composed correctly?  Later layers should override,
  not duplicate.
- Is `bin/main.wasm` present in the union?

### 4. Protocol correctness

- Do Cap'n Proto schemas match the implementation?
- Are stream protocols registered and discovered correctly?
- Is RPC bidirectionality used appropriately?

### 5. Epoch safety

- If `--stem` is used, do agents handle re-grafting correctly?
- Are stale capabilities caught and retried?
- Is there state that doesn't survive an epoch transition?

## Output

Present findings as:

1. **Summary** — overall assessment (1-2 sentences)
2. **Findings** — numbered list, each with severity
   (critical / warning / suggestion) and a concrete fix
3. **Capability map** — table showing each agent's current
   capabilities vs. recommended minimum

When done, offer to return to the main menu from PROMPT.md.
