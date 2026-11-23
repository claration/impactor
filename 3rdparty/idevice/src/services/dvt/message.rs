//! Instruments protocol message format implementation
//!
//! This module handles the serialization and deserialization of messages used in
//! the iOS instruments protocol. The message format consists of:
//! - 32-byte message header
//! - 16-byte payload header
//! - Optional auxiliary data section
//! - Payload data (typically NSKeyedArchive format)
//!
//! # Message Structure
//! ```text
//! +---------------------+
//! |   MessageHeader     | 32 bytes
//! +---------------------+
//! |   PayloadHeader     | 16 bytes
//! +---------------------+
//! |   AuxHeader         | 16 bytes (if aux present)
//! |   Aux data          | variable length
//! +---------------------+
//! |   Payload data      | variable length (NSKeyedArchive)
//! +---------------------+
//! ```
//!
//! # Example
//! ```rust,no_run
//! use plist::Value;
//! use your_crate::IdeviceError;
//! use your_crate::dvt::message::{Message, MessageHeader, PayloadHeader, AuxValue};
//!
//! # #[tokio::main]
//! # async fn main() -> Result<(), IdeviceError> {
//! // Create a new message
//! let header = MessageHeader::new(
//!     1,      // fragment_id
//!     1,      // fragment_count  
//!     123,    // identifier
//!     0,      // conversation_index
//!     42,     // channel
//!     true    // expects_reply
//! );
//!
//! let message = Message::new(
//!     header,
//!     PayloadHeader::method_invocation(),
//!     Some(AuxValue::from_values(vec![
//!         AuxValue::String("param".into()),
//!         AuxValue::U32(123),
//!     ])),
//!     Some(Value::String("data".into()))
//! );
//!
//! // Serialize message
//! let bytes = message.serialize();
//!
//! // Deserialize message (from async reader)
//! # let mut reader = &bytes[..];
//! let deserialized = Message::from_reader(&mut reader).await?;
//! # Ok(())
//! # }

use plist::Value;
use std::io::{Cursor, Read};
use tokio::io::{AsyncRead, AsyncReadExt};

use super::errors::DvtError;
use crate::{IdeviceError, pretty_print_plist};

/// Message header containing metadata about the message
///
/// 32-byte structure that appears at the start of every message
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MessageHeader {
    /// Magic number identifying the protocol (0x1F3D5B79)
    magic: u32,
    /// Length of this header (always 32)
    header_len: u32,
    /// Fragment identifier for multipart messages
    fragment_id: u16,
    /// Total number of fragments
    fragment_count: u16,
    /// Total length of payload (headers + aux + data)
    length: u32,
    /// Unique message identifier
    identifier: u32,
    /// Conversation tracking index
    conversation_index: u32,
    /// Channel number this message belongs to
    pub channel: i32,
    /// Whether a reply is expected
    expects_reply: bool,
}

/// Payload header containing information about the message contents
///
/// 16-byte structure following the message header
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct PayloadHeader {
    /// DTX message type (DISPATCH/OBJECT/OK/ERROR/DATA)
    msg_type: u8,
    /// Reserved bytes in the wire format
    flags_a: u8,
    /// Reserved bytes in the wire format
    flags_b: u8,
    /// Reserved byte in the wire format
    reserved: u8,
    /// Length of auxiliary data section
    aux_length: u32,
    /// Total length of payload (aux + data)
    total_length: u32,
    /// Additional payload flags
    flags: u32,
}

/// Header for auxiliary data section
///
/// 16-byte structure preceding auxiliary data
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct AuxHeader {
    /// Buffer size hint (often 496)
    buffer_size: u32,
    /// Unknown field (typically 0)
    unknown: u32,
    /// Actual size of auxiliary data
    aux_size: u32,
    /// Unknown field (typically 0)
    unknown2: u32,
}

/// Auxiliary data container
///
/// Contains a header and a collection of typed values
#[derive(Debug, Clone, PartialEq)]
pub struct Aux {
    /// Auxiliary data header
    pub header: AuxHeader,
    /// Collection of auxiliary values
    pub values: Vec<AuxValue>,
}

/// Typed auxiliary value that can be included in messages
#[derive(Clone, PartialEq)]
pub enum AuxValue {
    /// NULL value (type 0x0a) - no payload bytes
    Null,
    /// UTF-8 string value (type 0x01)
    String(String),
    /// Raw byte array (type 0x02)
    Array(Vec<u8>),
    /// 32-bit unsigned integer (type 0x03)
    U32(u32),
    /// 64-bit signed integer (type 0x06)
    I64(i64),
    /// 64-bit floating point (double) (type 0x09)
    Double(f64),
    /// Primitive dictionary (type 0xF0) - value is a list of primitives to match pymobiledevice3 format
    PrimitiveDictionary(Vec<(AuxValue, Vec<AuxValue>)>),
}

/// Complete protocol message
#[derive(Clone, PartialEq)]
pub struct Message {
    /// Message metadata header
    pub message_header: MessageHeader,
    /// Payload description header
    pub payload_header: PayloadHeader,
    /// Optional auxiliary data
    pub aux: Option<Aux>,
    /// Optional payload data (typically NSKeyedArchive)
    pub data: Option<Value>,
    /// Raw bytes of the data section before NSKeyedArchive decoding
    pub raw_data: Option<Vec<u8>>,
}

impl Aux {
    /// Parses the legacy aux wire format used on iOS 16 and earlier
    ///
    /// Layout: `[AuxHeader (16 B)][type (4 B)][data...][type (4 B)][data...]...`
    ///
    /// Type 0xF0 entries are `PrimitiveDictionary` blocks embedded inside
    /// the legacy envelope; their bodies are skipped since the useful values
    /// are the surrounding flat entries.
    fn parse_legacy_bytes(bytes: Vec<u8>) -> Result<Self, IdeviceError> {
        if bytes.len() < 16 {
            return Err(IdeviceError::NotEnoughBytes(bytes.len(), 16));
        }

        let mut cursor = Cursor::new(bytes.as_slice());
        let header = AuxHeader {
            buffer_size: Self::read_u32(&mut cursor)?,
            unknown: Self::read_u32(&mut cursor)?,
            aux_size: Self::read_u32(&mut cursor)?,
            unknown2: Self::read_u32(&mut cursor)?,
        };

        let mut values = Vec::new();
        while cursor.position() + 4 <= bytes.len() as u64 {
            let aux_type = Self::read_u32(&mut cursor)?;
            match aux_type {
                0x0a => {
                    // PNULL separator — used as dictionary keys; not a user value.
                }
                0x0f0 => {
                    // PrimitiveDictionary block embedded in a legacy envelope.
                    // Layout after the type: u32 flags, u64 body_length, [body].
                    // Skip the entire block; positional args appear as flat entries.
                    let _flags = Self::read_u32(&mut cursor)?;
                    let body_len = Self::read_u64(&mut cursor)?;
                    let pos = cursor.position() as usize;
                    let end = pos + body_len as usize;
                    if end > bytes.len() {
                        return Err(IdeviceError::NotEnoughBytes(bytes.len(), end));
                    }
                    cursor.set_position(end as u64);
                }
                _ => {
                    // All other types share the same encoding as parse_primitive,
                    // but the type word is already consumed above so we reconstruct
                    // a cursor over [type || remaining] to reuse parse_primitive.
                    let pos = cursor.position() as usize - 4;
                    let mut sub = Cursor::new(&bytes[pos..]);
                    values.push(Self::parse_primitive(&mut sub)?);
                    cursor.set_position(pos as u64 + sub.position());
                }
            }
        }

        Ok(Self { header, values })
    }

    fn read_u32(cursor: &mut Cursor<&[u8]>) -> Result<u32, IdeviceError> {
        let mut buf = [0u8; 4];
        Read::read_exact(cursor, &mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_u64(cursor: &mut Cursor<&[u8]>) -> Result<u64, IdeviceError> {
        let mut buf = [0u8; 8];
        Read::read_exact(cursor, &mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn read_f64(cursor: &mut Cursor<&[u8]>) -> Result<f64, IdeviceError> {
        let mut buf = [0u8; 8];
        Read::read_exact(cursor, &mut buf)?;
        Ok(f64::from_le_bytes(buf))
    }

    fn read_exact_vec(cursor: &mut Cursor<&[u8]>, len: usize) -> Result<Vec<u8>, IdeviceError> {
        let mut buf = vec![0u8; len];
        Read::read_exact(cursor, &mut buf)?;
        Ok(buf)
    }

    fn parse_primitive(cursor: &mut Cursor<&[u8]>) -> Result<AuxValue, IdeviceError> {
        let raw_type = Self::read_u32(cursor)?;
        let type_code = raw_type & 0xFF;
        match type_code {
            0x01 => {
                let len = Self::read_u32(cursor)? as usize;
                Ok(AuxValue::String(String::from_utf8(Self::read_exact_vec(
                    cursor, len,
                )?)?))
            }
            0x02 => {
                let len = Self::read_u32(cursor)? as usize;
                Ok(AuxValue::Array(Self::read_exact_vec(cursor, len)?))
            }
            0x03 => Ok(AuxValue::U32(Self::read_u32(cursor)?)),
            0x06 => Ok(AuxValue::I64(Self::read_u64(cursor)? as i64)),
            0x09 => Ok(AuxValue::Double(Self::read_f64(cursor)?)),
            0x0A => Ok(AuxValue::Null),
            _ => Err(DvtError::UnknownAuxValueType(raw_type).into()),
        }
    }

    /// Parses auxiliary data from bytes, selecting the correct wire format
    /// based on the leading magic byte.
    ///
    /// # Wire formats
    ///
    /// **Legacy**: the first byte is NOT `0xF0`.
    /// The buffer begins with a 16-byte `AuxHeader` followed by flat
    /// type-tagged value entries.
    ///
    /// **Modern** (iOS 17+, RSD/testmanagerd path): the first byte IS `0xF0`,
    /// indicating the entire buffer is a single `PrimitiveDictionary` block
    /// (`[flags(4B)][unknown(4B)][body_len(8B)][key-value pairs...]`).
    /// Keys are positional-null sentinels; only the values are collected.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, IdeviceError> {
        if bytes.is_empty() {
            return Ok(Self::from_values(Vec::new()));
        }

        if (bytes[0] as u32) != 0xF0 {
            return Self::parse_legacy_bytes(bytes);
        }

        if bytes.len() < 16 {
            return Err(IdeviceError::NotEnoughBytes(bytes.len(), 16));
        }

        let mut cursor = Cursor::new(bytes.as_slice());
        let _type_and_flags = Self::read_u32(&mut cursor)?;
        let _unknown_flags = Self::read_u32(&mut cursor)?;
        let body_len = Self::read_u64(&mut cursor)?;
        let body_end = 16u64 + body_len;
        if body_end > bytes.len() as u64 {
            return Err(IdeviceError::NotEnoughBytes(bytes.len(), body_end as usize));
        }

        let mut values = Vec::new();
        while cursor.position() < body_end {
            let _key = Self::parse_primitive(&mut cursor)?;
            let value = Self::parse_primitive(&mut cursor)?;
            values.push(value);
        }

        Ok(Self {
            header: AuxHeader::default(),
            values,
        })
    }

    /// Creates new auxiliary data from values
    ///
    /// Note: Header fields are populated during serialization
    ///
    /// # Arguments
    /// * `values` - Collection of auxiliary values to include
    pub fn from_values(values: Vec<AuxValue>) -> Self {
        Self {
            header: AuxHeader::default(),
            values,
        }
    }

    /// Serializes auxiliary data to bytes
    ///
    /// Includes properly formatted header with updated size fields
    pub fn serialize(&self) -> Vec<u8> {
        let mut values_payload = Vec::new();
        for v in self.values.iter() {
            values_payload.extend_from_slice(&0x0a_u32.to_le_bytes());
            match v {
                AuxValue::Null => {
                    // PNULL - type 0x0a with no payload bytes
                }
                AuxValue::String(s) => {
                    values_payload.extend_from_slice(&0x01_u32.to_le_bytes());
                    values_payload.extend_from_slice(&(s.len() as u32).to_le_bytes());
                    values_payload.extend_from_slice(s.as_bytes());
                }
                AuxValue::Array(v) => {
                    values_payload.extend_from_slice(&0x02_u32.to_le_bytes());
                    values_payload.extend_from_slice(&(v.len() as u32).to_le_bytes());
                    values_payload.extend_from_slice(v);
                }
                AuxValue::U32(u) => {
                    values_payload.extend_from_slice(&0x03_u32.to_le_bytes());
                    values_payload.extend_from_slice(&u.to_le_bytes());
                }
                AuxValue::I64(i) => {
                    values_payload.extend_from_slice(&0x06_u32.to_le_bytes());
                    values_payload.extend_from_slice(&i.to_le_bytes());
                }
                AuxValue::Double(d) => {
                    values_payload.extend_from_slice(&0x09_u32.to_le_bytes());
                    values_payload.extend_from_slice(&d.to_le_bytes());
                }
                AuxValue::PrimitiveDictionary(entries) => {
                    // PrimitiveDictionary: type=0xF0, entries are (key, [values]) pairs
                    // Header: u32 magic (0x1F0), u32 unknown (0), u64 body_length
                    // pymobiledevice3 format: {PNULL: [arg1, arg2, ...]}
                    let mut body_payload = Vec::new();
                    for (key, values) in entries {
                        // Write the key primitive once (typically NULL 0x0a)
                        body_payload.extend_from_slice(&0x0a_u32.to_le_bytes());
                        write_primitive_value(key, &mut body_payload);
                        // Write each value in the list
                        for value in values {
                            write_primitive_value(value, &mut body_payload);
                        }
                    }
                    let body_len = body_payload.len() as u64;
                    values_payload.extend_from_slice(&0xf0_u32.to_le_bytes());
                    values_payload.extend_from_slice(&0_u32.to_le_bytes()); // unknown flags
                    values_payload.extend_from_slice(&body_len.to_le_bytes());
                    values_payload.extend_from_slice(&body_payload);
                }
            }
        }

        let mut res = Vec::new();
        let buffer_size = 496_u32;
        res.extend_from_slice(&buffer_size.to_le_bytes());
        res.extend_from_slice(&0_u32.to_le_bytes());
        res.extend_from_slice(&(values_payload.len() as u32).to_le_bytes());
        res.extend_from_slice(&0_u32.to_le_bytes());
        res.extend_from_slice(&values_payload);
        res
    }
}

/// Helper to write a primitive value to the payload
fn write_primitive_value(v: &AuxValue, payload: &mut Vec<u8>) {
    match v {
        AuxValue::Null => {
            // PNULL - type 0x0a with no payload bytes
        }
        AuxValue::String(s) => {
            payload.extend_from_slice(&0x01_u32.to_le_bytes());
            payload.extend_from_slice(&(s.len() as u32).to_le_bytes());
            payload.extend_from_slice(s.as_bytes());
        }
        AuxValue::Array(bytes) => {
            payload.extend_from_slice(&0x02_u32.to_le_bytes());
            payload.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
            payload.extend_from_slice(bytes);
        }
        AuxValue::U32(val) => {
            payload.extend_from_slice(&0x03_u32.to_le_bytes());
            payload.extend_from_slice(&val.to_le_bytes());
        }
        AuxValue::I64(val) => {
            payload.extend_from_slice(&0x06_u32.to_le_bytes());
            payload.extend_from_slice(&val.to_le_bytes());
        }
        AuxValue::Double(val) => {
            payload.extend_from_slice(&0x09_u32.to_le_bytes());
            payload.extend_from_slice(&val.to_le_bytes());
        }
        AuxValue::PrimitiveDictionary(_) => {
            // Nested dictionaries not typically used as primitive values
            // Write as empty dict
            payload.extend_from_slice(&0xf0_u32.to_le_bytes());
            payload.extend_from_slice(&0_u32.to_le_bytes());
            payload.extend_from_slice(&0u64.to_le_bytes());
        }
    }
}

impl AuxValue {
    /// Creates an auxiliary value containing NSKeyedArchived data
    ///
    /// # Arguments
    /// * `v` - Plist value to archive
    pub fn archived_value(v: impl Into<plist::Value>) -> Self {
        Self::Array(ns_keyed_archive::encode::encode_to_bytes(v.into()).expect("Failed to encode"))
    }

    /// Creates a primitive buffer (immutable buffer) containing raw bytes
    /// This is used for passing primitive arrays as DTX arguments (wire type 0x02)
    ///
    /// # Arguments
    /// * `bytes` - Raw bytes to include in the buffer
    pub fn primitive_buffer(bytes: Vec<u8>) -> Self {
        Self::Array(bytes)
    }

    /// Creates a PrimitiveDictionary auxiliary value (wire type 0xF0)
    ///
    /// # Arguments
    /// * `entries` - List of (key, [values]) pairs
    pub fn primitive_dictionary(entries: Vec<(AuxValue, Vec<AuxValue>)>) -> Self {
        Self::PrimitiveDictionary(entries)
    }

    /// Creates a DTX method call auxiliary argument format matching pymobiledevice3
    ///
    /// This wraps all arguments in a single PrimitiveDictionary with a NULL key,
    /// producing the structure `{PNULL: [arg1, arg2, ...]}`.
    ///
    /// This matches pymobiledevice3's `MessageAux.build()` which creates `PDict({PNULL: converted_list})`.
    ///
    /// # Arguments
    /// * `args` - List of arguments to wrap
    pub fn dtx_method_args(args: Vec<Self>) -> Self {
        // Create a PrimitiveDictionary with a single entry: (PNULL, [arg1, arg2, ...])
        Self::PrimitiveDictionary(vec![(Self::Null, args)])
    }
}

impl MessageHeader {
    /// Creates a new message header
    ///
    /// Note: Length field is updated during message serialization
    ///
    /// # Arguments
    /// * `fragment_id` - Identifier for message fragments
    /// * `fragment_count` - Total fragments in message
    /// * `identifier` - Unique message ID
    /// * `conversation_index` - Conversation tracking number
    /// * `channel` - Channel number
    /// * `expects_reply` - Whether response is expected
    pub fn new(
        fragment_id: u16,
        fragment_count: u16,
        identifier: u32,
        conversation_index: u32,
        channel: i32,
        expects_reply: bool,
    ) -> Self {
        Self {
            magic: 0x1F3D5B79,
            header_len: 32,
            fragment_id,
            fragment_count,
            length: 0,
            identifier,
            conversation_index,
            channel,
            expects_reply,
        }
    }

    /// Returns the unique message identifier.
    pub(crate) fn identifier(&self) -> u32 {
        self.identifier
    }

    /// Returns the conversation index for this message.
    pub(crate) fn conversation_index(&self) -> u32 {
        self.conversation_index
    }

    /// Returns whether this message expects a reply.
    pub(crate) fn expects_reply(&self) -> bool {
        self.expects_reply
    }

    /// Serializes header to bytes
    pub fn serialize(&self) -> Vec<u8> {
        let mut res = Vec::new();
        res.extend_from_slice(&self.magic.to_le_bytes());
        res.extend_from_slice(&self.header_len.to_le_bytes());
        res.extend_from_slice(&self.fragment_id.to_le_bytes());
        res.extend_from_slice(&self.fragment_count.to_le_bytes());
        res.extend_from_slice(&self.length.to_le_bytes());
        res.extend_from_slice(&self.identifier.to_le_bytes());
        res.extend_from_slice(&self.conversation_index.to_le_bytes());
        res.extend_from_slice(&self.channel.to_le_bytes());
        res.extend_from_slice(&if self.expects_reply { 1_u32 } else { 0 }.to_le_bytes());

        res
    }
}

impl PayloadHeader {
    /// Creates a new payload header
    pub fn new() -> Self {
        Self::default()
    }

    /// Serializes header to bytes
    pub fn serialize(&self) -> Vec<u8> {
        let mut res = vec![self.msg_type, self.flags_a, self.flags_b, self.reserved];
        res.extend_from_slice(&self.aux_length.to_le_bytes());
        res.extend_from_slice(&self.total_length.to_le_bytes());
        res.extend_from_slice(&self.flags.to_le_bytes());

        res
    }

    /// Creates header for method invocation messages
    pub fn method_invocation() -> Self {
        Self {
            msg_type: 2,
            ..Default::default()
        }
    }
}

impl Message {
    /// Reads and parses a message from an async reader
    ///
    /// # Arguments
    /// * `reader` - Async reader to read from
    ///
    /// # Returns  
    /// * `Ok(Message)` - Parsed message
    /// * `Err(IdeviceError)` - If reading/parsing fails
    ///
    /// # Errors
    /// * Various IdeviceError variants for IO and parsing failures
    pub async fn from_reader<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, IdeviceError> {
        let mut packet_data: Vec<u8> = Vec::new();
        // loop for deal with multiple fragments
        let mheader = loop {
            let mut buf = [0u8; 32];
            reader.read_exact(&mut buf).await?;
            let header = MessageHeader {
                magic: u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
                header_len: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
                fragment_id: u16::from_le_bytes([buf[8], buf[9]]),
                fragment_count: u16::from_le_bytes([buf[10], buf[11]]),
                length: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
                identifier: u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]),
                conversation_index: u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]),
                channel: {
                    let wire_channel = i32::from_le_bytes([buf[24], buf[25], buf[26], buf[27]]);
                    let conversation_index =
                        u32::from_le_bytes([buf[20], buf[21], buf[22], buf[23]]);
                    if conversation_index.is_multiple_of(2) {
                        -wire_channel
                    } else {
                        wire_channel
                    }
                },
                expects_reply: u32::from_le_bytes([buf[28], buf[29], buf[30], buf[31]]) == 1,
            };
            if header.fragment_count > 1 && header.fragment_id == 0 {
                // when reading multiple message fragments, the first fragment contains only a message header.
                continue;
            }
            let mut buf = vec![0u8; header.length as usize];
            reader.read_exact(&mut buf).await?;
            packet_data.extend(buf);
            if header.fragment_id == header.fragment_count - 1 {
                break header;
            }
        };
        // read the payload header
        let buf = &packet_data[0..16];
        let pheader = PayloadHeader {
            msg_type: buf[0],
            flags_a: buf[1],
            flags_b: buf[2],
            reserved: buf[3],
            aux_length: u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]),
            total_length: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            flags: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
        };
        let aux = if pheader.aux_length > 0 {
            let buf = packet_data[16..(16 + pheader.aux_length as usize)].to_vec();
            Some(Aux::from_bytes(buf)?)
        } else {
            None
        };
        // read the data
        let need_len = (pheader.total_length - pheader.aux_length) as usize;
        let buf = packet_data
            [(pheader.aux_length + 16) as usize..pheader.aux_length as usize + 16 + need_len]
            .to_vec();
        let raw_data = if buf.is_empty() {
            None
        } else {
            Some(buf.clone())
        };
        let data = if buf.is_empty() {
            None
        } else {
            Some(
                ns_keyed_archive::decode::from_bytes(&buf)
                    .map_err(super::errors::DvtError::from)?,
            )
        };

        Ok(Message {
            message_header: mheader,
            payload_header: pheader,
            aux,
            data,
            raw_data,
        })
    }

    /// Creates a new message
    ///
    /// # Arguments
    /// * `message_header` - Message metadata
    /// * `payload_header` - Payload description  
    /// * `aux` - Optional auxiliary data
    /// * `data` - Optional payload data
    pub fn new(
        message_header: MessageHeader,
        payload_header: PayloadHeader,
        aux: Option<Aux>,
        data: Option<Value>,
    ) -> Self {
        Self {
            message_header,
            payload_header,
            aux,
            data,
            raw_data: None,
        }
    }

    /// Serializes message to bytes
    ///
    /// Updates length fields in headers automatically
    pub fn serialize(&self) -> Vec<u8> {
        let aux = match &self.aux {
            Some(a) => a.serialize(),
            None => Vec::new(),
        };
        let data = match &self.data {
            Some(d) => ns_keyed_archive::encode::encode_to_bytes(d.to_owned())
                .expect("Failed to encode value"),
            None => Vec::new(),
        };

        // Update the payload header
        let mut payload_header = self.payload_header.to_owned();
        payload_header.aux_length = aux.len() as u32;
        payload_header.total_length = (aux.len() + data.len()) as u32;
        let payload_header = payload_header.serialize();

        // Update the message header
        let mut message_header = self.message_header.to_owned();
        message_header.length = (payload_header.len() + aux.len() + data.len()) as u32;

        let mut res = Vec::new();
        res.extend_from_slice(&message_header.serialize());
        res.extend_from_slice(&payload_header);
        res.extend_from_slice(&aux);
        res.extend_from_slice(&data);

        res
    }

    /// Builds a raw reply frame for an incoming message, sending `data_bytes`
    /// verbatim as the payload without additional NSKeyedArchive encoding.
    ///
    /// This is used for replies where the payload is already a serialised
    /// NSKeyedArchive (e.g. `XCTestConfiguration`).  Pass an empty slice to
    /// send an acknowledgement with no payload.
    pub(crate) fn build_raw_reply(
        channel: i32,
        incoming_msg_id: u32,
        incoming_conversation_index: u32,
        data_bytes: &[u8],
    ) -> Vec<u8> {
        // Payload header (16 bytes): flags=0, aux_len=0, total_len
        let msg_type: u8 = if data_bytes.is_empty() { 0 } else { 3 };
        let flags_a: u8 = 0;
        let flags_b: u8 = 0;
        let reserved: u8 = 0;
        let aux_len: u32 = 0;
        let total_len: u32 = data_bytes.len() as u32;

        let payload_total = 16usize + data_bytes.len(); // payload_hdr + data

        // Message header (32 bytes)
        let magic: u32 = 0x1F3D5B79;
        let header_len: u32 = 32;
        let fragment_id: u16 = 0;
        let fragment_count: u16 = 1;
        let length: u32 = payload_total as u32;
        let conversation_index = incoming_conversation_index + 1;
        let expects_reply: u32 = 0;
        let wire_channel = if conversation_index.is_multiple_of(2) {
            channel
        } else {
            -channel
        };

        let mut buf = Vec::with_capacity(32 + 16 + data_bytes.len());
        buf.extend_from_slice(&magic.to_le_bytes());
        buf.extend_from_slice(&header_len.to_le_bytes());
        buf.extend_from_slice(&fragment_id.to_le_bytes());
        buf.extend_from_slice(&fragment_count.to_le_bytes());
        buf.extend_from_slice(&length.to_le_bytes());
        buf.extend_from_slice(&incoming_msg_id.to_le_bytes());
        buf.extend_from_slice(&conversation_index.to_le_bytes());
        buf.extend_from_slice(&wire_channel.to_le_bytes());
        buf.extend_from_slice(&expects_reply.to_le_bytes());
        // Payload header
        buf.push(msg_type);
        buf.push(flags_a);
        buf.push(flags_b);
        buf.push(reserved);
        buf.extend_from_slice(&aux_len.to_le_bytes());
        buf.extend_from_slice(&total_len.to_le_bytes());
        buf.extend_from_slice(&0_u32.to_le_bytes());
        // Data
        buf.extend_from_slice(data_bytes);
        buf
    }
}

impl std::fmt::Debug for AuxValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AuxValue::Null => write!(f, "Null"),
            AuxValue::String(s) => write!(f, "String({s:?})"),
            AuxValue::Array(arr) => write!(
                f,
                "Array(len={}, first_bytes={:?})",
                arr.len(),
                &arr[..arr.len().min(10)]
            ),
            AuxValue::U32(n) => write!(f, "U32({n})"),
            AuxValue::I64(n) => write!(f, "I64({n})"),
            AuxValue::Double(d) => write!(f, "Double({d})"),
            AuxValue::PrimitiveDictionary(_) => write!(f, "PrimitiveDictionary"),
        }
    }
}

impl std::fmt::Debug for Message {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Message")
            .field("message_header", &self.message_header)
            .field("payload_header", &self.payload_header)
            .field("aux", &self.aux)
            .field("data", &self.data.as_ref().map(pretty_print_plist))
            .finish()
    }
}
