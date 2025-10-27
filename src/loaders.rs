//! Loader implementations for resolving bytecode from various sources.
//!
//! This module provides loaders for IPFS (UnixFS), local filesystem, and a chain loader
//! that tries multiple loaders in sequence.

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::cell::Loader;

/// UnixFS loader for IPFS-based bytecode resolution
///
/// Handles IPFS paths: `/ipfs/...`, `/ipns/...`, `/ipld/...`
pub struct UnixFSLoader {
    ipfs_url: String,
}

impl UnixFSLoader {
    /// Create a new UnixFS loader with the given IPFS HTTP API endpoint
    pub fn new(ipfs_url: String) -> Self {
        Self { ipfs_url }
    }
}

#[async_trait]
impl Loader for UnixFSLoader {
    async fn load(&self, path: &str) -> Result<Vec<u8>> {
        // Only handle IPFS-family paths
        if !is_ipfs_path(path) {
            return Err(anyhow::anyhow!("Not an IPFS path: {}", path));
        }

        // Download from IPFS via HTTP API
        let client = reqwest::Client::new();
        let url = format!("{}/api/v0/cat?arg={}", self.ipfs_url, path);
        let response = client
            .post(&url)
            .send()
            .await
            .with_context(|| format!("Failed to connect to IPFS node at {}", self.ipfs_url))?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to download from IPFS: {}",
                response.status()
            ));
        }

        Ok(response.bytes().await?.to_vec())
    }
}

/// Local filesystem loader for resolving bytecode from local files
///
/// Handles:
/// - Absolute paths
/// - Relative paths (`.` prefix)
/// - `$PATH` lookup (via `which` command)
/// - Current directory
pub struct LocalFSLoader;

#[async_trait]
impl Loader for LocalFSLoader {
    async fn load(&self, name: &str) -> Result<Vec<u8>> {
        use std::fs;
        use std::path::Path;
        use std::process::Command;

        // Check if it's an absolute path
        if Path::new(name).is_absolute() {
            return fs::read(name).with_context(|| format!("Failed to read file: {}", name));
        }

        // Check if it's a relative path (starts with . or /)
        if name.starts_with('.') || name.starts_with('/') {
            return fs::read(name).with_context(|| format!("Failed to read file: {}", name));
        }

        // Check if it's in $PATH
        if let Ok(resolved_path) = Command::new("which").arg(name).output() {
            if resolved_path.status.success() {
                let path_str = String::from_utf8(resolved_path.stdout)?;
                let path_str = path_str.trim();
                if !path_str.is_empty() {
                    return fs::read(path_str)
                        .with_context(|| format!("Failed to read resolved binary: {}", path_str));
                }
            }
        }

        // Try as a relative path in current directory
        if Path::new(name).exists() {
            return fs::read(name).with_context(|| format!("Failed to read file: {}", name));
        }

        Err(anyhow::anyhow!("Binary not found: {}", name))
    }
}

/// Chain loader that tries multiple loaders in sequence
///
/// Attempts each loader in order and returns the first successful result,
/// or accumulates all errors if all loaders fail.
pub struct ChainLoader {
    loaders: Vec<Box<dyn Loader>>,
}

impl ChainLoader {
    /// Create a new chain loader with the given loaders
    pub fn new(loaders: Vec<Box<dyn Loader>>) -> Self {
        Self { loaders }
    }
}

#[async_trait]
impl Loader for ChainLoader {
    async fn load(&self, path: &str) -> Result<Vec<u8>> {
        let mut errors = Vec::new();
        for (i, loader) in self.loaders.iter().enumerate() {
            match loader.load(path).await {
                Ok(data) => return Ok(data),
                Err(e) => {
                    errors.push(format!("Loader {}: {}", i, e));
                }
            }
        }
        Err(anyhow::anyhow!(
            "All loaders failed for '{}': {}",
            path,
            errors.join("; ")
        ))
    }
}

/// Check if a path is a valid IPFS-family path (IPFS, IPNS, or IPLD)
///
/// This centralizes IPFS path validation similar to Go's `path.NewPath(str)`.
/// Returns true if the path starts with a valid IPFS namespace prefix.
fn is_ipfs_path(path: &str) -> bool {
    path.starts_with("/ipfs/") || path.starts_with("/ipns/") || path.starts_with("/ipld/")
}

