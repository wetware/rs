//! Loader implementations for resolving bytecode from various sources.
//!
//! This module provides loaders for IPFS (UnixFS), host filesystem paths, and a chain loader
//! that tries multiple loaders in sequence.

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::ipfs::{is_ipfs_path, UnixFS};
use crate::Loader;
use std::path::{Path, PathBuf};

/// IPFS filesystem loader for IPFS-based bytecode resolution
///
/// Handles IPFS paths: `/ipfs/...`, `/ipns/...`, `/ipld/...`
pub struct IpfsUnixfs {
    unixfs: UnixFS,
}

impl IpfsUnixfs {
    /// Create a new IPFS filesystem loader with the given IPFS client
    pub fn new(ipfs: crate::ipfs::HttpClient) -> Self {
        Self {
            unixfs: ipfs.unixfs(),
        }
    }
}

#[async_trait]
impl Loader for IpfsUnixfs {
    async fn load(&self, path: &str) -> Result<Vec<u8>> {
        // Only handle IPFS-family paths
        if !is_ipfs_path(path) {
            return Err(anyhow::anyhow!("Not an IPFS path: {path}"));
        }

        // Download from IPFS using the Unixfs API
        self.unixfs.get(path).await
    }
}

/// Loader that reads directly from whatever host path is provided.
///
/// This is a fallback used when no IPFS or mounted prefix handles the request.
pub struct HostPath {
    prefix: PathBuf,
}

impl HostPath {
    /// Create a new host path loader with the given path
    pub fn new(prefix: PathBuf) -> Self {
        Self { prefix }
    }
}

#[async_trait]
impl Loader for HostPath {
    async fn load(&self, name: &str) -> Result<Vec<u8>> {
        use std::fs;

        if self.prefix.as_os_str().is_empty() {
            return Err(anyhow::anyhow!("HostPath loader has no prefix set."));
        }

        let path = Path::new(name);
        if !path.starts_with(&self.prefix) {
            return Err(anyhow::anyhow!(
                "File '{}' does not match prefix '{}'",
                name,
                self.prefix.display()
            ));
        }

        if !path.exists() || !path.is_file() {
            return Err(anyhow::anyhow!("File not found: {name}"));
        }

        fs::read(path).with_context(|| format!("Failed to read file: {}", path.display()))
    }
}

/// Chain loader that tries multiple loaders in sequence
///
/// Attempts each loader in order and returns the first successful result,
/// or accumulates all errors if all loaders fail.
pub struct Chain {
    loaders: Vec<Box<dyn Loader>>,
}

impl Chain {
    /// Create a new chain loader with the given loaders
    pub fn new(loaders: Vec<Box<dyn Loader>>) -> Self {
        Self { loaders }
    }
}

#[async_trait]
impl Loader for Chain {
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
