# TODOs

## Async NativeFn variant
**What:** Add `Val::AsyncNativeFn` — a Rust-side closure returning a Future.
**Why:** Current NativeFn is sync-only. Future stdlib (http, file I/O) needs async native fns.
**Context:** Resume function is sync (channel send is instant), so not needed for #247. But NativeFn will be used beyond resume. The async gap surfaces when Glia stdlib expands.
**Depends on:** #247 (NativeFn ships there)

## Glia-level finally / resource cleanup via effects
**What:** `with-resource` or `finally` pattern — cleanup handlers that run on scope exit.
**Why:** Rust Drop handles Rust-side cleanup, but Glia code can't hook into scope exit.
**Context:** Design doc notes this as follow-up. Key question: does `finally` run if handler resumes?
**Depends on:** #247 (needs with-handler resume infrastructure)

## `glia lint` — static analysis for effect type consistency
**What:** A lint pass that checks effect type keywords used in `perform` against those handled in `match`/`with-effect-handler`. Catches typos like `:typo` vs `:fail` at dev time.
**Why:** Runtime can't warn without noise — retry handlers that succeed on first try would warn every time. Static analysis catches the real bugs before production.
**Context:** Erlang has Dialyzer for this. Glia's dynamic typing limits what's statically checkable, but keyword constants in perform/effect clauses are low-hanging fruit. Start with: collect all `perform :X` and `(effect :X ...)` in a file/module, flag mismatches.
**Depends on:** match + pattern matching

## Guard clauses for `match`
**What:** `(pattern :when guard-expr) body` — conditional pattern matching.
**Why:** Completes the pattern matching story. Clojure's core.match and Erlang both have guards.
**Context:** Guards evaluate in the scope of the pattern's bindings. If guard is falsy, fall through to next clause. The pattern module's `match_pattern` is currently pure (no eval dependency); guards would require threading the evaluator through pattern matching. Design doc has full syntax spec in Deferred Work section.
**Depends on:** match + pattern matching
