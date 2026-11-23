//! Abstraction for BTPacketLogger
//! You must have the Bluetooth profile installed, or you'll get no data.
//! https://developer.apple.com/bug-reporting/profiles-and-logs/?name=bluetooth

use std::pin::Pin;

use futures::Stream;
use tracing::warn;

use crate::{Idevice, IdeviceError, IdeviceService, obf};

/// Client for interacting with the BTPacketLogger service on the device.
/// You must have the Bluetooth profile installed, or you'll get no data.
///
/// ``https://developer.apple.com/bug-reporting/profiles-and-logs/?name=bluetooth``
#[derive(Debug)]
pub struct BtPacketLoggerClient {
    /// The underlying device connection with established logger service
    pub idevice: Idevice,
}

#[derive(Debug, Clone)]
pub struct BtFrame {
    pub hdr: BtHeader,
    pub kind: BtPacketKind,
    /// H4-ready payload (first byte is H4 type: 0x01 cmd, 0x02 ACL, 0x03 SCO, 0x04 evt)
    pub h4: Vec<u8>,
}

#[derive(Debug, Clone, Copy)]
pub struct BtHeader {
    /// Advisory length for [kind + payload]; may not equal actual frame len - 12
    pub length: u32, // BE on the wire
    pub ts_secs: u32,  // BE
    pub ts_usecs: u32, // BE
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BtPacketKind {
    HciCmd,  // 0x00
    HciEvt,  // 0x01
    AclSent, // 0x02
    AclRecv, // 0x03
    ScoSent, // 0x08
    ScoRecv, // 0x09
    Other(u8),
}

impl BtPacketKind {
    fn from_byte(b: u8) -> Self {
        match b {
            0x00 => BtPacketKind::HciCmd,
            0x01 => BtPacketKind::HciEvt,
            0x02 => BtPacketKind::AclSent,
            0x03 => BtPacketKind::AclRecv,
            0x08 => BtPacketKind::ScoSent,
            0x09 => BtPacketKind::ScoRecv,
            x => BtPacketKind::Other(x),
        }
    }
    fn h4_type(self) -> Option<u8> {
        match self {
            BtPacketKind::HciCmd => Some(0x01),
            BtPacketKind::AclSent | BtPacketKind::AclRecv => Some(0x02),
            BtPacketKind::ScoSent | BtPacketKind::ScoRecv => Some(0x03),
            BtPacketKind::HciEvt => Some(0x04),
            BtPacketKind::Other(_) => None,
        }
    }
}

impl IdeviceService for BtPacketLoggerClient {
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.bluetooth.BTPacketLogger")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl BtPacketLoggerClient {
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    /// Read a single *outer* frame and return one parsed record from it.
    /// (This service typically delivers one record per frame.)
    pub async fn next_packet(
        &mut self,
    ) -> Result<Option<(BtHeader, BtPacketKind, Vec<u8>)>, IdeviceError> {
        // 2-byte outer length is **little-endian**
        let len = self.idevice.read_raw(2).await?;
        if len.len() != 2 {
            return Ok(None); // EOF
        }
        let frame_len = u16::from_le_bytes([len[0], len[1]]) as usize;

        if !(13..=64 * 1024).contains(&frame_len) {
            return Err(IdeviceError::UnexpectedResponse(
                "BT frame length out of valid range (13..=65536)".into(),
            ));
        }

        let frame = self.idevice.read_raw(frame_len).await?;
        if frame.len() != frame_len {
            return Err(IdeviceError::NotEnoughBytes(frame.len(), frame_len));
        }

        // Parse header at fixed offsets (BE u32s)
        let (hdr, off) = BtHeader::parse(&frame).ok_or(IdeviceError::UnexpectedResponse(
            "failed to parse BT packet header".into(),
        ))?;
        // packet_type at byte 12, payload starts at 13
        let kind = BtPacketKind::from_byte(frame[off]);
        let payload = &frame[off + 1..]; // whatever remains

        // Build H4 buffer (prepend type byte)
        let mut h4 = Vec::with_capacity(1 + payload.len());
        if let Some(t) = kind.h4_type() {
            h4.push(t);
        } else {
            return Ok(None);
        }
        h4.extend_from_slice(payload);

        Ok(Some((hdr, kind, h4)))
    }

    /// Continuous stream of parsed frames.
    pub fn into_stream(
        mut self,
    ) -> Pin<Box<dyn Stream<Item = Result<BtFrame, IdeviceError>> + Send>> {
        Box::pin(async_stream::try_stream! {
            loop {
                // outer length (LE)
                let len = self.idevice.read_raw(2).await?;
                if len.len() != 2 { break; }
                let frame_len = u16::from_le_bytes([len[0], len[1]]) as usize;
                if !(13..=64 * 1024).contains(&frame_len) {
                    warn!("invalid frame_len {}", frame_len);
                    continue;
                }

                // frame bytes
                let frame = self.idevice.read_raw(frame_len).await?;
                if frame.len() != frame_len {
                    Err(IdeviceError::NotEnoughBytes(frame.len(), frame_len))?;
                }

                // header + kind + payload
                let (hdr, off) = BtHeader::parse(&frame).ok_or(IdeviceError::UnexpectedResponse("failed to parse BT packet header in stream".into()))?;
                let kind = BtPacketKind::from_byte(frame[off]);
                let payload = &frame[off + 1..];

                // make H4 buffer
                let mut h4 = Vec::with_capacity(1 + payload.len());
                if let Some(t) = kind.h4_type() {
                    h4.push(t);
                } else {
                    // unknown kind
                    continue;
                }
                h4.extend_from_slice(payload);

                yield BtFrame { hdr, kind, h4 };
            }
        })
    }
}

impl BtHeader {
    /// Parse 12-byte header at the start of a frame.
    /// Returns (header, next_offset) where next_offset == 12 (start of packet_type).
    fn parse(buf: &[u8]) -> Option<(Self, usize)> {
        if buf.len() < 12 {
            return None;
        }
        let length = u32::from_be_bytes(buf[0..4].try_into().ok()?);
        let ts_secs = u32::from_be_bytes(buf[4..8].try_into().ok()?);
        let ts_usecs = u32::from_be_bytes(buf[8..12].try_into().ok()?);
        Some((
            BtHeader {
                length,
                ts_secs,
                ts_usecs,
            },
            12,
        ))
    }
}

#[cfg(feature = "rsd")]
impl crate::RsdService for BtPacketLoggerClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        crate::obf!("com.apple.bluetooth.BTPacketLogger.shim.remote")
    }
    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = crate::Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}
