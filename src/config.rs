use anyhow::Result;
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
    /// List of multiaddrs to listen on
    pub listen_addrs: Vec<String>,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            listen_addrs: vec![
                "/ip4/0.0.0.0/tcp/2020".to_string(),
                "/ip6/::/tcp/2020".to_string(),
            ],
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
    /// let config = HostConfig::builder().build();
    ///
    /// // Using presets directly
    /// let config = HostConfig::minimal();
    /// let config = HostConfig::development();
    /// let config = HostConfig::production();
    ///
    /// ```
    pub fn builder() -> HostConfigBuilder {
        HostConfigBuilder::default()
    }

    /// Create a minimal configuration with only essential features enabled
    pub fn minimal() -> Self {
        Self::default()
    }

    /// Create a development configuration with most features enabled
    pub fn development() -> Self {
        Self::default()
    }

    /// Create a production configuration with all features enabled
    pub fn production() -> Self {
        Self::default()
    }

    /// Validate the configuration and return any errors
    pub fn validate(&self) -> Result<(), String> {
        // Check for invalid addresses
        for (i, addr) in self.listen_addrs.iter().enumerate() {
            if addr.is_empty() {
                return Err(format!("Listen address {} is empty", i));
            } else if !addr.starts_with("/ip4/") && !addr.starts_with("/ip6/") {
                return Err(format!("Listen address '{}' is invalid (should start with /ip4/ or /ip6/)", addr));
            }
        }

        // Check for duplicate addresses
        let mut seen = std::collections::HashSet::new();
        for addr in &self.listen_addrs {
            if !seen.insert(addr) {
                return Err(format!("Duplicate listen address: {}", addr));
            }
        }

        Ok(())
    }

    /// Get configuration warnings (non-fatal issues)
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings = Vec::new();

        // Warn about empty listen addresses (not an error, but worth noting)
        if self.listen_addrs.is_empty() {
            warnings.push("No listening addresses configured - node will not accept incoming connections".to_string());
        }

        warnings
    }

    /// Get a human-readable summary of the configuration
    pub fn summary(&self) -> String {
        if self.listen_addrs.is_empty() {
            "Client configuration: no listen addresses configured".to_string()
        } else {
            format!(
                "Server configuration: listening on {} address(es): {}",
                self.listen_addrs.len(),
                self.listen_addrs.join(", ")
            )
        }
    }
}

/// Builder for HostConfig
#[derive(Debug, Default, Clone)]
pub struct HostConfigBuilder {}

impl HostConfigBuilder {
    /// Build the HostConfig
    pub fn build(self) -> HostConfig {
        HostConfig::default()
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
    pub fn from_args_and_env(
        ipfs: Option<String>,
        loglvl: Option<LogLevel>,
        preset: Option<String>,
        listen_addrs: Option<Vec<String>>,
    ) -> Self {
        let ipfs_url = ipfs.unwrap_or_else(get_ipfs_url);
        let log_level = loglvl.unwrap_or_else(get_log_level);

        let mut host_config = if let Some(preset) = &preset {
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
            let builder = HostConfig::builder();
            builder.build()
        };

        // Override listen addresses if provided via CLI
        if let Some(addrs) = listen_addrs {
            host_config.listen_addrs = addrs;
        }

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

        // Validate configuration and log any errors
        if let Err(error) = self.host_config.validate() {
            warn!("Configuration error: {}", error);
        }

        // Log warnings (non-fatal issues)
        let warnings = self.host_config.warnings();
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
