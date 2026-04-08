/*! Top-level configuration module for Wetware

This module centralizes configuration primitives that were previously
scoped under `cli::config`. Moving these items to the crate root allows
non-CLI subsystems (like `cell`) to depend on configuration without
creating circular dependencies.

Exports:
- `init_tracing()`: initializes global tracing subscriber

*/

/// Initialize tracing using `RUST_LOG`.
///
/// Default log level depends on context:
/// - TTY (interactive shell): `ww=warn` — keep the Glia REPL clean
/// - Non-TTY (daemon/pipe): `ww=info` — standard Rust log behavior
///
/// `RUST_LOG` always takes precedence when set.
///
/// When `stderr` is true, logs are written to stderr instead of stdout.
/// This is required for MCP mode where stdout carries JSON-RPC.
///
/// Attempts to initialize a global `tracing_subscriber` (no-op if already set).
pub fn init_tracing_to_stderr(stderr: bool) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::io::IsTerminal;
        let default_filter = if std::io::stdout().is_terminal() {
            "ww=warn"
        } else {
            "ww=info"
        };
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default_filter));
        if stderr {
            let _ = tracing_subscriber::fmt()
                .with_writer(std::io::stderr)
                .with_env_filter(filter)
                .try_init();
        } else {
            let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
        }
    }
}

/// Initialize tracing using `RUST_LOG` (default: `ww=info`).
///
/// Attempts to initialize a global `tracing_subscriber` (no-op if already set).
pub fn init_tracing() {
    init_tracing_to_stderr(false);
}
