//! Multi-layer FHS image resolution.
//!
//! Takes N image root paths (local filesystem or IPFS), resolves them
//! in order, and materializes a merged FHS tree. Per-file union: later
//! layers win on file conflicts. Directories merge. No deletes.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use tempfile::TempDir;
use walkdir::WalkDir;

use crate::ipfs;

/// A merged FHS image root on the host filesystem.
///
/// Holds either a direct reference to a single local layer (no copy)
/// or a temp directory containing the union of multiple layers.
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
    /// Single local layer — use the original path directly.
    Direct(PathBuf),
    /// Multi-layer union materialized in a temp directory.
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

/// Resolve and merge N image layers into a single FHS root.
///
/// Each layer is either a local filesystem path or an IPFS path.
/// Layers are applied in order: first layer is the base, last layer
/// wins on file conflicts. Directories merge.
///
/// The merged root must contain `bin/main.wasm` or an error is returned.
pub async fn merge_layers(
    layers: &[String],
    ipfs_client: &ipfs::HttpClient,
) -> Result<MergedImage> {
    if layers.is_empty() {
        bail!("No image layers provided");
    }

    // Optimization: single local layer — skip the temp dir.
    if layers.len() == 1 && !ipfs::is_ipfs_path(&layers[0]) {
        let path = PathBuf::from(&layers[0]);
        if !path.exists() {
            bail!("Image path does not exist: {}", layers[0]);
        }
        let main_wasm = path.join("bin/main.wasm");
        if !main_wasm.exists() {
            bail!(
                "Image missing bin/main.wasm: {}",
                path.display()
            );
        }
        return Ok(MergedImage {
            root: ImageRoot::Direct(path),
        });
    }

    // Multiple layers or IPFS — materialize into a temp dir.
    let merged_dir = TempDir::new().context("Failed to create temp dir for merged image")?;

    for (i, layer) in layers.iter().enumerate() {
        if ipfs::is_ipfs_path(layer) {
            apply_ipfs_layer(layer, merged_dir.path(), ipfs_client)
                .await
                .with_context(|| format!("Failed to apply IPFS layer {i}: {layer}"))?;
        } else {
            let src = Path::new(layer);
            if !src.exists() {
                bail!("Image layer path does not exist: {layer}");
            }
            apply_local_layer(src, merged_dir.path())
                .with_context(|| format!("Failed to apply local layer {i}: {layer}"))?;
        }
    }

    // Verify the merged root has bin/main.wasm.
    let main_wasm = merged_dir.path().join("bin/main.wasm");
    if !main_wasm.exists() {
        bail!(
            "Merged image missing bin/main.wasm (from {} layers)",
            layers.len()
        );
    }

    Ok(MergedImage {
        root: ImageRoot::Merged(merged_dir),
    })
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

    #[tokio::test]
    async fn test_merge_single_local_layer() {
        let layer = make_layer(&[("bin/main.wasm", b"(wasm)")]);
        let client = stub_ipfs_client();
        let merged = merge_layers(&[layer.path().to_string_lossy().into()], &client)
            .await
            .unwrap();
        // Single local layer should use direct reference (no copy).
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

        // bin/main.wasm from base should be present.
        assert!(merged.path().join("bin/main.wasm").exists());
        // etc/config should be overridden by overlay.
        let config = fs::read_to_string(merged.path().join("etc/config")).unwrap();
        assert_eq!(config, "overlay-config");
    }

    #[tokio::test]
    async fn test_merge_directories_merge() {
        let base = make_layer(&[
            ("bin/main.wasm", b"(wasm)"),
            ("boot/QmPeerA", b"addr-a"),
        ]);
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
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("missing bin/main.wasm")
        );
    }

    #[tokio::test]
    async fn test_merge_empty_layers_errors() {
        let client = stub_ipfs_client();
        let result = merge_layers(&[], &client).await;
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("No image layers")
        );
    }

    #[tokio::test]
    async fn test_merge_nonexistent_path_errors() {
        let client = stub_ipfs_client();
        let result = merge_layers(&["/nonexistent/path/abc123".into()], &client).await;
        assert!(result.is_err());
    }
}
