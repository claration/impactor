use plist_macro::plist;
use std::{
    ffi::CString,
    io::{BufRead, Cursor, Read},
    ops::{BitOr, BitOrAssign},
};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use super::errors::XpcError;
use crate::{CdTunnelError, IdeviceError};

#[derive(Clone, Copy, Debug)]
#[repr(u32)]
pub enum XPCFlag {
    AlwaysSet,
    DataFlag,
    Reply,
    WantingReply,
    InitHandshake,

    FileTxStreamRequest,
    FileTxStreamResponse,

    Custom(u32),
}

impl From<XPCFlag> for u32 {
    fn from(value: XPCFlag) -> Self {
        match value {
            XPCFlag::AlwaysSet => 0x00000001,
            XPCFlag::DataFlag => 0x00000100,
            XPCFlag::Reply => 0x00020000,
            XPCFlag::WantingReply => 0x00010000,
            XPCFlag::InitHandshake => 0x00400000,
            XPCFlag::FileTxStreamRequest => 0x00100000,
            XPCFlag::FileTxStreamResponse => 0x00200000,
            XPCFlag::Custom(inner) => inner,
        }
    }
}

impl BitOr for XPCFlag {
    fn bitor(self, rhs: Self) -> Self::Output {
        XPCFlag::Custom(u32::from(self) | u32::from(rhs))
    }

    type Output = XPCFlag;
}

impl BitOrAssign for XPCFlag {
    fn bitor_assign(&mut self, rhs: Self) {
        *self = self.bitor(rhs);
    }
}

impl PartialEq for XPCFlag {
    fn eq(&self, other: &Self) -> bool {
        u32::from(*self) == u32::from(*other)
    }
}

#[repr(u32)]
pub enum XPCType {
    Null = 0x00001000,
    Bool = 0x00002000,
    Dictionary = 0x0000f000,
    Array = 0x0000e000,

    Int64 = 0x00003000,
    UInt64 = 0x00004000,
    Double = 0x00005000,

    Date = 0x00007000,

    String = 0x00009000,
    Data = 0x00008000,
    Uuid = 0x0000a000,
    FileTransfer = 0x0001a000,
}

impl TryFrom<u32> for XPCType {
    type Error = IdeviceError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0x00001000 => Ok(Self::Null),
            0x00002000 => Ok(Self::Bool),
            0x0000f000 => Ok(Self::Dictionary),
            0x0000e000 => Ok(Self::Array),
            0x00003000 => Ok(Self::Int64),
            0x00005000 => Ok(Self::Double),
            0x00004000 => Ok(Self::UInt64),
            0x00007000 => Ok(Self::Date),
            0x00009000 => Ok(Self::String),
            0x00008000 => Ok(Self::Data),
            0x0000a000 => Ok(Self::Uuid),
            0x0001a000 => Ok(Self::FileTransfer),
            _ => Err(XpcError::UnknownXpcType(value))?,
        }
    }
}

pub type Dictionary = IndexMap<String, XPCObject>;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum XPCObject {
    Null,
    Bool(bool),
    Dictionary(Dictionary),
    Array(Vec<XPCObject>),

    Double(f64),
    Int64(i64),
    UInt64(u64),

    Date(std::time::SystemTime),

    String(String),
    Data(Vec<u8>),
    Uuid(uuid::Uuid),

    FileTransfer { msg_id: u64, data: Box<XPCObject> },
}

impl From<plist::Value> for XPCObject {
    fn from(value: plist::Value) -> Self {
        match value {
            plist::Value::Array(v) => {
                XPCObject::Array(v.iter().map(|item| XPCObject::from(item.clone())).collect())
            }
            plist::Value::Dictionary(v) => {
                let mut dict = Dictionary::new();
                for (k, v) in v.into_iter() {
                    dict.insert(k.clone(), XPCObject::from(v));
                }
                XPCObject::Dictionary(dict)
            }
            plist::Value::Boolean(v) => XPCObject::Bool(v),
            plist::Value::Data(v) => XPCObject::Data(v),
            plist::Value::Date(_) => todo!(),
            plist::Value::Real(f) => XPCObject::Double(f),
            plist::Value::Integer(v) => XPCObject::Int64(v.as_signed().unwrap()),
            plist::Value::String(v) => XPCObject::String(v),
            plist::Value::Uid(_) => todo!(),
            _ => todo!(),
        }
    }
}

impl XPCObject {
    pub fn to_plist(&self) -> plist::Value {
        match self {
            Self::Null => plist::Value::String("".into()),
            Self::Bool(v) => plist::Value::Boolean(*v),
            Self::Uuid(uuid) => plist::Value::String(uuid.to_string()),
            Self::Double(f) => plist::Value::Real(*f),
            Self::UInt64(v) => plist::Value::Integer({ *v }.into()),
            Self::Int64(v) => plist::Value::Integer({ *v }.into()),
            Self::Date(d) => plist::Value::Date(plist::Date::from(*d)),
            Self::String(v) => plist::Value::String(v.clone()),
            Self::Data(v) => plist::Value::Data(v.clone()),
            Self::Array(v) => plist::Value::Array(v.iter().map(|item| item.to_plist()).collect()),
            Self::Dictionary(v) => {
                let mut dict = plist::Dictionary::new();
                for (k, v) in v.into_iter() {
                    dict.insert(k.clone(), v.to_plist());
                }
                plist::Value::Dictionary(dict)
            }
            Self::FileTransfer { msg_id, data } => {
                plist!({
                    "msg_id": *msg_id,
                    "data": data.to_plist(),
                })
            }
        }
    }

    pub fn encode(&self) -> Result<Vec<u8>, IdeviceError> {
        let mut buf = Vec::new();
        buf.extend_from_slice(&0x42133742_u32.to_le_bytes());
        buf.extend_from_slice(&0x00000005_u32.to_le_bytes());
        self.encode_object(&mut buf)?;
        Ok(buf)
    }

    fn encode_object(&self, buf: &mut Vec<u8>) -> Result<(), IdeviceError> {
        match self {
            XPCObject::Null => buf.extend_from_slice(&(XPCType::Null as u32).to_le_bytes()),
            XPCObject::Bool(val) => {
                buf.extend_from_slice(&(XPCType::Bool as u32).to_le_bytes());
                buf.push(if *val { 1 } else { 0 });
                buf.extend_from_slice(&[0].repeat(3));
            }
            XPCObject::Dictionary(dict) => {
                buf.extend_from_slice(&(XPCType::Dictionary as u32).to_le_bytes());
                let mut content_buf = Vec::new();
                content_buf.extend_from_slice(&(dict.len() as u32).to_le_bytes());
                for (k, v) in dict {
                    let padding = Self::calculate_padding(k.len() + 1);
                    content_buf.extend_from_slice(k.as_bytes());
                    content_buf.push(0);
                    content_buf.extend_from_slice(&[0].repeat(padding));
                    v.encode_object(&mut content_buf)?;
                }
                buf.extend_from_slice(&(content_buf.len() as u32).to_le_bytes());
                buf.extend_from_slice(&content_buf);
            }
            XPCObject::Array(items) => {
                buf.extend_from_slice(&(XPCType::Array as u32).to_le_bytes());
                let mut content_buf = Vec::new();
                content_buf.extend_from_slice(&(items.len() as u32).to_le_bytes());
                for item in items {
                    item.encode_object(&mut content_buf)?;
                }
                buf.extend_from_slice(&(content_buf.len() as u32).to_le_bytes());
                buf.extend_from_slice(&content_buf);
            }

            XPCObject::Double(f) => {
                buf.extend_from_slice(&(XPCType::Double as u32).to_le_bytes());
                buf.extend_from_slice(&f.to_le_bytes());
            }
            XPCObject::Int64(num) => {
                buf.extend_from_slice(&(XPCType::Int64 as u32).to_le_bytes());
                buf.extend_from_slice(&num.to_le_bytes());
            }
            XPCObject::UInt64(num) => {
                buf.extend_from_slice(&(XPCType::UInt64 as u32).to_le_bytes());
                buf.extend_from_slice(&num.to_le_bytes());
            }
            XPCObject::Date(date) => {
                buf.extend_from_slice(&(XPCType::Date as u32).to_le_bytes());
                buf.extend_from_slice(
                    &(date
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_nanos() as u64)
                        .to_le_bytes(),
                );
            }
            XPCObject::String(item) => {
                let l = item.len() + 1;
                let padding = Self::calculate_padding(l);
                buf.extend_from_slice(&(XPCType::String as u32).to_le_bytes());
                buf.extend_from_slice(&(l as u32).to_le_bytes());
                buf.extend_from_slice(item.as_bytes());
                buf.push(0);
                buf.extend_from_slice(&[0].repeat(padding));
            }
            XPCObject::Data(data) => {
                let l = data.len();
                let padding = Self::calculate_padding(l);
                buf.extend_from_slice(&(XPCType::Data as u32).to_le_bytes());
                buf.extend_from_slice(&(l as u32).to_le_bytes());
                buf.extend_from_slice(data);
                buf.extend_from_slice(&[0].repeat(padding));
            }
            XPCObject::Uuid(uuid) => {
                buf.extend_from_slice(&(XPCType::Uuid as u32).to_le_bytes());
                buf.extend_from_slice(uuid.as_bytes());
            }
            XPCObject::FileTransfer { msg_id, data } => {
                buf.extend_from_slice(&(XPCType::FileTransfer as u32).to_le_bytes());
                buf.extend_from_slice(&msg_id.to_le_bytes());
                data.encode_object(buf)?;
            }
        }
        Ok(())
    }

    pub fn decode(buf: &[u8]) -> Result<Self, IdeviceError> {
        if buf.len() < 8 {
            return Err(IdeviceError::NotEnoughBytes(buf.len(), 8));
        }
        let magic = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if magic != 0x42133742 {
            warn!("Invalid magic for XPCObject");
            return Err(XpcError::InvalidXpcMagic.into());
        }

        let version = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
        if version != 0x00000005 {
            warn!("Unexpected version for XPCObject");
            return Err(XpcError::UnexpectedXpcVersion.into());
        }

        Self::decode_object(&mut Cursor::new(&buf[8..]))
    }

    fn decode_object(mut cursor: &mut Cursor<&[u8]>) -> Result<Self, IdeviceError> {
        let mut buf_32: [u8; 4] = Default::default();
        cursor.read_exact(&mut buf_32)?;
        let xpc_type = u32::from_le_bytes(buf_32);
        let xpc_type: XPCType = xpc_type.try_into()?;
        match xpc_type {
            XPCType::Null => Ok(XPCObject::Null),
            XPCType::Dictionary => {
                let mut ret = IndexMap::new();

                cursor.read_exact(&mut buf_32)?;
                let _l = u32::from_le_bytes(buf_32);
                cursor.read_exact(&mut buf_32)?;
                let num_entries = u32::from_le_bytes(buf_32);
                for _ in 0..num_entries {
                    let mut key_buf = Vec::new();
                    BufRead::read_until(&mut cursor, 0, &mut key_buf)?;
                    let key = match CString::from_vec_with_nul(key_buf)
                        .ok()
                        .and_then(|x| x.to_str().ok().map(|x| x.to_string()))
                    {
                        Some(k) => k,
                        None => {
                            return Err(XpcError::InvalidCString.into());
                        }
                    };
                    let padding = Self::calculate_padding(key.len() + 1);

                    BufRead::consume(&mut cursor, padding);
                    ret.insert(key, Self::decode_object(cursor)?);
                }
                Ok(XPCObject::Dictionary(ret))
            }
            XPCType::Array => {
                cursor.read_exact(&mut buf_32)?;
                let _l = u32::from_le_bytes(buf_32);
                cursor.read_exact(&mut buf_32)?;
                let num_entries = u32::from_le_bytes(buf_32);

                let mut ret = Vec::new();
                for _i in 0..num_entries {
                    ret.push(Self::decode_object(cursor)?);
                }
                Ok(XPCObject::Array(ret))
            }
            XPCType::Double => {
                let mut buf: [u8; 8] = Default::default();
                cursor.read_exact(&mut buf)?;
                Ok(XPCObject::Double(f64::from_le_bytes(buf)))
            }
            XPCType::Int64 => {
                let mut buf: [u8; 8] = Default::default();
                cursor.read_exact(&mut buf)?;
                Ok(XPCObject::Int64(i64::from_le_bytes(buf)))
            }
            XPCType::UInt64 => {
                let mut buf: [u8; 8] = Default::default();
                cursor.read_exact(&mut buf)?;
                Ok(XPCObject::UInt64(u64::from_le_bytes(buf)))
            }

            XPCType::Date => {
                let mut buf: [u8; 8] = Default::default();
                cursor.read_exact(&mut buf)?;
                Ok(XPCObject::Date(
                    std::time::UNIX_EPOCH
                        + std::time::Duration::from_nanos(u64::from_le_bytes(buf)),
                ))
            }

            XPCType::String => {
                // 'l' includes utf8 '\0' character.
                cursor.read_exact(&mut buf_32)?;
                let l = u32::from_le_bytes(buf_32) as usize;
                let padding = Self::calculate_padding(l);

                let mut key_buf = vec![0; l];
                cursor.read_exact(&mut key_buf)?;
                let key = match CString::from_vec_with_nul(key_buf)
                    .ok()
                    .and_then(|x| x.to_str().ok().map(|x| x.to_string()))
                {
                    Some(k) => k,
                    None => return Err(XpcError::InvalidCString.into()),
                };
                BufRead::consume(&mut cursor, padding);
                Ok(XPCObject::String(key))
            }
            XPCType::Bool => {
                let mut buf: [u8; 4] = Default::default();
                cursor.read_exact(&mut buf)?;
                Ok(XPCObject::Bool(buf[0] != 0))
            }
            XPCType::Data => {
                cursor.read_exact(&mut buf_32)?;
                let l = u32::from_le_bytes(buf_32) as usize;
                let padding = Self::calculate_padding(l);

                let mut data = vec![0; l];
                cursor.read_exact(&mut data)?;
                BufRead::consume(&mut cursor, padding);
                Ok(XPCObject::Data(data))
            }
            XPCType::Uuid => {
                let mut data: [u8; 16] = Default::default();
                cursor.read_exact(&mut data)?;
                Ok(XPCObject::Uuid(uuid::Builder::from_bytes(data).into_uuid()))
            }
            XPCType::FileTransfer => {
                let mut id_buf = [0u8; 8];
                cursor.read_exact(&mut id_buf)?;
                let msg_id = u64::from_le_bytes(id_buf);

                // The next thing in the stream is a full XPC object
                let inner = Self::decode_object(cursor)?;
                Ok(XPCObject::FileTransfer {
                    msg_id,
                    data: Box::new(inner),
                })
            }
        }
    }

    pub fn as_dictionary(&self) -> Option<&Dictionary> {
        match self {
            XPCObject::Dictionary(dict) => Some(dict),
            _ => None,
        }
    }

    pub fn to_dictionary(self) -> Option<Dictionary> {
        match self {
            XPCObject::Dictionary(dict) => Some(dict),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&Vec<Self>> {
        match self {
            XPCObject::Array(array) => Some(array),
            _ => None,
        }
    }

    pub fn as_string(&self) -> Option<&str> {
        match self {
            XPCObject::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<&bool> {
        match self {
            XPCObject::Bool(b) => Some(b),
            _ => None,
        }
    }

    pub fn as_signed_integer(&self) -> Option<i64> {
        match self {
            XPCObject::String(s) => s.parse().ok(),
            XPCObject::Int64(v) => Some(*v),
            _ => None,
        }
    }

    pub fn as_unsigned_integer(&self) -> Option<u64> {
        match self {
            XPCObject::String(s) => s.parse().ok(),
            XPCObject::UInt64(v) => Some(*v),
            _ => None,
        }
    }

    fn calculate_padding(len: usize) -> usize {
        let c = ((len as f64) / 4.0).ceil();
        (c * 4.0 - (len as f64)) as usize
    }
}

impl From<Dictionary> for XPCObject {
    fn from(value: Dictionary) -> Self {
        XPCObject::Dictionary(value)
    }
}

pub struct XPCMessage {
    pub flags: u32,
    pub message: Option<XPCObject>,
    pub message_id: Option<u64>,
}

impl XPCMessage {
    pub fn new(
        flags: Option<XPCFlag>,
        message: Option<XPCObject>,
        message_id: Option<u64>,
    ) -> XPCMessage {
        XPCMessage {
            flags: flags.unwrap_or(XPCFlag::AlwaysSet).into(),
            message,
            message_id,
        }
    }

    pub fn decode(data: &[u8]) -> Result<XPCMessage, IdeviceError> {
        if data.len() < 24 {
            Err(IdeviceError::NotEnoughBytes(data.len(), 24))?
        }

        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        if magic != 0x29b00b92_u32 {
            warn!("XPCMessage magic is invalid.");
            Err(XpcError::MalformedXpc)?
        }

        let flags = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let body_len = u64::from_le_bytes([
            data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
        ]);
        debug!("Body_len: {body_len}");
        let message_id = u64::from_le_bytes([
            data[16], data[17], data[18], data[19], data[20], data[21], data[22], data[23],
        ]);
        if body_len + 24 > data.len() as u64 {
            warn!(
                "Body length is {body_len}, but received bytes is {}",
                data.len()
            );
            Err(CdTunnelError::SizeMismatch)?
        }

        let res = XPCMessage {
            flags,
            message: if body_len > 0 {
                Some(XPCObject::decode(&data[24..24 + body_len as usize])?)
            } else {
                None
            },
            message_id: Some(message_id),
        };

        debug!("Decoded {res:#?}");
        Ok(res)
    }

    pub fn encode(self, message_id: u64) -> Result<Vec<u8>, IdeviceError> {
        let mut out = 0x29b00b92_u32.to_le_bytes().to_vec();
        out.extend_from_slice(&self.flags.to_le_bytes());
        match self.message {
            Some(message) => {
                let body = message.encode()?;
                out.extend_from_slice(&(body.len() as u64).to_le_bytes()); // body length
                out.extend_from_slice(&message_id.to_le_bytes()); // messageId
                out.extend_from_slice(&body);
            }
            _ => {
                out.extend_from_slice(&0_u64.to_le_bytes());
                out.extend_from_slice(&message_id.to_le_bytes());
            }
        }
        Ok(out)
    }
}

impl std::fmt::Debug for XPCMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts = Vec::new();

        if self.flags & 0x00000001 != 0 {
            parts.push("AlwaysSet".to_string());
        }
        if self.flags & 0x00000100 != 0 {
            parts.push("DataFlag".to_string());
        }
        if self.flags & 0x00010000 != 0 {
            parts.push("WantingReply".to_string());
        }
        if self.flags & 0x00020000 != 0 {
            parts.push("Reply".to_string());
        }
        if self.flags & 0x00400000 != 0 {
            parts.push("InitHandshake".to_string());
        }

        // Check for any unknown bits (not covered by known flags)
        let known_mask = 0x00000001 | 0x00000100 | 0x00010000 | 0x00020000 | 0x00400000;
        let custom_bits = self.flags & !known_mask;
        if custom_bits != 0 {
            parts.push(format!("Custom(0x{custom_bits:08X})"));
        }

        write!(
            f,
            "XPCMessage {{ flags: [{}], message_id: {:?}, message: {:?} }}",
            parts.join(" | "),
            self.message_id,
            self.message
        )
    }
}
