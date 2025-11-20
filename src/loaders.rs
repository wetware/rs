//! Loader implementations for resolving bytecode from various sources.
//!
//! This module provides loaders for IPFS (UnixFS), host filesystem paths, and a chain loader
//! that tries multiple loaders in sequence.

use anyhow::{Context, Result};
use async_trait::async_trait;

use crate::cell::Loader;
use crate::ipfs::{is_ipfs_path, UnixFS};
use std::path::Path;

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

/// Loader that reads directly from whatever host path is provided.
///
/// This is a fallback used when no IPFS or mounted prefix handles the request.
pub struct HostPathLoader;

#[async_trait]
impl Loader for HostPathLoader {
    async fn load(&self, name: &str) -> Result<Vec<u8>> {
        use std::fs;

        let path = Path::new(name);
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
