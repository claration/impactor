//! Network monitor service - Monitor network connections on the device

use plist::Value;

use super::remote_server::{Channel, RemoteServerClient};
use crate::{IdeviceError, ReadWrite, obf};

pub const MESSAGE_TYPE_INTERFACE_DETECTION: u64 = 0;
pub const MESSAGE_TYPE_CONNECTION_DETECTION: u64 = 1;
pub const MESSAGE_TYPE_CONNECTION_UPDATE: u64 = 2;

/// A network interface that was detected
#[derive(Debug, Clone)]
pub struct InterfaceDetectionEvent {
    pub interface_index: u32,
    pub name: String,
}

/// A socket address (IPv4 or IPv6)
#[derive(Debug, Clone)]
pub struct SocketAddress {
    pub family: u8,
    pub port: u16,
    pub addr: String,
}

impl SocketAddress {
    /// Parse a raw socket address byte slice.
    /// Layout: u8 length, u8 family, u16be port, then family-specific address bytes.
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < 4 {
            return None;
        }
        let length = data[0];
        let family = data[1];
        let port = u16::from_be_bytes([data[2], data[3]]);

        let addr = match length {
            // IPv4: 16 bytes total, 4 addr bytes at offset 4
            0x10 if data.len() >= 8 => {
                format!("{}.{}.{}.{}", data[4], data[5], data[6], data[7])
            }
            // IPv6: 28 bytes total, 16 addr bytes at offset 8
            0x1c if data.len() >= 24 => {
                let bytes: [u8; 16] = data[8..24].try_into().ok()?;
                let addr = std::net::Ipv6Addr::from(bytes);
                addr.to_string()
            }
            _ => format!("unknown(family={})", family),
        };

        Some(SocketAddress { family, port, addr })
    }
}

/// A new network connection was detected
#[derive(Debug, Clone)]
pub struct ConnectionDetectionEvent {
    pub local_address: Option<SocketAddress>,
    pub remote_address: Option<SocketAddress>,
    pub interface_index: u32,
    pub pid: u32,
    pub recv_buffer_size: u64,
    pub recv_buffer_used: u64,
    pub serial_number: u64,
    pub kind: u32,
}

/// An existing connection was updated with new stats
#[derive(Debug, Clone, Copy)]
pub struct ConnectionUpdateEvent {
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub rx_dups: u64,
    pub rx_ooo: u64,
    pub tx_retx: u64,
    pub min_rtt: u64,
    pub avg_rtt: u64,
    pub connection_serial: u64,
    pub time: u64,
}

/// A network monitoring event
#[derive(Debug, Clone)]
pub enum NetworkEvent {
    InterfaceDetection(InterfaceDetectionEvent),
    ConnectionDetection(ConnectionDetectionEvent),
    ConnectionUpdate(ConnectionUpdateEvent),
    Unknown(u64),
}

/// Client for monitoring network connections
#[derive(Debug)]
pub struct NetworkMonitorClient<'a, R: ReadWrite> {
    channel: Channel<'a, R>,
}

impl<'a, R: ReadWrite> NetworkMonitorClient<'a, R> {
    pub async fn new(client: &'a mut RemoteServerClient<R>) -> Result<Self, IdeviceError> {
        let channel = client
            .make_channel(obf!("com.apple.instruments.server.services.networking"))
            .await?;
        Ok(Self { channel })
    }

    /// Starts monitoring. No reply is expected.
    pub async fn start_monitoring(&mut self) -> Result<(), IdeviceError> {
        self.channel
            .call_method(Some(Value::String("startMonitoring".into())), None, false)
            .await
    }

    /// Stops monitoring.
    pub async fn stop_monitoring(&mut self) -> Result<(), IdeviceError> {
        self.channel
            .call_method(Some(Value::String("stopMonitoring".into())), None, false)
            .await
    }

    /// Reads the next network event from the device.
    /// The device pushes events as arrays: [type, [args...]].
    pub async fn next_event(&mut self) -> Result<NetworkEvent, IdeviceError> {
        loop {
            let msg = self.channel.read_message().await?;

            let data = match msg.data {
                Some(d) => d,
                None => continue,
            };

            let arr = match data.into_array() {
                Some(a) => a,
                None => continue,
            };
            if arr.len() < 2 {
                continue;
            }
            let msg_type = match &arr[0] {
                Value::Integer(i) => i.as_unsigned().unwrap_or(u64::MAX),
                _ => continue,
            };
            let args = match arr.into_iter().nth(1).and_then(|v| v.into_array()) {
                Some(a) => a,
                None => continue,
            };

            let event = match msg_type {
                MESSAGE_TYPE_INTERFACE_DETECTION => {
                    let interface_index = get_u32_at(&args, 0);
                    let name = get_string_at(&args, 1);
                    NetworkEvent::InterfaceDetection(InterfaceDetectionEvent {
                        interface_index,
                        name,
                    })
                }
                MESSAGE_TYPE_CONNECTION_DETECTION => {
                    let local_address = get_bytes_at(&args, 0).and_then(SocketAddress::from_bytes);
                    let remote_address = get_bytes_at(&args, 1).and_then(SocketAddress::from_bytes);
                    let interface_index = get_u32_at(&args, 2);
                    let pid = get_u32_at(&args, 3);
                    let recv_buffer_size = get_u64_at(&args, 4);
                    let recv_buffer_used = get_u64_at(&args, 5);
                    let serial_number = get_u64_at(&args, 6);
                    let kind = get_u32_at(&args, 7);
                    NetworkEvent::ConnectionDetection(ConnectionDetectionEvent {
                        local_address,
                        remote_address,
                        interface_index,
                        pid,
                        recv_buffer_size,
                        recv_buffer_used,
                        serial_number,
                        kind,
                    })
                }
                MESSAGE_TYPE_CONNECTION_UPDATE => {
                    NetworkEvent::ConnectionUpdate(ConnectionUpdateEvent {
                        rx_packets: get_u64_at(&args, 0),
                        rx_bytes: get_u64_at(&args, 1),
                        tx_packets: get_u64_at(&args, 2),
                        tx_bytes: get_u64_at(&args, 3),
                        rx_dups: get_u64_at(&args, 4),
                        rx_ooo: get_u64_at(&args, 5),
                        tx_retx: get_u64_at(&args, 6),
                        min_rtt: get_u64_at(&args, 7),
                        avg_rtt: get_u64_at(&args, 8),
                        connection_serial: get_u64_at(&args, 9),
                        time: get_u64_at(&args, 10),
                    })
                }
                _ => NetworkEvent::Unknown(msg_type),
            };
            return Ok(event);
        }
    }
}

fn get_u32_at(arr: &[Value], idx: usize) -> u32 {
    arr.get(idx)
        .and_then(|v| match v {
            Value::Integer(i) => i.as_unsigned().map(|x| x as u32),
            _ => None,
        })
        .unwrap_or(0)
}

fn get_u64_at(arr: &[Value], idx: usize) -> u64 {
    arr.get(idx)
        .and_then(|v| match v {
            Value::Integer(i) => i.as_unsigned(),
            _ => None,
        })
        .unwrap_or(0)
}

fn get_string_at(arr: &[Value], idx: usize) -> String {
    arr.get(idx)
        .and_then(|v| v.as_string())
        .unwrap_or("")
        .to_string()
}

fn get_bytes_at(arr: &[Value], idx: usize) -> Option<&[u8]> {
    arr.get(idx).and_then(|v| v.as_data())
}
