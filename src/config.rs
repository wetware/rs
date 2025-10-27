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
                "Invalid log level: {}. Must be one of: trace, debug, info, warn, error",
                s
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
pub fn get_log_level() -> LogLevel {
    std::env::var("RUST_LOG")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(LogLevel::Info)
}

/// Initialize tracing with the specified log level
pub fn init_tracing(log_level: LogLevel, _explicit_level: Option<LogLevel>) {
    let level_str = match log_level {
        LogLevel::Trace => "trace",
        LogLevel::Debug => "debug",
        LogLevel::Info => "info",
        LogLevel::Warn => "warn",
        LogLevel::Error => "error",
    };

    // Set the RUST_LOG environment variable if not already set
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", level_str);
    }

    // Initialize tracing
    let _ = tracing_subscriber::fmt::try_init();
}

/// Get the default IPFS URL from environment or default
pub fn get_ipfs_url() -> String {
    std::env::var("WW_IPFS").unwrap_or_else(|_| "http://localhost:5001".to_string())
}
