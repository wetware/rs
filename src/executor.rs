use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::Path;

use std::process::{Command, Stdio};
use tempfile::TempDir;
use tracing::{debug, info};

use crate::boot::FDManager;

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

        Ok(Self { temp_dir, ipfs_url })
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
                response
                    .text()
                    .await
                    .unwrap_or_else(|_| "Unknown error".to_string())
            ));
        }

        let content = response.bytes().await?;
        info!(size = content.len(), "Downloaded file from IPFS");

        Ok(content.to_vec())
    }
}

/// Execute a binary in a subprocess with the given arguments and environment
///
/// This function launches a subprocess with proper file descriptor inheritance,
/// including the Unix domain socket pair for host-guest communication (FD3) and
/// user file descriptors (FD4+) passed via --with-fd flags.
///
/// # Arguments
///
/// * `binary` - Path to the executable to run
/// * `args` - Command line arguments to pass to the executable
/// * `env_vars` - Environment variables to set for the subprocess
/// * `fd_manager` - File descriptor manager for FD inheritance
/// * `executor_env` - Execution environment with IPFS and temp directory
///
/// # Returns
///
/// Returns the exit code of the subprocess.
///
/// # Errors
///
/// Returns an error if subprocess execution fails or if FD preparation fails.
///
/// File descriptors will be automatically cleaned up when fd_manager is dropped
/// due to the Drop trait implementation.
pub async fn execute_subprocess(
    binary: String,
    args: Vec<String>,
    env_vars: HashMap<String, String>,
    mut fd_manager: Option<FDManager>,
    executor_env: &ExecutorEnv,
) -> Result<i32> {
    info!(binary = %binary, args = ?args, "Executing subprocess");

    // Resolve the executable path
    let resolved_binary = executor_env.resolve_exec_path(&binary).await?;
    debug!(resolved_binary = %resolved_binary, "Resolved executable path");

    // Prepare file descriptors for subprocess
    let extra_fds = if let Some(ref mut fd_manager) = fd_manager {
        fd_manager.prepare_fds()?
    } else {
        Vec::new()
    };

    // Build the command
    let mut cmd = Command::new(&resolved_binary);
    cmd.args(&args);

    // Set up environment variables
    for (key, value) in &env_vars {
        cmd.env(key, value);
    }

    // Set up standard streams (passthrough to parent)
    cmd.stdin(Stdio::inherit());
    cmd.stdout(Stdio::inherit());
    cmd.stderr(Stdio::inherit());

    // Set up file descriptor inheritance using pre_exec hook
    let extra_fds_clone = extra_fds.clone();
    unsafe {
        cmd.pre_exec(move || {
            // Set up process group for proper signal handling
            libc::setpgid(0, 0);

            // Duplicate file descriptors to the child process
            for (i, fd) in extra_fds_clone.iter().enumerate() {
                let target_fd = 3 + i as i32; // Start from FD 3

                // Duplicate the file descriptor to the target FD
                if libc::dup2(*fd, target_fd) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }

            Ok(())
        });
    }

    info!(
        extra_files_count = env_vars.len(),
        "Launching subprocess with {} extra file descriptors",
        env_vars.len()
    );

    // Execute the command
    let mut child = cmd
        .spawn()
        .map_err(|e| anyhow!("Failed to spawn subprocess '{}': {}", resolved_binary, e))?;

    // Wait for the subprocess to complete
    let status = child
        .wait()
        .map_err(|e| anyhow!("Failed to wait for subprocess: {}", e))?;

    // Get exit code
    let exit_code = status.code().unwrap_or(1);
    info!(exit_code = exit_code, "Subprocess completed");

    Ok(exit_code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::unix::io::AsRawFd;

    #[test]
    fn test_fd_manager_parsing() {
        // Create temporary files to get valid file descriptors
        // We need to keep the File objects alive to prevent them from being closed
        let _temp_file1 = File::create("/tmp/test_db").unwrap();
        let _temp_file2 = File::create("/tmp/test_cache").unwrap();

        let db_fd = _temp_file1.as_raw_fd();
        let cache_fd = _temp_file2.as_raw_fd();

        let fd_flags = vec![format!("db={}", db_fd), format!("cache={}", cache_fd)];
        let fd_manager = FDManager::new(fd_flags).unwrap();

        let env_vars = fd_manager.generate_env_vars();
        assert_eq!(env_vars.get("WW_FD_DB"), Some(&"4".to_string()));
        assert_eq!(env_vars.get("WW_FD_CACHE"), Some(&"5".to_string()));

        // Clean up temporary files
        std::fs::remove_file("/tmp/test_db").ok();
        std::fs::remove_file("/tmp/test_cache").ok();
    }

    #[test]
    fn test_fd_manager_duplicate_names() {
        // Create temporary files to get valid file descriptors
        // We need to keep the File objects alive to prevent them from being closed
        let _temp_file1 = File::create("/tmp/test_db1").unwrap();
        let _temp_file2 = File::create("/tmp/test_db2").unwrap();

        let db_fd1 = _temp_file1.as_raw_fd();
        let db_fd2 = _temp_file2.as_raw_fd();

        let fd_flags = vec![format!("db={}", db_fd1), format!("db={}", db_fd2)];
        let result = FDManager::new(fd_flags);
        assert!(result.is_err());

        // Clean up temporary files
        std::fs::remove_file("/tmp/test_db1").ok();
        std::fs::remove_file("/tmp/test_db2").ok();
    }

    #[test]
    fn test_fd_manager_invalid_format() {
        let fd_flags = vec!["invalid".to_string()];
        let result = FDManager::new(fd_flags);
        assert!(result.is_err());
    }
}
