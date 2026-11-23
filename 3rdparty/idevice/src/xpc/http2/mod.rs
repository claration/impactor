// Jackson Coxson

use frame::HttpFrame;
use std::collections::{HashMap, VecDeque};
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

use crate::{IdeviceError, ReadWrite};

pub mod frame;
pub use frame::Setting;

const HTTP2_MAGIC: &[u8] = "PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n".as_bytes();

#[derive(Debug)]
pub struct Http2Client<R: ReadWrite> {
    inner: R,
    cache: HashMap<u32, VecDeque<Vec<u8>>>,
}

impl<R: ReadWrite> Http2Client<R> {
    /// Writes the magic and inits the caches
    pub async fn new(mut inner: R) -> Result<Self, IdeviceError> {
        inner.write_all(HTTP2_MAGIC).await?;
        inner.flush().await?;
        Ok(Self {
            inner,
            cache: HashMap::new(),
        })
    }

    pub async fn set_settings(
        &mut self,
        settings: Vec<frame::Setting>,
        stream_id: u32,
    ) -> Result<(), IdeviceError> {
        let frame = frame::SettingsFrame {
            settings,
            stream_id,
            flags: 0,
        }
        .serialize();
        self.inner.write_all(&frame).await?;
        self.inner.flush().await?;
        Ok(())
    }

    pub async fn window_update(
        &mut self,
        increment_size: u32,
        stream_id: u32,
    ) -> Result<(), IdeviceError> {
        let frame = frame::WindowUpdateFrame {
            increment_size,
            stream_id,
        }
        .serialize();
        self.inner.write_all(&frame).await?;
        self.inner.flush().await?;
        Ok(())
    }

    pub async fn open_stream(&mut self, stream_id: u32) -> Result<(), IdeviceError> {
        // Sometimes Apple is silly and sends data to a stream that isn't open
        self.cache.entry(stream_id).or_default();
        let frame = frame::HeadersFrame { stream_id }.serialize();
        self.inner.write_all(&frame).await?;
        self.inner.flush().await?;
        Ok(())
    }

    pub async fn send(&mut self, payload: Vec<u8>, stream_id: u32) -> Result<(), IdeviceError> {
        let frame = frame::DataFrame { stream_id, payload }.serialize();
        self.inner.write_all(&frame).await?;
        self.inner.flush().await?;
        Ok(())
    }

    pub async fn read(&mut self, stream_id: u32) -> Result<Vec<u8>, IdeviceError> {
        // See if we already have a cached message from another read
        match self.cache.get_mut(&stream_id) {
            Some(c) => {
                if let Some(d) = c.pop_front() {
                    return Ok(d);
                }
            }
            None => {
                self.cache.insert(stream_id, VecDeque::new());
            }
        };

        // handle packets until we get what we want
        loop {
            let frame = frame::Frame::next(&mut self.inner).await?;
            // debug!("Got frame: {frame:#?}");
            match frame {
                frame::Frame::Settings(settings_frame) if settings_frame.flags != 1 => {
                    // ack that
                    let frame = frame::SettingsFrame {
                        settings: Vec::new(),
                        stream_id: settings_frame.stream_id,
                        flags: 1,
                    }
                    .serialize();
                    self.inner.write_all(&frame).await?;
                    self.inner.flush().await?;
                }
                frame::Frame::Data(data_frame) => {
                    debug!(
                        "Got data frame for {} with {} bytes",
                        data_frame.stream_id,
                        data_frame.payload.len()
                    );

                    if data_frame.stream_id % 2 == 0 {
                        self.window_update(data_frame.payload.len() as u32, 0)
                            .await?;
                        self.window_update(data_frame.payload.len() as u32, data_frame.stream_id)
                            .await?;
                    }
                    if data_frame.stream_id == stream_id {
                        return Ok(data_frame.payload);
                    } else {
                        let c = match self.cache.get_mut(&data_frame.stream_id) {
                            Some(c) => c,
                            None => {
                                // Sometimes Apple is a little silly and sends data before the
                                // stream is open.
                                warn!(
                                    "Received message for stream ID {} not in cache",
                                    data_frame.stream_id
                                );
                                self.cache.insert(data_frame.stream_id, VecDeque::new());
                                self.cache.get_mut(&data_frame.stream_id).unwrap()
                            }
                        };
                        c.push_back(data_frame.payload);
                    }
                }
                _ => {
                    // do nothing, we shouldn't receive these frames
                }
            }
        }
    }
}
