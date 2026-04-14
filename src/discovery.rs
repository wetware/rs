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

/// Candidate directories where running nodes may store their lockfiles,
/// in priority order:
///
/// 1. `/var/run/ww/` — system-wide, discoverable across users (preferred
///    on bare metal where the daemon can write to `/var/run/`).
/// 2. `$HOME/.ww/run/` — per-user fallback (used in containers, sandboxes,
///    or anywhere `/var/run/` is not writable).
///
/// The writer picks the first writable directory. The reader scans all
/// candidates and merges results.
pub fn run_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::from("/var/run/ww")];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".ww/run"));
    }
    dirs
}

/// Primary directory for *writing* lockfiles. Picks the first candidate
/// that exists or can be created with write access. Falls back to the
/// last candidate if none are writable (the writer will then log the
/// failure and move on).
fn writable_run_dir() -> PathBuf {
    let candidates = run_dirs();
    for dir in &candidates {
        if std::fs::create_dir_all(dir).is_ok() {
            // Probe write access by creating and removing a temp file.
            let probe = dir.join(".write-probe");
            if std::fs::write(&probe, b"").is_ok() {
                let _ = std::fs::remove_file(&probe);
                return dir.clone();
            }
        }
    }
    candidates
        .into_iter()
        .last()
        .unwrap_or_else(|| PathBuf::from("/var/run/ww"))
}

/// A running wetware node discovered via lockfile.
#[derive(Debug, Clone)]
pub struct LocalNode {
    pub peer_id: String,
    pub addrs: Vec<String>,
}

/// Write a lockfile for the current node.
///
/// Picks the first writable directory from `run_dirs()` and creates
/// `<dir>/<peer_id>` with one multiaddr per line. The addrs should be
/// transport-only (no `/p2p/<peer_id>` suffix).
///
/// On a system-wide directory (e.g. `/var/run/ww/`), the sticky bit is
/// set so users can only remove their own lockfiles.
pub fn write_lockfile(peer_id: &str, addrs: &[String]) -> std::io::Result<PathBuf> {
    let dir = writable_run_dir();
    std::fs::create_dir_all(&dir)?;
    // If this is a shared system directory, set sticky bit so users can
    // only delete their own lockfiles. Best-effort; ignore failures.
    #[cfg(unix)]
    if dir.starts_with("/var/run") || dir.starts_with("/run") {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o1777));
    }
    let path = dir.join(peer_id);
    let contents = addrs.join("\n");
    std::fs::write(&path, contents)?;
    Ok(path)
}

/// Remove the lockfile for the given peer from every candidate directory.
pub fn remove_lockfile(peer_id: &str) {
    for dir in run_dirs() {
        let _ = std::fs::remove_file(dir.join(peer_id));
    }
}

/// List all locally running wetware nodes by reading lockfiles from
/// every candidate directory. Duplicate peer IDs across directories are
/// deduplicated (first occurrence wins).
///
/// Does not validate whether the nodes are actually reachable.
pub fn list_local_nodes() -> Vec<LocalNode> {
    use std::collections::HashSet;

    let mut nodes = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for dir in run_dirs() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let peer_id = file_name.to_string_lossy().to_string();

            // Skip dotfiles, temp files, etc.
            if peer_id.starts_with('.') {
                continue;
            }
            if !seen.insert(peer_id.clone()) {
                continue; // already saw this peer in a higher-priority dir
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
    }

    nodes
}
