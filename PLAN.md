# Plan: Real Async Guest Transport (WASI io/streams + io/poll)

## Context
- The guest transport currently uses a custom `wetware:streams` interface with stub pollables and a busy-spin loop in guest code.
- Host side streams are built on unbounded channels and `try_recv`, so readiness is never awaited.
- Guest code (pid0/child-echo) has been refactored to a shared guest runtime, but it still spins when idle.

## Motivation
- Remove CPU busy-spins in guests when waiting for RPC traffic.
- Align transport with standard WASI `io/streams` + `io/poll`, enabling real async readiness.
- Use simple in-memory pipes to keep the implementation small and robust.

## Decision
- Proceed with **Option A**: replace custom pollables with WASI streams/poll.
- Use **in-memory pipes** (`tokio::io::duplex`) with Wasmtime's `AsyncReadStream` / `AsyncWriteStream`.

## Progress (completed)
- Added shared guest runtime crate in `guests/guest-runtime` and refactored `child-echo` + `pid0` to use it.
- Guest stream setup and polling loop are centralized in `guests/guest-runtime/src/lib.rs`.
- Updated `wit/streams.wit` to return WASI `io/streams` resources.
- Replaced channel-based transport with `tokio::io::duplex` pipes and WASI `AsyncReadStream`/`AsyncWriteStream`.
- Updated host RPC setup to use host-side duplex halves directly.
- Switched guest runtime wait loop to WASI `io/poll::poll` (no busy spin).

## Plan (next steps)
1) Verify build + runtime
   - Rebuild host and guest artifacts to ensure the new WIT bindings compile.
   - Run the existing guest samples (pid0/child-echo) to confirm no regressions.

2) Tighten documentation
   - Document the new WASI `io/streams` transport contract and recommended poll loop.
   - Note the duplex buffer size and how to tune it.

## Open Questions
- Confirm preferred duplex buffer size for host<->guest pipes.
- Decide where to centralize the WASI io/streams linkage: keep in `Proc` or lift to a shared helper.
