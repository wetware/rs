/// Simple configuration for cell modules
///
/// This provides basic configuration for cell module instantiation.
/// The actual WASI setup is handled in the runtime.
pub struct ModuleConfig {
    /// Environment variables to set for the cell module
    pub env: Vec<String>,
    /// Command line arguments for the cell module
    pub args: Vec<String>,
}

impl ModuleConfig {
    /// Create a new module configuration
    pub fn new() -> Self {
        Self {
            env: Vec::new(),
            args: Vec::new(),
        }
    }

    /// Add an environment variable
    pub fn with_env(mut self, key: &str, value: &str) -> Self {
        self.env.push(format!("{}={}", key, value));
        self
    }

    /// Add multiple environment variables
    pub fn with_envs(mut self, envs: Vec<String>) -> Self {
        self.env.extend(envs);
        self
    }

    /// Add command line arguments
    pub fn with_args(mut self, args: Vec<String>) -> Self {
        self.args = args;
        self
    }
}

impl Default for ModuleConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_module_config_creation() {
        let config = ModuleConfig::new();
        assert!(config.env.is_empty());
        assert!(config.args.is_empty());
    }

    #[test]
    fn test_module_config_builder() {
        let config = ModuleConfig::new()
            .with_env("DEBUG", "1")
            .with_envs(vec!["TEST_VAR=test_value".to_string()])
            .with_args(vec!["arg1".to_string(), "arg2".to_string()]);

        assert_eq!(config.env.len(), 2);
        assert_eq!(config.args.len(), 2);
    }
}
