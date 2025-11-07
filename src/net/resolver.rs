use anyhow::{anyhow, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tracing::{debug, info};

/// Service resolver for versioned WASM services
///
/// Maps libp2p protocol paths to filesystem locations following the
/// `/ww/<version>/` namespace convention.
pub struct ServiceResolver {
    /// Root directory containing `/ww/` versions
    root: PathBuf,
    /// IPFS API endpoint URL (e.g., "http://localhost:5001")
    #[allow(dead_code)]
    ipfs_url: String,
    /// Cache for IPFS content (hash -> content mapping)
    #[allow(dead_code)]
    ipfs_cache: HashMap<String, Vec<u8>>,
    /// Cache for IPFS directory structures (hash -> directory listing)
    #[allow(dead_code)]
    ipfs_dir_cache: HashMap<String, HashMap<String, String>>,
}

impl ServiceResolver {
    /// Create a new service resolver
    pub fn new(root: PathBuf, ipfs_url: String) -> Self {
        Self {
            root,
            ipfs_url,
            ipfs_cache: HashMap::new(),
            ipfs_dir_cache: HashMap::new(),
        }
    }

    /// Detect available versions by scanning the root directory
    pub fn detect_versions(&self) -> Result<Vec<String>> {
        let ww_dir = self.root.join("ww");

        if !ww_dir.exists() {
            return Ok(vec![]);
        }

        let mut versions = Vec::new();

        for entry in fs::read_dir(&ww_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                if let Some(version) = path.file_name().and_then(|n| n.to_str()) {
                    // Validate version format (semantic versioning)
                    if self.is_valid_version(version) {
                        versions.push(version.to_string());
                    }
                }
            }
        }

        // Sort versions (newest first)
        versions.sort_by(|a, b| {
            let a_parts: Vec<u32> = a.split('.').filter_map(|s| s.parse().ok()).collect();
            let b_parts: Vec<u32> = b.split('.').filter_map(|s| s.parse().ok()).collect();
            b_parts.cmp(&a_parts) // Reverse order for newest first
        });

        info!(versions = ?versions, "Detected available versions");
        Ok(versions)
    }

    /// Get the latest version available
    pub fn get_latest_version(&self) -> Result<Option<String>> {
        let versions = self.detect_versions()?;
        Ok(versions.first().cloned())
    }

    /// Resolve a service path to WASM bytecode
    ///
    /// Examples:
    /// - `/ww/0.1.0/echo` → `/ww/0.1.0/public/echo.wasm`
    /// - `/ww/0.1.0/math/add` → `/ww/0.1.0/public/math/add.wasm`
    /// - `/ipfs/QmHash` → fetch from IPFS and extract service
    #[allow(dead_code)]
    pub async fn resolve_service(&mut self, service_path: &str) -> Result<Vec<u8>> {
        debug!(service_path = %service_path, "Resolving service");

        if service_path.starts_with("/ipfs/") {
            return self.resolve_ipfs_service(service_path).await;
        }

        if service_path.starts_with("/ww/") {
            return self.resolve_local_service(service_path).await;
        }

        Err(anyhow!("Invalid service path format: {}", service_path))
    }

    /// Resolve a local filesystem service
    async fn resolve_local_service(&self, service_path: &str) -> Result<Vec<u8>> {
        // Parse path: /ww/<version>/<service_path>
        let parts: Vec<&str> = service_path.trim_start_matches("/ww/").split('/').collect();

        if parts.is_empty() {
            return Err(anyhow!("Invalid service path: {}", service_path));
        }

        let version = parts[0];
        let service_path = if parts.len() > 1 {
            parts[1..].join("/")
        } else {
            return Err(anyhow!("No service specified in path: {}", service_path));
        };

        // Map to filesystem: /ww/<version>/public/<service_path>.wasm
        let wasm_path = self
            .root
            .join("ww")
            .join(version)
            .join("public")
            .join(format!("{service_path}.wasm"));

        debug!(wasm_path = %wasm_path.display(), "Resolving local WASM file");

        if !wasm_path.exists() {
            return Err(anyhow!("Service not found: {}", wasm_path.display()));
        }

        let bytecode = fs::read(&wasm_path)?;
        info!(service_path = %service_path, version = %version, "Resolved local service");
        Ok(bytecode)
    }

    /// Resolve an IPFS service
    async fn resolve_ipfs_service(&mut self, service_path: &str) -> Result<Vec<u8>> {
        // Parse IPFS path: /ipfs/QmHash/path/to/service
        let path_parts: Vec<&str> = service_path
            .trim_start_matches("/ipfs/")
            .split('/')
            .collect();
        let root_hash = path_parts[0];
        let service_path = if path_parts.len() > 1 {
            path_parts[1..].join("/")
        } else {
            return Err(anyhow!("No service path specified in IPFS URL"));
        };

        debug!(root_hash = %root_hash, service_path = %service_path, "Resolving IPFS service");

        // Check cache first
        let cache_key = format!("{root_hash}:{service_path}");
        if let Some(content) = self.ipfs_cache.get(&cache_key) {
            debug!(cache_key = %cache_key, "Using cached IPFS content");
            return Ok(content.clone());
        }

        // Resolve the service within the IPFS directory structure
        let content = self.resolve_ipfs_path(root_hash, &service_path).await?;

        // Cache the result
        self.ipfs_cache.insert(cache_key, content.clone());

        info!(root_hash = %root_hash, service_path = %service_path, "Resolved IPFS service");
        Ok(content)
    }

    /// Resolve a path within an IPFS directory structure
    async fn resolve_ipfs_path(&mut self, root_hash: &str, service_path: &str) -> Result<Vec<u8>> {
        // Get directory structure for the root hash
        let dir_structure = self.get_ipfs_directory_structure(root_hash).await?;

        // Look for the service path in the structure
        if let Some(file_hash) = dir_structure.get(service_path) {
            // Fetch the file content
            self.fetch_ipfs_content(file_hash).await
        } else {
            // Try to find the service in the /ww/<version>/public/ structure
            self.find_service_in_ww_structure(root_hash, service_path)
                .await
        }
    }

    /// Find a service within the /ww/<version>/public/ structure
    async fn find_service_in_ww_structure(
        &mut self,
        root_hash: &str,
        service_path: &str,
    ) -> Result<Vec<u8>> {
        // Get the root directory structure
        let root_structure = self.get_ipfs_directory_structure(root_hash).await?;

        // Look for ww directory
        if let Some(ww_hash) = root_structure.get("ww") {
            let ww_structure = self.get_ipfs_directory_structure(ww_hash).await?;

            // Find the latest version
            let mut versions: Vec<&String> = ww_structure
                .keys()
                .filter(|k| self.is_valid_version(k))
                .collect();
            versions.sort_by(|a, b| {
                let a_parts: Vec<u32> = a.split('.').filter_map(|s| s.parse().ok()).collect();
                let b_parts: Vec<u32> = b.split('.').filter_map(|s| s.parse().ok()).collect();
                b_parts.cmp(&a_parts) // Newest first
            });

            if let Some(latest_version) = versions.first() {
                if let Some(version_hash) = ww_structure.get(*latest_version) {
                    let version_structure = self.get_ipfs_directory_structure(version_hash).await?;

                    // Look in public directory
                    if let Some(public_hash) = version_structure.get("public") {
                        let public_structure =
                            self.get_ipfs_directory_structure(public_hash).await?;

                        // Look for the service file
                        let service_file = format!("{service_path}.wasm");
                        if let Some(file_hash) = public_structure.get(&service_file) {
                            return self.fetch_ipfs_content(file_hash).await;
                        }
                    }
                }
            }
        }

        Err(anyhow!(
            "Service not found in IPFS structure: {}",
            service_path
        ))
    }

    /// Get directory structure for an IPFS hash
    async fn get_ipfs_directory_structure(
        &mut self,
        hash: &str,
    ) -> Result<HashMap<String, String>> {
        // Check cache first
        if let Some(structure) = self.ipfs_dir_cache.get(hash) {
            return Ok(structure.clone());
        }

        // Fetch directory listing from IPFS
        let structure = self.fetch_ipfs_directory_listing(hash).await?;

        // Cache the result
        self.ipfs_dir_cache
            .insert(hash.to_string(), structure.clone());

        Ok(structure)
    }

    /// Fetch directory listing from IPFS using ls command
    async fn fetch_ipfs_directory_listing(&self, hash: &str) -> Result<HashMap<String, String>> {
        use reqwest::Client;

        let client = Client::new();
        let url = format!("{}/api/v0/ls?arg={}", self.ipfs_url, hash);

        debug!(url = %url, "Fetching IPFS directory listing");

        let response = client.get(&url).send().await?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "IPFS directory listing failed: {}",
                response.status()
            ));
        }

        let json: Value = response.json().await?;
        let mut structure = HashMap::new();

        if let Some(objects) = json["Objects"].as_array() {
            for obj in objects {
                if let Some(links) = obj["Links"].as_array() {
                    for link in links {
                        if let (Some(name), Some(hash)) =
                            (link["Name"].as_str(), link["Hash"].as_str())
                        {
                            structure.insert(name.to_string(), hash.to_string());
                        }
                    }
                }
            }
        }

        debug!(hash = %hash, entries = structure.len(), "Fetched IPFS directory structure");
        Ok(structure)
    }

    /// Fetch content from IPFS using the Kubo API
    async fn fetch_ipfs_content(&self, hash: &str) -> Result<Vec<u8>> {
        use reqwest::Client;

        let client = Client::new();
        let url = format!("{}/api/v0/cat?arg={}", self.ipfs_url, hash);

        debug!(url = %url, "Fetching IPFS content");

        let response = client.get(&url).send().await?;

        if !response.status().is_success() {
            return Err(anyhow!("IPFS fetch failed: {}", response.status()));
        }

        let content = response.bytes().await?;
        Ok(content.to_vec())
    }

    /// Validate version string format (semantic versioning)
    fn is_valid_version(&self, version: &str) -> bool {
        // Simple validation: should be in format "X.Y.Z" where X, Y, Z are numbers
        let parts: Vec<&str> = version.split('.').collect();
        if parts.len() != 3 {
            return false;
        }

        for part in parts {
            if part.parse::<u32>().is_err() {
                return false;
            }
        }

        true
    }

    /// Get the protocol string for a given version
    pub fn get_protocol(&self, version: &str) -> String {
        format!("/ww/{version}/")
    }

    /// Parse a protocol string to extract version and service path
    #[allow(dead_code)]
    pub fn parse_protocol(&self, protocol: &str) -> Result<(String, String)> {
        if !protocol.starts_with("/ww/") {
            return Err(anyhow!("Invalid protocol format: {}", protocol));
        }

        let path = protocol.trim_start_matches("/ww/").trim_end_matches('/');
        let parts: Vec<&str> = path.split('/').collect();

        if parts.is_empty() {
            return Err(anyhow!("Empty protocol path: {}", protocol));
        }

        let version = parts[0].to_string();
        let service_path = if parts.len() > 1 {
            parts[1..].join("/")
        } else {
            "".to_string()
        };

        Ok((version, service_path))
    }

    /// Clear IPFS cache
    #[allow(dead_code)]
    pub fn clear_ipfs_cache(&mut self) {
        self.ipfs_cache.clear();
        self.ipfs_dir_cache.clear();
        info!("Cleared IPFS cache");
    }

    /// Get cache statistics
    #[allow(dead_code)]
    pub fn get_cache_stats(&self) -> (usize, usize) {
        (self.ipfs_cache.len(), self.ipfs_dir_cache.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_version_detection() {
        let temp_dir = TempDir::new().unwrap();
        let resolver = ServiceResolver::new(
            temp_dir.path().to_path_buf(),
            "http://localhost:5001".to_string(),
        );

        // Create test directory structure
        fs::create_dir_all(temp_dir.path().join("ww/0.1.0/public")).unwrap();
        fs::create_dir_all(temp_dir.path().join("ww/0.2.0/public")).unwrap();
        fs::create_dir_all(temp_dir.path().join("ww/invalid/public")).unwrap();

        let versions = resolver.detect_versions().unwrap();
        assert_eq!(versions, vec!["0.2.0", "0.1.0"]);

        let latest = resolver.get_latest_version().unwrap();
        assert_eq!(latest, Some("0.2.0".to_string()));
    }

    #[test]
    fn test_protocol_parsing() {
        let temp_dir = TempDir::new().unwrap();
        let resolver = ServiceResolver::new(
            temp_dir.path().to_path_buf(),
            "http://localhost:5001".to_string(),
        );

        let (version, service) = resolver.parse_protocol("/ww/0.1.0/echo").unwrap();
        assert_eq!(version, "0.1.0");
        assert_eq!(service, "echo");

        let (version, service) = resolver.parse_protocol("/ww/0.1.0/math/add").unwrap();
        assert_eq!(version, "0.1.0");
        assert_eq!(service, "math/add");

        let (version, service) = resolver.parse_protocol("/ww/0.1.0/").unwrap();
        assert_eq!(version, "0.1.0");
        assert_eq!(service, "");
    }

    #[test]
    fn test_version_validation() {
        let temp_dir = TempDir::new().unwrap();
        let resolver = ServiceResolver::new(
            temp_dir.path().to_path_buf(),
            "http://localhost:5001".to_string(),
        );

        assert!(resolver.is_valid_version("0.1.0"));
        assert!(resolver.is_valid_version("1.0.0"));
        assert!(resolver.is_valid_version("10.20.30"));

        assert!(!resolver.is_valid_version("0.1"));
        assert!(!resolver.is_valid_version("0.1.0.0"));
        assert!(!resolver.is_valid_version("invalid"));
        assert!(!resolver.is_valid_version("0.1.a"));
    }

    #[tokio::test]
    async fn test_service_resolution() {
        let temp_dir = TempDir::new().unwrap();
        let mut resolver = ServiceResolver::new(
            temp_dir.path().to_path_buf(),
            "http://localhost:5001".to_string(),
        );

        // Create test service
        fs::create_dir_all(temp_dir.path().join("ww/0.1.0/public")).unwrap();
        fs::write(
            temp_dir.path().join("ww/0.1.0/public/echo.wasm"),
            b"wasm_content",
        )
        .unwrap();

        // Test service resolution
        let result = resolver.resolve_service("/ww/0.1.0/echo").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"wasm_content");
    }

    #[tokio::test]
    async fn test_nested_service_resolution() {
        let temp_dir = TempDir::new().unwrap();
        let mut resolver = ServiceResolver::new(
            temp_dir.path().to_path_buf(),
            "http://localhost:5001".to_string(),
        );

        // Create nested service structure
        fs::create_dir_all(temp_dir.path().join("ww/0.1.0/public/math")).unwrap();
        fs::write(
            temp_dir.path().join("ww/0.1.0/public/math/add.wasm"),
            b"add_wasm_content",
        )
        .unwrap();

        // Test nested service resolution
        let result = resolver.resolve_service("/ww/0.1.0/math/add").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), b"add_wasm_content");
    }

    #[test]
    fn test_cache_management() {
        let temp_dir = TempDir::new().unwrap();
        let mut resolver = ServiceResolver::new(
            temp_dir.path().to_path_buf(),
            "http://localhost:5001".to_string(),
        );

        // Test initial cache stats
        let (content_cache, dir_cache) = resolver.get_cache_stats();
        assert_eq!(content_cache, 0);
        assert_eq!(dir_cache, 0);

        // Test cache clearing
        resolver.clear_ipfs_cache();
        let (content_cache, dir_cache) = resolver.get_cache_stats();
        assert_eq!(content_cache, 0);
        assert_eq!(dir_cache, 0);
    }

    #[test]
    fn test_get_latest_version() {
        let temp_dir = TempDir::new().unwrap();
        let resolver = ServiceResolver::new(
            temp_dir.path().to_path_buf(),
            "http://localhost:5001".to_string(),
        );

        // Create multiple versions
        fs::create_dir_all(temp_dir.path().join("ww/0.1.0/public")).unwrap();
        fs::create_dir_all(temp_dir.path().join("ww/0.2.0/public")).unwrap();
        fs::create_dir_all(temp_dir.path().join("ww/1.0.0/public")).unwrap();

        // Test latest version detection
        let latest = resolver.get_latest_version().unwrap();
        assert_eq!(latest, Some("1.0.0".to_string()));
    }
}
