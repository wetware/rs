use std::fmt::Debug;

/// Simplified host configuration for basic WASM execution
#[derive(Debug, Clone)]
pub struct HostConfig {
    /// Whether to enable debug logging
    pub debug: bool,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self { debug: false }
    }
}

/// Simple host manager for basic WASM execution
/// This replaces the complex SwarmManager for now
#[derive(Debug)]
pub struct HostManager {
    config: HostConfig,
}

impl HostManager {
    pub fn new(config: HostConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &HostConfig {
        &self.config
    }
}

impl Default for HostManager {
    fn default() -> Self {
        Self::new(HostConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_host_manager_creation() {
        let config = HostConfig { debug: true };
        let manager = HostManager::new(config);
        assert!(manager.config().debug);
    }

    #[test]
    fn test_host_manager_default() {
        let manager = HostManager::default();
        assert!(!manager.config().debug);
    }
}