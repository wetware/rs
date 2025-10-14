use anyhow::{anyhow, Result};
use libp2p::{Multiaddr, PeerId};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, debug, warn};

/// Boot peer discovery and DHT bootstrap configuration
pub struct BootConfig {
    /// Root directory containing `/ww/` versions
    pub root: PathBuf,
    /// IPFS bootstrap peers (standard IPFS public nodes)
    pub ipfs_bootstrap_peers: Vec<Multiaddr>,
}

impl BootConfig {
    /// Create a new boot configuration
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            ipfs_bootstrap_peers: Self::get_ipfs_bootstrap_peers(),
        }
    }

    /// Get standard IPFS bootstrap peers
    fn get_ipfs_bootstrap_peers() -> Vec<Multiaddr> {
        vec![
            "/dnsaddr/bootstrap.libp2p.io/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN".parse().unwrap(),
            "/dnsaddr/bootstrap.libp2p.io/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa".parse().unwrap(),
            "/dnsaddr/bootstrap.libp2p.io/p2p/QmbLHAnMoJPWSCR5ZphtHh5F5FjGp6YhjaQ1VyaeoLjuXm".parse().unwrap(),
            "/dnsaddr/bootstrap.libp2p.io/p2p/QmcZf59bWwK5XFi76CZX8cbJ4BhTzzA3gU1ZjYZcYW3dwt".parse().unwrap(),
            "/ip4/104.131.131.82/tcp/4001/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ".parse().unwrap(),
            "/ip4/104.131.131.82/udp/4001/quic/p2p/QmaCpDMGvV2BGHeYERUEnRQAwe3N8SzbUtfsmvsqQLuvuJ".parse().unwrap(),
        ]
    }

    /// Get the path to the boot peers directory for a given version
    pub fn get_boot_peers_dir(&self, version: &str) -> PathBuf {
        self.root
            .join("ww")
            .join(version)
            .join("boot")
            .join("peers")
    }

    /// Load boot peers from filesystem for a specific version
    pub fn load_boot_peers(&self, version: &str) -> Result<HashMap<PeerId, Vec<Multiaddr>>> {
        let boot_peers_dir = self.get_boot_peers_dir(version);

        debug!(boot_peers_dir = %boot_peers_dir.display(), version = %version, "Loading boot peers");

        if !boot_peers_dir.exists() {
            info!(version = %version, "No boot peers directory found");
            return Ok(HashMap::new());
        }

        let mut boot_peers = HashMap::new();

        // Check if it's a directory or a single file
        if boot_peers_dir.is_dir() {
            // Directory mode: each file = peer ID (filename), contents = multiaddrs
            for entry in fs::read_dir(&boot_peers_dir)? {
                let entry = entry?;
                let path = entry.path();
                
                if path.is_file() {
                    let peer_id_str = path.file_stem()
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| anyhow!("Invalid peer ID filename: {}", path.display()))?;
                    
                    // Try to parse peer ID, skip if invalid
                    if let Ok(peer_id) = peer_id_str.parse::<PeerId>() {
                        let multiaddrs = self.parse_multiaddrs_file(&path)?;
                        boot_peers.insert(peer_id, multiaddrs);
                    } else {
                        warn!(peer_id = %peer_id_str, "Skipping invalid peer ID filename");
                    }
                }
            }
        } else if boot_peers_dir.is_file() {
            // File mode: newline-separated full multiaddrs
            let multiaddrs = self.parse_multiaddrs_file(&boot_peers_dir)?;
            
            // Extract peer IDs from multiaddrs
            for multiaddr in multiaddrs {
                if let Some(peer_id) = self.extract_peer_id(&multiaddr)? {
                    boot_peers.entry(peer_id).or_insert_with(Vec::new).push(multiaddr);
                }
            }
        }

        info!(version = %version, peer_count = boot_peers.len(), "Loaded boot peers");
        Ok(boot_peers)
    }

    /// Parse a file containing newline-separated multiaddrs
    fn parse_multiaddrs_file(&self, path: &Path) -> Result<Vec<Multiaddr>> {
        let content = fs::read_to_string(path)?;
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
        
        debug!(path = %path.display(), count = multiaddrs.len(), "Parsed multiaddrs from file");
        Ok(multiaddrs)
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

    /// Get all boot peers (IPFS + version-specific)
    pub fn get_all_boot_peers(&self, version: &str) -> Result<Vec<Multiaddr>> {
        let mut all_peers = Vec::new();
        
        // Add IPFS bootstrap peers
        all_peers.extend(self.ipfs_bootstrap_peers.clone());
        
        // Add version-specific boot peers
        let version_peers = self.load_boot_peers(version)?;
        for multiaddrs in version_peers.values() {
            all_peers.extend(multiaddrs.clone());
        }
        
        info!(version = %version, total_peers = all_peers.len(), "Collected all boot peers");
        Ok(all_peers)
    }

    /// Get unique peer IDs from all boot peers
    pub fn get_boot_peer_ids(&self, version: &str) -> Result<Vec<PeerId>> {
        let multiaddrs = self.get_all_boot_peers(version)?;
        let mut peer_ids = Vec::new();
        
        for multiaddr in multiaddrs {
            if let Some(peer_id) = self.extract_peer_id(&multiaddr)? {
                if !peer_ids.contains(&peer_id) {
                    peer_ids.push(peer_id);
                }
            }
        }
        
        info!(version = %version, unique_peers = peer_ids.len(), "Extracted unique peer IDs");
        Ok(peer_ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_boot_config_creation() {
        let temp_dir = TempDir::new().unwrap();
        let config = BootConfig::new(temp_dir.path().to_path_buf());
        
        assert!(!config.ipfs_bootstrap_peers.is_empty());
        assert!(config.root.exists());
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
        let config = BootConfig::new(temp_dir.path().to_path_buf());
        
        // Use a valid peer ID from IPFS bootstrap peers
        let multiaddr: Multiaddr = "/ip4/127.0.0.1/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN".parse().unwrap();
        let peer_id = config.extract_peer_id(&multiaddr).unwrap();
        assert!(peer_id.is_some());
        
        let multiaddr_no_p2p: Multiaddr = "/ip4/127.0.0.1/tcp/4001".parse().unwrap();
        let peer_id = config.extract_peer_id(&multiaddr_no_p2p).unwrap();
        assert!(peer_id.is_none());
    }

    #[test]
    fn test_multiaddr_file_parsing() {
        let temp_dir = TempDir::new().unwrap();
        let config = BootConfig::new(temp_dir.path().to_path_buf());
        
        // Create test file with valid peer IDs
        let test_file = temp_dir.path().join("peers.txt");
        fs::write(&test_file, "/ip4/127.0.0.1/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN\n/ip4/127.0.0.1/tcp/4002/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa\n").unwrap();
        
        let multiaddrs = config.parse_multiaddrs_file(&test_file).unwrap();
        assert_eq!(multiaddrs.len(), 2);
    }
    
    #[test]
    fn test_boot_peer_directory_parsing() {
        let temp_dir = TempDir::new().unwrap();
        let config = BootConfig::new(temp_dir.path().to_path_buf());
        let version = "0.1.0";
        
        // Create boot peers directory
        let peers_dir = config.get_boot_peers_dir(version);
        fs::create_dir_all(&peers_dir).unwrap();
        
        // Create peer files with valid peer IDs
        fs::write(peers_dir.join("QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN"), "/ip4/127.0.0.1/tcp/4001\n/ip4/127.0.0.1/tcp/4002").unwrap();
        fs::write(peers_dir.join("QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa"), "/ip4/127.0.0.1/tcp/4003").unwrap();
        
        let multiaddrs = config.get_all_boot_peers(version).unwrap();
        // Should include IPFS bootstrap peers (6) + version-specific peers (3) = 9 total
        assert_eq!(multiaddrs.len(), 9);
        
        // Verify specific multiaddrs are present
        let multiaddr_strings: Vec<String> = multiaddrs.iter().map(|m| m.to_string()).collect();
        assert!(multiaddr_strings.contains(&"/ip4/127.0.0.1/tcp/4001".to_string()));
        assert!(multiaddr_strings.contains(&"/ip4/127.0.0.1/tcp/4002".to_string()));
        assert!(multiaddr_strings.contains(&"/ip4/127.0.0.1/tcp/4003".to_string()));
    }
    
    #[test]
    fn test_boot_peer_file_parsing() {
        let temp_dir = TempDir::new().unwrap();
        let config = BootConfig::new(temp_dir.path().to_path_buf());
        let version = "0.1.0";
        
        // Create boot peers file (not directory)
        let peers_file = config.get_boot_peers_dir(version);
        fs::create_dir_all(peers_file.parent().unwrap()).unwrap();
        fs::write(&peers_file, "/ip4/127.0.0.1/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN\n/ip4/127.0.0.1/tcp/4002/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa\n").unwrap();
        
        let multiaddrs = config.get_all_boot_peers(version).unwrap();
        // Should include IPFS bootstrap peers (6) + version-specific peers (2) = 8 total
        assert_eq!(multiaddrs.len(), 8);
        
        // Verify specific multiaddrs are present
        let multiaddr_strings: Vec<String> = multiaddrs.iter().map(|m| m.to_string()).collect();
        assert!(multiaddr_strings.contains(&"/ip4/127.0.0.1/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN".to_string()));
        assert!(multiaddr_strings.contains(&"/ip4/127.0.0.1/tcp/4002/p2p/QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa".to_string()));
    }
    
    #[test]
    fn test_boot_peer_ids_extraction() {
        let temp_dir = TempDir::new().unwrap();
        let config = BootConfig::new(temp_dir.path().to_path_buf());
        let version = "0.1.0";
        
        // Create boot peers directory
        let peers_dir = config.get_boot_peers_dir(version);
        fs::create_dir_all(&peers_dir).unwrap();
        
        // Create peer files with valid peer IDs
        fs::write(peers_dir.join("QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN"), "/ip4/127.0.0.1/tcp/4001").unwrap();
        fs::write(peers_dir.join("QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa"), "/ip4/127.0.0.1/tcp/4002").unwrap();
        fs::write(peers_dir.join("invalid-peer"), "/ip4/127.0.0.1/tcp/4003").unwrap();
        
        // Test directory-based peer ID extraction (from filenames)
        let boot_peers = config.load_boot_peers(version).unwrap();
        assert_eq!(boot_peers.len(), 2); // Only valid peer IDs
        
        // Verify peer IDs are correct
        let peer_id_strings: Vec<String> = boot_peers.keys().map(|p| p.to_string()).collect();
        assert!(peer_id_strings.contains(&"QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN".to_string()));
        assert!(peer_id_strings.contains(&"QmQCU2EcMqAqQPR2i9bChDtGNJchTbq5TbXJJ16u19uLTa".to_string()));
        assert!(!peer_id_strings.contains(&"invalid-peer".to_string()));
    }
    
    #[test]
    fn test_missing_boot_peers() {
        let temp_dir = TempDir::new().unwrap();
        let config = BootConfig::new(temp_dir.path().to_path_buf());
        let version = "0.1.0";
        
        // Test with non-existent boot peers directory
        let multiaddrs = config.get_all_boot_peers(version).unwrap();
        assert_eq!(multiaddrs.len(), 6); // Only IPFS bootstrap peers
        
        let peer_ids = config.get_boot_peer_ids(version).unwrap();
        // Some IPFS bootstrap peers may not have extractable peer IDs
        assert!(peer_ids.len() >= 5); // At least 5 should have peer IDs
    }
    
    #[test]
    fn test_invalid_multiaddr_parsing() {
        let temp_dir = TempDir::new().unwrap();
        let config = BootConfig::new(temp_dir.path().to_path_buf());
        
        // Create test file with invalid multiaddrs
        let test_file = temp_dir.path().join("invalid_peers.txt");
        fs::write(&test_file, "invalid_multiaddr\n/ip4/127.0.0.1/tcp/4001/p2p/QmNnooDu7bfjPFoTZYxMNLWUQJyrVwtbZg5gBMjTezGAJN\n").unwrap();
        
        // Should succeed but only return valid multiaddrs
        let result = config.parse_multiaddrs_file(&test_file);
        assert!(result.is_ok());
        let multiaddrs = result.unwrap();
        assert_eq!(multiaddrs.len(), 1); // Only the valid one
    }
}