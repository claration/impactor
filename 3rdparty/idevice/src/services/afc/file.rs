// Jackson Coxson

use std::{io::SeekFrom, marker::PhantomPinned, pin::Pin};

use tokio::io::{AsyncRead, AsyncSeek, AsyncWrite};

use crate::{
    IdeviceError,
    afc::{
        AfcClient,
        inner_file::{InnerFileDescriptor, OwnedInnerFileDescriptor},
    },
};

/// Handle for an open file on the device.
///
/// This handle implements **async `Drop`** to automatically close the file for users who forget,
/// but only on a **Tokio multi-threaded runtime**
///
///
/// Consider calling `.close()`, it won't drop if expliclity closed
#[derive(Debug)]
pub struct FileDescriptor<'a> {
    inner: Pin<Box<InnerFileDescriptor<'a>>>,
}

/// Owned handle for an open file on the device.
///
/// This handle implements **async `Drop`** to automatically close the file for users who forget,
/// but only on a **Tokio multi-threaded runtime**
///
///
/// Consider calling `.close()`, it won't drop if expliclity closed
#[derive(Debug)]
pub struct OwnedFileDescriptor {
    inner: Pin<Box<OwnedInnerFileDescriptor>>,
}

impl<'a> FileDescriptor<'a> {
    /// create a new FileDescriptor from a raw fd
    ///
    /// # Safety
    /// make sure the fd is an opened file, and that you got it from a previous
    /// FileDescriptor via `as_raw_fd()` method
    pub unsafe fn new(client: &'a mut AfcClient, fd: u64, path: String) -> Self {
        Self {
            inner: Box::pin(InnerFileDescriptor {
                client,
                fd,
                path,
                pending_fut: None,
                _m: PhantomPinned,
                dropped: false,
            }),
        }
    }

    /// Closes the file descriptor
    pub async fn close(self) -> Result<(), IdeviceError> {
        self.inner.close().await
    }
}

impl OwnedFileDescriptor {
    /// create a new OwnedFileDescriptor from a raw fd
    ///
    /// # Safety
    /// make sure the fd is an opened file, and that you got it from a previous
    /// OwnedFileDescriptor via `as_raw_fd()` method
    pub unsafe fn new(client: AfcClient, fd: u64, path: String) -> Self {
        Self {
            inner: Box::pin(OwnedInnerFileDescriptor {
                client,
                fd,
                path,
                pending_fut: None,
                _m: PhantomPinned,
                dropped: false,
            }),
        }
    }

    /// Closes the file descriptor
    pub async fn close(self) -> Result<AfcClient, IdeviceError> {
        self.inner.close().await
    }

    /// gets the owned afc
    ///
    /// # Safety
    /// this get's the afc out without closing, if you want to get the afc and close the file, use
    /// `.close()`
    pub unsafe fn get_inner_afc(self) -> AfcClient {
        self.inner.get_inner_afc()
    }
}

crate::impl_to_structs!(FileDescriptor<'_>, OwnedFileDescriptor;  {
    pub fn as_raw_fd(&self) -> u64 {
        self.inner.fd
    }

    /// Returns the current cursor position for the file
    pub async fn seek_tell(&mut self) -> Result<u64, IdeviceError> {
        self.inner.as_mut().seek_tell().await
    }

    /// Reads the entire contents of the file
    ///
    /// # Returns
    /// A vector containing the file's data
    pub async fn read_entire(&mut self) -> Result<Vec<u8>, IdeviceError> {
        self.inner.as_mut().read().await
    }

    /// Writes data to the file
    ///
    /// # Arguments
    /// * `bytes` - Data to write to the file
    pub async fn write_entire(&mut self, bytes: &[u8]) -> Result<(), IdeviceError> {
        self.inner.as_mut().write(bytes).await
    }
});

crate::impl_trait_to_structs!(AsyncRead for FileDescriptor<'_>, OwnedFileDescriptor; {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let inner = self.inner.as_mut();
        inner.poll_read(cx, buf)
    }
});

crate::impl_trait_to_structs!(AsyncWrite for FileDescriptor<'_>, OwnedFileDescriptor; {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        let inner = self.inner.as_mut();
        inner.poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        let inner = self.inner.as_mut();
        inner.poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        let inner = self.inner.as_mut();
        inner.poll_shutdown(cx)
    }
});

crate::impl_trait_to_structs!(AsyncSeek for FileDescriptor<'_>, OwnedFileDescriptor; {
    fn start_seek(mut self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
        let this = self.inner.as_mut();
        this.start_seek(position)
    }

    fn poll_complete(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<u64>> {
        let this = self.inner.as_mut();
        this.poll_complete(cx)
    }
});
