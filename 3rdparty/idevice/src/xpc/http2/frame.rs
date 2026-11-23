// Jackson Coxson

use crate::xpc::errors::XpcError;
use crate::{IdeviceError, ReadWrite};
use tokio::io::AsyncReadExt;

pub trait HttpFrame {
    fn serialize(&self) -> Vec<u8>;
}

#[derive(Debug)]
#[allow(dead_code)] // we don't care about frames from the device
pub enum Frame {
    Settings(SettingsFrame),
    WindowUpdate(WindowUpdateFrame),
    Headers(HeadersFrame),
    Data(DataFrame),
}

impl Frame {
    pub async fn next(mut socket: &mut impl ReadWrite) -> Result<Self, IdeviceError> {
        // Read the len of the frame
        let mut buf = [0u8; 3];
        tokio::io::AsyncReadExt::read_exact(&mut socket, &mut buf).await?;
        let frame_len = u32::from_be_bytes([0x00, buf[0], buf[1], buf[2]]);

        // Read the fields
        let frame_type = socket.read_u8().await?;
        let flags = socket.read_u8().await?;
        let stream_id = socket.read_u32().await?;

        let mut body = vec![0; frame_len as usize];
        socket.read_exact(&mut body).await?;

        Ok(match frame_type {
            0x00 => {
                // data
                Self::Data(DataFrame {
                    stream_id,
                    payload: body,
                })
            }
            0x01 => {
                // headers
                Self::Headers(HeadersFrame { stream_id })
            }
            0x03 => return Err(XpcError::HttpStreamReset.into()),
            0x04 => {
                // settings
                let mut body = std::io::Cursor::new(body);
                let mut settings = Vec::new();

                while let Ok(setting_type) = body.read_u16().await {
                    settings.push(match setting_type {
                        0x03 => {
                            let max_streams = body.read_u32().await?;
                            Setting::MaxConcurrentStreams(max_streams)
                        }
                        0x04 => {
                            let window_size = body.read_u32().await?;
                            Setting::InitialWindowSize(window_size)
                        }
                        _ => {
                            return Err(XpcError::UnknownHttpSetting(setting_type).into());
                        }
                    });
                }
                Self::Settings(SettingsFrame {
                    settings,
                    stream_id,
                    flags,
                })
            }
            0x07 => {
                let msg = if body.len() < 8 {
                    "<MISSING>".to_string()
                } else {
                    String::from_utf8_lossy(&body[8..]).to_string()
                };
                return Err(XpcError::HttpGoAway(msg).into());
            }
            0x08 => {
                // window update
                if body.len() != 4 {
                    return Err(IdeviceError::UnexpectedResponse(
                        "HTTP/2 window update frame body was not 4 bytes".into(),
                    ));
                }

                let window = u32::from_be_bytes([body[0], body[1], body[2], body[3]]);
                Self::WindowUpdate(WindowUpdateFrame {
                    increment_size: window,
                    stream_id,
                })
            }
            _ => {
                return Err(XpcError::UnknownFrame(frame_type).into());
            }
        })
    }
}

#[derive(Debug, Clone)]
pub struct SettingsFrame {
    pub settings: Vec<Setting>,
    pub stream_id: u32,
    pub flags: u8,
}

#[derive(Debug, Clone)]
pub enum Setting {
    MaxConcurrentStreams(u32),
    InitialWindowSize(u32),
}

impl Setting {
    fn serialize(&self) -> Vec<u8> {
        match self {
            Setting::MaxConcurrentStreams(m) => {
                let mut res = vec![0x00, 0x03];
                res.extend(m.to_be_bytes());
                res
            }
            Setting::InitialWindowSize(s) => {
                let mut res = vec![0x00, 0x04];
                res.extend(s.to_be_bytes());
                res
            }
        }
    }
}

impl HttpFrame for SettingsFrame {
    fn serialize(&self) -> Vec<u8> {
        let settings = self
            .settings
            .iter()
            .map(|x| x.serialize())
            .collect::<Vec<Vec<u8>>>()
            .concat();
        let settings_len = (settings.len() as u32).to_be_bytes();
        let mut res = vec![
            settings_len[1],
            settings_len[2],
            settings_len[3],
            0x04,
            self.flags,
        ];
        res.extend(self.stream_id.to_be_bytes());
        res.extend(settings);
        res
    }
}

#[derive(Debug, Clone)]
pub struct WindowUpdateFrame {
    pub increment_size: u32,
    pub stream_id: u32,
}

impl HttpFrame for WindowUpdateFrame {
    fn serialize(&self) -> Vec<u8> {
        let mut res = vec![0x00, 0x00, 0x04, 0x08, 0x00]; // size, frame ID, flags
        res.extend(self.stream_id.to_be_bytes());
        res.extend(self.increment_size.to_be_bytes());
        res
    }
}

#[derive(Debug, Clone)]
/// We don't actually care about this frame according to spec. This is just to open new channels.
pub struct HeadersFrame {
    pub stream_id: u32,
}

impl HttpFrame for HeadersFrame {
    fn serialize(&self) -> Vec<u8> {
        let mut res = vec![0x00, 0x00, 0x00, 0x01, 0x04];
        res.extend(self.stream_id.to_be_bytes());
        res
    }
}

#[derive(Debug, Clone)]
pub struct DataFrame {
    pub stream_id: u32,
    pub payload: Vec<u8>,
}

impl HttpFrame for DataFrame {
    fn serialize(&self) -> Vec<u8> {
        let mut res = (self.payload.len() as u32).to_be_bytes().to_vec();
        res.remove(0); // only 3 significant bytes
        res.extend([0x00, 0x00]); // frame type, flags
        res.extend(self.stream_id.to_be_bytes());
        res.extend(self.payload.clone());
        res
    }
}
