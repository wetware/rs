/*! Top-level configuration module for Wetware

This module centralizes configuration primitives that were previously
scoped under `cli::config`. Moving these items to the crate root allows
non-CLI subsystems (like `cell`) to depend on configuration without
creating circular dependencies.

Exports:
- `LogLevel`: common log-level type used across the crate
- `get_log_level()`: resolves the default log level (from env or default)
- `init_tracing()`: initializes global tracing subscriber

*/

/// Log level options
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl std::str::FromStr for LogLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "trace" => Ok(LogLevel::Trace),
            "debug" => Ok(LogLevel::Debug),
            "info" => Ok(LogLevel::Info),
            "warn" => Ok(LogLevel::Warn),
            "error" => Ok(LogLevel::Error),
            _ => Err(format!(
                "Invalid log level: {s}. Must be one of: trace, debug, info, warn, error"
            )),
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LogLevel::Trace => write!(f, "trace"),
            LogLevel::Debug => write!(f, "debug"),
            LogLevel::Info => write!(f, "info"),
            LogLevel::Warn => write!(f, "warn"),
            LogLevel::Error => write!(f, "error"),
        }
    }
}

/// Get the default log level from environment or default to Info
///
/// Current resolution uses the `RUST_LOG` environment variable if present,
/// otherwise defaults to `info`.
pub fn get_log_level() -> LogLevel {
    std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(LogLevel::Info)
}

/// Initialize tracing with the specified log level
///
/// - Sets `RUST_LOG` if not already set, using the provided `log_level`.
/// - Attempts to initialize a global `tracing_subscriber` (no-op if already set).
pub fn init_tracing(log_level: LogLevel, _explicit_level: Option<LogLevel>) {
    let level_str = match log_level {
        LogLevel::Trace => "trace",
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    };

    let extra_filter = "capnp=trace,capnp_rpc=trace,wasmtime=trace,wasmtime_wasi=trace,wasmtime::runtime::type_registry=off,wasmtime_internal_cranelift=off,cranelift_codegen=off,cranelift_codegen::egraph=off,cranelift_codegen::context=off,cranelift_codegen::ir=off,cranelift_codegen::machinst=off,cranelift_frontend=off,cranelift_frontend::frontend=off,wasmtime_environ=off";
    let rust_log = match std::env::var("RUST_LOG") {
        Ok(existing) if !existing.is_empty() => format!("{existing},{extra_filter}"),
        _ => format!("{level},{extra_filter}", level = level_str),
    };
    std::env::set_var("RUST_LOG", rust_log);

    // Initialize tracing (ignore error if already initialized)
    #[cfg(not(target_arch = "wasm32"))]
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
}
