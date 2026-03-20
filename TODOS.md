# TODOS

Deferred work items from eng review (2026-03-20). Each item has full context
so someone picking it up in 3 months understands the motivation and where to
start.

## Agent Wallet — Membrane-attenuated signing constraints

**What:** Extend the Signer capability with spending limits, transaction
whitelists, and rate limiting via Membrane attenuation.

**Why:** Enables agents to hold and spend assets safely. Without wallet
constraints, agents have unlimited signing authority — fine for demos,
dangerous for production. Unlocks Pillar 1 (Wallet) of the 12-month vision.

**Pros:** Makes the 'economic agent' story concrete. Builds on existing
`EpochGuardedMembraneSigner` in `src/rpc/membrane.rs`.

**Cons:** Requires defining a spending policy language/format. May need
on-chain integration for balance tracking.

**Context:** CEO review accepted as 3-month item #2. Deferred because
identity-based authority attenuation (its prerequisite) was also deferred.

**Depends on:** ~~Terminal(T) refactor~~ (done — PR #186), Reference Attenuating Membranes.

---

## Hot Code Reload via Epoch Advancement

**What:** A `ww publish` workflow: build WASM → publish to IPFS → update
Atom contract head → trigger epoch advancement. Running processes re-graft
with new capabilities pointing to the updated image.

**Why:** Today deploying requires manual stop/rebuild/republish/restart.
Hot reload makes the dev loop competitive with cloud platforms.

**Pros:** Killer DX. Exercises the epoch mechanism end-to-end. Proves
'code as content-addressed artifact.'

**Cons:** Needs CLI tooling. Requires careful handling of in-flight RPC
during epoch transitions.

**Context:** `EpochManager` in `src/epoch.rs` watches the Atom contract.
`EpochGuard` in `crates/membrane/src/epoch.rs` forces re-graft. Pieces
exist but aren't wired into a publish flow. CEO review item #6.

**Depends on:** Dev tooling workflows, Atom indexer (exists).

---

## Reference Attenuating Membranes — concrete GraftBuilder implementations

**What:** 3-4 reference implementations: ReadOnlyMembrane (IPFS cat but no
add), NetworkIsolatedMembrane (no Listener/Dialer), RateLimitedMembrane,
SandboxMembrane (no executor, no routing).

**Why:** The capability model's value proposition is attenuation. Without
concrete examples, it's theoretical. These serve as documentation and
building blocks.

**Pros:** Each impl is small (~50 LOC) since `GraftBuilder` trait exists.
Shows developers how to write their own attenuating Membranes.

**Cons:** Design question: should attenuation compose (chain GraftBuilders)
or be monolithic?

**Context:** `GraftBuilder` trait in `crates/membrane/src/membrane.rs`.
`HostGraftBuilder` in `src/rpc/membrane.rs` is the only implementation.
CEO review item #7.

**Depends on:** ~~Terminal(T) refactor~~ (done — PR #186, auth removed from Membrane).

---

## Wasmtime Fuel — host-level resource limits and preemptive scheduling

**What:** Configure Wasmtime's fuel mechanism to cap computation per-process.
Host sets a fuel budget per WASM instance; execution traps when exhausted.

**Why:** Without resource limits, a misbehaving guest can consume unbounded
CPU. More importantly, fuel is the path to preemptive scheduling — the host
needs to be able to suspend/resume guest execution.

**Pros:** Prevents DoS. Composable with epoch guards. Battle-tested in
Wasmtime.

**Cons:** Choosing the right budget is non-obvious. The bigger challenge:
investigate whether a fuel-exhausted process can be **resumed** (refueled)
rather than terminated. If not, we need machinery to refill fuel each time
the process returns control to the host (e.g., on WASI polls, syscalls,
os_yield). This is critical for preemptive scheduling.

**Context:** `Cell`/`CellBuilder` in `src/cell/executor.rs` creates Wasmtime
instances. Fuel is a one-line config (`config.consume_fuel(true)` +
`store.set_fuel(budget)`). The deeper question is fuel-as-scheduling, not
just fuel-as-limits.

**Depends on:** Nothing for basic limits. Preemptive scheduling research
is independent.

---

## Identity-based Authority Attenuation — GraftPolicy / auth policy oracle

**What:** A policy mechanism where Terminal.login() returns different
capabilities based on WHO logged in. Different remote peers get different
Membranes or capability sets.

**Why:** Without this, all authenticated remote peers get the same
capabilities. The CEO review identified this as 'core governance mechanism.'

**Pros:** Enables fine-grained access control at scale.

**Cons:** Requires the identity model to mature (currently one host
identity). Risk of re-introducing the 'embedded platform antipattern'
(two competing auth models) if not designed carefully — Terminal(T)
separation was specifically created to prevent this.

**Context:** Identified during eng review arch Issue 3. The bigger vision:
root authority can be anchored in a smart contract or other external auth
policy oracle. The Go `wetware/pkg` auth package had a `Policy` interface
abstracting over flat files, databases, and blockchains. That's the right
precedent — `Terminal` is where policy evaluation happens, and the `Policy`
trait maps `VerifyingKey → T` (the capability set to return).

**Depends on:** ~~Terminal(T) refactor~~ (done — PR #186), possibly per-proc identities.

---

## Glia Threading Macros and Destructuring

**What:** Add `->` (thread-first), `->>` (thread-last), and destructuring
in `let`/`fn` bindings.

**Why:** Developer ergonomics are critical path for adoption. Without
threading macros, capability chains become deeply nested:
`(query (authenticate (connect (discover "oracle"))))`.
With `->`: `(-> "oracle" discover connect authenticate query)`.
Ugly code is a liability.

**Pros:** Dramatically improves readability. Aligns with Clojure design
thesis and 'everything is a capability invocation' philosophy.

**Cons:** Threading macros done right require a **macro system**, which is
non-trivial. Research needed on hygienic macros before committing to an
implementation. This is not a "wing it" situation — understand what you're
building rather than cargo-culting Clojure's defmacro.

**Context:** CEO plan listed threading macros as later Glia stdlib phase.
The 3-month scope covers `def/let/fn/if/do/loop/recur` (core). Threading
macros are the natural next step, gated on macro system design.

**Depends on:** Glia evaluator extraction into `crates/glia`, macro system
research (hygienic macros).
