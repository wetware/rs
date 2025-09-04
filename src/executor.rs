use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::path::Path;
use std::fs;
use std::os::unix::fs::PermissionsExt;

use reqwest;
use subprocess::{Exec, Redirection};
use tempfile::TempDir;
use tracing::{debug, info, warn};

/// Environment for subprocess execution
pub struct ExecutorEnv {
    /// Temporary directory for cell execution
    pub temp_dir: TempDir,
    /// IPFS client for resolving IPFS paths
    pub ipfs_url: String,
}

impl ExecutorEnv {
    /// Create a new executor environment
    pub fn new(ipfs_url: String) -> Result<Self> {
        let temp_dir = TempDir::new()?;
        debug!(temp_dir = %temp_dir.path().display(), "Created temporary directory for execution");
        
        Ok(Self {
            temp_dir,
            ipfs_url,
        })
    }

    /// Resolve an executable path, handling both IPFS paths and local filesystem paths
    pub async fn resolve_exec_path(&self, name: &str) -> Result<String> {
        // Check if it's an IPFS path (starts with /ipfs/)
        if name.starts_with("/ipfs/") {
            return self.resolve_ipfs_path(name).await;
        }

        // Handle local paths - resolve relative paths to absolute
        let path = Path::new(name);
        if path.is_relative() {
            let current_dir = std::env::current_dir()?;
            let abs_path = current_dir.join(path);
            Ok(abs_path.to_string_lossy().to_string())
        } else {
            Ok(name.to_string())
        }
    }

    /// Resolve an IPFS path by downloading it to the temp directory
    async fn resolve_ipfs_path(&self, ipfs_path: &str) -> Result<String> {
        debug!(ipfs_path = %ipfs_path, "Resolving IPFS path");
        
        // Download the file from IPFS
        let file_content = self.download_from_ipfs(ipfs_path).await?;
        
        // Create a temporary file in the temp directory
        let temp_file_path = self.temp_dir.path().join("downloaded_binary");
        
        // Write the content to the temporary file
        fs::write(&temp_file_path, file_content)?;
        
        // Make the file executable
        let mut perms = fs::metadata(&temp_file_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&temp_file_path, perms)?;
        
        let resolved_path = temp_file_path.to_string_lossy().to_string();
        debug!(resolved_path = %resolved_path, "IPFS path resolved to local file");
        
        Ok(resolved_path)
    }
    
    /// Download a file from IPFS using the HTTP API
    async fn download_from_ipfs(&self, ipfs_path: &str) -> Result<Vec<u8>> {
        let client = reqwest::Client::new();
        let url = format!("{}/api/v0/cat?arg={}", self.ipfs_url, ipfs_path);
        
        debug!(url = %url, "Downloading file from IPFS");
        
        let response = client.post(&url).send().await?;
        if !response.status().is_success() {
            return Err(anyhow!(
                "Failed to download from IPFS: {} - {}",
                response.status(),
                response.text().await.unwrap_or_else(|_| "Unknown error".to_string())
            ));
        }
        
        let content = response.bytes().await?;
        info!(size = content.len(), "Downloaded file from IPFS");
        
        Ok(content.to_vec())
    }
}

/// File descriptor manager for passing FDs to child processes
pub struct FDManager {
    mappings: Vec<FDMapping>,
}

struct FDMapping {
    name: String,
    source_fd: i32,
    target_fd: i32,
}

impl FDManager {
    /// Create a new FD manager from --with-fd flag values
    pub fn new(fd_flags: Vec<String>) -> Result<Self> {
        let mut mappings = Vec::new();
        let mut used_names = std::collections::HashSet::new();

        for (i, flag) in fd_flags.iter().enumerate() {
            let (name, source_fd) = Self::parse_fd_flag(flag)?;
            
            if used_names.contains(&name) {
                return Err(anyhow!("Duplicate name '{}' in --with-fd flags", name));
            }

            // Target FD starts at 3 and increments sequentially
            let target_fd = 3 + i as i32;

            mappings.push(FDMapping {
                name: name.clone(),
                source_fd,
                target_fd,
            });

            used_names.insert(name);
        }

        Ok(Self { mappings })
    }

    /// Parse a --with-fd flag value in "name=fdnum" format
    fn parse_fd_flag(value: &str) -> Result<(String, i32)> {
        let parts: Vec<&str> = value.splitn(2, '=').collect();
        if parts.len() != 2 {
            return Err(anyhow!("Invalid format: expected 'name=fdnum', got '{}'", value));
        }

        let name = parts[0].to_string();
        if name.is_empty() {
            return Err(anyhow!("Name cannot be empty"));
        }

        let fdnum: i32 = parts[1].parse()
            .map_err(|_| anyhow!("Invalid fd number '{}'", parts[1]))?;

        if fdnum < 0 {
            return Err(anyhow!("FD number must be non-negative, got {}", fdnum));
        }

        Ok((name, fdnum))
    }

    /// Generate environment variables for the child process
    pub fn generate_env_vars(&self) -> HashMap<String, String> {
        let mut env_vars = HashMap::new();
        
        for mapping in &self.mappings {
            let env_var = format!("WW_FD_{}", mapping.name.to_uppercase());
            env_vars.insert(env_var, mapping.target_fd.to_string());
        }

        env_vars
    }
}

/// Execute a binary in a subprocess with the given arguments and environment
pub async fn execute_subprocess(
    binary: String,
    args: Vec<String>,
    env_vars: HashMap<String, String>,
    fd_manager: Option<FDManager>,
    executor_env: &ExecutorEnv,
) -> Result<i32> {
    info!(binary = %binary, args = ?args, "Executing subprocess");

    // Resolve the executable path
    let resolved_binary = executor_env.resolve_exec_path(&binary).await?;
    debug!(resolved_binary = %resolved_binary, "Resolved executable path");

    // Build the command
    let mut cmd = Exec::cmd(&resolved_binary);
    
    // Add arguments
    for arg in args {
        cmd = cmd.arg(arg);
    }

    // Set up environment variables
    for (key, value) in env_vars {
        cmd = cmd.env(key, value);
    }

    // Set up standard streams
    cmd = cmd
        .stdout(Redirection::Pipe)
        .stderr(Redirection::Pipe);

    // TODO: Handle file descriptor passing when FDManager is implemented
    if fd_manager.is_some() {
        warn!("File descriptor passing not yet implemented");
    }

    // Execute the command and capture output
    let result = cmd.capture()?;
    
    // Print the output
    print!("{}", result.stdout_str());
    eprint!("{}", result.stderr_str());

    // Get exit code
    let exit_code = if result.success() { 0 } else { 1 };
    info!(exit_code = exit_code, "Subprocess completed");

    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fd_manager_parsing() {
        let fd_flags = vec!["db=3".to_string(), "cache=4".to_string()];
        let fd_manager = FDManager::new(fd_flags).unwrap();
        
        let env_vars = fd_manager.generate_env_vars();
        assert_eq!(env_vars.get("WW_FD_DB"), Some(&"3".to_string()));
        assert_eq!(env_vars.get("WW_FD_CACHE"), Some(&"4".to_string()));
    }

    #[test]
    fn test_fd_manager_duplicate_names() {
        let fd_flags = vec!["db=3".to_string(), "db=4".to_string()];
        let result = FDManager::new(fd_flags);
        assert!(result.is_err());
    }

    #[test]
    fn test_fd_manager_invalid_format() {
        let fd_flags = vec!["invalid".to_string()];
        let result = FDManager::new(fd_flags);
        assert!(result.is_err());
    }
}
 