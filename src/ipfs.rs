//! IPFS client API for interacting with IPFS nodes
//!
//! This module provides a clean interface for IPFS operations, similar to Go's
//! CoreAPI from github.com/ipfs/kubo/core/coreapi.
//!
//! Currently implements only the methods needed for file retrieval operations.

use anyhow::{Context, Result};
use reqwest;

/// IPFS client for interacting with an IPFS node via HTTP API
///
///
/// Similar to Go's CoreAPI, this provides access to various IPFS APIs
/// through sub-APIs like Unixfs, Block, etc.
#[derive(Clone)]
pub struct HttpClient {
    pub(crate) http_client: reqwest::Client,
    pub(crate) base_url: String,
}

impl HttpClient {
    /// Create a new IPFS client with the given HTTP API endpoint URL
    ///
    /// # Arguments
    ///
    /// * `ipfs_url` - The base URL of the IPFS HTTP API (e.g., "http://localhost:5001")
    pub fn new(ipfs_url: String) -> Self {
        Self {
            http_client: reqwest::Client::new(),
            base_url: ipfs_url.trim_end_matches('/').to_string(),
        }
    }

    /// Get the Unixfs API for file operations
    ///
    /// Unixfs is the filesystem abstraction layer for IPFS, providing
    /// operations for reading and writing files and directories.
    pub fn unixfs(self) -> UnixFS {
        UnixFS { client: self }
    }
}

/// Unixfs API for file and directory operations
///
/// Provides methods for interacting with IPFS files and directories,
/// similar to Go's UnixfsAPI.
pub struct UnixFS {
    pub(crate) client: HttpClient,
}

impl UnixFS {
    /// Get file content from an IPFS path
    ///
    /// This is equivalent to the `/api/v0/cat` endpoint and Go's UnixfsAPI.Get.
    /// It reads the content of a file from IPFS, IPNS, or IPLD paths.
    ///
    /// # Arguments
    ///
    /// * `path` - An IPFS path (e.g., "/ipfs/QmHash...", "/ipns/...", "/ipld/...")
    ///
    /// # Returns
    ///
    /// Returns the file content as bytes, or an error if the file cannot be retrieved.
    ///
    /// # Example
    ///
    /// ```no_run
    /// use ww::ipfs::HttpClient;
    ///
    /// # async fn example() -> anyhow::Result<()> {
    /// let client = HttpClient::new("http://localhost:5001".to_string());
    /// let content = client.unixfs().get("/ipfs/QmHash...").await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get(&self, path: &str) -> Result<Vec<u8>> {
        let url = format!("{}/api/v0/cat?arg={}", self.client.base_url, path);
        let response = self
            .client
            .http_client
            .post(&url)
            .send()
            .await
            .with_context(|| {
                format!("Failed to connect to IPFS node at {}", self.client.base_url)
            })?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to retrieve file from IPFS: {} (path: {})",
                response.status(),
                path
            ));
        }

        response
            .bytes()
            .await
            .with_context(|| format!("Failed to read IPFS content from {path}"))
            .map(|b| b.to_vec())
    }
}

/// Check if a path is a valid IPFS-family path (IPFS, IPNS, or IPLD)
///
/// This centralizes IPFS path validation similar to Go's `path.NewPath(str)`.
/// Returns true if the path starts with a valid IPFS namespace prefix.
pub fn is_ipfs_path(path: &str) -> bool {
    path.starts_with("/ipfs/") || path.starts_with("/ipns/") || path.starts_with("/ipld/")
}
