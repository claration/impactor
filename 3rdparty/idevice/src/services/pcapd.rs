//! Abstraction for pcapd
//! Note that this service only works over USB or through RSD.

use plist::Value;
use tokio::io::AsyncWrite;
use tokio::io::AsyncWriteExt;

use crate::{Idevice, IdeviceError, IdeviceService, RsdService, obf};

const ETHERNET_HEADER: &[u8] = &[
    0xBE, 0xEF, 0xBE, 0xEF, 0xBE, 0xEF, 0xBE, 0xEF, 0xBE, 0xEF, 0xBE, 0xEF, 0x08, 0x00,
];

/// Client for interacting with the pcapd service on the device.
/// Note that this service only works over USB or through RSD.
#[derive(Debug)]
pub struct PcapdClient {
    /// The underlying device connection with established service
    pub idevice: Idevice,
}

impl IdeviceService for PcapdClient {
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.pcapd")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl RsdService for PcapdClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.pcapd.shim.remote")
    }

    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}

/// A Rust representation of the iOS pcapd device packet header and data.
#[derive(Debug, Clone)]
pub struct DevicePacket {
    pub header_length: u32,
    pub header_version: u8,
    pub packet_length: u32,
    pub interface_type: u8,
    pub unit: u16,
    pub io: u8,
    pub protocol_family: u32,
    pub frame_pre_length: u32,
    pub frame_post_length: u32,
    pub interface_name: String,
    pub pid: u32,
    pub comm: String,
    pub svc: u32,
    pub epid: u32,
    pub ecomm: String,
    pub seconds: u32,
    pub microseconds: u32,
    pub data: Vec<u8>,
}

impl PcapdClient {
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    pub async fn next_packet(&mut self) -> Result<DevicePacket, IdeviceError> {
        let packet = self.idevice.read_plist_value().await?;
        let packet = match packet {
            Value::Data(p) => p,
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "expected Data plist value for packet".into(),
                ));
            }
        };
        let mut packet = DevicePacket::from_vec(&packet)?;
        packet.normalize_data();
        Ok(packet)
    }
}

impl DevicePacket {
    /// Normalizes the packet data by adding a fake Ethernet header if necessary.
    /// This is required for tools like Wireshark to correctly dissect raw IP packets.
    pub fn normalize_data(&mut self) {
        if self.frame_pre_length == 0 {
            // Prepend the fake ethernet header for raw IP packets.
            let mut new_data = ETHERNET_HEADER.to_vec();
            new_data.append(&mut self.data);
            self.data = new_data;
        } else if self.interface_name.starts_with("pdp_ip") {
            // For cellular interfaces, skip the first 4 bytes of the original data
            // before prepending the header.
            if self.data.len() >= 4 {
                let mut new_data = ETHERNET_HEADER.to_vec();
                new_data.extend_from_slice(&self.data[4..]);
                self.data = new_data;
            }
        }
    }

    /// Parses a byte vector into a DevicePacket.
    ///
    /// This is the primary method for creating a struct from the raw data
    /// received from the device.
    ///
    /// # Arguments
    /// * `bytes` - A `Vec<u8>` containing the raw bytes of a single packet frame.
    ///
    /// # Returns
    /// A `Result` containing the parsed `DevicePacket`
    pub fn from_vec(bytes: &[u8]) -> Result<Self, IdeviceError> {
        let mut r = ByteReader::new(bytes);

        // --- Parse Header ---
        let header_length = r.read_u32_be()?;
        let header_version = r.read_u8()?;
        let packet_length = r.read_u32_be()?;
        let interface_type = r.read_u8()?;
        let unit = r.read_u16_be()?;
        let io = r.read_u8()?;
        let protocol_family = r.read_u32_be()?;
        let frame_pre_length = r.read_u32_be()?;
        let frame_post_length = r.read_u32_be()?;
        let interface_name = r.read_cstr(16)?;
        let pid = r.read_u32_le()?; // Little Endian
        let comm = r.read_cstr(17)?;
        let svc = r.read_u32_be()?;
        let epid = r.read_u32_le()?; // Little Endian
        let ecomm = r.read_cstr(17)?;
        let seconds = r.read_u32_be()?;
        let microseconds = r.read_u32_be()?;

        // --- Extract Packet Data ---
        // The data starts at an absolute offset defined by `header_length`.
        let data_start = header_length as usize;
        let data_end = data_start.saturating_add(packet_length as usize);

        if data_end > bytes.len() {
            return Err(IdeviceError::NotEnoughBytes(bytes.len(), data_end));
        }
        let data = bytes[data_start..data_end].to_vec();

        Ok(DevicePacket {
            header_length,
            header_version,
            packet_length,
            interface_type,
            unit,
            io,
            protocol_family,
            frame_pre_length,
            frame_post_length,
            interface_name,
            pid,
            comm,
            svc,
            epid,
            ecomm,
            seconds,
            microseconds,
            data,
        })
    }
}

/// A helper struct to safely read from a byte slice.
struct ByteReader<'a> {
    slice: &'a [u8],
    cursor: usize,
}

impl<'a> ByteReader<'a> {
    fn new(slice: &'a [u8]) -> Self {
        Self { slice, cursor: 0 }
    }

    /// Reads an exact number of bytes and advances the cursor.
    fn read_exact(&mut self, len: usize) -> Result<&'a [u8], IdeviceError> {
        let end = self
            .cursor
            .checked_add(len)
            .ok_or(IdeviceError::IntegerOverflow)?;
        if end > self.slice.len() {
            return Err(IdeviceError::NotEnoughBytes(len, self.slice.len()));
        }
        let result = &self.slice[self.cursor..end];
        self.cursor = end;
        Ok(result)
    }

    fn read_u8(&mut self) -> Result<u8, IdeviceError> {
        self.read_exact(1).map(|s| s[0])
    }

    fn read_u16_be(&mut self) -> Result<u16, IdeviceError> {
        self.read_exact(2)
            .map(|s| u16::from_be_bytes(s.try_into().unwrap()))
    }

    fn read_u32_be(&mut self) -> Result<u32, IdeviceError> {
        self.read_exact(4)
            .map(|s| u32::from_be_bytes(s.try_into().unwrap()))
    }

    fn read_u32_le(&mut self) -> Result<u32, IdeviceError> {
        self.read_exact(4)
            .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
    }

    /// Reads a fixed-size, null-padded C-style string.
    fn read_cstr(&mut self, len: usize) -> Result<String, IdeviceError> {
        let buffer = self.read_exact(len)?;
        let end = buffer.iter().position(|&b| b == 0).unwrap_or(len);
        String::from_utf8(buffer[..end].to_vec()).map_err(IdeviceError::Utf8)
    }
}

/// A writer for creating `.pcap` files from DevicePackets without external dependencies.
#[derive(Debug)]
pub struct PcapFileWriter<W: AsyncWrite + Unpin> {
    writer: W,
}

impl<W: AsyncWrite + Unpin> PcapFileWriter<W> {
    /// Creates a new writer and asynchronously writes the pcap global header.
    pub async fn new(mut writer: W) -> Result<Self, std::io::Error> {
        // Correct pcap global header for LINKTYPE_ETHERNET.
        // We use big-endian format, as is traditional.
        let header = [
            0xa1, 0xb2, 0xc3, 0xd4, // magic number (big-endian)
            0x00, 0x02, // version_major
            0x00, 0x04, // version_minor
            0x00, 0x00, 0x00, 0x00, // thiszone (GMT)
            0x00, 0x00, 0x00, 0x00, // sigfigs (accuracy)
            0x00, 0x04, 0x00, 0x00, // snaplen (max packet size, 262144)
            0x00, 0x00, 0x00, 0x01, // network (LINKTYPE_ETHERNET)
        ];
        writer.write_all(&header).await?;
        Ok(Self { writer })
    }

    /// Asynchronously writes a single DevicePacket to the pcap file.
    pub async fn write_packet(&mut self, packet: &DevicePacket) -> Result<(), std::io::Error> {
        let mut record_header = [0u8; 16];

        // Use the packet's own timestamp for accuracy.
        record_header[0..4].copy_from_slice(&packet.seconds.to_be_bytes());
        record_header[4..8].copy_from_slice(&packet.microseconds.to_be_bytes());

        // incl_len and orig_len
        let len_bytes = (packet.data.len() as u32).to_be_bytes();
        record_header[8..12].copy_from_slice(&len_bytes);
        record_header[12..16].copy_from_slice(&len_bytes);

        // Write the record header and packet data sequentially.
        self.writer.write_all(&record_header).await?;
        self.writer.write_all(&packet.data).await?;

        Ok(())
    }
}
