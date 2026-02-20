/*! Top-level configuration module for Wetware

This module centralizes configuration primitives that were previously
scoped under `cli::config`. Moving these items to the crate root allows
non-CLI subsystems (like `cell`) to depend on configuration without
creating circular dependencies.

Exports:
- `init_tracing()`: initializes global tracing subscriber

*/

/// Initialize tracing using `RUST_LOG` (default: `ww=info`).
///
/// Attempts to initialize a global `tracing_subscriber` (no-op if already set).
pub fn init_tracing() {
    // Initialize tracing (ignore error if already initialized)
    #[cfg(not(target_arch = "wasm32"))]
    {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("ww=info"));
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .try_init();
    }
}
