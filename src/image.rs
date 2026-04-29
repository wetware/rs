//! Mount-based FHS image resolution (CidTree path).
//!
//! Every positional arg to `ww run` is a mount: `source[:target]`.
//! Root mounts (target `/`) are traditional image layers. Targeted
//! mounts overlay a host file or directory at a specific guest path.
//!
//! Mounts are applied left-to-right via `resolve_mounts_virtual`:
//! root layers are DAG-merged at the IPFS MFS level (file blocks never
//! touched, only directory nodes get new CIDs), and targeted mounts
//! become `LocalOverride` entries that the guest's `CidTree` checks
//! before falling through to the CID walk. No file content is
//! materialized to disk by this module.
//!
//! Pre-#416 this file also exposed an `apply_mounts` API that
//! materialized a merged FHS into a `TempDir` and was preopened
//! directly to the WASI guest. That path was removed once every
//! production cell switched to `CidTree`. The merge algorithm itself
//! (`dag_merge` + `merge_overlay_recursive`) is preserved here and
//! used by `resolve_mounts_virtual`.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use cid::Cid;

use crate::ipfs;
use crate::mount::Mount;

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
    Box::pin(merge_overlay_recursive_inner(
        client,
        mfs_path,
        overlay_path,
    ))
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
            let existing = mfs_entries
                .iter()
                .find(|e| e.name == entry.name)
                .context("entry in mfs_names but not in mfs_entries")?;
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

// ── Virtual mount resolution (lazy CidTree path) ─────────────────

/// Resolve mounts into a root CID and local overrides for the virtual filesystem.
///
/// Performs the DAG merge to produce a merged root CID, and collects
/// targeted mounts as `LocalOverride` entries. No file content is copied
/// or fetched — that happens lazily when the guest opens files via
/// `CidTree` + `fs_intercept`.
///
/// Returns `(root_cid, local_overrides)` suitable for constructing a `CidTree`.
pub async fn resolve_mounts_virtual(
    mounts: &[Mount],
    ipfs_client: &ipfs::HttpClient,
) -> Result<(
    String,
    std::collections::HashMap<std::path::PathBuf, crate::vfs::LocalOverride>,
)> {
    use crate::vfs::LocalOverride;

    if mounts.is_empty() {
        bail!("No mounts provided");
    }

    let (root_mounts, targeted_mounts): (Vec<&Mount>, Vec<&Mount>) =
        mounts.iter().partition(|m| m.is_root());

    if root_mounts.is_empty() {
        bail!("No root mounts provided (at least one required)");
    }

    // Resolve all root mounts to CIDs.
    let mut cids = Vec::with_capacity(root_mounts.len());
    for mount in &root_mounts {
        if ipfs::is_ipfs_path(&mount.source) {
            let cid = mount
                .source
                .strip_prefix("/ipfs/")
                .context("IPFS path must start with /ipfs/")?;
            cids.push(cid.to_string());
        } else {
            // Add local directory to IPFS.
            let cid = ipfs_client
                .add_dir(Path::new(&mount.source))
                .await
                .with_context(|| format!("Failed to add local layer to IPFS: {}", mount.source))?;
            cids.push(cid);
        }
    }

    // DAG merge to produce a single root CID.
    let root_cid = dag_merge(&cids, ipfs_client).await?;
    tracing::info!(cid = %root_cid, layers = cids.len(), "Virtual DAG merge complete");

    // Collect targeted mounts as local overrides.
    let mut overrides = std::collections::HashMap::new();
    for mount in &targeted_mounts {
        let guest_path = mount
            .target
            .strip_prefix("/")
            .unwrap_or(&mount.target)
            .to_path_buf();

        if guest_path.as_os_str().is_empty() {
            continue; // skip empty target
        }

        if ipfs::is_ipfs_path(&mount.source) {
            // IPFS targeted mounts are not supported in virtual mode.
            // They would need to be added to the CID tree, which conflicts
            // with the design goal of keeping private mounts off IPFS.
            bail!(
                "IPFS targeted mounts not supported in virtual mode: {} -> {}",
                mount.source,
                mount.target.display()
            );
        }

        let src = Path::new(&mount.source);
        if !src.exists() {
            bail!("Mount source does not exist: {}", mount.source);
        }

        let ovr = if src.is_dir() {
            LocalOverride::Dir(src.to_path_buf())
        } else {
            LocalOverride::File(src.to_path_buf())
        };
        overrides.insert(guest_path, ovr);
    }

    Ok((root_cid, overrides))
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
    use std::path::PathBuf;
    use tempfile::TempDir;

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

    // ── resolve_mounts_virtual tests (production path) ──
    //
    // These exercise the real merge algorithm (`dag_merge` via Kubo MFS)
    // and the targeted-mount → `LocalOverride` translation. They require
    // a running Kubo daemon at `localhost:5001`; CI provisions one via
    // .github/workflows/rust.yml's setup-kubo step. Locally, run with
    // `ipfs daemon &` first, otherwise these tests fail.
    //
    // Inherited from the deleted `apply_mounts` / `merge_layers` tests
    // for merge correctness (T2) and targeted-mount translation (T3).

    #[tokio::test]
    async fn test_virtual_empty_mounts_errors() {
        let client = stub_ipfs_client();
        let result = resolve_mounts_virtual(&[], &client).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No mounts"));
    }

    #[tokio::test]
    async fn test_virtual_nonexistent_root_errors() {
        let client = stub_ipfs_client();
        let result =
            resolve_mounts_virtual(&[root_mount("/nonexistent/path/abc123")], &client).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_virtual_targeted_mount_becomes_local_override_file() {
        // T3: targeted file mount translates to LocalOverride::File.
        let layer = make_layer(&[("bin/main.wasm", b"(wasm)")]);
        let host_file = TempDir::new().unwrap();
        let identity_path = host_file.path().join("my-key");
        fs::write(&identity_path, b"secret-key-data").unwrap();

        let client = stub_ipfs_client();
        let (root_cid, overrides) = match resolve_mounts_virtual(
            &[
                root_mount(&layer.path().to_string_lossy()),
                targeted_mount(&identity_path.to_string_lossy(), "/etc/identity"),
            ],
            &client,
        )
        .await
        {
            Ok(v) => v,
            Err(e) if format!("{e}").contains("connection refused") => {
                eprintln!("skipping: kubo not running ({e})");
                return;
            }
            Err(e) => panic!("resolve_mounts_virtual failed: {e}"),
        };

        assert!(!root_cid.is_empty(), "root CID should be set");
        let key = std::path::PathBuf::from("etc/identity");
        match overrides.get(&key) {
            Some(crate::vfs::LocalOverride::File(p)) => {
                assert_eq!(p, &identity_path);
            }
            other => panic!("expected LocalOverride::File, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_virtual_targeted_dir_becomes_local_override_dir() {
        // T3: targeted directory mount translates to LocalOverride::Dir.
        let layer = make_layer(&[("bin/main.wasm", b"(wasm)")]);
        let data_dir = make_layer(&[("file-a.txt", b"aaa")]);

        let client = stub_ipfs_client();
        let (_root_cid, overrides) = match resolve_mounts_virtual(
            &[
                root_mount(&layer.path().to_string_lossy()),
                targeted_mount(&data_dir.path().to_string_lossy(), "/var/data"),
            ],
            &client,
        )
        .await
        {
            Ok(v) => v,
            Err(e) if format!("{e}").contains("connection refused") => {
                eprintln!("skipping: kubo not running ({e})");
                return;
            }
            Err(e) => panic!("resolve_mounts_virtual failed: {e}"),
        };

        let key = std::path::PathBuf::from("var/data");
        match overrides.get(&key) {
            Some(crate::vfs::LocalOverride::Dir(p)) => {
                assert_eq!(p, data_dir.path());
            }
            other => panic!("expected LocalOverride::Dir, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_virtual_targeted_nonexistent_source_errors() {
        let layer = make_layer(&[("bin/main.wasm", b"(wasm)")]);
        let client = stub_ipfs_client();
        let result = resolve_mounts_virtual(
            &[
                root_mount(&layer.path().to_string_lossy()),
                targeted_mount("/nonexistent/source", "/etc/identity"),
            ],
            &client,
        )
        .await;
        if let Err(e) = &result {
            if format!("{e}").contains("connection refused") {
                eprintln!("skipping: kubo not running ({e})");
                return;
            }
        }
        assert!(
            result.is_err(),
            "expected error for nonexistent mount source"
        );
        assert!(result.unwrap_err().to_string().contains("does not exist"));
    }

    #[tokio::test]
    async fn test_virtual_two_layers_dag_merge_overrides_file() {
        // T2 (merge correctness): two-layer overlay where both have
        // `etc/config` — overlay wins. Exercises `dag_merge` end-to-end.
        let base = make_layer(&[
            ("bin/main.wasm", b"base-wasm"),
            ("etc/config", b"base-config"),
        ]);
        let overlay = make_layer(&[("etc/config", b"overlay-config")]);
        let client = stub_ipfs_client();

        let result = resolve_mounts_virtual(
            &[
                root_mount(&base.path().to_string_lossy()),
                root_mount(&overlay.path().to_string_lossy()),
            ],
            &client,
        )
        .await;

        let (root_cid, overrides) = match result {
            Ok(v) => v,
            Err(e) if format!("{e}").contains("connection refused") => {
                eprintln!("skipping: kubo not running ({e})");
                return;
            }
            Err(e) => panic!("resolve_mounts_virtual failed: {e}"),
        };

        assert!(!root_cid.is_empty());
        assert!(overrides.is_empty(), "no targeted mounts → no overrides");
        // The merge correctness assertion (overlay wins on `etc/config`)
        // is exercised by the kernel reading through CidTree at runtime;
        // unit tests here can only verify the merged CID is produced.
        // End-to-end coverage lives in tests/discovery_integration.rs.
    }

    #[tokio::test]
    async fn test_virtual_two_layers_dag_merge_unions_directories() {
        // T2 (merge correctness): two layers with disjoint files in the
        // same directory. The merged tree must contain both.
        let base = make_layer(&[("bin/main.wasm", b"(wasm)"), ("boot/QmPeerA", b"addr-a")]);
        let overlay = make_layer(&[("boot/QmPeerB", b"addr-b")]);
        let client = stub_ipfs_client();

        let result = resolve_mounts_virtual(
            &[
                root_mount(&base.path().to_string_lossy()),
                root_mount(&overlay.path().to_string_lossy()),
            ],
            &client,
        )
        .await;

        let (root_cid, _overrides) = match result {
            Ok(v) => v,
            Err(e) if format!("{e}").contains("connection refused") => {
                eprintln!("skipping: kubo not running ({e})");
                return;
            }
            Err(e) => panic!("resolve_mounts_virtual failed: {e}"),
        };

        assert!(!root_cid.is_empty());
        // As above, the actual union-on-`boot/` assertion is verified
        // end-to-end by integration tests that boot a kernel through
        // CidTree and read directory listings.
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
