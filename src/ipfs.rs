//! IPFS client API for interacting with IPFS nodes
//!
//! This module provides a clean interface for IPFS operations, similar to Go's
//! CoreAPI from github.com/ipfs/kubo/core/coreapi.
//!
//! Currently implements only the methods needed for file retrieval operations.

use std::path::Path;

use anyhow::{Context, Result};

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

    /// Get a borrowing reference to the Unixfs API.
    pub fn unixfs_ref(&self) -> UnixFSRef<'_> {
        UnixFSRef { client: self }
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

/// Borrowing reference to the Unixfs API.
pub struct UnixFSRef<'a> {
    client: &'a HttpClient,
}

impl UnixFSRef<'_> {
    /// Fetch an IPFS directory and extract it to a local path.
    ///
    /// Uses kubo's `/api/v0/get?arg=<path>&archive=true` which returns
    /// the directory contents as a TAR archive. The top-level CID
    /// directory in the TAR is stripped so files land directly under `dst`.
    pub async fn get_dir(&self, ipfs_path: &str, dst: &Path) -> Result<()> {
        let url = format!(
            "{}/api/v0/get?arg={}&archive=true",
            self.client.base_url, ipfs_path
        );
        let response = self
            .client
            .http_client
            .post(&url)
            .send()
            .await
            .with_context(|| {
                format!(
                    "Failed to fetch IPFS directory from {}",
                    self.client.base_url
                )
            })?;

        if !response.status().is_success() {
            anyhow::bail!(
                "Failed to fetch IPFS directory: {} (path: {})",
                response.status(),
                ipfs_path
            );
        }

        let bytes = response
            .bytes()
            .await
            .with_context(|| format!("Failed to read IPFS archive from {ipfs_path}"))?;

        let cursor = std::io::Cursor::new(bytes);
        let mut archive = tar::Archive::new(cursor);

        // Kubo wraps the directory in a top-level entry named after the CID.
        // Strip that prefix so files land directly under dst.
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();

            // Strip the first component (the CID directory).
            let stripped: std::path::PathBuf = path.components().skip(1).collect();
            if stripped.as_os_str().is_empty() {
                continue; // skip the root CID entry itself
            }

            let target = dst.join(&stripped);
            if entry.header().entry_type().is_dir() {
                std::fs::create_dir_all(&target)?;
            } else {
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let mut file = std::fs::File::create(&target)?;
                std::io::copy(&mut entry, &mut file)?;
            }
        }

        Ok(())
    }
}

/// Directory listing entry from Kubo's `/api/v0/ls` endpoint.
#[derive(Debug, Clone)]
pub struct LsEntry {
    pub name: String,
    pub hash: String,
    pub size: u64,
    pub entry_type: u32, // 1 = directory, 2 = file
}

impl HttpClient {
    /// List directory entries at an IPFS path.
    ///
    /// Calls Kubo's `/api/v0/ls?arg=<path>` and parses the JSON response.
    pub async fn ls(&self, path: &str) -> Result<Vec<LsEntry>> {
        let url = format!("{}/api/v0/ls?arg={}", self.base_url, path);
        let response = self
            .http_client
            .post(&url)
            .send()
            .await
            .with_context(|| format!("Failed to ls IPFS path {path}"))?;

        if !response.status().is_success() {
            anyhow::bail!("IPFS ls failed: {} (path: {})", response.status(), path);
        }

        let body: serde_json::Value = response
            .json()
            .await
            .with_context(|| format!("Failed to parse IPFS ls response for {path}"))?;

        let links = body["Objects"]
            .as_array()
            .and_then(|objs| objs.first())
            .and_then(|obj| obj["Links"].as_array())
            .unwrap_or(&Vec::new())
            .clone();

        let entries = links
            .iter()
            .map(|link| LsEntry {
                name: link["Name"].as_str().unwrap_or("").to_string(),
                hash: link["Hash"].as_str().unwrap_or("").to_string(),
                size: link["Size"].as_u64().unwrap_or(0),
                entry_type: link["Type"].as_u64().unwrap_or(0) as u32,
            })
            .collect();

        Ok(entries)
    }

    /// Pin a CID on the IPFS node.
    ///
    /// Calls Kubo's `/api/v0/pin/add?arg=<cid>`.
    pub async fn pin_add(&self, cid: &str) -> Result<()> {
        let url = format!("{}/api/v0/pin/add?arg={}", self.base_url, cid);
        let response = self
            .http_client
            .post(&url)
            .send()
            .await
            .with_context(|| format!("Failed to pin {cid}"))?;

        if !response.status().is_success() {
            anyhow::bail!("IPFS pin add failed: {} (cid: {})", response.status(), cid);
        }

        Ok(())
    }

    /// Unpin a CID from the IPFS node.
    ///
    /// Calls Kubo's `/api/v0/pin/rm?arg=<cid>`.
    pub async fn pin_rm(&self, cid: &str) -> Result<()> {
        let url = format!("{}/api/v0/pin/rm?arg={}", self.base_url, cid);
        let response = self
            .http_client
            .post(&url)
            .send()
            .await
            .with_context(|| format!("Failed to unpin {cid}"))?;

        if !response.status().is_success() {
            anyhow::bail!("IPFS pin rm failed: {} (cid: {})", response.status(), cid);
        }

        Ok(())
    }

    /// Add a directory tree to IPFS and return the root CID.
    ///
    /// Adds all files in the directory to IPFS and returns
    /// the CID of the root directory. Skips common build artifacts.
    pub async fn add_dir(&self, dir_path: &Path) -> Result<String> {
        use std::collections::VecDeque;
        use std::fs;

        let url = format!(
            "{}/api/v0/add?wrap-with-directory=true&progress=false",
            self.base_url
        );
        let mut form = reqwest::multipart::Form::new();

        // Collect all files first to get directory structure right
        let mut files_to_add: Vec<(String, Vec<u8>)> = Vec::new();

        // Use iterative approach to collect all files
        let mut queue = VecDeque::new();
        queue.push_back((dir_path.to_path_buf(), String::new()));

        while let Some((current_dir, prefix)) = queue.pop_front() {
            let entries = fs::read_dir(&current_dir)
                .with_context(|| format!("Failed to read directory: {}", current_dir.display()))?;

            let mut dir_entries: Vec<_> = entries.collect::<std::result::Result<Vec<_>, _>>()?;
            // Sort for consistent ordering
            dir_entries.sort_by_key(|e| e.file_name());

            for entry in dir_entries {
                let path = entry.path();
                let file_name = entry.file_name();
                let file_name_str = file_name.to_string_lossy().to_string();

                // Skip Cargo build artifacts and version control
                if file_name_str == "target"
                    || file_name_str == ".git"
                    || file_name_str == ".gitignore"
                    || file_name_str == "Cargo.lock"
                {
                    continue;
                }

                let rel_path = if prefix.is_empty() {
                    file_name_str.clone()
                } else {
                    format!("{}/{}", prefix, file_name_str)
                };

                if path.is_dir() {
                    // Queue subdirectories for processing
                    queue.push_back((path, rel_path));
                } else {
                    // Read file
                    let bytes = fs::read(&path)
                        .with_context(|| format!("Failed to read file: {}", path.display()))?;
                    files_to_add.push((rel_path, bytes));
                }
            }
        }

        // Add files to form in sorted order for consistent structure
        for (path, bytes) in files_to_add {
            let part = reqwest::multipart::Part::bytes(bytes);
            form = form.part("file".to_string(), part.file_name(path));
        }

        let response = self
            .http_client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .context("Failed to add directory to IPFS")?;

        if !response.status().is_success() {
            anyhow::bail!("IPFS add failed: {}", response.status());
        }

        let body = response.text().await?;

        // Parse all lines and find the wrapped root directory
        // With wrap-with-directory=true, the last entry is the wrapping directory
        for line in body.lines().rev() {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                if let Some(hash) = json.get("Hash").and_then(|h| h.as_str()) {
                    return Ok(hash.to_string());
                }
            }
        }

        anyhow::bail!("Failed to extract CID from IPFS response")
    }
}

// ── Routing / Node Identity API ────────────────────────────────────

/// Node identity returned by Kubo's `/api/v0/id` endpoint.
///
/// Used by the Wetware swarm to bootstrap the in-process Kademlia client
/// against the local Kubo node (Amino DHT).
#[derive(Debug, Clone)]
pub struct KuboInfo {
    /// Kubo's libp2p peer ID as a base58-encoded string (e.g. `"12D3KooW..."`).
    pub peer_id: String,
    /// Kubo's swarm listen addresses (may include `/p2p/<peer-id>` suffix).
    pub swarm_addrs: Vec<String>,
}

impl HttpClient {
    /// Fetch the local Kubo node's identity for Kad bootstrap.
    ///
    /// Calls `POST /api/v0/id` and returns the peer ID and swarm addresses.
    /// Returns an error if Kubo is not reachable.
    pub async fn kubo_info(&self) -> Result<KuboInfo> {
        let url = format!("{}/api/v0/id", self.base_url);
        let response = self
            .http_client
            .post(&url)
            .send()
            .await
            .with_context(|| format!("kubo id failed: {}", self.base_url))?;

        if !response.status().is_success() {
            anyhow::bail!("kubo id failed: {}", response.status());
        }

        let body: serde_json::Value = response
            .json()
            .await
            .context("kubo id: failed to parse JSON response")?;

        let peer_id = body["ID"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("kubo id: missing ID field"))?
            .to_string();

        let swarm_addrs = body["Addresses"]
            .as_array()
            .unwrap_or(&Vec::new())
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();

        Ok(KuboInfo {
            peer_id,
            swarm_addrs,
        })
    }

    /// Add raw bytes to IPFS, returning the CID.
    ///
    /// Calls Kubo's `POST /api/v0/add`.  This is a UnixFS operation, not
    /// a routing operation — it belongs alongside `cat` and `ls`.
    pub async fn add_bytes(&self, data: &[u8]) -> Result<String> {
        let url = format!("{}/api/v0/add", self.base_url);
        let part = reqwest::multipart::Part::bytes(data.to_vec()).file_name("data");
        let form = reqwest::multipart::Form::new().part("file", part);

        let response = self
            .http_client
            .post(&url)
            .multipart(form)
            .send()
            .await
            .context("ipfs add failed")?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("ipfs add failed: {body}");
        }

        let body: serde_json::Value = response.json().await?;
        body.get("Hash")
            .and_then(|h| h.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("ipfs add: missing Hash in response"))
    }
}

/// Check if a path is a valid IPFS-family path (IPFS, IPNS, or IPLD)
///
/// This centralizes IPFS path validation similar to Go's `path.NewPath(str)`.
/// Returns true if the path starts with a valid IPFS namespace prefix.
pub fn is_ipfs_path(path: &str) -> bool {
    path.starts_with("/ipfs/") || path.starts_with("/ipns/") || path.starts_with("/ipld/")
}

// ── MFS (Mutable File System) API ──────────────────────────────────

/// MFS directory entry from `/api/v0/files/ls`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct MfsEntry {
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "Hash")]
    pub hash: String,
    #[serde(rename = "Size")]
    pub size: u64,
    /// 0 = file, 1 = directory
    #[serde(rename = "Type")]
    pub entry_type: u32,
}

/// MFS stat result from `/api/v0/files/stat`.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct MfsStat {
    #[serde(rename = "Hash")]
    pub hash: String,
    #[serde(rename = "Size")]
    pub size: u64,
    #[serde(rename = "Type")]
    pub entry_type: String,
}

/// Borrowing reference to the MFS API.
pub struct MFS<'a> {
    client: &'a HttpClient,
}

impl HttpClient {
    /// Get a borrowing reference to the MFS API.
    pub fn mfs(&self) -> MFS<'_> {
        MFS { client: self }
    }
}

impl MFS<'_> {
    /// Create a directory in MFS.
    pub async fn files_mkdir(&self, path: &str, parents: bool) -> Result<()> {
        let url = format!(
            "{}/api/v0/files/mkdir?arg={}&parents={}",
            self.client.base_url, path, parents
        );
        let response = self
            .client
            .http_client
            .post(&url)
            .send()
            .await
            .with_context(|| format!("MFS mkdir failed for {path}"))?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("MFS mkdir failed for {path}: {body}");
        }
        Ok(())
    }

    /// Copy a file or directory in MFS. Source can be `/ipfs/<cid>`.
    pub async fn files_cp(&self, src: &str, dst: &str) -> Result<()> {
        let url = format!(
            "{}/api/v0/files/cp?arg={}&arg={}",
            self.client.base_url, src, dst
        );
        let response = self
            .client
            .http_client
            .post(&url)
            .send()
            .await
            .with_context(|| format!("MFS cp failed: {src} -> {dst}"))?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("MFS cp failed ({src} -> {dst}): {body}");
        }
        Ok(())
    }

    /// List entries in an MFS directory.
    pub async fn files_ls(&self, path: &str) -> Result<Vec<MfsEntry>> {
        let url = format!(
            "{}/api/v0/files/ls?arg={}&long=true",
            self.client.base_url, path
        );
        let response = self
            .client
            .http_client
            .post(&url)
            .send()
            .await
            .with_context(|| format!("MFS ls failed for {path}"))?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("MFS ls failed for {path}: {body}");
        }

        let body: serde_json::Value = response
            .json()
            .await
            .with_context(|| format!("Failed to parse MFS ls response for {path}"))?;

        let entries: Vec<MfsEntry> = body
            .get("Entries")
            .and_then(|e| e.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| serde_json::from_value(v.clone()).ok())
                    .collect()
            })
            .unwrap_or_default();

        Ok(entries)
    }

    /// Stat an MFS path, optionally computing its hash.
    pub async fn files_stat(&self, path: &str, hash: bool) -> Result<MfsStat> {
        let url = format!(
            "{}/api/v0/files/stat?arg={}&hash={}",
            self.client.base_url, path, hash
        );
        let response = self
            .client
            .http_client
            .post(&url)
            .send()
            .await
            .with_context(|| format!("MFS stat failed for {path}"))?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("MFS stat failed for {path}: {body}");
        }

        response
            .json()
            .await
            .with_context(|| format!("Failed to parse MFS stat response for {path}"))
    }

    /// Remove an MFS path.
    pub async fn files_rm(&self, path: &str, recursive: bool) -> Result<()> {
        let url = format!(
            "{}/api/v0/files/rm?arg={}&recursive={}",
            self.client.base_url, path, recursive
        );
        let response = self
            .client
            .http_client
            .post(&url)
            .send()
            .await
            .with_context(|| format!("MFS rm failed for {path}"))?;

        if !response.status().is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("MFS rm failed for {path}: {body}");
        }
        Ok(())
    }
}
