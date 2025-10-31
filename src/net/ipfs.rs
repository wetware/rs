/*!
Networking helpers for IPFS.

This module centralizes IPFS-related configuration that is needed by runtime
components (e.g., cell executor) without depending on the CLI layer.
*/

/// Default Kubo (go-ipfs) HTTP API endpoint used when `WW_IPFS` is not set.
pub const DEFAULT_IPFS_URL: &str = "http://localhost:5001";

/// Resolve the IPFS HTTP API endpoint from the environment.
///
/// Uses the `WW_IPFS` environment variable if set; otherwise falls back
/// to `DEFAULT_IPFS_URL`.
///
/// Returns:
/// - A `String` containing the IPFS HTTP API base URL (e.g., "http://127.0.0.1:5001")
pub fn get_ipfs_url() -> String {
    std::env::var("WW_IPFS").unwrap_or_else(|_| DEFAULT_IPFS_URL.to_string())
}
