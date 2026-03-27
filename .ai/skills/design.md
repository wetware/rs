# Design

Help the user design a Wetware application.  Follow a structured
process — don't jump straight to code.

## Phase 1: Discovery

Understand what the user wants to build.  Ask about:

- **What does the app do?**  What problem does it solve?
- **Who are the agents?**  How many, what roles, do they trust
  each other?
- **What capabilities do they need?**  Networking, storage, identity,
  custom protocols?
- **How do they coordinate?**  Single node?  Multi-node?  On-chain
  epoch coordination?
- **What are the trust boundaries?**  What should agents NOT be able
  to do?

Summarize your understanding back to the user before proceeding.

## Phase 2: Architecture

Based on discovery, design the system with the user.  Cover:

- **Image layout**: which layers, what goes in `boot/`, `bin/`,
  `svc/`, `etc/`.  Read `doc/images.md` for conventions.
- **Capability map**: which capabilities each agent needs.  Draw
  from the capability table in `doc/capabilities.md`.  Identify
  what should be attenuated.
- **Membrane design**: what pid0 exports to the network, what
  children receive.  Read `doc/architecture.md`
  (§ "The Membrane pattern").
- **Protocol design**: if agents communicate, what stream protocols
  do they use?  Read the chess example's listener/dialer pattern
  in `examples/chess/`.
- **Coordination model**: standalone, multi-node, or epoch-managed
  via `--stem`?

Present the architecture as a clear diagram or structured outline.
Get user sign-off before moving to implementation.

## Phase 3: Handoff

Produce a design document the user (or another AI) can execute from:

- System diagram showing agents, capabilities, and data flow
- Capability map: per-agent list of required capabilities and
  any attenuations
- Image layout: directory tree for each image layer
- Protocol specs: Cap'n Proto interface sketches for custom RPCs
- Open questions and risks

Do not write code in this skill.  The output is a design, not an
implementation.
