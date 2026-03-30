# Design

Help the user design a Wetware application.  Follow a structured
process — don't jump straight to code.

## Phase 1: Discovery

Understand what the user wants to build.  Ask about:

- **What does the app do?**  What problem does it solve?
- **Who are the agents?**  How many, what roles, do they trust
  each other?
- **What cell type fits?**  This is the first architectural decision:
  - `raw` — raw byte streams over libp2p (custom binary protocols,
    file transfer, tunneling)
  - `http` — HTTP/FastCGI (REST APIs, web services, familiar tooling)
  - `capnp` — Cap'n Proto RPC (typed capabilities, schema-addressed
    discovery, bidirectional)
  - pid0 (no section) — the agent IS the kernel, grafts onto
    Membrane directly
- **What capabilities do they need?**  Networking, storage, identity,
  custom protocols?
- **How do they coordinate?**  Single node?  Multi-node?  On-chain
  epoch coordination?
- **What are the trust boundaries?**  What should agents NOT be able
  to do?

Summarize your understanding back to the user before proceeding.

## Phase 2: Architecture

Based on discovery, design the system with the user.  Cover:

- **Cell type and protocol**: confirm the cell type, define the
  protocol ID / path prefix / schema.  For `capnp` cells, sketch
  the `.capnp` interface.  For `http` cells, define endpoints.
  For `raw` cells, define the wire format.
- **Image layout**: which layers, what goes in `bin/`, `svc/`,
  `etc/`.  Read `doc/images.md` for conventions.
- **Build pipeline**: source -> WASM component -> `schema-inject`
  (if non-pid0).  Show how the Makefile should look, referencing
  `examples/counter/Makefile` as a template.
- **Capability map**: which capabilities each agent needs.  Draw
  from the capability table in `doc/capabilities.md`.  Identify
  what should be attenuated.
- **Membrane design**: what pid0 exports to the network, what
  children receive.  Read `doc/architecture.md`, section
  "The Membrane pattern".
- **Coordination model**: standalone, multi-node, or epoch-managed
  via `--stem`?

Present the architecture as a clear diagram or structured outline.
Get user sign-off before moving to implementation.

## Phase 3: Handoff

Produce a design document the user (or another AI) can execute from:

- System diagram showing agents, capabilities, and data flow
- Cell type for each agent (raw / http / capnp / pid0)
- Capability map: per-agent list of required capabilities and
  any attenuations
- Image layout: directory tree for each image layer
- Build pipeline: compile + inject steps
- Protocol specs: Cap'n Proto interface sketches, HTTP endpoints,
  or wire format for custom RPCs
- Open questions and risks

Do not write code in this skill.  The output is a design, not an
implementation.

When done, offer to return to the main menu from PROMPT.md, or
suggest running the **review** skill on the design.
