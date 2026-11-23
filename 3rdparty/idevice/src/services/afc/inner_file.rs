// Jackson Coxson

use std::{io::SeekFrom, pin::Pin};

use futures::{FutureExt, future::BoxFuture};
use tokio::io::{AsyncRead, AsyncSeek, AsyncWrite};

use crate::{
    IdeviceError,
    afc::{
        AfcClient, MAGIC,
        opcode::AfcOpcode,
        packet::{AfcPacket, AfcPacketHeader},
    },
};

/// Maximum transfer size for file operations (1MB)
const MAX_TRANSFER: u64 = 1024 * 1024; // this is what libimobiledevice uses in afcclient

fn chunk_number(n: usize, chunk_size: usize) -> impl Iterator<Item = usize> {
    (0..n)
        .step_by(chunk_size)
        .map(move |i| (n - i).min(chunk_size))
}

/// Descripes what the future returns
#[derive(Debug)]
pub(crate) enum PendingResult {
    // writing
    Empty,
    // seeking
    SeekPos(u64),
    // reading
    Bytes(Vec<u8>),
}

type OwnedBoxFuture = Pin<Box<dyn Future<Output = Result<PendingResult, IdeviceError>> + Send>>;

pub(crate) struct InnerFileDescriptor<'a> {
    pub(crate) client: &'a mut AfcClient,
    pub(crate) fd: u64,
    pub(crate) path: String,

    pub(crate) pending_fut: Option<BoxFuture<'a, Result<PendingResult, IdeviceError>>>,
    pub(crate) _m: std::marker::PhantomPinned,

    pub(crate) dropped: bool,
}

pub(crate) struct OwnedInnerFileDescriptor {
    pub(crate) client: AfcClient,
    pub(crate) fd: u64,
    pub(crate) path: String,

    pub(crate) pending_fut: Option<OwnedBoxFuture>,
    pub(crate) _m: std::marker::PhantomPinned,

    pub(crate) dropped: bool,
}

crate::impl_to_structs!(InnerFileDescriptor<'_>, OwnedInnerFileDescriptor; {
    /// Generic helper to send an AFC packet and read the response
    pub async fn send_packet(
        self: Pin<&mut Self>,
        opcode: AfcOpcode,
        header_payload: Vec<u8>,
        payload: Vec<u8>,
    ) -> Result<AfcPacket, IdeviceError> {
        // SAFETY: we don't modify pinned fileds, it's ok
        let this = unsafe { self.get_unchecked_mut() };

        let header_len = header_payload.len() as u64 + AfcPacketHeader::LEN;
        let header = AfcPacketHeader {
            magic: MAGIC,
            entire_len: header_len + payload.len() as u64,
            header_payload_len: header_len,
            packet_num: this.client.package_number,
            operation: opcode,
        };
        this.client.package_number += 1;

        let packet = AfcPacket {
            header,
            header_payload,
            payload,
        };

        this.client.send(packet).await?;
        this.client.read().await
    }

    /// Returns the current cursor position for the file
    pub async fn seek_tell(self: Pin<&mut Self>) -> Result<u64, IdeviceError> {
        let header_payload = self.fd.to_le_bytes().to_vec();
        let res = self
            .send_packet(AfcOpcode::FileTell, header_payload, Vec::new())
            .await?;

        let cur_pos = res
            .header_payload
            .get(..8)
            .ok_or(IdeviceError::UnexpectedResponse("AFC FileTell response missing position bytes".into()))?
            .try_into()
            .map(u64::from_le_bytes)
            .map_err(|_| IdeviceError::UnexpectedResponse("AFC FileTell position bytes invalid length".into()))?;

        Ok(cur_pos)
    }

    /// Moves the file cursor
    async fn seek(mut self: Pin<&mut Self>, pos: SeekFrom) -> Result<u64, IdeviceError> {
        let (offset, whence) = match pos {
            SeekFrom::Start(off) => (off as i64, 0),
            SeekFrom::Current(off) => (off, 1),
            SeekFrom::End(off) => (off, 2),
        };

        let header_payload = [
            self.fd.to_le_bytes(),
            (whence as u64).to_le_bytes(),
            offset.to_le_bytes(),
        ]
        .concat();

        self.as_mut()
            .send_packet(AfcOpcode::FileSeek, header_payload, Vec::new())
            .await?;

        self.as_mut().seek_tell().await
    }


    /// Reads n size of contents from the file
    ///
    /// # Arguments
    /// * `n` - amount of bytes to read
    /// # Returns
    /// A vector containing the file's data
    pub async fn read_n(mut self: Pin<&mut Self>, n: usize) -> Result<Vec<u8>, IdeviceError> {
        let mut collected_bytes = Vec::with_capacity(n);

        for chunk in chunk_number(n, MAX_TRANSFER as usize) {
            let header_payload = [self.fd.to_le_bytes(), (chunk as u64).to_le_bytes()].concat();
            let res = self
                .as_mut()
                .send_packet(AfcOpcode::Read, header_payload, Vec::new())
                .await?;

            collected_bytes.extend(res.payload);
        }
        Ok(collected_bytes)
    }

    /// Reads the entire contents of the file
    ///
    /// # Returns
    /// A vector containing the file's data
    pub async fn read(mut self: Pin<&mut Self>) -> Result<Vec<u8>, IdeviceError> {
        let seek_pos = self.as_mut().seek_tell().await? as usize;

        let file_info = unsafe {
            let this = self.as_mut().get_unchecked_mut();

            this.client.get_file_info(&this.path).await?
        };

        let mut bytes_left = file_info.size.saturating_sub(seek_pos);
        let mut collected_bytes = Vec::with_capacity(bytes_left);

        while bytes_left > 0 {
            let bytes = self.as_mut().read_n(MAX_TRANSFER as usize).await?;

            bytes_left -= bytes.len();
            collected_bytes.extend(bytes);
        }

        Ok(collected_bytes)
    }

    /// Writes data to the file
    ///
    /// # Arguments
    /// * `bytes` - Data to write to the file
    pub async fn write(mut self: Pin<&mut Self>, bytes: &[u8]) -> Result<(), IdeviceError> {
        for chunk in bytes.chunks(MAX_TRANSFER as usize) {
            let header_payload = self.as_ref().fd.to_le_bytes().to_vec();
            self.as_mut()
                .send_packet(AfcOpcode::Write, header_payload, chunk.to_vec())
                .await?;
        }
        Ok(())
    }

    fn store_pending_read(mut self: Pin<&mut Self>, buf_rem: usize) {
        unsafe {
            let this = self.as_mut().get_unchecked_mut() as *mut Self;

            let fut = Some(
                // SAFETY: we already know that self is pinned
                Pin::new_unchecked(&mut *this)
                    .read_n(buf_rem)
                    .map(|r| r.map(PendingResult::Bytes))
                    .boxed(),
            );

            (&mut *this).pending_fut = fut;
        }
    }

    fn store_pending_seek(mut self: Pin<&mut Self>, position: std::io::SeekFrom) {
        unsafe {
            let this = self.as_mut().get_unchecked_mut() as *mut Self;

            let fut = Some(
                Pin::new_unchecked(&mut *this)
                    .seek(position)
                    .map(|r| r.map(PendingResult::SeekPos))
                    .boxed(),
            );

            (&mut *this).pending_fut = fut;
        }
    }

    fn store_pending_write(mut self: Pin<&mut Self>, buf: &'_ [u8]) {
        unsafe {
            let this = self.as_mut().get_unchecked_mut();

            let this = this as *mut Self;

            // move the entire buffer into the future so we don't have to store it somewhere
            let pined_this = Pin::new_unchecked(&mut *this);
            let buf = buf.to_vec();
            let fut =
                async move { pined_this.write(&buf).await.map(|_| PendingResult::Empty) }.boxed();

            (&mut *this).pending_fut = Some(fut);
        }
    }
});

impl<'a> InnerFileDescriptor<'a> {
    fn get_or_init_read_fut(
        mut self: Pin<&mut Self>,
        buf_rem: usize,
    ) -> &mut BoxFuture<'a, Result<PendingResult, IdeviceError>> {
        if self.as_ref().pending_fut.is_none() {
            self.as_mut().store_pending_read(buf_rem);
        }

        unsafe { self.get_unchecked_mut().pending_fut.as_mut().unwrap() }
    }

    fn get_or_init_write_fut(
        mut self: Pin<&mut Self>,
        buf: &'_ [u8],
    ) -> &mut BoxFuture<'a, Result<PendingResult, IdeviceError>> {
        if self.as_ref().pending_fut.is_none() {
            self.as_mut().store_pending_write(buf);
        }

        unsafe { self.get_unchecked_mut().pending_fut.as_mut().unwrap() }
    }

    fn get_seek_fut(
        self: Pin<&mut Self>,
    ) -> Option<&mut BoxFuture<'a, Result<PendingResult, IdeviceError>>> {
        unsafe { self.get_unchecked_mut().pending_fut.as_mut() }
    }

    fn remove_pending_fut(mut self: Pin<&mut Self>) {
        unsafe {
            self.as_mut().get_unchecked_mut().pending_fut.take();
        }
    }

    /// Closes the file descriptor
    pub async fn close(mut self: Pin<Box<Self>>) -> Result<(), IdeviceError> {
        self.as_mut().close_inner().await
    }

    async fn close_inner(mut self: Pin<&mut Self>) -> Result<(), IdeviceError> {
        let header_payload = self.fd.to_le_bytes().to_vec();

        self.as_mut()
            .send_packet(AfcOpcode::FileClose, header_payload, Vec::new())
            .await?;

        unsafe { Pin::into_inner_unchecked(self).dropped = true }
        Ok(())
    }
}

impl OwnedInnerFileDescriptor {
    fn get_or_init_read_fut(mut self: Pin<&mut Self>, buf_rem: usize) -> &mut OwnedBoxFuture {
        if self.as_ref().pending_fut.is_none() {
            self.as_mut().store_pending_read(buf_rem);
        }

        unsafe { self.get_unchecked_mut().pending_fut.as_mut().unwrap() }
    }

    fn get_or_init_write_fut(mut self: Pin<&mut Self>, buf: &'_ [u8]) -> &mut OwnedBoxFuture {
        if self.as_ref().pending_fut.is_none() {
            self.as_mut().store_pending_write(buf);
        }

        unsafe { self.get_unchecked_mut().pending_fut.as_mut().unwrap() }
    }

    fn get_seek_fut(self: Pin<&mut Self>) -> Option<&mut OwnedBoxFuture> {
        unsafe { self.get_unchecked_mut().pending_fut.as_mut() }
    }

    fn remove_pending_fut(mut self: Pin<&mut Self>) {
        unsafe {
            self.as_mut().get_unchecked_mut().pending_fut.take();
        }
    }

    /// Closes the file descriptor
    pub async fn close(mut self: Pin<Box<Self>>) -> Result<AfcClient, IdeviceError> {
        self.as_mut().close_inner().await
    }

    async fn close_inner(mut self: Pin<&mut Self>) -> Result<AfcClient, IdeviceError> {
        let header_payload = self.fd.to_le_bytes().to_vec();

        self.as_mut()
            .send_packet(AfcOpcode::FileClose, header_payload, Vec::new())
            .await?;

        Ok(self.into_inner_afc())
    }

    fn into_inner_afc(mut self: Pin<&mut Self>) -> AfcClient {
        let this = unsafe { Pin::into_inner_unchecked(self.as_mut()) };

        this.dropped = true;

        let dummy_afc = AfcClient::new(crate::Idevice::new(
            Box::new(std::io::Cursor::new(vec![])),
            "67",
        ));

        // the `.drop()` won't use the `self.client` if we already dropped it (or don't want to
        // drop it)
        std::mem::replace(&mut this.client, dummy_afc)
    }

    pub fn get_inner_afc(mut self: Pin<Box<Self>>) -> AfcClient {
        self.as_mut().into_inner_afc()
    }
}

crate::impl_trait_to_structs!(AsyncRead for InnerFileDescriptor<'_>, OwnedInnerFileDescriptor; {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let contents = {
            let read_func = self.as_mut().get_or_init_read_fut(buf.remaining());
            match std::task::ready!(read_func.as_mut().poll(cx)) {
                Ok(PendingResult::Bytes(c)) => {
                    self.as_mut().remove_pending_fut();
                    c
                }
                Err(e) => return std::task::Poll::Ready(Err(std::io::Error::other(e.to_string()))),

                _ => unreachable!("a non read future was stored, this shouldn't happen"),
            }
        };

        buf.put_slice(&contents);

        std::task::Poll::Ready(Ok(()))
    }
});

crate::impl_trait_to_structs!(AsyncWrite for InnerFileDescriptor<'_>, OwnedInnerFileDescriptor; {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<Result<usize, std::io::Error>> {
        let write_func = self.as_mut().get_or_init_write_fut(buf);

        match std::task::ready!(write_func.as_mut().poll(cx)) {
            Ok(PendingResult::Empty) => self.as_mut().remove_pending_fut(),
            Err(e) => {
                println!("error: {e}");
                return std::task::Poll::Ready(Err(std::io::Error::other(e.to_string())));
            }

            _ => unreachable!("a non write future was stored, this shouldn't happen"),
        }

        std::task::Poll::Ready(Ok(buf.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), std::io::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

});

crate::impl_trait_to_structs!(AsyncSeek for InnerFileDescriptor<'_>, OwnedInnerFileDescriptor; {
    fn start_seek(self: Pin<&mut Self>, position: SeekFrom) -> std::io::Result<()> {
        self.store_pending_seek(position);

        Ok(())
    }

    fn poll_complete(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<u64>> {
        let Some(fut) = self.as_mut().get_seek_fut() else {
            // tokio runs the `poll_complete` before the `start_seek` to ensure no previous seek is in progress
            return std::task::Poll::Ready(Ok(0));
        };

        match std::task::ready!(fut.as_mut().poll(cx)) {
            Ok(PendingResult::SeekPos(pos)) => {
                self.as_mut().remove_pending_fut();
                std::task::Poll::Ready(Ok(pos))
            }
            Err(e) => std::task::Poll::Ready(Err(std::io::Error::other(e.to_string()))),
            _ => unreachable!("a non seek future was stored, this shouldn't happen"),
        }
    }
});

crate::impl_trait_to_structs!(Drop for InnerFileDescriptor<'_>, OwnedInnerFileDescriptor; {
    fn drop(&mut self) {
        if !self.dropped {
            // The pending_fut (if Some) holds a Pin<&mut Self> derived from a
            // raw pointer to this struct. Dropping it here ensures that
            // mutable reference is released before we create a second one via
            // Pin::new_unchecked(self) below. Two live &mut Self to the same
            // struct is UB under Stacked Borrows even if neither is actively
            // dereferenced.
            self.pending_fut = None;

            let handle = tokio::runtime::Handle::current();

            if matches!(
                handle.runtime_flavor(),
                tokio::runtime::RuntimeFlavor::CurrentThread
            ) {
                return;
            }

            tokio::task::block_in_place(move || {
                handle.block_on(async move {
                    unsafe { Pin::new_unchecked(self) }
                        .close_inner()
                        .await
                        .ok();
                })
            });
        }
    }
});

impl std::fmt::Debug for InnerFileDescriptor<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InnerFileDescriptor")
            .field("client", &self.client)
            .field("fd", &self.fd)
            .field("path", &self.path)
            .finish()
    }
}

impl std::fmt::Debug for OwnedInnerFileDescriptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OwnedInnerFileDescriptor")
            .field("client", &self.client)
            .field("fd", &self.fd)
            .field("path", &self.path)
            .finish()
    }
}
