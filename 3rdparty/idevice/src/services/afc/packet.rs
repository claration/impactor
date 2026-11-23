// Jackson Coxson

use tracing::debug;

use super::errors::AfcError;
use crate::{Idevice, IdeviceError};

use super::opcode::AfcOpcode;

#[derive(Clone, Copy, Debug)]
pub struct AfcPacketHeader {
    pub magic: u64,
    pub entire_len: u64,
    pub header_payload_len: u64,
    pub packet_num: u64,
    pub operation: AfcOpcode,
}

#[derive(Clone, Debug)]
pub struct AfcPacket {
    pub header: AfcPacketHeader,
    pub header_payload: Vec<u8>,
    pub payload: Vec<u8>,
}

impl AfcPacketHeader {
    pub const LEN: u64 = 40;

    pub fn serialize(&self) -> Vec<u8> {
        let mut res = Vec::with_capacity(Self::LEN as usize);

        res.extend_from_slice(&self.magic.to_le_bytes());
        res.extend_from_slice(&self.entire_len.to_le_bytes());
        res.extend_from_slice(&self.header_payload_len.to_le_bytes());
        res.extend_from_slice(&self.packet_num.to_le_bytes());
        res.extend_from_slice(&(self.operation as u64).to_le_bytes());

        res
    }

    pub async fn read(reader: &mut Idevice) -> Result<Self, IdeviceError> {
        let header_bytes = reader.read_raw(Self::LEN as usize).await?;
        let mut chunks = header_bytes.chunks_exact(8);
        let res = Self {
            magic: u64::from_le_bytes(chunks.next().unwrap().try_into().unwrap()),
            entire_len: u64::from_le_bytes(chunks.next().unwrap().try_into().unwrap()),
            header_payload_len: u64::from_le_bytes(chunks.next().unwrap().try_into().unwrap()),
            packet_num: u64::from_le_bytes(chunks.next().unwrap().try_into().unwrap()),
            operation: match AfcOpcode::try_from(u64::from_le_bytes(
                chunks.next().unwrap().try_into().unwrap(),
            )) {
                Ok(o) => o,
                Err(_) => {
                    return Err(AfcError::UnknownOpcode.into());
                }
            },
        };
        if res.magic != super::MAGIC {
            return Err(AfcError::InvalidMagic.into());
        }
        Ok(res)
    }
}

impl AfcPacket {
    pub fn serialize(&self) -> Vec<u8> {
        let mut res = Vec::new();

        res.extend_from_slice(&self.header.serialize());
        res.extend_from_slice(&self.header_payload);
        res.extend_from_slice(&self.payload);

        res
    }

    pub async fn read(reader: &mut Idevice) -> Result<Self, IdeviceError> {
        let header = AfcPacketHeader::read(reader).await?;
        debug!("afc header: {header:?}");
        let header_payload = reader
            .read_raw((header.header_payload_len - AfcPacketHeader::LEN) as usize)
            .await?;

        let payload = if header.header_payload_len == header.entire_len {
            Vec::new() // no payload
        } else {
            reader
                .read_raw((header.entire_len - header.header_payload_len) as usize)
                .await?
        };

        let res = Self {
            header,
            header_payload,
            payload,
        };
        debug!("Recv afc: {res:?}");
        Ok(res)
    }
}
