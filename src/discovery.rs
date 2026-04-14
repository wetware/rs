//! Local node discovery via lockfiles and LAN Kademlia DHT.
//!
//! Running nodes write a lockfile to `~/.ww/run/<peer_id>` containing
//! their listen multiaddrs (one per line, transport-only — no `/p2p/`
//! suffix).  Clients read these files to discover local nodes without
//! network round-trips.
//!
//! The DHT discovery CID is also provided for LAN Kademlia advertisement.
#![cfg(not(target_arch = "wasm32"))]

use std::path::PathBuf;
use std::sync::LazyLock;

/// Well-known CID that wetware nodes provide on the LAN DHT.
///
/// Computed as `CIDv1(raw, BLAKE3(b"wetware"))`.  Any peer providing
/// this key is advertising itself as a wetware host.
pub static DISCOVERY_CID: LazyLock<cid::Cid> = LazyLock::new(|| {
    let digest = blake3::hash(b"wetware");
    let mh = cid::multihash::Multihash::<64>::wrap(0x1e, digest.as_bytes())
        .expect("blake3 digest always fits in 64-byte multihash");
    cid::Cid::new_v1(0x55, mh)
});

/// The discovery CID as a Kad record key (raw CID bytes).
pub fn discovery_record_key() -> libp2p::kad::RecordKey {
    libp2p::kad::RecordKey::new(&DISCOVERY_CID.to_bytes())
}

/// Directory where running nodes store their lockfiles.
pub fn run_dir() -> PathBuf {
    dirs::home_dir()
        .expect("home directory required")
        .join(".ww/run")
}

/// A running wetware node discovered via lockfile.
#[derive(Debug, Clone)]
pub struct LocalNode {
    pub peer_id: String,
    pub addrs: Vec<String>,
}

/// Write a lockfile for the current node.
///
/// Creates `~/.ww/run/<peer_id>` with one multiaddr per line.
/// The addrs should be transport-only (no `/p2p/<peer_id>` suffix).
pub fn write_lockfile(peer_id: &str, addrs: &[String]) -> std::io::Result<PathBuf> {
    let dir = run_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(peer_id);
    let contents = addrs.join("\n");
    std::fs::write(&path, contents)?;
    Ok(path)
}

/// Remove the lockfile for the given peer.
pub fn remove_lockfile(peer_id: &str) {
    let path = run_dir().join(peer_id);
    let _ = std::fs::remove_file(path);
}

/// List all locally running wetware nodes by reading lockfiles.
///
/// Returns nodes whose lockfiles exist in `~/.ww/run/`.
/// Does not validate whether the nodes are actually reachable.
pub fn list_local_nodes() -> Vec<LocalNode> {
    let dir = run_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };

    let mut nodes = Vec::new();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let peer_id = file_name.to_string_lossy().to_string();

        // Skip dotfiles, temp files, etc.
        if peer_id.starts_with('.') {
            continue;
        }

        if let Ok(contents) = std::fs::read_to_string(entry.path()) {
            let addrs: Vec<String> = contents
                .lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect();

            if !addrs.is_empty() {
                nodes.push(LocalNode { peer_id, addrs });
            }
        }
    }

    nodes
}
