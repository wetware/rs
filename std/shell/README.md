# kernel — Init Process

The first process to run in a wetware node; the root of the capability hierarchy.

## What it is

When `ww` boots, it launches the kernel as its initial WASM guest. The kernel occupies
a privileged position not because of any mechanism in the runtime — it is simply the
first process to receive the host's full capability surface.

The host bootstraps a `Membrane(WetwareSession)` capability over the WASI stream
connection. The kernel grafts onto this membrane to obtain an epoch-scoped session
containing `Host`, `Executor`, and `StatusPoller`.

## What it does

1. **Receives** the host's raw `Membrane` capability via `wetware_guest::serve()`.
2. **Wraps** it in a `MembraneProxy` that enforces policy (attenuation, auditing, etc.).
3. **Re-exports** the proxy back to the host as its own bootstrap capability.

The host then holds this proxy and hands it to external peers connecting over TCP.
Every capability a remote peer can exercise flows through the kernel's proxy — giving
the kernel full control over what is exposed and to whom.

## Relationship to guest processes

The kernel spawns child processes via the `Executor` capability it received. Those
children get a scoped capability surface derived from the kernel's own session —
they never have direct access to the raw host membrane.

In this sense the kernel acts like an OS init process: it bootstraps the environment,
manages process lifecycle, and mediates access to the underlying system.
