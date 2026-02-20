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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn test_host_path_loader_reads_file() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"wasm-bytes").unwrap();

        let loader = HostPathLoader;
        let data = loader.load(tmp.path().to_str().unwrap()).await.unwrap();
        assert_eq!(data, b"wasm-bytes");
    }

    #[tokio::test]
    async fn test_host_path_loader_missing_file() {
        let loader = HostPathLoader;
        let err = loader.load("/no/such/file.wasm").await;
        assert!(err.is_err());
        assert!(err.unwrap_err().to_string().contains("File not found"));
    }

    #[tokio::test]
    async fn test_host_path_loader_rejects_directory() {
        let dir = tempfile::tempdir().unwrap();
        let loader = HostPathLoader;
        let err = loader.load(dir.path().to_str().unwrap()).await;
        assert!(err.is_err());
    }

    #[tokio::test]
    async fn test_chain_loader_first_success_wins() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"data").unwrap();
        let path = tmp.path().to_str().unwrap().to_string();

        let chain = ChainLoader::new(vec![Box::new(HostPathLoader)]);
        let data = chain.load(&path).await.unwrap();
        assert_eq!(data, b"data");
    }

    #[tokio::test]
    async fn test_chain_loader_falls_through() {
        // First loader fails (bad path), second succeeds
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        tmp.write_all(b"fallback").unwrap();
        let good_path = tmp.path().to_str().unwrap().to_string();

        struct FailLoader;
        #[async_trait]
        impl Loader for FailLoader {
            async fn load(&self, _path: &str) -> Result<Vec<u8>> {
                Err(anyhow::anyhow!("always fails"))
            }
        }

        // FailLoader first, then HostPathLoader
        let chain = ChainLoader::new(vec![Box::new(FailLoader), Box::new(HostPathLoader)]);
        let data = chain.load(&good_path).await.unwrap();
        assert_eq!(data, b"fallback");
    }

    #[tokio::test]
    async fn test_chain_loader_all_fail() {
        struct FailLoader;
        #[async_trait]
        impl Loader for FailLoader {
            async fn load(&self, _path: &str) -> Result<Vec<u8>> {
                Err(anyhow::anyhow!("nope"))
            }
        }

        let chain = ChainLoader::new(vec![Box::new(FailLoader), Box::new(FailLoader)]);
        let err = chain.load("anything").await;
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("All loaders failed"));
        assert!(msg.contains("Loader 0"));
        assert!(msg.contains("Loader 1"));
    }

    #[tokio::test]
    async fn test_chain_loader_empty_chain_fails() {
        let chain = ChainLoader::new(vec![]);
        let err = chain.load("any").await;
        assert!(err.is_err());
    }
}
