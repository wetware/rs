//! Unified mount-based FHS image resolution.
//!
//! Every positional arg to `ww run` is a mount: `source[:target]`.
//! Root mounts (target `/`) are traditional image layers. Targeted
//! mounts overlay a host file or directory at a specific guest path.
//!
//! Mounts are applied left-to-right. Later mounts win on file
//! conflicts. Directories merge. No deletes.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use cid::Cid;
use tempfile::TempDir;
use walkdir::WalkDir;

use crate::ipfs;
use crate::mount::Mount;

/// A merged FHS image root on the host filesystem.
///
/// Holds either a direct reference to a single local directory (no copy)
/// or a temp directory containing the union of all mounts.
/// The temp directory is cleaned up on drop.
pub struct MergedImage {
    root: ImageRoot,
}

impl std::fmt::Debug for MergedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MergedImage")
            .field("path", &self.path())
            .finish()
    }
}

enum ImageRoot {
    /// Single local root mount — use the original path directly.
    Direct(PathBuf),
    /// Multiple mounts materialized in a temp directory.
    Merged(TempDir),
}

impl MergedImage {
    /// Path to the merged FHS root on the host filesystem.
    pub fn path(&self) -> &Path {
        match &self.root {
            ImageRoot::Direct(p) => p.as_path(),
            ImageRoot::Merged(t) => t.path(),
        }
    }
}

/// Apply mounts to produce a merged FHS root.
///
/// Each mount is `source[:target]`. Root mounts (target `/`) act as
/// traditional image layers. Targeted mounts overlay at a specific path.
///
/// Mounts are applied left-to-right: later mounts win on file conflicts.
/// The merged root must contain `bin/main.wasm` after all mounts.
pub async fn apply_mounts(
    mounts: &[Mount],
    ipfs_client: &ipfs::HttpClient,
) -> Result<MergedImage> {
    if mounts.is_empty() {
        bail!("No mounts provided");
    }

    // Optimization: single local root mount — skip the temp dir.
    if mounts.len() == 1 && mounts[0].is_root() && !ipfs::is_ipfs_path(&mounts[0].source) {
        let path = PathBuf::from(&mounts[0].source);
        if !path.exists() {
            bail!("Image path does not exist: {}", mounts[0].source);
        }
        let main_wasm = path.join("bin/main.wasm");
        if !main_wasm.exists() {
            bail!("Image missing bin/main.wasm: {}", path.display());
        }
        return Ok(MergedImage {
            root: ImageRoot::Direct(path),
        });
    }

    // Multiple mounts or non-trivial source — materialize into a temp dir.
    let merged_dir = TempDir::new().context("Failed to create temp dir for merged image")?;

    for (i, mount) in mounts.iter().enumerate() {
        let dst = resolve_target(merged_dir.path(), &mount.target);

        if ipfs::is_ipfs_path(&mount.source) {
            apply_ipfs_layer(&mount.source, &dst, ipfs_client)
                .await
                .with_context(|| format!("Failed to apply IPFS mount {i}: {}", mount.source))?;
        } else {
            let src = Path::new(&mount.source);
            if !src.exists() {
                bail!("Mount source does not exist: {}", mount.source);
            }
            apply_local_mount(src, &dst)
                .with_context(|| format!("Failed to apply mount {i}: {}", mount.source))?;
        }
    }

    // Verify the merged root has bin/main.wasm.
    let main_wasm = merged_dir.path().join("bin/main.wasm");
    if !main_wasm.exists() {
        bail!(
            "Merged image missing bin/main.wasm (from {} mounts)",
            mounts.len()
        );
    }

    Ok(MergedImage {
        root: ImageRoot::Merged(merged_dir),
    })
}

/// Resolve a mount target to a host path within the merged root.
///
/// `/etc/identity` → `merged_root/etc/identity`
/// `/` → `merged_root`
fn resolve_target(merged_root: &Path, target: &Path) -> PathBuf {
    let stripped = target.strip_prefix("/").unwrap_or(target);
    if stripped == Path::new("") {
        merged_root.to_path_buf()
    } else {
        merged_root.join(stripped)
    }
}

/// Apply a local mount source to a destination.
///
/// If the source is a directory, recursively copies all files (union merge).
/// If the source is a file, copies the single file (creating parent dirs).
fn apply_local_mount(src: &Path, dst: &Path) -> Result<()> {
    if src.is_dir() {
        apply_local_layer(src, dst)
    } else {
        // Single file mount — copy to exact destination path.
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create parent dir: {}", parent.display()))?;
        }
        std::fs::copy(src, dst).with_context(|| {
            format!("Failed to copy {} -> {}", src.display(), dst.display())
        })?;
        Ok(())
    }
}

/// Apply a local filesystem layer to the merged root.
///
/// Walks the source directory recursively, copying files into the
/// destination. Existing files are overwritten (later layer wins).
fn apply_local_layer(src: &Path, dst: &Path) -> Result<()> {
    for entry in WalkDir::new(src).min_depth(1) {
        let entry = entry.with_context(|| format!("Error walking {}", src.display()))?;
        let relative = entry
            .path()
            .strip_prefix(src)
            .expect("walkdir entry must be under src");
        let target = dst.join(relative);

        if entry.file_type().is_dir() {
            std::fs::create_dir_all(&target)
                .with_context(|| format!("Failed to create dir: {}", target.display()))?;
        } else {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(entry.path(), &target).with_context(|| {
                format!(
                    "Failed to copy {} -> {}",
                    entry.path().display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

/// Apply an IPFS layer to the merged root.
///
/// Fetches the directory as a TAR archive via kubo `/api/v0/get`
/// and extracts it, stripping the top-level CID directory.
async fn apply_ipfs_layer(ipfs_path: &str, dst: &Path, client: &ipfs::HttpClient) -> Result<()> {
    client.unixfs_ref().get_dir(ipfs_path, dst).await
}

// ── Keep the old API as a thin wrapper for backward compatibility ──

/// Resolve and merge N image layers into a single FHS root.
///
/// Deprecated: prefer `apply_mounts()` which supports targeted mounts.
/// This wrapper treats each layer string as a root mount (`source:/`).
pub async fn merge_layers(
    layers: &[String],
    ipfs_client: &ipfs::HttpClient,
) -> Result<MergedImage> {
    let mounts: Vec<Mount> = layers
        .iter()
        .map(|s| Mount {
            source: s.clone(),
            target: PathBuf::from("/"),
        })
        .collect();
    apply_mounts(&mounts, ipfs_client).await
}

/// Read the current head from an Atom contract via one-shot `eth_call`.
///
/// Returns `CurrentHead { seq, cid }` where `cid` is raw binary bytes
/// from the contract's `head()` view function.
pub async fn read_contract_head(rpc_url: &str, contract: &[u8; 20]) -> Result<atom::CurrentHead> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .build()
        .context("Failed to build HTTP client")?;

    let params = serde_json::json!([{
        "to": format!("0x{}", hex::encode(contract)),
        "data": format!("0x{}", hex::encode(atom::abi::HEAD_SELECTOR)),
    }, "latest"]);

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_call",
        "params": params,
    });

    let resp = client
        .post(rpc_url)
        .json(&body)
        .send()
        .await
        .context("eth_call request failed")?;

    let json: serde_json::Value = resp.json().await.context("Failed to parse RPC response")?;

    if let Some(err) = json.get("error") {
        bail!("RPC error: {err}");
    }

    let result_str = json
        .get("result")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Missing result in RPC response"))?;

    let bytes = hex::decode(result_str.strip_prefix("0x").unwrap_or(result_str))
        .context("Failed to decode hex from eth_call result")?;

    atom::abi::decode_head_return(&bytes).context("Failed to decode head() return data")
}

/// Convert raw binary CID bytes to an IPFS path string.
///
/// CIDv0 renders as `/ipfs/Qm...` (base58btc), CIDv1 as `/ipfs/bafy...` (base32lower).
pub fn cid_bytes_to_ipfs_path(cid_bytes: &[u8]) -> Result<String> {
    if cid_bytes.is_empty() {
        bail!("Empty CID bytes");
    }
    let cid = Cid::read_bytes(cid_bytes).context("Failed to parse CID from bytes")?;
    Ok(format!("/ipfs/{cid}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// Helper: create a temp dir with an FHS image layout.
    fn make_layer(files: &[(&str, &[u8])]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for (path, content) in files {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(&full, content).unwrap();
        }
        dir
    }

    fn stub_ipfs_client() -> ipfs::HttpClient {
        ipfs::HttpClient::new("http://localhost:5001".into())
    }

    fn root_mount(path: &str) -> Mount {
        Mount {
            source: path.to_string(),
            target: PathBuf::from("/"),
        }
    }

    fn targeted_mount(source: &str, target: &str) -> Mount {
        Mount {
            source: source.to_string(),
            target: PathBuf::from(target),
        }
    }

    // ── apply_mounts tests (new API) ──

    #[tokio::test]
    async fn test_single_root_mount_direct() {
        let layer = make_layer(&[("bin/main.wasm", b"(wasm)")]);
        let client = stub_ipfs_client();
        let merged = apply_mounts(
            &[root_mount(&layer.path().to_string_lossy())],
            &client,
        )
        .await
        .unwrap();
        // Single local root mount should use direct reference (no copy).
        assert!(merged.path().join("bin/main.wasm").exists());
        assert_eq!(merged.path(), layer.path());
    }

    #[tokio::test]
    async fn test_root_mount_plus_targeted_file() {
        let layer = make_layer(&[("bin/main.wasm", b"(wasm)")]);
        // Create a host file to mount.
        let host_file = TempDir::new().unwrap();
        let identity_path = host_file.path().join("my-key");
        fs::write(&identity_path, b"secret-key-data").unwrap();

        let client = stub_ipfs_client();
        let merged = apply_mounts(
            &[
                root_mount(&layer.path().to_string_lossy()),
                targeted_mount(
                    &identity_path.to_string_lossy(),
                    "/etc/identity",
                ),
            ],
            &client,
        )
        .await
        .unwrap();

        assert!(merged.path().join("bin/main.wasm").exists());
        let identity = fs::read_to_string(merged.path().join("etc/identity")).unwrap();
        assert_eq!(identity, "secret-key-data");
    }

    #[tokio::test]
    async fn test_root_mount_plus_targeted_dir() {
        let layer = make_layer(&[("bin/main.wasm", b"(wasm)")]);
        let data_dir = make_layer(&[("file-a.txt", b"aaa"), ("sub/file-b.txt", b"bbb")]);

        let client = stub_ipfs_client();
        let merged = apply_mounts(
            &[
                root_mount(&layer.path().to_string_lossy()),
                targeted_mount(
                    &data_dir.path().to_string_lossy(),
                    "/var/data",
                ),
            ],
            &client,
        )
        .await
        .unwrap();

        assert!(merged.path().join("bin/main.wasm").exists());
        let a = fs::read_to_string(merged.path().join("var/data/file-a.txt")).unwrap();
        assert_eq!(a, "aaa");
        let b = fs::read_to_string(merged.path().join("var/data/sub/file-b.txt")).unwrap();
        assert_eq!(b, "bbb");
    }

    #[tokio::test]
    async fn test_targeted_mount_overrides_layer_file() {
        let layer = make_layer(&[
            ("bin/main.wasm", b"(wasm)"),
            ("etc/identity", b"old-key"),
        ]);
        let host_file = TempDir::new().unwrap();
        let new_key = host_file.path().join("new-key");
        fs::write(&new_key, b"new-key-data").unwrap();

        let client = stub_ipfs_client();
        let merged = apply_mounts(
            &[
                root_mount(&layer.path().to_string_lossy()),
                targeted_mount(&new_key.to_string_lossy(), "/etc/identity"),
            ],
            &client,
        )
        .await
        .unwrap();

        let identity = fs::read_to_string(merged.path().join("etc/identity")).unwrap();
        assert_eq!(identity, "new-key-data");
    }

    // ── merge_layers backward compat tests ──

    #[tokio::test]
    async fn test_merge_single_local_layer() {
        let layer = make_layer(&[("bin/main.wasm", b"(wasm)")]);
        let client = stub_ipfs_client();
        let merged = merge_layers(&[layer.path().to_string_lossy().into()], &client)
            .await
            .unwrap();
        assert!(merged.path().join("bin/main.wasm").exists());
        assert_eq!(merged.path(), layer.path());
    }

    #[tokio::test]
    async fn test_merge_two_layers_file_override() {
        let base = make_layer(&[
            ("bin/main.wasm", b"base-wasm"),
            ("etc/config", b"base-config"),
        ]);
        let overlay = make_layer(&[("etc/config", b"overlay-config")]);
        let client = stub_ipfs_client();

        let merged = merge_layers(
            &[
                base.path().to_string_lossy().into(),
                overlay.path().to_string_lossy().into(),
            ],
            &client,
        )
        .await
        .unwrap();

        assert!(merged.path().join("bin/main.wasm").exists());
        let config = fs::read_to_string(merged.path().join("etc/config")).unwrap();
        assert_eq!(config, "overlay-config");
    }

    #[tokio::test]
    async fn test_merge_directories_merge() {
        let base = make_layer(&[("bin/main.wasm", b"(wasm)"), ("boot/QmPeerA", b"addr-a")]);
        let overlay = make_layer(&[("boot/QmPeerB", b"addr-b")]);
        let client = stub_ipfs_client();

        let merged = merge_layers(
            &[
                base.path().to_string_lossy().into(),
                overlay.path().to_string_lossy().into(),
            ],
            &client,
        )
        .await
        .unwrap();

        assert!(merged.path().join("boot/QmPeerA").exists());
        assert!(merged.path().join("boot/QmPeerB").exists());
    }

    #[tokio::test]
    async fn test_merge_missing_main_wasm_errors() {
        let base = make_layer(&[("etc/config", b"data")]);
        let overlay = make_layer(&[("boot/QmPeer", b"addr")]);
        let client = stub_ipfs_client();

        let result = merge_layers(
            &[
                base.path().to_string_lossy().into(),
                overlay.path().to_string_lossy().into(),
            ],
            &client,
        )
        .await;

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("missing bin/main.wasm"));
    }

    #[tokio::test]
    async fn test_merge_empty_layers_errors() {
        let client = stub_ipfs_client();
        let result = merge_layers(&[], &client).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No mounts"));
    }

    #[tokio::test]
    async fn test_merge_nonexistent_path_errors() {
        let client = stub_ipfs_client();
        let result = merge_layers(&["/nonexistent/path/abc123".into()], &client).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_cid_bytes_to_ipfs_path_v0() {
        let mut cid_bytes = vec![0x12, 0x20];
        cid_bytes.extend_from_slice(&[0xAB; 32]);
        let path = cid_bytes_to_ipfs_path(&cid_bytes).unwrap();
        assert!(
            path.starts_with("/ipfs/Qm"),
            "CIDv0 should start with /ipfs/Qm, got: {path}"
        );
    }

    #[test]
    fn test_cid_bytes_to_ipfs_path_v1() {
        let mut mh_bytes = vec![0x12, 0x20];
        mh_bytes.extend_from_slice(&[0xAB; 32]);
        let mh = cid::multihash::Multihash::from_bytes(&mh_bytes).unwrap();
        let cid = Cid::new_v1(0x70, mh);
        let cid_bytes = cid.to_bytes();
        let path = cid_bytes_to_ipfs_path(&cid_bytes).unwrap();
        assert!(
            path.starts_with("/ipfs/bafy"),
            "CIDv1 should start with /ipfs/bafy, got: {path}"
        );
    }

    #[test]
    fn test_cid_bytes_to_ipfs_path_empty_errors() {
        let result = cid_bytes_to_ipfs_path(&[]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Empty CID bytes"));
    }
}
