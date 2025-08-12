use anyhow::Result;
use clap::Parser;
use tracing::{debug, warn};

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

/// Configuration for libp2p host features
#[derive(Debug, Clone, PartialEq)]
pub struct HostConfig {
    /// Enable periodic DHT bootstrap (default: false)
    pub periodic_bootstrap: bool,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            periodic_bootstrap: false,
        }
    }
}

impl HostConfig {
    /// Create a new builder for HostConfig
    ///
    /// # Examples
    ///
    /// ```rust
    /// use config::HostConfig;
    ///
    /// // Basic usage
    /// let config = HostConfig::builder()
    ///     .periodic_bootstrap(true)
    ///     .build();
    ///
    /// // Using convenience methods
    /// let config = HostConfig::builder()
    ///     .enable_periodic_bootstrap()
    ///     .build();
    ///
    /// // Starting from a preset and customizing
    /// let config = HostConfig::builder()
    ///     .from_preset("development")
    ///     .enable_periodic_bootstrap()
    ///     .build();
    ///
    /// // Using presets directly
    /// let config = HostConfig::minimal();
    /// let config = HostConfig::development();
    /// let config = HostConfig::production();
    ///
    /// // From environment variables
    /// let config = HostConfig::from_env();
    /// ```
    pub fn builder() -> HostConfigBuilder {
        HostConfigBuilder::default()
    }

    /// Create a minimal configuration with only essential features enabled
    pub fn minimal() -> Self {
        Self {
            periodic_bootstrap: false,
        }
    }

    /// Create a development configuration with most features enabled
    pub fn development() -> Self {
        Self {
            periodic_bootstrap: false,
        }
    }

    /// Create a production configuration with all features enabled
    pub fn production() -> Self {
        Self {
            periodic_bootstrap: true,
        }
    }

    /// Create configuration from environment variables
    pub fn from_env() -> Self {
        let mut builder = Self::builder();

        if let Ok(val) = std::env::var("WW_PERIODIC_BOOTSTRAP") {
            if let Ok(enabled) = val.parse::<bool>() {
                builder = builder.periodic_bootstrap(enabled);
            }
        }

        builder.build()
    }

    /// Validate the configuration and return any warnings
    pub fn validate(&self) -> Vec<String> {
        let warnings = Vec::new();

        // No validation warnings for basic configuration
        warnings
    }

    /// Get a human-readable summary of the configuration
    pub fn summary(&self) -> String {
        if self.periodic_bootstrap {
            "Features: Periodic Bootstrap".to_string()
        } else {
            "Minimal (TCP only)".to_string()
        }
    }
}

/// Builder for HostConfig
#[derive(Debug, Default, Clone)]
pub struct HostConfigBuilder {
    periodic_bootstrap: Option<bool>,
}

impl HostConfigBuilder {
    /// Enable or disable periodic DHT bootstrap
    pub fn periodic_bootstrap(mut self, enabled: bool) -> Self {
        self.periodic_bootstrap = Some(enabled);
        self
    }

    /// Build the HostConfig
    pub fn build(self) -> HostConfig {
        HostConfig {
            periodic_bootstrap: self.periodic_bootstrap.unwrap_or(false),
        }
    }
}

/// Application configuration that combines all settings
#[derive(Debug)]
pub struct AppConfig {
    pub ipfs_url: String,
    pub log_level: LogLevel,
    pub host_config: HostConfig,
}

impl AppConfig {
    /// Create configuration from command line arguments and environment
    pub fn from_args_and_env(args: &Args) -> Self {
        let ipfs_url = args.ipfs.clone().unwrap_or_else(get_ipfs_url);
        let log_level = args.loglvl.unwrap_or_else(get_log_level);

        let host_config = if args.env_config {
            debug!("Using host configuration from environment variables");
            HostConfig::from_env()
        } else if let Some(preset) = &args.preset {
            match preset.as_str() {
                "minimal" => HostConfig::minimal(),
                "development" => HostConfig::development(),
                "production" => HostConfig::production(),
                _ => {
                    warn!("Unknown preset '{}', using default configuration", preset);
                    HostConfig::default()
                }
            }
        } else {
            let mut builder = HostConfig::builder();

            if args.periodic_bootstrap {
                builder = builder.periodic_bootstrap(true);
            }

            builder.build()
        };

        Self {
            ipfs_url,
            log_level,
            host_config,
        }
    }

    /// Log the configuration summary
    pub fn log_summary(&self) {
        debug!(ipfs_url = %self.ipfs_url, log_level = %self.log_level, "Application configuration");

        if let Some(preset) = &self.get_preset_name() {
            debug!(preset = %preset, "Using preset host configuration");
        }

        debug!(summary = %self.host_config.summary(), "Host configuration");

        // Validate configuration and log warnings
        let warnings = self.host_config.validate();
        for warning in &warnings {
            warn!("Configuration warning: {}", warning);
        }
    }

    /// Get the preset name if one was used
    fn get_preset_name(&self) -> Option<String> {
        // This is a simple heuristic - in practice you might want to store the preset name
        if self.host_config == HostConfig::minimal() {
            Some("minimal".to_string())
        } else if self.host_config == HostConfig::development() {
            Some("development".to_string())
        } else if self.host_config == HostConfig::production() {
            Some("production".to_string())
        } else {
            None
        }
    }
}

/// Command line arguments
#[derive(Parser)]
#[command(name = "ww")]
#[command(
    about = "P2P sandbox for Web3 applications that execute untrusted code on public networks."
)]
pub struct Args {
    /// IPFS node HTTP API endpoint (e.g., http://127.0.0.1:5001)
    /// If not provided, uses WW_IPFS environment variable or defaults to http://localhost:5001
    #[arg(long)]
    pub ipfs: Option<String>,

    /// Log level (trace, debug, info, warn, error)
    /// If not provided, uses WW_LOGLVL environment variable or defaults to info
    #[arg(long, value_name = "LEVEL")]
    pub loglvl: Option<LogLevel>,

    /// Enable periodic DHT bootstrap (default: false)
    #[arg(long)]
    pub periodic_bootstrap: bool,

    /// Use preset configuration (minimal, development, production)
    #[arg(long, value_name = "PRESET")]
    pub preset: Option<String>,

    /// Use configuration from environment variables
    #[arg(long)]
    pub env_config: bool,
}

/// Get the log level from environment variable or use default
pub fn get_log_level() -> LogLevel {
    std::env::var("WW_LOGLVL")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| LogLevel::Info)
}

/// Get the IPFS node URL from environment variable or use default
pub fn get_ipfs_url() -> String {
    std::env::var("WW_IPFS").unwrap_or_else(|_| "http://localhost:5001".to_string())
}

/// Initialize tracing with the given log level
pub fn init_tracing(log_level: LogLevel, cli_level: Option<LogLevel>) {
    let env_filter = if let Some(cli_level) = cli_level {
        // Command line flag takes precedence
        format!("ww={},libp2p={}", cli_level, cli_level).into()
    } else {
        // Use environment variable or default
        tracing_subscriber::EnvFilter::try_from_env("WW_LOGLVL")
            .unwrap_or_else(|_| format!("ww={},libp2p={}", log_level, log_level).into())
    };

    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(true)
        .with_thread_ids(true)
        .with_thread_names(true)
        .init();
}
