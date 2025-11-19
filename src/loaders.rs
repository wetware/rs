//! Loader implementations for resolving bytecode from various sources.
//!
//! This module provides loaders for IPFS (UnixFS), local filesystem, and a chain loader
//! that tries multiple loaders in sequence.

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::cell::Loader;
use crate::ipfs::{is_ipfs_path, UnixFS};

/// IPFS filesystem loader for IPFS-based bytecode resolution
///
/// Handles IPFS paths: `/ipfs/...`, `/ipns/...`, `/ipld/...`
pub struct IpfsUnixfsLoader {
    unixfs: UnixFS,
}

impl IpfsUnixfsLoader {
    /// Create a new IPFS filesystem loader with the given IPFS client
    pub fn new(ipfs: crate::ipfs::HttpClient) -> Self {
        Self {
            unixfs: ipfs.unixfs(),
        }
    }
}

#[async_trait]
impl Loader for IpfsUnixfsLoader {
    async fn load(&self, path: &str) -> Result<Vec<u8>> {
        // Only handle IPFS-family paths
        if !is_ipfs_path(path) {
            return Err(anyhow::anyhow!("Not an IPFS path: {path}"));
        }

        // Download from IPFS using the Unixfs API
        self.unixfs.get(path).await
    }
}

/// Local filesystem loader for resolving bytecode from local files
///
/// Mounts a host filesystem path under a guest path prefix, with all access
/// jailed to the host path to prevent directory traversal.
pub struct LocalFSLoader {
    /// Guest path prefix (e.g., "/app")
    prefix: String,
    /// Host filesystem path to jail to (e.g., "/home/user/app")
    jail_path: String,
}

impl LocalFSLoader {
    /// Create a new local filesystem loader
    ///
    /// # Arguments
    ///
    /// * `prefix` - Guest path prefix that this loader handles (e.g., "/app")
    /// * `jail_path` - Host filesystem path to jail all access to
    pub fn new(prefix: String, jail_path: String) -> Self {
        Self { prefix, jail_path }
    }
}

#[async_trait]
impl Loader for LocalFSLoader {
    async fn load(&self, name: &str) -> Result<Vec<u8>> {
        use std::fs;
        use std::path::Path;

        // Check if the path starts with our prefix
        let path = if name.starts_with(&self.prefix) {
            // Strip the prefix and get the relative path
            name.strip_prefix(&self.prefix)
                .unwrap_or(name)
                .trim_start_matches('/')
        } else {
            // Path doesn't match this loader's prefix
            let expected_prefix = &self.prefix;
            return Err(anyhow::anyhow!(
                "Path does not match prefix: {name} (expected prefix: {expected_prefix})"
            ));
        };

        let jail_path = Path::new(&self.jail_path);

        // Normalize the requested path by removing leading slashes and dots
        let normalized_path = path.trim_start_matches('/').trim_start_matches("./");

        // Build the jailed path
        let mut jailed_path = jail_path.to_path_buf();
        jailed_path.push(normalized_path);

        // Normalize the path (resolves . and .. components)
        let jailed_path = jailed_path.canonicalize().unwrap_or(jailed_path);

        // Verify the path is still within the jail
        let canonical_jail = jail_path
            .canonicalize()
            .unwrap_or_else(|_| jail_path.to_path_buf());

        if !jailed_path.starts_with(&canonical_jail) {
            let jail = &self.jail_path;
            return Err(anyhow::anyhow!(
                "Path would escape jail directory: {name} (jail: {jail})"
            ));
        }

        // Try reading from the jailed path
        if jailed_path.exists() && jailed_path.is_file() {
            let display_path = jailed_path.display().to_string();
            return fs::read(&jailed_path)
                .with_context(|| format!("Failed to read file: {display_path}"));
        }

        let prefix = &self.prefix;
        let jail = &self.jail_path;
        Err(anyhow::anyhow!(
            "Binary not found: {name} (mounted at {prefix} -> {jail})"
        ))
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
                    errors.push(format!("Loader {i}: {e}"));
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
