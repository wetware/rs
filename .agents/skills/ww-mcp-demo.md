---
name: ww-mcp-demo
description: 3-minute MCP demo -- verified WASM execution with provenance
reads:
  - doc/ai-context.md
---

# MCP Quickstart

Get from installed to "my AI agent just executed verified WASM" in
3 minutes.  This is the ONE skill that matters for first-time users.

## Prerequisites

Run these checks yourself before proceeding:

1. `ww --version` — binary installed?
2. `ww doctor` — look at the "Install state" section:
   - Identity exists?
   - Daemon running?
   - Claude Code MCP configured?

If anything is missing, run `ww perform install` and come back.

## Step 1 of 3: Execute WASM via MCP (~30 seconds)

Use the wetware MCP tool to evaluate a simple Glia expression:

```
Tool: wetware eval
Expression: (+ 1 2)
```

The response should include `[CID: bafyrei...]` followed by the
result.  That CID is the content hash of the WASM binary that
executed your expression.  Point it out:

> "See that `[CID: ...]` line?  That's the content address of the
> exact WASM binary that ran your code.  Every execution includes
> the hash of what ran it."

## Step 2 of 3: Provenance demo (~1 minute)

Now show that different code produces a different CID:

1. Run a different expression: `(perform host :id)`
2. Compare the CID — it should be the SAME as Step 1 (same WASM
   binary, different expression).

Then explain:

> "The CID didn't change because the same binary ran both
> expressions.  The CID tracks the CODE, not the input.  If
> someone swapped the binary, the CID would change — your agent
> can verify what ran."

## Step 3 of 3: Capability security (~1 minute)

Show that the Membrane controls what code can do:

1. Run: `(perform host :peers)` — should work (Host capability
   is granted)
2. Try an action on a capability that ISN'T granted to this cell.

When the second call fails, explain:

> "That failed because the Membrane didn't grant that capability
> to this cell.  No ambient authority — every permission is
> explicit.  Your AI agent runs sandboxed code that can ONLY do
> what it's been allowed to do."

## Name the win

> "Your AI agent just executed verified, capability-sandboxed code
> on your machine.  No cloud.  No Docker.  No trust assumptions.
> The CID proves what ran.  The Membrane controls what it can do."

## What's next?

Ask what brought them here and route accordingly:

- Building something specific -> `/ww-build-app`
- Want to understand the architecture -> `/ww-concepts`
- Want to review existing code -> `/ww-review-app`
