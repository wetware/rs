//! Scheduling constants and primitives for cell fuel metering.
//!
//! Shared between `cell::proc` (EWMA estimator, call_hook) and `runtime`
//! (epoch tick task).  The fuel budget is the scheduling quantum: larger
//! budgets mean more instructions before yielding, giving the cell higher
//! effective priority.
//!
//! Design doc: `doc/designs/fuel-scheduling.md`

/// Starting budget for a freshly spawned cell (~1 ms at 1 GHz).
pub const INITIAL_FUEL: u64 = 1_000_000;

/// Hard ceiling.  An I/O-bound cell that never exhausts its budget will
/// converge here via the EWMA estimator.
pub const MAX_FUEL: u64 = 10_000_000;

/// Floor.  Prevents a compute-heavy cell from being throttled to zero and
/// starved of all forward progress.
pub const MIN_FUEL: u64 = 10_000;

/// Wasmtime yields the guest back to the Tokio LocalSet every this many
/// instructions.  Controls preemption granularity independently of the
/// EWMA budget — a guest with a large budget still yields frequently.
pub const YIELD_INTERVAL: u64 = 10_000;

/// Fixed-point scaling for the consumed/budget ratio.
/// 0 = pure I/O (consumed nothing), 1000 = pure compute (consumed everything).
pub const RATIO_SCALE: u64 = 1000;

/// Epoch tick interval in milliseconds.  The epoch tick task calls
/// `Engine::increment_epoch()` at this rate, triggering the
/// `epoch_deadline_callback` in every Store to refuel compute-bound cells.
pub const EPOCH_TICK_MS: u64 = 10;
