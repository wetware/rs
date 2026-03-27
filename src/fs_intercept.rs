//! WASI filesystem interceptor for `/ipfs/` paths.
//!
//! Intercepts `open_at` calls for paths under `/ipfs/<CID>/…` and resolves them
//! lazily through the pinset cache, materializing content to a staging directory.
//! All other filesystem operations delegate to the standard wasmtime-wasi impl.

use crate::cell::proc::ComponentRunStates;
use anyhow::Result;
use tempfile::TempDir;
use wasmtime::component::{HasData, Linker, Resource};
use wasmtime_wasi::filesystem::{WasiFilesystemCtx, WasiFilesystemCtxView};
use wasmtime_wasi::p2::bindings::filesystem::{preopens, types};
use wasmtime_wasi::p2::{FsError, FsResult};
use wasmtime_wasi_io::streams::{DynInputStream, DynOutputStream};

// ── Marker type for HasData ────────────────────────────────────────

pub(crate) struct IpfsFilesystem;

impl HasData for IpfsFilesystem {
    type Data<'a> = IpfsFilesystemView<'a>;
}

// ── View type: wraps WasiFilesystemCtxView + cache ─────────────────

pub(crate) struct IpfsFilesystemView<'a> {
    pub ctx: &'a mut WasiFilesystemCtx,
    pub table: &'a mut wasmtime::component::ResourceTable,
    pub cache_mode: &'a Option<cache::CacheMode>,
    pub staging: &'a TempDir,
}

impl IpfsFilesystemView<'_> {
    /// Construct a temporary `WasiFilesystemCtxView` for delegation.
    fn as_wasi_view(&mut self) -> WasiFilesystemCtxView<'_> {
        WasiFilesystemCtxView {
            ctx: &mut *self.ctx,
            table: &mut *self.table,
        }
    }
}

// ── Accessor function ──────────────────────────────────────────────

fn ipfs_filesystem(state: &mut ComponentRunStates) -> IpfsFilesystemView<'_> {
    // Split borrow across distinct fields of ComponentRunStates.
    // wasi_ctx.filesystem() borrows wasi_ctx; resource_table, cache_mode,
    // and ipfs_staging are separate fields.
    IpfsFilesystemView {
        ctx: state.wasi_ctx.filesystem(),
        table: &mut state.resource_table,
        cache_mode: &state.cache_mode,
        staging: state
            .ipfs_staging
            .as_ref()
            .expect("ipfs_staging must be set when IPFS intercept is active"),
    }
}

// ── CID path parsing ───────────────────────────────────────────────

/// Parsed IPFS path: CID + optional subpath.
pub(crate) struct IpfsCidPath {
    pub cid: cid::Cid,
    pub subpath: String,
}

/// Parse a relative path like `ipfs/QmHash/sub/file` into CID + subpath.
/// Returns None if the path doesn't start with `ipfs/` or contains path
/// traversal components (`..`).
pub(crate) fn parse_ipfs_path(path: &str) -> Option<IpfsCidPath> {
    let rest = path.strip_prefix("ipfs/")?;
    let (cid_str, subpath) = match rest.find('/') {
        Some(idx) => (&rest[..idx], &rest[idx + 1..]),
        None => (rest, ""),
    };

    // Reject path traversal: any ".." component could escape the staging directory.
    if subpath.split('/').any(|seg| seg == "..") {
        return None;
    }

    let cid = cid_str.parse::<cid::Cid>().ok()?;
    Some(IpfsCidPath {
        cid,
        subpath: subpath.to_string(),
    })
}

// ── open_at interception ───────────────────────────────────────────

impl IpfsFilesystemView<'_> {
    /// Handle an `open_at` for an IPFS path.
    ///
    /// Ensures the CID is cached, materializes content to the staging dir,
    /// and opens it as a real file descriptor.
    async fn open_ipfs(
        &mut self,
        ipfs_path: IpfsCidPath,
        _oflags: types::OpenFlags,
        flags: types::DescriptorFlags,
    ) -> FsResult<Resource<types::Descriptor>> {
        use wasmtime_wasi::{DirPerms, FilePerms, OpenMode};

        // Reject writes — /ipfs/ is content-addressed and immutable
        if flags.contains(types::DescriptorFlags::WRITE) {
            return Err(types::ErrorCode::NotPermitted.into());
        }

        let cache = self
            .cache_mode
            .as_ref()
            .ok_or_else(|| -> FsError { types::ErrorCode::NoEntry.into() })?;

        // Ensure CID is pinned in IPFS
        match cache {
            cache::CacheMode::Shared(pinset) => pinset.ensure(&ipfs_path.cid).await,
            cache::CacheMode::Isolated(isolated) => isolated.ensure(&ipfs_path.cid).await,
        }
        .map_err(|e| {
            tracing::warn!(cid = %ipfs_path.cid, err = %e, "IPFS cache ensure failed");
            FsError::from(types::ErrorCode::Io)
        })?;

        // Materialize to staging directory (local filesystem is our cache)
        let staging_path = self.staging.path().join(ipfs_path.cid.to_string());
        let file_path = if ipfs_path.subpath.is_empty() {
            staging_path.clone()
        } else {
            staging_path.join(&ipfs_path.subpath)
        };

        // Skip fetch if already staged (disk cache hit)
        if !file_path.exists() {
            let bytes = match cache {
                cache::CacheMode::Shared(pinset) => pinset.fetch(&ipfs_path.cid).await,
                cache::CacheMode::Isolated(isolated) => isolated.fetch(&ipfs_path.cid).await,
            }
            .map_err(|e| {
                tracing::warn!(cid = %ipfs_path.cid, err = %e, "IPFS fetch failed");
                FsError::from(types::ErrorCode::Io)
            })?;

            if let Some(parent) = file_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|_| -> FsError { types::ErrorCode::Io.into() })?;
            }
            std::fs::write(&file_path, &bytes)
                .map_err(|_| -> FsError { types::ErrorCode::Io.into() })?;
        }

        let target_path = if ipfs_path.subpath.is_empty() {
            staging_path
        } else {
            staging_path.join(&ipfs_path.subpath)
        };

        if !target_path.exists() {
            return Err(types::ErrorCode::NoEntry.into());
        }

        // Open as a real filesystem descriptor
        let meta = std::fs::metadata(&target_path)
            .map_err(|_| -> FsError { types::ErrorCode::Io.into() })?;

        let descriptor = if meta.is_dir() {
            let dir =
                cap_std::fs::Dir::open_ambient_dir(&target_path, cap_std::ambient_authority())
                    .map_err(|_| -> FsError { types::ErrorCode::Io.into() })?;
            let wasi_dir = wasmtime_wasi::filesystem::Dir::new(
                dir,
                DirPerms::READ,
                FilePerms::READ,
                OpenMode::READ,
                false,
            );
            wasmtime_wasi::filesystem::Descriptor::Dir(wasi_dir)
        } else {
            let file = cap_std::fs::Dir::open_ambient_dir(
                target_path.parent().unwrap_or(&target_path),
                cap_std::ambient_authority(),
            )
            .map_err(|_| -> FsError { types::ErrorCode::Io.into() })?
            .open(target_path.file_name().unwrap_or_default())
            .map_err(|_| -> FsError { types::ErrorCode::Io.into() })?;

            let wasi_file =
                wasmtime_wasi::filesystem::File::new(file, FilePerms::READ, OpenMode::READ, false);
            wasmtime_wasi::filesystem::Descriptor::File(wasi_file)
        };

        let fd = self
            .table
            .push(descriptor)
            .map_err(|_| -> FsError { types::ErrorCode::Io.into() })?;
        Ok(fd)
    }
}

// ── HostDescriptor — delegate everything, intercept open_at ────────

impl types::HostDescriptor for IpfsFilesystemView<'_> {
    async fn advise(
        &mut self,
        fd: Resource<types::Descriptor>,
        offset: types::Filesize,
        len: types::Filesize,
        advice: types::Advice,
    ) -> FsResult<()> {
        self.as_wasi_view().advise(fd, offset, len, advice).await
    }

    async fn sync_data(&mut self, fd: Resource<types::Descriptor>) -> FsResult<()> {
        self.as_wasi_view().sync_data(fd).await
    }

    async fn get_flags(
        &mut self,
        fd: Resource<types::Descriptor>,
    ) -> FsResult<types::DescriptorFlags> {
        self.as_wasi_view().get_flags(fd).await
    }

    async fn get_type(
        &mut self,
        fd: Resource<types::Descriptor>,
    ) -> FsResult<types::DescriptorType> {
        self.as_wasi_view().get_type(fd).await
    }

    async fn set_size(
        &mut self,
        fd: Resource<types::Descriptor>,
        size: types::Filesize,
    ) -> FsResult<()> {
        self.as_wasi_view().set_size(fd, size).await
    }

    async fn set_times(
        &mut self,
        fd: Resource<types::Descriptor>,
        atim: types::NewTimestamp,
        mtim: types::NewTimestamp,
    ) -> FsResult<()> {
        self.as_wasi_view().set_times(fd, atim, mtim).await
    }

    async fn read(
        &mut self,
        fd: Resource<types::Descriptor>,
        len: types::Filesize,
        offset: types::Filesize,
    ) -> FsResult<(Vec<u8>, bool)> {
        self.as_wasi_view().read(fd, len, offset).await
    }

    async fn write(
        &mut self,
        fd: Resource<types::Descriptor>,
        buf: Vec<u8>,
        offset: types::Filesize,
    ) -> FsResult<types::Filesize> {
        self.as_wasi_view().write(fd, buf, offset).await
    }

    async fn read_directory(
        &mut self,
        fd: Resource<types::Descriptor>,
    ) -> FsResult<Resource<types::DirectoryEntryStream>> {
        self.as_wasi_view().read_directory(fd).await
    }

    async fn sync(&mut self, fd: Resource<types::Descriptor>) -> FsResult<()> {
        self.as_wasi_view().sync(fd).await
    }

    async fn create_directory_at(
        &mut self,
        fd: Resource<types::Descriptor>,
        path: String,
    ) -> FsResult<()> {
        self.as_wasi_view().create_directory_at(fd, path).await
    }

    async fn stat(&mut self, fd: Resource<types::Descriptor>) -> FsResult<types::DescriptorStat> {
        self.as_wasi_view().stat(fd).await
    }

    async fn stat_at(
        &mut self,
        fd: Resource<types::Descriptor>,
        path_flags: types::PathFlags,
        path: String,
    ) -> FsResult<types::DescriptorStat> {
        self.as_wasi_view().stat_at(fd, path_flags, path).await
    }

    async fn set_times_at(
        &mut self,
        fd: Resource<types::Descriptor>,
        path_flags: types::PathFlags,
        path: String,
        atim: types::NewTimestamp,
        mtim: types::NewTimestamp,
    ) -> FsResult<()> {
        self.as_wasi_view()
            .set_times_at(fd, path_flags, path, atim, mtim)
            .await
    }

    async fn link_at(
        &mut self,
        fd: Resource<types::Descriptor>,
        old_path_flags: types::PathFlags,
        old_path: String,
        new_descriptor: Resource<types::Descriptor>,
        new_path: String,
    ) -> FsResult<()> {
        self.as_wasi_view()
            .link_at(fd, old_path_flags, old_path, new_descriptor, new_path)
            .await
    }

    async fn open_at(
        &mut self,
        fd: Resource<types::Descriptor>,
        path_flags: types::PathFlags,
        path: String,
        oflags: types::OpenFlags,
        flags: types::DescriptorFlags,
    ) -> FsResult<Resource<types::Descriptor>> {
        // Intercept /ipfs/ paths
        if let Some(ipfs_path) = parse_ipfs_path(&path) {
            tracing::debug!(cid = %ipfs_path.cid, subpath = %ipfs_path.subpath, "Intercepting IPFS open_at");
            return self.open_ipfs(ipfs_path, oflags, flags).await;
        }

        // Delegate to standard filesystem
        self.as_wasi_view()
            .open_at(fd, path_flags, path, oflags, flags)
            .await
    }

    fn drop(&mut self, fd: Resource<types::Descriptor>) -> anyhow::Result<()> {
        self.as_wasi_view().drop(fd)
    }

    async fn readlink_at(
        &mut self,
        fd: Resource<types::Descriptor>,
        path: String,
    ) -> FsResult<String> {
        self.as_wasi_view().readlink_at(fd, path).await
    }

    async fn remove_directory_at(
        &mut self,
        fd: Resource<types::Descriptor>,
        path: String,
    ) -> FsResult<()> {
        self.as_wasi_view().remove_directory_at(fd, path).await
    }

    async fn rename_at(
        &mut self,
        fd: Resource<types::Descriptor>,
        old_path: String,
        new_fd: Resource<types::Descriptor>,
        new_path: String,
    ) -> FsResult<()> {
        self.as_wasi_view()
            .rename_at(fd, old_path, new_fd, new_path)
            .await
    }

    async fn symlink_at(
        &mut self,
        fd: Resource<types::Descriptor>,
        src_path: String,
        dest_path: String,
    ) -> FsResult<()> {
        self.as_wasi_view()
            .symlink_at(fd, src_path, dest_path)
            .await
    }

    async fn unlink_file_at(
        &mut self,
        fd: Resource<types::Descriptor>,
        path: String,
    ) -> FsResult<()> {
        self.as_wasi_view().unlink_file_at(fd, path).await
    }

    fn read_via_stream(
        &mut self,
        fd: Resource<types::Descriptor>,
        offset: types::Filesize,
    ) -> FsResult<Resource<DynInputStream>> {
        self.as_wasi_view().read_via_stream(fd, offset)
    }

    fn write_via_stream(
        &mut self,
        fd: Resource<types::Descriptor>,
        offset: types::Filesize,
    ) -> FsResult<Resource<DynOutputStream>> {
        self.as_wasi_view().write_via_stream(fd, offset)
    }

    fn append_via_stream(
        &mut self,
        fd: Resource<types::Descriptor>,
    ) -> FsResult<Resource<DynOutputStream>> {
        self.as_wasi_view().append_via_stream(fd)
    }

    async fn is_same_object(
        &mut self,
        a: Resource<types::Descriptor>,
        b: Resource<types::Descriptor>,
    ) -> anyhow::Result<bool> {
        self.as_wasi_view().is_same_object(a, b).await
    }

    async fn metadata_hash(
        &mut self,
        fd: Resource<types::Descriptor>,
    ) -> FsResult<types::MetadataHashValue> {
        self.as_wasi_view().metadata_hash(fd).await
    }

    async fn metadata_hash_at(
        &mut self,
        fd: Resource<types::Descriptor>,
        path_flags: types::PathFlags,
        path: String,
    ) -> FsResult<types::MetadataHashValue> {
        self.as_wasi_view()
            .metadata_hash_at(fd, path_flags, path)
            .await
    }
}

// ── Host trait (error code conversion) ─────────────────────────────

impl types::Host for IpfsFilesystemView<'_> {
    fn convert_error_code(&mut self, err: FsError) -> anyhow::Result<types::ErrorCode> {
        self.as_wasi_view().convert_error_code(err)
    }

    fn filesystem_error_code(
        &mut self,
        err: Resource<anyhow::Error>,
    ) -> anyhow::Result<Option<types::ErrorCode>> {
        self.as_wasi_view().filesystem_error_code(err)
    }
}

// ── HostDirectoryEntryStream ───────────────────────────────────────

impl types::HostDirectoryEntryStream for IpfsFilesystemView<'_> {
    async fn read_directory_entry(
        &mut self,
        stream: Resource<types::DirectoryEntryStream>,
    ) -> FsResult<Option<types::DirectoryEntry>> {
        self.as_wasi_view().read_directory_entry(stream).await
    }

    fn drop(&mut self, stream: Resource<types::DirectoryEntryStream>) -> anyhow::Result<()> {
        types::HostDirectoryEntryStream::drop(&mut self.as_wasi_view(), stream)
    }
}

// ── Preopens ───────────────────────────────────────────────────────

impl preopens::Host for IpfsFilesystemView<'_> {
    fn get_directories(&mut self) -> wasmtime::Result<Vec<(Resource<types::Descriptor>, String)>> {
        self.as_wasi_view().get_directories()
    }
}

// ── Linker override ────────────────────────────────────────────────

/// Override the filesystem linker bindings with our IPFS interceptor.
///
/// Call this AFTER `add_to_linker_async` to replace the standard filesystem
/// implementation with one that intercepts `/ipfs/` paths.
pub(crate) fn override_filesystem_linker(linker: &mut Linker<ComponentRunStates>) -> Result<()> {
    // Enable shadowing so we can override the already-registered filesystem bindings
    linker.allow_shadowing(true);

    types::add_to_linker::<ComponentRunStates, IpfsFilesystem>(linker, ipfs_filesystem)?;
    preopens::add_to_linker::<ComponentRunStates, IpfsFilesystem>(linker, ipfs_filesystem)?;

    // Restore default (no shadowing) for safety
    linker.allow_shadowing(false);

    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Arc;

    // ── CID path parsing tests ─────────────────────────────────────

    #[test]
    fn test_parse_ipfs_path_with_subpath() {
        let cid_str = "QmYwAPJzv5CZsnN625s3Xf2nemtYgPpHdWEz79ojWnPbdG";
        let path = format!("ipfs/{cid_str}/sub/file.txt");
        let parsed = parse_ipfs_path(&path).expect("should parse");
        assert_eq!(parsed.cid.to_string(), cid_str);
        assert_eq!(parsed.subpath, "sub/file.txt");
    }

    #[test]
    fn test_parse_ipfs_path_root() {
        let cid_str = "QmYwAPJzv5CZsnN625s3Xf2nemtYgPpHdWEz79ojWnPbdG";
        let path = format!("ipfs/{cid_str}");
        let parsed = parse_ipfs_path(&path).expect("should parse");
        assert_eq!(parsed.cid.to_string(), cid_str);
        assert_eq!(parsed.subpath, "");
    }

    #[test]
    fn test_parse_non_ipfs_path() {
        assert!(parse_ipfs_path("usr/local/bin").is_none());
        assert!(parse_ipfs_path("etc/config").is_none());
    }

    #[test]
    fn test_parse_ipfs_path_invalid_cid() {
        assert!(parse_ipfs_path("ipfs/not-a-valid-cid/file").is_none());
    }

    #[test]
    fn test_parse_ipfs_path_rejects_traversal() {
        let cid_str = "QmYwAPJzv5CZsnN625s3Xf2nemtYgPpHdWEz79ojWnPbdG";
        // Direct traversal
        assert!(parse_ipfs_path(&format!("ipfs/{cid_str}/../../etc/passwd")).is_none());
        // Mid-path traversal
        assert!(parse_ipfs_path(&format!("ipfs/{cid_str}/sub/../../../etc")).is_none());
        // Single dotdot
        assert!(parse_ipfs_path(&format!("ipfs/{cid_str}/..")).is_none());
        // Valid subpaths still work
        assert!(parse_ipfs_path(&format!("ipfs/{cid_str}/sub/file.txt")).is_some());
        assert!(parse_ipfs_path(&format!("ipfs/{cid_str}/file..name")).is_some());
    }

    // ── Mock pinner for integration tests ──────────────────────────

    struct MockPinner {
        data: HashMap<cid::Cid, Vec<u8>>,
    }

    #[async_trait::async_trait]
    impl cache::Pinner for MockPinner {
        async fn pin(&self, _cid: &cid::Cid) -> anyhow::Result<()> {
            Ok(())
        }
        async fn unpin(&self, _cid: &cid::Cid) -> anyhow::Result<()> {
            Ok(())
        }
        async fn fetch(&self, cid: &cid::Cid) -> anyhow::Result<Vec<u8>> {
            self.data
                .get(cid)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("CID not found in mock"))
        }
        async fn size(&self, cid: &cid::Cid) -> anyhow::Result<u64> {
            self.data
                .get(cid)
                .map(|d| d.len() as u64)
                .ok_or_else(|| anyhow::anyhow!("CID not found in mock"))
        }
    }

    /// Helper: construct a test CID + mock pinner with known content.
    fn test_cid_and_pinner(content: &[u8]) -> (cid::Cid, Arc<MockPinner>) {
        let cid_str = "QmYwAPJzv5CZsnN625s3Xf2nemtYgPpHdWEz79ojWnPbdG";
        let cid: cid::Cid = cid_str.parse().unwrap();
        let mut data = HashMap::new();
        data.insert(cid, content.to_vec());
        (cid, Arc::new(MockPinner { data }))
    }

    /// Helper: build the view for testing open_ipfs.
    struct TestHarness {
        wasi_ctx: wasmtime_wasi::WasiCtx,
        resource_table: wasmtime::component::ResourceTable,
        cache_mode: Option<cache::CacheMode>,
        staging: TempDir,
    }

    impl TestHarness {
        fn new(cache_mode: Option<cache::CacheMode>) -> Self {
            Self {
                wasi_ctx: wasmtime_wasi::WasiCtxBuilder::new().build(),
                resource_table: wasmtime::component::ResourceTable::new(),
                cache_mode,
                staging: TempDir::new().unwrap(),
            }
        }

        fn view(&mut self) -> IpfsFilesystemView<'_> {
            IpfsFilesystemView {
                ctx: self.wasi_ctx.filesystem(),
                table: &mut self.resource_table,
                cache_mode: &self.cache_mode,
                staging: &self.staging,
            }
        }
    }

    // ── Integration tests ──────────────────────────────────────────

    #[tokio::test]
    async fn test_open_ipfs_file_materializes_and_returns_descriptor() {
        let content = b"hello ipfs world";
        let (cid, pinner) = test_cid_and_pinner(content);

        let isolated = cache::IsolatedPinset::new(pinner);
        let mut harness = TestHarness::new(Some(cache::CacheMode::Isolated(isolated)));

        let ipfs_path = IpfsCidPath {
            cid,
            subpath: String::new(),
        };
        let fd = harness
            .view()
            .open_ipfs(
                ipfs_path,
                types::OpenFlags::empty(),
                types::DescriptorFlags::READ,
            )
            .await
            .expect("open_ipfs should succeed");

        // Descriptor was pushed to the resource table
        let desc = harness.resource_table.get(&fd);
        assert!(desc.is_ok(), "descriptor should be in resource table");

        // Content was materialized to staging
        let staging_file = harness.staging.path().join(cid.to_string());
        assert!(staging_file.exists(), "staging file should exist");
        assert_eq!(
            std::fs::read(&staging_file).unwrap(),
            content,
            "staging file should contain the IPFS content"
        );
    }

    #[tokio::test]
    async fn test_open_ipfs_write_rejected() {
        let (cid, pinner) = test_cid_and_pinner(b"data");
        let isolated = cache::IsolatedPinset::new(pinner);
        let mut harness = TestHarness::new(Some(cache::CacheMode::Isolated(isolated)));

        let ipfs_path = IpfsCidPath {
            cid,
            subpath: String::new(),
        };
        let result = harness
            .view()
            .open_ipfs(
                ipfs_path,
                types::OpenFlags::empty(),
                types::DescriptorFlags::READ | types::DescriptorFlags::WRITE,
            )
            .await;

        assert!(result.is_err(), "write to /ipfs/ should be rejected");
    }

    #[tokio::test]
    async fn test_open_ipfs_no_cache_returns_error() {
        let cid: cid::Cid = "QmYwAPJzv5CZsnN625s3Xf2nemtYgPpHdWEz79ojWnPbdG"
            .parse()
            .unwrap();
        let mut harness = TestHarness::new(None); // no cache

        let ipfs_path = IpfsCidPath {
            cid,
            subpath: String::new(),
        };
        let result = harness
            .view()
            .open_ipfs(
                ipfs_path,
                types::OpenFlags::empty(),
                types::DescriptorFlags::READ,
            )
            .await;

        assert!(result.is_err(), "open without cache should fail");
    }

    #[tokio::test]
    async fn test_open_ipfs_unknown_cid_returns_error() {
        // Pinner has no data for the CID we'll request
        let pinner = Arc::new(MockPinner {
            data: HashMap::new(),
        });
        let isolated = cache::IsolatedPinset::new(pinner);
        let mut harness = TestHarness::new(Some(cache::CacheMode::Isolated(isolated)));

        let cid: cid::Cid = "QmYwAPJzv5CZsnN625s3Xf2nemtYgPpHdWEz79ojWnPbdG"
            .parse()
            .unwrap();
        let ipfs_path = IpfsCidPath {
            cid,
            subpath: String::new(),
        };
        let result = harness
            .view()
            .open_ipfs(
                ipfs_path,
                types::OpenFlags::empty(),
                types::DescriptorFlags::READ,
            )
            .await;

        assert!(result.is_err(), "unknown CID should fail");
    }

    #[tokio::test]
    async fn test_open_ipfs_with_shared_cache() {
        let content = b"shared cache content";
        let (cid, pinner) = test_cid_and_pinner(content);

        let pinset = Arc::new(cache::PinsetCache::new(pinner, 10 * 1024 * 1024));
        let mut harness = TestHarness::new(Some(cache::CacheMode::Shared(pinset)));

        let ipfs_path = IpfsCidPath {
            cid,
            subpath: String::new(),
        };
        let fd = harness
            .view()
            .open_ipfs(
                ipfs_path,
                types::OpenFlags::empty(),
                types::DescriptorFlags::READ,
            )
            .await
            .expect("open_ipfs with shared cache should succeed");

        assert!(harness.resource_table.get(&fd).is_ok());

        let staging_file = harness.staging.path().join(cid.to_string());
        assert_eq!(std::fs::read(&staging_file).unwrap(), content);
    }

    #[tokio::test]
    async fn test_open_ipfs_with_subpath() {
        let content = b"nested file content";
        let (cid, pinner) = test_cid_and_pinner(content);

        let isolated = cache::IsolatedPinset::new(pinner);
        let mut harness = TestHarness::new(Some(cache::CacheMode::Isolated(isolated)));

        let ipfs_path = IpfsCidPath {
            cid,
            subpath: "sub/dir/file.txt".to_string(),
        };
        let fd = harness
            .view()
            .open_ipfs(
                ipfs_path,
                types::OpenFlags::empty(),
                types::DescriptorFlags::READ,
            )
            .await
            .expect("open_ipfs with subpath should succeed");

        assert!(harness.resource_table.get(&fd).is_ok());

        // Verify nested path was created in staging
        let nested_file = harness
            .staging
            .path()
            .join(cid.to_string())
            .join("sub/dir/file.txt");
        assert!(nested_file.exists(), "nested staging file should exist");
        assert_eq!(std::fs::read(&nested_file).unwrap(), content);
    }

    #[tokio::test]
    async fn test_open_ipfs_skips_fetch_on_staging_hit() {
        let content = b"cached on disk";
        let (cid, pinner) = test_cid_and_pinner(content);

        let isolated = cache::IsolatedPinset::new(pinner);
        let mut harness = TestHarness::new(Some(cache::CacheMode::Isolated(isolated)));

        // First open: fetches and stages
        let ipfs_path = IpfsCidPath {
            cid,
            subpath: String::new(),
        };
        harness
            .view()
            .open_ipfs(
                ipfs_path,
                types::OpenFlags::empty(),
                types::DescriptorFlags::READ,
            )
            .await
            .expect("first open should succeed");

        // Second open: should hit staging (file already exists)
        let ipfs_path = IpfsCidPath {
            cid,
            subpath: String::new(),
        };
        let fd = harness
            .view()
            .open_ipfs(
                ipfs_path,
                types::OpenFlags::empty(),
                types::DescriptorFlags::READ,
            )
            .await
            .expect("second open should hit staging cache");

        assert!(harness.resource_table.get(&fd).is_ok());
    }
}
