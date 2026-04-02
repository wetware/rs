# Glia Effect System

## Overview

Glia's error handling and effect system, implemented in two phases:
- **Phase 1 (#205):** `try`/`throw` with `Err(Val)` — error recovery for init.d scripts and services
- **Phase 2:** `perform`/`with-handler` — full algebraic effect system with capability-effect fusion

The design follows **Approach C (Capability-Effect Fusion):** standard Unison-style algebraic effects where the Membrane is the outermost effect handler. Every capability call is a `perform`. The handler stack mirrors the capability chain.

## Motivation

### Errors vs Faults (Hickey's distinction)
- **Errors** = programmer mistakes or bad input (in-process, fix the code)
- **Faults** = things that go wrong in the world (systemic, must respond)

### Why effects, not just try/catch
An effect system generalizes error handling to cover nine additional concerns:

| Concern | Mechanism |
|---------|-----------|
| Error recovery | `throw`/`try` (Phase 1) |
| Capability boundary visibility | `perform` makes proc-exit explicit |
| Concurrency without async/await | Fiber yield/resume via effects |
| Distributed state with policy | Handler decides local vs remote |
| Transparent retry / circuit-breaking | Handler retries, code doesn't know |
| Deterministic replay / debugging | Record effect log, replay for debugging |
| Transactional effect batching | Handler buffers, commits atomically |
| Audit trail | Handler logs every capability access |
| Supervision | Crash = effect, supervisor = handler |
| Resource lifecycle | Handler tracks acquire/release |

All share one structure: **interposing policy at a boundary** — the same thing the Membrane does at the RPC level. Effects are the Membrane's language-level twin.

## Design Decisions

1. **Effects are the ONLY way to interact with the outside world from Glia.** No backdoor calls that bypass the effect system.
2. **One-shot continuations only.** Resume or abort, no cloning (OCaml 5's pragmatic choice).
3. **Dynamic handler lookup.** `perform` walks up the handler stack at runtime (Unison-style, not Koka-style static evidence passing). Glia is dynamically typed.
4. **`throw`/`try` are sugar over `:fail` effect.** Phase 1 ships them as special forms; Phase 2 replaces them with prelude macros over `perform`/`with-handler`.

## Phase 1: Error Handling (#205)

### Language additions
- `throw` — `(throw data)` signals an error. `data` is any Val (idiomatically a map with `:type`).
- `try` — `(try expr)` evaluates expr; returns `{:ok val}` or `{:err data}`.
- `try-let` — prelude macro for bind-or-catch.
- `or-else` — prelude macro for default-on-failure.
- `guard` — prelude macro: `(guard test error-data)` throws if test is falsy.
- `ex-info` — builtin: `(ex-info "msg" {:type :foo})` constructs error map with `:message` merged with user data.

### Implementation
- Change `Result<Val, String>` to `Result<Val, Val>` internally
- All existing `Err(format!(...))` sites produce `Val::Map` with `{:type :internal :message "..."}`
- `Dispatch` trait signature changes to `Result<Val, Val>` (cross-crate API break)
- Add `Expr::Throw` and `Expr::Try` to analyzer
- `try` must NOT intercept `Val::Recur` — only `Err(Val)` values

### Examples
```clojure
(try (/ 1 0))
;; => {:err {:type :arithmetic :message "division by zero"}}

(try-let [id (perform host :id)]
  (println "connected:" id)
  (catch e
    (println "failed:" (:type e))))

(throw (ex-info "peer unreachable" {:type :network :peer "QmFoo"}))

(or-else (perform host :id) "unknown")

(guard (> n 0) (ex-info "must be positive" {:type :invalid}))
```

## Phase 2: Full Effect System (Q2)

### Language additions
- `perform` — `(perform :effect-type data)` suspends computation. Returns value from handler's `resume`.
- `with-handler` — `(with-handler {:effect-type handler-fn} body)` installs handlers. Handler fn receives `(data resume)`.

### Capability-effect fusion
- `(perform host :id)` lowers to `(perform :host {:method "id"})`
- Kernel installs Membrane as outermost handler for capability effects
- Authority checks happen in the handler (epoch validation, capability revocation)
- Stale epoch detected → handler re-grafts and resumes transparently

### Handler semantics
- **One-shot continuations:** `resume` can be called at most once. Calling twice is a runtime error.
- **Handler stack recursion:** `perform` inside a handler skips the current handler frame, dispatches to next outer handler for the same effect type.
- **`:fail` handlers CAN resume:** default handler (via `try`) does not resume. User-installed `:fail` handlers can resume, enabling retry/recovery.

### Phase transition (Q1 → Q2)
Phase 2 removes `Expr::Try`/`Expr::Throw` from the analyzer. `try`/`throw` become prelude macros:
```clojure
(defmacro throw [data]
  `(perform :fail ~data))

(defmacro try [body]
  `(with-handler
     {:fail (fn [err _resume] {:err err})}
     {:ok ~body}))
```

### Examples
```clojure
;; Testing — mock capabilities
(with-handler
  {:host (fn [req resume] (resume {:id "mock-peer"}))}
  (assert (= (perform host :id) "mock-peer")))

;; Retry on transient failure
(with-handler
  {:fail (fn [err resume]
           (when (= :network (:type err))
             (sleep 1000)
             (resume :retry)))}
  (publish-data))

;; Supervision
(with-handler
  {:fail (fn [err _resume]
           (log "crashed:" err)
           (restart-proc))}
  (run-service))

;; Audit trail
(with-handler
  {:host (fn [req resume]
           (log "host access:" req)
           (resume (perform :host req)))}
  (run-service))
```

## Open Questions

1. Should `with-handler` support a `finally` clause?
2. Should capability effects use namespaced keywords (`:ww/host`) to avoid collision?
3. Should `perform` without a matching handler error (fail-closed) or fall through?
4. Continuation mechanism for Phase 2: likely `tokio::sync::oneshot` channel pair.

## De-risk Strategy

Build standard effects (Approach A) first. Wire Membrane as handler second. If the Membrane-as-handler pattern creates problems, fall back to "Membrane handles RPC, effects handle in-proc concerns." No work is lost — the language primitives are identical either way.
