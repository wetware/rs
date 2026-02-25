//! Unified mount-based FHS image resolution.
//!
//! Every positional arg to `ww run` is a mount: `source[:target]`.
//! Root mounts (target `/`) are traditional image layers. Targeted
//! mounts overlay a host file or directory at a specific guest path.
//!
//! Mounts are applied left-to-right. Later mounts win on file
//! conflicts. Directories merge. No deletes.
//!
//! When IPFS is available, root layers are merged at the UnixFS DAG
//! level via MFS — file blocks are never touched, only directory nodes
//! get new CIDs. Falls back to copy-merge on MFS failure.

use std::collections::HashSet;
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
///
/// Root layers are merged via IPFS MFS DAG operations when possible,
/// falling back to copy-merge on failure.
pub async fn apply_mounts(
    mounts: &[Mount],
    ipfs_client: &ipfs::HttpClient,
) -> Result<MergedImage> {
    if mounts.is_empty() {
        bail!("No mounts provided");
    }

    // Partition into root mounts and targeted mounts.
    let (root_mounts, targeted_mounts): (Vec<&Mount>, Vec<&Mount>) =
        mounts.iter().partition(|m| m.is_root());

    // Optimization: single local root mount with no targeted mounts.
    if root_mounts.len() == 1
        && targeted_mounts.is_empty()
        && !ipfs::is_ipfs_path(&root_mounts[0].source)
    {
        let path = PathBuf::from(&root_mounts[0].source);
        if !path.exists() {
            bail!("Image path does not exist: {}", root_mounts[0].source);
        }
        let main_wasm = path.join("bin/main.wasm");
        if !main_wasm.exists() {
            bail!("Image missing bin/main.wasm: {}", path.display());
        }
        return Ok(MergedImage {
            root: ImageRoot::Direct(path),
        });
    }

    // Materialize into a temp dir.
    let merged_dir = TempDir::new().context("Failed to create temp dir for merged image")?;

    // Try DAG merge; fall back to copy-merge on failure.
    match try_dag_merge(&root_mounts, ipfs_client, merged_dir.path()).await {
        Ok(()) => {}
        Err(e) => {
            tracing::warn!("DAG merge failed, falling back to copy-merge: {e}");
            copy_merge(&root_mounts, ipfs_client, merged_dir.path()).await?;
        }
    }

    // Apply targeted mounts (always local overlays on top of merged root).
    for mount in &targeted_mounts {
        let dst = resolve_target(merged_dir.path(), &mount.target);
        if ipfs::is_ipfs_path(&mount.source) {
            apply_ipfs_layer(&mount.source, &dst, ipfs_client).await?;
        } else {
            let src = Path::new(&mount.source);
            if !src.exists() {
                bail!("Mount source does not exist: {}", mount.source);
            }
            apply_local_mount(src, &dst)?;
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

// ── DAG merge via IPFS MFS ─────────────────────────────────────────

/// RAII guard that cleans up an MFS namespace on drop.
struct MfsNamespaceGuard<'a> {
    client: &'a ipfs::HttpClient,
    path: String,
}

impl<'a> MfsNamespaceGuard<'a> {
    async fn new(client: &'a ipfs::HttpClient) -> Result<Self> {
        let id: u64 = rand::random();
        let path = format!("/ww-merge-{id:016x}");
        // Don't pre-create the directory — files_cp will create it when
        // copying the base layer, and fails if the destination already exists.
        Ok(Self { client, path })
    }

    fn path(&self) -> &str {
        &self.path
    }

    async fn cleanup(&self) {
        if let Err(e) = self.client.mfs().files_rm(&self.path, true).await {
            tracing::warn!(path = %self.path, "MFS cleanup failed: {e}");
        }
    }
}

/// Attempt DAG merge: resolve layers to CIDs, merge via MFS, materialize.
async fn try_dag_merge(
    root_mounts: &[&Mount],
    client: &ipfs::HttpClient,
    dst: &Path,
) -> Result<()> {
    // Resolve all root mounts to CIDs.
    let mut cids = Vec::with_capacity(root_mounts.len());
    for mount in root_mounts {
        if ipfs::is_ipfs_path(&mount.source) {
            let cid = mount
                .source
                .strip_prefix("/ipfs/")
                .context("IPFS path must start with /ipfs/")?;
            cids.push(cid.to_string());
        } else {
            // Add local directory to IPFS.
            let cid = client
                .add_dir(Path::new(&mount.source))
                .await
                .with_context(|| {
                    format!("Failed to add local layer to IPFS: {}", mount.source)
                })?;
            cids.push(cid);
        }
    }

    // DAG merge.
    let merged_cid = dag_merge(&cids, client).await?;
    tracing::info!(cid = %merged_cid, layers = cids.len(), "DAG merge complete");

    // Materialize: single TAR fetch of the merged tree.
    client
        .unixfs_ref()
        .get_dir(&format!("/ipfs/{merged_cid}"), dst)
        .await
        .context("Failed to materialize merged CID")?;

    Ok(())
}

/// Merge multiple root layer CIDs using IPFS MFS operations.
///
/// Layers are applied left-to-right. Later layers win on file conflicts.
/// Directories are merged recursively. Returns the root CID of the merged tree.
async fn dag_merge(cids: &[String], client: &ipfs::HttpClient) -> Result<String> {
    if cids.is_empty() {
        bail!("No CIDs to merge");
    }
    if cids.len() == 1 {
        return Ok(cids[0].clone());
    }

    let guard = MfsNamespaceGuard::new(client).await?;

    // Copy the base layer (O(1) DAG link).
    client
        .mfs()
        .files_cp(&format!("/ipfs/{}", cids[0]), guard.path())
        .await
        .context("Failed to copy base layer to MFS")?;

    // Overlay each subsequent layer.
    for cid in &cids[1..] {
        merge_overlay_recursive(client, guard.path(), &format!("/ipfs/{cid}"))
            .await
            .with_context(|| format!("Failed to merge overlay {cid}"))?;
    }

    // Stat to get merged root CID.
    let stat = client
        .mfs()
        .files_stat(guard.path(), true)
        .await
        .context("Failed to stat merged MFS namespace")?;

    guard.cleanup().await;
    Ok(stat.hash)
}

/// Recursively merge an overlay into the MFS namespace.
///
/// For each entry in the overlay:
/// - Not in base → `files cp` (add)
/// - Both directories → recurse
/// - Any conflict → `files rm` + `files cp` (replace)
fn merge_overlay_recursive<'a>(
    client: &'a ipfs::HttpClient,
    mfs_path: &'a str,
    overlay_path: &'a str,
) -> futures::future::BoxFuture<'a, Result<()>> {
    Box::pin(merge_overlay_recursive_inner(client, mfs_path, overlay_path))
}

async fn merge_overlay_recursive_inner(
    client: &ipfs::HttpClient,
    mfs_path: &str,
    overlay_path: &str,
) -> Result<()> {
    let mfs = client.mfs();

    // List overlay entries via the regular ls API.
    let overlay_entries = client
        .ls(overlay_path)
        .await
        .with_context(|| format!("ls overlay {overlay_path}"))?;

    // List existing MFS entries (may be empty if dir is new).
    let mfs_entries = mfs.files_ls(mfs_path).await.unwrap_or_default();
    let mfs_names: HashSet<&str> = mfs_entries.iter().map(|e| e.name.as_str()).collect();

    for entry in &overlay_entries {
        let child_mfs = format!("{}/{}", mfs_path, entry.name);
        let child_overlay = format!("{}/{}", overlay_path, entry.name);
        let is_overlay_dir = entry.entry_type == 1;

        if mfs_names.contains(entry.name.as_str()) {
            // Entry exists in base. Check if both are directories.
            let existing = mfs_entries.iter().find(|e| e.name == entry.name).unwrap();
            let is_existing_dir = existing.entry_type == 1;

            if is_overlay_dir && is_existing_dir {
                // Both dirs → recurse.
                merge_overlay_recursive(client, &child_mfs, &child_overlay).await?;
            } else {
                // Conflict: replace.
                mfs.files_rm(&child_mfs, true)
                    .await
                    .with_context(|| format!("rm {child_mfs}"))?;
                mfs.files_cp(&format!("/ipfs/{}", entry.hash), &child_mfs)
                    .await
                    .with_context(|| format!("cp overlay entry {}", entry.name))?;
            }
        } else {
            // New entry → cp.
            mfs.files_cp(&format!("/ipfs/{}", entry.hash), &child_mfs)
                .await
                .with_context(|| format!("cp new entry {}", entry.name))?;
        }
    }

    Ok(())
}

/// Copy-merge: the original strategy, used as a fallback when DAG merge fails.
async fn copy_merge(
    root_mounts: &[&Mount],
    client: &ipfs::HttpClient,
    dst: &Path,
) -> Result<()> {
    for (i, mount) in root_mounts.iter().enumerate() {
        let target_dst = resolve_target(dst, &mount.target);
        if ipfs::is_ipfs_path(&mount.source) {
            apply_ipfs_layer(&mount.source, &target_dst, client)
                .await
                .with_context(|| format!("Failed to apply IPFS mount {i}: {}", mount.source))?;
        } else {
            let src = Path::new(&mount.source);
            if !src.exists() {
                bail!("Mount source does not exist: {}", mount.source);
            }
            apply_local_mount(src, &target_dst)
                .with_context(|| format!("Failed to apply mount {i}: {}", mount.source))?;
        }
    }
    Ok(())
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
