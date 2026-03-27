# Concepts

Walk the user through *why* Wetware exists and the mental model
behind it.  Read and reference these files:

| Topic | Key files |
|-------|-----------|
| Ambient authority vs capabilities | `doc/architecture.md` (§ "No ambient authority") |
| The three layers (host → kernel → children) | `doc/architecture.md` (§ "Layers") |
| The Membrane pattern | `doc/architecture.md` (§ "The Membrane pattern") |
| Capability lifecycle and epoch scoping | `doc/capabilities.md` |
| Image layers and FHS convention | `doc/images.md` |
| Network architecture | `doc/architecture.md` (§ "Network architecture") |
| On-chain coordination | `doc/architecture.md` (§ "Epoch lifecycle") |

## Suggested order

1. Start with **the problem**: agentic frameworks give agents ambient
   authority.  Any code can call any API, read any secret.  Show the
   comparison table from `doc/architecture.md`.
2. Introduce **capabilities as the alternative**.  A process can only
   do what it's been handed a capability to do.  Explain the ocap
   model: having a reference IS authorization.
3. Walk through the **three layers** (host, kernel, children) using
   the ASCII diagram.  Emphasize that the host is deliberately simple
   — it's the sandbox, the agent is the policy engine.
4. Explain the **Membrane pattern**: how pid0 receives capabilities,
   wraps/attenuates them, and exports them to the network.
5. Cover **epochs**: on-chain coordination, capability revocation,
   re-grafting.

After each topic, ask if the user wants to go deeper or move on.
