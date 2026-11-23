// Jackson Coxson

use plist_macro::plist_to_xml_bytes;
use tracing::warn;

#[derive(Debug)]
pub struct RawPacket {
    pub size: u32,
    pub version: u32,
    pub message: u32,
    pub tag: u32,
    pub plist: plist::Dictionary,
}

impl RawPacket {
    pub fn new(plist: plist::Dictionary, version: u32, message: u32, tag: u32) -> RawPacket {
        let plist_bytes = plist_to_xml_bytes(&plist);
        let size = plist_bytes.len() as u32 + 16;
        RawPacket {
            size,
            version,
            message,
            tag,
            plist,
        }
    }
}

impl From<RawPacket> for Vec<u8> {
    fn from(raw_packet: RawPacket) -> Vec<u8> {
        let mut packet = vec![];
        packet.extend_from_slice(&raw_packet.size.to_le_bytes());
        packet.extend_from_slice(&raw_packet.version.to_le_bytes());
        packet.extend_from_slice(&raw_packet.message.to_le_bytes());
        packet.extend_from_slice(&raw_packet.tag.to_le_bytes());
        packet.extend_from_slice(&plist_to_xml_bytes(&raw_packet.plist));
        packet
    }
}

impl TryFrom<&mut Vec<u8>> for RawPacket {
    type Error = ();
    fn try_from(packet: &mut Vec<u8>) -> Result<Self, Self::Error> {
        let packet: &[u8] = packet;
        packet.try_into()
    }
}

impl TryFrom<&[u8]> for RawPacket {
    type Error = ();
    fn try_from(packet: &[u8]) -> Result<Self, ()> {
        // Determine if we have enough data to parse
        if packet.len() < 16 {
            warn!("Not enough data to parse a raw packet header");
            return Err(());
        }

        let packet_size = &packet[0..4];
        let packet_size = u32::from_le_bytes(match packet_size.try_into() {
            Ok(packet_size) => packet_size,
            Err(_) => {
                warn!("Failed to parse packet size");
                return Err(());
            }
        });

        // Determine if we have enough data to parse
        if packet.len() < packet_size as usize {
            warn!("Not enough data to parse a raw packet body");
            return Err(());
        }

        let packet_version = &packet[4..8];
        let packet_version = u32::from_le_bytes(match packet_version.try_into() {
            Ok(packet_version) => packet_version,
            Err(_) => {
                warn!("Failed to parse packet version");
                return Err(());
            }
        });

        let message = &packet[8..12];
        let message = u32::from_le_bytes(match message.try_into() {
            Ok(message) => message,
            Err(_) => {
                warn!("Failed to parse packet message");
                return Err(());
            }
        });

        let packet_tag = &packet[12..16];
        let packet_tag = u32::from_le_bytes(match packet_tag.try_into() {
            Ok(packet_tag) => packet_tag,
            Err(_) => {
                warn!("Failed to parse packet tag");
                return Err(());
            }
        });

        let plist = &packet[16..packet_size as usize];
        let plist = if let Ok(p) = plist::from_bytes(plist) {
            p
        } else {
            warn!("Failed to parse packet plist");
            return Err(());
        };

        Ok(RawPacket {
            size: packet_size,
            version: packet_version,
            message,
            tag: packet_tag,
            plist,
        })
    }
}
