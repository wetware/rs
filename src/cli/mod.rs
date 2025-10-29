//! WW CLI module
//!
//! This module groups CLI-specific functionality and provides a simple entrypoint
//! surface you can import as `ww::cli`. It organizes the following components:
//! - `boot`: boot peer discovery and DHT bootstrap configuration
//! - `config`: CLI-oriented configuration (log levels, tracing, IPFS URL helpers)
//! - `resolver`: service resolver for versioned WASM services and IPFS content
//!
//! Additionally, it re-exports the `Command` entrypoint used to run a cell,
//! so callers can do `ww::cli::Command { ... }.run().await`.

pub mod boot;
pub mod resolver;

// Re-export the cell execution entrypoint for convenience under `ww::cli::Command`.
pub use crate::cell::executor::Command;

/// Convenience wrapper to run the CLI command directly from `ww::cli`.
pub async fn run(cmd: Command) -> anyhow::Result<()> {
    crate::cell::executor::run_cell(cmd).await
}
