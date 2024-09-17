use futures::future::BoxFuture;
use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, SeekFrom};
use std::marker::{Send, Sync};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncSeek, AsyncWrite, ReadBuf};

use wasmer_wasix::virtual_fs;

use net::ipfs::Client;

pub struct IpfsFs {
    client: Client,
}

impl IpfsFs {
    pub fn new(client: Client) -> IpfsFs {
        return IpfsFs { client: client };
    }
}

// We need to implement Debug to ble able to implement the other traits.
impl fmt::Debug for IpfsFs {
    fn fmt(&self, _: &mut fmt::Formatter) -> fmt::Result {
        Ok(())
    }
}

// TODO we'll likely need to create a separate struct for opening files (1).
impl virtual_fs::FileOpener for IpfsFs {
    fn open(
        &self,
        path: &Path,
        conf: &virtual_fs::OpenOptionsConfig,
    ) -> virtual_fs::Result<Box<dyn virtual_fs::VirtualFile + Send + Sync + 'static>> {
        Ok(Box::new(IpfsFile {}))
    }
}

impl virtual_fs::FileSystem for IpfsFs {
    fn readlink(&self, path: &Path) -> virtual_fs::Result<PathBuf> {
        Ok(PathBuf::new())
    }

    fn read_dir(&self, path: &Path) -> virtual_fs::Result<virtual_fs::ReadDir> {
        Err(virtual_fs::FsError::UnknownError)
    }

    fn create_dir(&self, path: &Path) -> virtual_fs::Result<()> {
        Ok(())
    }
    fn remove_dir(&self, path: &Path) -> virtual_fs::Result<()> {
        Ok(())
    }
    fn rename<'a>(&'a self, from: &'a Path, to: &'a Path) -> BoxFuture<'a, virtual_fs::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn metadata(&self, path: &Path) -> virtual_fs::Result<virtual_fs::Metadata> {
        Ok(virtual_fs::Metadata::default())
    }

    fn symlink_metadata(&self, path: &Path) -> virtual_fs::Result<virtual_fs::Metadata> {
        Ok(virtual_fs::Metadata::default())
    }

    fn remove_file(&self, path: &Path) -> virtual_fs::Result<()> {
        Ok(())
    }

    fn new_open_options(&self) -> virtual_fs::OpenOptions {
        // TODO we'll likely need to create a separate struct for opening files (2).
        virtual_fs::OpenOptions::new(self)
    }

    fn mount(
        &self,
        name: String,
        path: &Path,
        fs: Box<dyn virtual_fs::FileSystem + Send + Sync>,
    ) -> virtual_fs::Result<()> {
        Ok(())
    }
}

unsafe impl Send for IpfsFs {}

unsafe impl Sync for IpfsFs {}

pub struct IpfsFile {}

impl fmt::Debug for IpfsFile {
    fn fmt(&self, _: &mut fmt::Formatter) -> fmt::Result {
        Ok(())
    }
}

impl AsyncRead for IpfsFile {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

impl AsyncSeek for IpfsFile {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> io::Result<()> {
        Ok(())
    }

    fn poll_complete(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<u64>> {
        Poll::Ready(Ok(0))
    }
}

impl AsyncWrite for IpfsFile {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        Poll::Ready(Ok(0))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }
}

unsafe impl Send for IpfsFile {}

impl Unpin for IpfsFile {}

impl virtual_fs::VirtualFile for IpfsFile {
    fn last_accessed(&self) -> u64 {
        0
    }

    fn last_modified(&self) -> u64 {
        0
    }

    fn created_time(&self) -> u64 {
        0
    }

    #[allow(unused_variables)]
    fn set_times(&mut self, atime: Option<u64>, mtime: Option<u64>) -> virtual_fs::Result<()> {
        Ok(())
    }

    fn size(&self) -> u64 {
        0
    }

    fn set_len(&mut self, new_size: u64) -> virtual_fs::Result<()> {
        Ok(())
    }

    fn unlink(&mut self) -> virtual_fs::Result<()> {
        Ok(())
    }

    fn is_open(&self) -> bool {
        true
    }

    fn get_special_fd(&self) -> Option<u32> {
        None
    }

    fn write_from_mmap(&mut self, _offset: u64, _len: u64) -> std::io::Result<()> {
        Err(std::io::ErrorKind::Unsupported.into())
    }

    fn copy_reference(
        &mut self,
        mut src: Box<dyn virtual_fs::VirtualFile + Send + Sync + 'static>,
    ) -> BoxFuture<'_, std::io::Result<()>> {
        Box::pin(async move {
            let bytes_written = tokio::io::copy(&mut src, self).await?;
            tracing::trace!(bytes_written, "Copying file into host filesystem",);
            Ok(())
        })
    }

    fn poll_read_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(0))
    }

    fn poll_write_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<usize>> {
        Poll::Ready(Ok(0))
    }
}
