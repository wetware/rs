use anyhow::{anyhow, Context, Result};
use libp2p::{Multiaddr, PeerId};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::loaders;

/// Boot peer discovery and DHT bootstrap configuration
pub struct BootConfig {
    /// Root directory for local filesystem paths (jailed)
    pub root: Option<PathBuf>,
    /// IPFS path for configuration (if using IPFS)
    pub ipfs_path: Option<String>,
    /// IPFS HTTP API endpoint URL
    pub ipfs_url: String,
    /// IPFS bootstrap peers (standard IPFS public nodes)
    pub ipfs_bootstrap_peers: Vec<Multiaddr>,
}

impl BootConfig {
    /// Create a new boot configuration
    ///
    /// `root` can be either a local filesystem path (jailed to that root) or
    /// an IPFS UnixFS path (e.g., `/ipfs/QmHash...`).
    pub fn new(root: String, ipfs_url: String) -> Self {
        let (local_root, ipfs_path) = if loaders::is_ipfs_path(&root) {
            (None, Some(root))
        } else {
            (Some(PathBuf::from(root)), None)
        };

        Self {
            root: local_root,
            ipfs_path,
            ipfs_url,
            ipfs_bootstrap_peers: Self::get_ipfs_bootstrap_peers(),
        }
    }

    /// Sanitize a relative path to ensure it stays within the jail root
    ///
    /// This prevents directory traversal attacks (e.g., `../` escaping).
    /// Returns an error if the path would escape the root directory.
    pub fn sanitize_path(root: &Path, rel_path: &str) -> Result<PathBuf> {
        // Normalize the path by resolving components
        let mut components = Vec::new();

        for component in rel_path.split('/') {
            match component {
                "" | "." => continue,
                ".." => {
                    // If we have components, remove the last one (go up one level)
                    // If we don't have components, this would escape root - reject it
                    if components.pop().is_none() {
                        return Err(anyhow!("Path would escape root directory: {}", rel_path));
                    }
                }
                _ => {
                    components.push(component);
                }
            }
        }

        // Build the sanitized path
        let mut sanitized = root.to_path_buf();
        for component in components {
            sanitized.push(component);
        }

        // Verify the final path is still within the root
        // If the path exists, canonicalize and verify
        // If it doesn't exist, verify by checking that all components stay within root
        if root.exists() && sanitized.exists() {
            let canonical_root = root
                .canonicalize()
                .with_context(|| format!("Failed to canonicalize root: {}", root.display()))?;
            let canonical_path = sanitized
                .canonicalize()
                .with_context(|| format!("Failed to canonicalize path: {}", sanitized.display()))?;

            if !canonical_path.starts_with(&canonical_root) {
                return Err(anyhow!(
                    "Path would escape root directory: {} (root: {})",
                    rel_path,
                    root.display()
                ));
            }
        } else {
            // Path doesn't exist yet - verify by checking components don't escape
            // We already did this above by rejecting ".." that would go below root
            // Additional check: ensure the path string representation starts with root
            let root_str = root.to_string_lossy();
            let sanitized_str = sanitized.to_string_lossy();
            if !sanitized_str.starts_with(root_str.as_ref()) {
                return Err(anyhow!(
                    "Path would escape root directory: {} (root: {})",
                    rel_path,
                    root.display()
                ));
            }
        }

        Ok(sanitized)
    }

    /// Get standard IPFS bootstrap peers
    pub fn get_ipfs_bootstrap_peers() -> Vec<Multiaddr> {
        vec![
            "/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN"
                .parse()
                .unwrap(),
            "/dnsaddr/bootstrap.libp2p.io/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa"
                .parse()
                .unwrap(),
            "/dnsaddr/bootstrap.libp2p.io/p2p/QmbLHAnMoJPWSCR5ZphtHh5F5FjGp6YhjaQ1VyaeoLjuXm"
                .parse()
                .unwrap(),
            "/dnsaddr/bootstrap.libp2p.io/p2p/QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt"
                .parse()
                .unwrap(),
            "/ip4/104.131.131.82/tcp/4001/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ"
                .parse()
                .unwrap(),
            "/ip4/104.131.131.82/udp/4001/quic/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ"
                .parse()
                .unwrap(),
        ]
    }

    /// Get the path to the boot peers file/directory
    ///
    /// Returns a String for IPFS paths or a sanitized PathBuf for local paths.
    pub fn get_boot_peers_path(&self) -> Result<String> {
        if let Some(ref ipfs_path) = self.ipfs_path {
            // IPFS path: construct <ipfs_path>/boot/peers
            Ok(format!("{}/boot/peers", ipfs_path.trim_end_matches('/')))
        } else if let Some(ref root) = self.root {
            // Local path: sanitize and construct {root}/boot/peers
            let sanitized = Self::sanitize_path(root, "boot/peers")?;
            Ok(sanitized.to_string_lossy().to_string())
        } else {
            Err(anyhow!("No root or IPFS path configured"))
        }
    }

    /// Read file content from IPFS or local filesystem
    ///
    /// Returns `Ok(None)` if the file doesn't exist (not an error).
    /// Returns `Ok(Some(content))` if the file exists and was read successfully.
    async fn read_file_content(path: &str, ipfs_url: &str) -> Result<Option<String>> {
        if loaders::is_ipfs_path(path) {
            // IPFS path: fetch via HTTP API
            let client = reqwest::Client::new();
            let url = format!("{}/api/v0/cat?arg={}", ipfs_url, path);
            let response = client
                .post(&url)
                .send()
                .await
                .with_context(|| format!("Failed to connect to IPFS node at {}", ipfs_url))?;

            if !response.status().is_success() {
                return Ok(None);
            }

            let content = response
                .text()
                .await
                .with_context(|| format!("Failed to read IPFS content from {}", path))?;
            Ok(Some(content))
        } else {
            // Local path: read from filesystem
            let path_buf = PathBuf::from(path);
            if !path_buf.exists() {
                return Ok(None);
            }

            if path_buf.is_dir() {
                // Directories are not supported by this function
                return Err(anyhow!("Path is a directory, not a file: {}", path));
            }

            let content = fs::read_to_string(&path_buf)
                .with_context(|| format!("Failed to read file: {}", path))?;
            Ok(Some(content))
        }
    }

    /// Parse newline-separated multiaddrs from content
    ///
    /// This is a pure parsing function that takes a string of newline-separated
    /// multiaddrs and returns a vector of parsed Multiaddr values.
    /// Invalid lines are skipped with a warning.
    pub fn parse_multiaddrs_from_content(content: &str) -> Vec<Multiaddr> {
        let mut multiaddrs = Vec::new();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            match line.parse::<Multiaddr>() {
                Ok(multiaddr) => multiaddrs.push(multiaddr),
                Err(e) => {
                    warn!(line = %line, error = ?e, "Failed to parse multiaddr");
                }
            }
        }

        debug!(count = multiaddrs.len(), "Parsed multiaddrs from content");
        multiaddrs
    }

    /// Load boot peers from filesystem or IPFS
    pub async fn load_boot_peers(&self) -> Result<HashMap<PeerId, Vec<Multiaddr>>> {
        let boot_peers_path = self.get_boot_peers_path()?;

        debug!(boot_peers_path = %boot_peers_path, "Loading boot peers");

        // Check if it's a local directory (special case for directory mode)
        if !loaders::is_ipfs_path(&boot_peers_path) {
            let path = PathBuf::from(&boot_peers_path);
            if path.exists() && path.is_dir() {
                // Directory mode: each file = peer ID (filename), contents = multiaddrs
                return self.load_boot_peers_from_directory(&path).await;
            }
        }

        // File mode: read content and parse
        let content = match Self::read_file_content(&boot_peers_path, &self.ipfs_url).await? {
            Some(content) => content,
            None => {
                info!("No boot peers file found at path: {}", boot_peers_path);
                return Ok(HashMap::new());
            }
        };

        // Parse content and extract peer IDs
        let multiaddrs = Self::parse_multiaddrs_from_content(&content);
        let mut boot_peers = HashMap::new();

        for multiaddr in multiaddrs {
            if let Some(peer_id) = self.extract_peer_id(&multiaddr)? {
                boot_peers
                    .entry(peer_id)
                    .or_insert_with(Vec::new)
                    .push(multiaddr);
            }
        }

        info!(peer_count = boot_peers.len(), "Loaded boot peers");
        Ok(boot_peers)
    }

    /// Load boot peers from a local directory
    ///
    /// Each file in the directory represents a peer ID (filename), and the file
    /// contents are newline-separated multiaddrs for that peer.
    async fn load_boot_peers_from_directory(
        &self,
        dir_path: &Path,
    ) -> Result<HashMap<PeerId, Vec<Multiaddr>>> {
        let mut boot_peers = HashMap::new();

        for entry in fs::read_dir(dir_path)? {
            let entry = entry?;
            let file_path = entry.path();

            if file_path.is_file() {
                let peer_id_str = file_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .ok_or_else(|| anyhow!("Invalid peer ID filename: {}", file_path.display()))?;

                // Try to parse peer ID, skip if invalid
                if let Ok(peer_id) = peer_id_str.parse::<PeerId>() {
                    let file_content = fs::read_to_string(&file_path)
                        .with_context(|| format!("Failed to read file: {}", file_path.display()))?;
                    let multiaddrs = Self::parse_multiaddrs_from_content(&file_content);
                    boot_peers.insert(peer_id, multiaddrs);
                } else {
                    warn!(peer_id = %peer_id_str, "Skipping invalid peer ID filename");
                }
            }
        }

        info!(
            peer_count = boot_peers.len(),
            "Loaded boot peers from directory"
        );
        Ok(boot_peers)
    }

    /// Extract peer ID from a multiaddr
    fn extract_peer_id(&self, multiaddr: &Multiaddr) -> Result<Option<PeerId>> {
        for protocol in multiaddr.iter() {
            if let multiaddr::Protocol::P2p(peer_id) = protocol {
                return Ok(Some(peer_id));
            }
        }
        Ok(None)
    }

    /// Get all boot peers (IPFS + filesystem)
    pub async fn get_all_boot_peers(&self) -> Result<Vec<Multiaddr>> {
        let mut all_peers = Vec::new();

        // Add IPFS bootstrap peers
        all_peers.extend(self.ipfs_bootstrap_peers.clone());

        // Add filesystem/IPFS boot peers
        let filesystem_peers = self.load_boot_peers().await?;
        for multiaddrs in filesystem_peers.values() {
            all_peers.extend(multiaddrs.clone());
        }

        info!(total_peers = all_peers.len(), "Collected all boot peers");
        Ok(all_peers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_boot_config_creation() {
        let temp_dir = TempDir::new().unwrap();
        let root_path = temp_dir.path().to_string_lossy().to_string();
        let config = BootConfig::new(root_path, "http://localhost:5001".to_string());

        assert!(!config.ipfs_bootstrap_peers.is_empty());
        assert!(config.root.is_some());
        assert!(config.root.as_ref().unwrap().exists());
    }

    #[test]
    fn test_ipfs_bootstrap_peers() {
        let peers = BootConfig::get_ipfs_bootstrap_peers();
        assert!(!peers.is_empty());

        // Check that all peers are valid multiaddrs
        for peer in peers {
            assert!(peer.to_string().contains("p2p/"));
        }
    }

    #[test]
    fn test_peer_id_extraction() {
        let temp_dir = TempDir::new().unwrap();
        let root_path = temp_dir.path().to_string_lossy().to_string();
        let config = BootConfig::new(root_path, "http://localhost:5001".to_string());

        // Use a valid peer ID from IPFS bootstrap peers
        let multiaddr: Multiaddr =
            "/ip4/127.0.0.1/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN"
                .parse()
                .unwrap();
        let peer_id = config.extract_peer_id(&multiaddr).unwrap();
        assert!(peer_id.is_some());

        let multiaddr_no_p2p: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().unwrap();
        let peer_id = config.extract_peer_id(&multiaddr_no_p2p).unwrap();
        assert!(peer_id.is_none());
    }

    #[test]
    fn test_multiaddr_content_parsing() {
        let content = "/ip4/127.0.0.1/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN\n/ip4/127.0.0.1/tcp/4002/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa\n";
        let multiaddrs = BootConfig::parse_multiaddrs_from_content(content);
        assert_eq!(multiaddrs.len(), 2);
    }

    #[tokio::test]
    async fn test_boot_peer_directory_parsing() {
        let temp_dir = TempDir::new().unwrap();
        let root_path = temp_dir.path().to_string_lossy().to_string();
        let config = BootConfig::new(root_path, "http://localhost:5001".to_string());

        // Create boot peers directory
        let peers_dir = temp_dir.path().join("boot").join("peers");
        fs::create_dir_all(&peers_dir).unwrap();

        // Create peer files with valid peer IDs
        fs::write(
            peers_dir.join("QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN"),
            "/ip4/127.0.0.1/tcp/4001\n/ip4/127.0.0.1/tcp/4002",
        )
        .unwrap();
        fs::write(
            peers_dir.join("QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa"),
            "/ip4/127.0.0.1/tcp/4003",
        )
        .unwrap();

        let multiaddrs = config.get_all_boot_peers().await.unwrap();
        // Should include IPFS bootstrap peers (6) + filesystem peers (3) = 9 total
        assert_eq!(multiaddrs.len(), 9);

        // Verify specific multiaddrs are present
        let multiaddr_strings: Vec<String> = multiaddrs.iter().map(|m| m.to_string()).collect();
        assert!(multiaddr_strings.contains(&"/ip4/127.0.0.1/tcp/4001".to_string()));
        assert!(multiaddr_strings.contains(&"/ip4/127.0.0.1/tcp/4002".to_string()));
        assert!(multiaddr_strings.contains(&"/ip4/127.0.0.1/tcp/4003".to_string()));
    }

    #[tokio::test]
    async fn test_boot_peer_file_parsing() {
        let temp_dir = TempDir::new().unwrap();
        let root_path = temp_dir.path().to_string_lossy().to_string();
        let config = BootConfig::new(root_path, "http://localhost:5001".to_string());

        // Create boot peers file (not directory)
        let peers_file = temp_dir.path().join("boot").join("peers");
        fs::create_dir_all(peers_file.parent().unwrap()).unwrap();
        fs::write(&peers_file, "/ip4/127.0.0.1/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN\n/ip4/127.0.0.1/tcp/4002/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa\n").unwrap();

        let multiaddrs = config.get_all_boot_peers().await.unwrap();
        // Should include IPFS bootstrap peers (6) + filesystem peers (2) = 8 total
        assert_eq!(multiaddrs.len(), 8);

        // Verify specific multiaddrs are present
        let multiaddr_strings: Vec<String> = multiaddrs.iter().map(|m| m.to_string()).collect();
        assert!(multiaddr_strings.contains(
            &"/ip4/127.0.0.1/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN"
                .to_string()
        ));
        assert!(multiaddr_strings.contains(
            &"/ip4/127.0.0.1/tcp/4002/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa"
                .to_string()
        ));
    }

    #[tokio::test]
    async fn test_boot_peer_ids_extraction() {
        let temp_dir = TempDir::new().unwrap();
        let root_path = temp_dir.path().to_string_lossy().to_string();
        let config = BootConfig::new(root_path, "http://localhost:5001".to_string());

        // Create boot peers directory
        let peers_dir = temp_dir.path().join("boot").join("peers");
        fs::create_dir_all(&peers_dir).unwrap();

        // Create peer files with valid peer IDs
        fs::write(
            peers_dir.join("QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN"),
            "/ip4/127.0.0.1/tcp/4001",
        )
        .unwrap();
        fs::write(
            peers_dir.join("QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa"),
            "/ip4/127.0.0.1/tcp/4002",
        )
        .unwrap();
        fs::write(peers_dir.join("invalid-peer"), "/ip4/127.0.0.1/tcp/4003").unwrap();

        // Test directory-based peer ID extraction (from filenames)
        let boot_peers = config.load_boot_peers().await.unwrap();
        assert_eq!(boot_peers.len(), 2); // Only valid peer IDs

        // Verify peer IDs are correct
        let peer_id_strings: Vec<String> = boot_peers.keys().map(|p| p.to_string()).collect();
        assert!(
            peer_id_strings.contains(&"QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN".to_string())
        );
        assert!(
            peer_id_strings.contains(&"QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa".to_string())
        );
        assert!(!peer_id_strings.contains(&"invalid-peer".to_string()));
    }

    #[tokio::test]
    async fn test_missing_boot_peers() {
        let temp_dir = TempDir::new().unwrap();
        let root_path = temp_dir.path().to_string_lossy().to_string();
        let config = BootConfig::new(root_path, "http://localhost:5001".to_string());

        // Test with non-existent boot peers directory
        let multiaddrs = config.get_all_boot_peers().await.unwrap();
        assert_eq!(multiaddrs.len(), 6); // Only IPFS bootstrap peers
    }

    #[test]
    fn test_invalid_multiaddr_parsing() {
        let content = "invalid_multiaddr\n/ip4/127.0.0.1/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN\n";
        let multiaddrs = BootConfig::parse_multiaddrs_from_content(content);
        assert_eq!(multiaddrs.len(), 1); // Only the valid one
    }

    #[test]
    fn test_path_sanitization() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Valid path should work
        let valid = BootConfig::sanitize_path(root, "boot/peers").unwrap();
        assert!(valid.ends_with("boot/peers"));

        // Path with .. should be rejected
        assert!(BootConfig::sanitize_path(root, "../etc/passwd").is_err());
        assert!(BootConfig::sanitize_path(root, "boot/../../etc/passwd").is_err());

        // Path with . should be normalized
        let normalized = BootConfig::sanitize_path(root, "./boot/./peers").unwrap();
        assert!(normalized.ends_with("boot/peers"));
    }

    #[test]
    fn test_ipfs_path_detection() {
        let config = BootConfig::new(
            "/ipfs/QmHash...".to_string(),
            "http://localhost:5001".to_string(),
        );
        assert!(config.ipfs_path.is_some());
        assert!(config.root.is_none());

        let config = BootConfig::new("./local".to_string(), "http://localhost:5001".to_string());
        assert!(config.ipfs_path.is_none());
        assert!(config.root.is_some());
    }
}
