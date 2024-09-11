use futures::future::BoxFuture;
use std::marker::{Send, Sync};
use std::path::{Path, PathBuf};

use wasmer_wasix::virtual_fs;

#[derive(Debug)]
pub struct IpfsFs {}

impl virtual_fs::FileSystem for IpfsFs {
    fn readlink(&self, path: &Path) -> virtual_fs::Result<PathBuf> {}
    fn read_dir(&self, path: &Path) -> virtual_fs::Result<virtual_fs::ReadDir> {}
    fn create_dir(&self, path: &Path) -> virtual_fs::Result<()> {}
    fn remove_dir(&self, path: &Path) -> virtual_fs::Result<()> {}
    fn rename<'a>(&'a self, from: &'a Path, to: &'a Path) -> BoxFuture<'a, virtual_fs::Result<()>> {
    }
    fn metadata(&self, path: &Path) -> virtual_fs::Result<virtual_fs::Metadata> {}
    /// This method gets metadata without following symlinks in the path.
    /// Currently identical to `metadata` because symlinks aren't implemented
    /// yet.
    fn symlink_metadata(&self, path: &Path) -> virtual_fs::Result<virtual_fs::Metadata> {}
    fn remove_file(&self, path: &Path) -> virtual_fs::Result<()> {}

    fn new_open_options(&self) -> virtual_fs::OpenOptions {}

    fn mount(
        &self,
        name: String,
        path: &Path,
        fs: Box<dyn virtual_fs::FileSystem + Send + Sync>,
    ) -> virtual_fs::Result<()> {
    }
}

unsafe impl Send for IpfsFs {}

unsafe impl Sync for IpfsFs {}
