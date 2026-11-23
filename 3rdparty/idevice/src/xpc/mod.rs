// Jackson Coxson

use async_stream::try_stream;
use futures::Stream;
use http2::Setting;
use tracing::debug;

use crate::{CdTunnelError, IdeviceError, ReadWrite};

pub mod errors;
mod format;
mod http2;
pub mod xpc_macro;

use format::XPCFlag;
pub use format::{Dictionary, XPCMessage, XPCObject};

const ROOT_CHANNEL: u32 = 1;
const REPLY_CHANNEL: u32 = 3;

#[derive(Debug)]
pub struct RemoteXpcClient<R: ReadWrite> {
    h2_client: http2::Http2Client<R>,
    root_id: u64,
    // reply_id: u64 // maybe not used?
}

impl<R: ReadWrite> RemoteXpcClient<R> {
    pub async fn new(socket: R) -> Result<Self, IdeviceError> {
        Ok(Self {
            h2_client: http2::Http2Client::new(socket).await?,
            root_id: 1,
        })
    }

    pub async fn do_handshake(&mut self) -> Result<(), IdeviceError> {
        self.h2_client
            .set_settings(
                vec![
                    Setting::MaxConcurrentStreams(100),
                    Setting::InitialWindowSize(1048576),
                ],
                0,
            )
            .await?;
        self.h2_client.window_update(983041, 0).await?;
        self.h2_client.open_stream(1).await?; // root channel

        debug!("Sending empty dictionary");
        self.send_root(XPCMessage::new(
            Some(XPCFlag::AlwaysSet),
            Some(XPCObject::Dictionary(Default::default())),
            None,
        ))
        .await?;

        debug!("Opening reply stream");
        self.h2_client.open_stream(REPLY_CHANNEL).await?;
        self.send_reply(XPCMessage::new(
            Some(XPCFlag::InitHandshake | XPCFlag::AlwaysSet),
            None,
            None,
        ))
        .await?;

        debug!("Sending weird flags");
        self.send_root(XPCMessage::new(Some(XPCFlag::Custom(0x201)), None, None))
            .await?;

        Ok(())
    }

    pub async fn recv(&mut self) -> Result<plist::Value, IdeviceError> {
        self.recv_from_channel(REPLY_CHANNEL).await
    }

    pub async fn recv_root(&mut self) -> Result<plist::Value, IdeviceError> {
        self.recv_from_channel(ROOT_CHANNEL).await
    }

    async fn recv_from_channel(&mut self, channel: u32) -> Result<plist::Value, IdeviceError> {
        let mut msg_buffer = Vec::new();
        loop {
            msg_buffer.extend(self.h2_client.read(channel).await?);
            let msg = match XPCMessage::decode(&msg_buffer) {
                Ok(m) => m,
                Err(IdeviceError::CdTunnel(CdTunnelError::SizeMismatch)) => continue,
                Err(e) => break Err(e),
            };

            match msg.message {
                Some(msg) => {
                    if let Some(d) = msg.as_dictionary()
                        && d.is_empty()
                    {
                        msg_buffer.clear();
                        continue;
                    }
                    break Ok(msg.to_plist());
                }
                None => {
                    // don't care didn't ask
                    msg_buffer.clear();
                    continue;
                }
            }
        }
    }

    pub async fn send_object(
        &mut self,
        msg: impl Into<XPCObject>,
        expect_reply: bool,
    ) -> Result<(), IdeviceError> {
        let msg: XPCObject = msg.into();

        let mut flag = XPCFlag::DataFlag | XPCFlag::AlwaysSet;
        if expect_reply {
            flag |= XPCFlag::WantingReply;
        }

        let msg = XPCMessage::new(Some(flag), Some(msg), Some(self.root_id));
        self.send_root(msg).await?;

        Ok(())
    }

    async fn send_root(&mut self, msg: XPCMessage) -> Result<(), IdeviceError> {
        self.h2_client
            .send(msg.encode(self.root_id)?, ROOT_CHANNEL)
            .await?;
        Ok(())
    }

    async fn send_reply(&mut self, msg: XPCMessage) -> Result<(), IdeviceError> {
        self.h2_client
            .send(msg.encode(self.root_id)?, REPLY_CHANNEL)
            .await?;
        Ok(())
    }

    pub fn iter_file_chunks<'a>(
        &'a mut self,
        total_size: usize,
        file_idx: u32,
    ) -> impl Stream<Item = Result<Vec<u8>, IdeviceError>> + 'a {
        let stream_id = (file_idx + 1) * 2;

        try_stream! {
            fn strip_xpc_wrapper_prefix(buf: &[u8]) -> (&[u8], bool) {
                // Returns (data_after_wrapper, stripped_anything)
                const MAGIC: u32 = 0x29b00b92;

                if buf.len() < 24 {
                    return (buf, false);
                }

                let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
                if magic != MAGIC {
                    return (buf, false);
                }

                // flags at [4..8] – not needed to compute size
                let body_len = u64::from_le_bytes([
                    buf[8], buf[9], buf[10], buf[11], buf[12], buf[13], buf[14], buf[15],
                ]) as usize;

                let wrapper_len = 24 + body_len;
                if buf.len() < wrapper_len {
                    // Incomplete wrapper (shouldn’t happen with your read API), keep as-is.
                    return (buf, false);
                }

                (&buf[wrapper_len..], true)
            }
            self.open_file_stream_for_response(stream_id).await?;

            let mut got = 0usize;
            while got < total_size {
                let bytes = self.h2_client.read(stream_id).await?;
                let (after, stripped) = strip_xpc_wrapper_prefix(&bytes);
                if stripped && after.is_empty() {
                    continue; // pure control wrapper, don't count
                }

                let data = if stripped { after.to_vec() } else { bytes };

                if data.is_empty() {
                    continue;
                }

                got += data.len();
                yield data;
            }
        }
    }

    pub async fn open_file_stream_for_response(
        &mut self,
        stream_id: u32,
    ) -> Result<(), IdeviceError> {
        // 1) Open the HTTP/2 stream
        self.h2_client.open_stream(stream_id).await?;

        // 2) Send an empty XPC wrapper on that same stream with FILE_TX_STREAM_RESPONSE
        let flags = XPCFlag::AlwaysSet | XPCFlag::FileTxStreamResponse;

        let msg = XPCMessage::new(Some(flags), None, Some(0));

        // IMPORTANT: send on `stream_id`, not ROOT/REPLY
        let bytes = msg.encode(0)?;
        self.h2_client.send(bytes, stream_id).await?;
        Ok(())
    }
}
