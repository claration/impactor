// Jackson Coxson

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use serde::Deserialize;
use tracing::{debug, warn};

#[cfg(not(windows))]
use libc::{AF_INET, AF_INET6};

#[cfg(windows)]
const AF_INET: i32 = 2;
#[cfg(windows)]
const AF_INET6: i32 = 23;

use crate::{
    IdeviceError,
    usbmuxd::{Connection, UsbmuxdDevice},
};

#[derive(Deserialize)]
pub struct ListDevicesResponse {
    #[serde(rename = "DeviceList")]
    pub device_list: Vec<DeviceListResponse>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DeviceListResponse {
    #[serde(rename = "DeviceID")]
    pub device_id: u32,
    #[serde(rename = "Properties")]
    pub properties: DevicePropertiesResponse,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DevicePropertiesResponse {
    #[serde(rename = "ConnectionType")]
    pub connection_type: String,
    #[serde(rename = "NetworkAddress")]
    pub network_address: Option<plist::Data>,
    #[serde(rename = "SerialNumber")]
    pub serial_number: String,
}

impl DeviceListResponse {
    pub fn into_usbmuxd_dev(self) -> Result<UsbmuxdDevice, IdeviceError> {
        self.try_into()
    }
}

impl TryFrom<DeviceListResponse> for UsbmuxdDevice {
    type Error = IdeviceError;

    fn try_from(dev: DeviceListResponse) -> Result<Self, Self::Error> {
        let connection_type = match dev.properties.connection_type.as_str() {
            "Network" => {
                if let Some(addr) = dev.properties.network_address {
                    let addr = &Into::<Vec<u8>>::into(addr);
                    if addr.len() < 8 {
                        warn!("Device address bytes len < 8");
                        return Err(IdeviceError::UnexpectedResponse(
                            "network address too short, expected at least 8 bytes".into(),
                        ));
                    }

                    // macOS sets the first byte as the len, which are:
                    //  0x10 -> IPv4
                    //  0x1C -> IPv6
                    //
                    // either way, IPv4 is always 16 bytes and IPv6 is always 28 bytes
                    match addr.as_slice() {
                        // it's an IPv4 address, but the len is short
                        [family, ..] | [0x10, family, ..]
                            if *family == AF_INET as u8 && addr.len() < 0x10 =>
                        {
                            warn!("IPv4 address is less than 16 bytes");
                            return Err(IdeviceError::UnexpectedResponse(
                                "IPv4 network address too short, expected 16 bytes".into(),
                            ));
                        }

                        [family, ..] | [0x10, family, ..] if *family == AF_INET as u8 => {
                            // IPv4
                            Connection::Network(IpAddr::V4(Ipv4Addr::new(
                                addr[4], addr[5], addr[6], addr[7],
                            )))
                        }

                        [family, ..] | [0x1C, family, ..]
                            if *family == AF_INET6 as u8 && addr.len() < 28 =>
                        {
                            warn!("IPv6 address is less than 28 bytes");
                            return Err(IdeviceError::UnexpectedResponse(
                                "IPv6 network address too short, expected 28 bytes".into(),
                            ));
                        }

                        [family, ..] | [0x1C, family, ..] if *family == AF_INET6 as u8 => {
                            // IPv6
                            Connection::Network(IpAddr::V6(Ipv6Addr::new(
                                u16::from_be_bytes([addr[8], addr[9]]),
                                u16::from_be_bytes([addr[10], addr[11]]),
                                u16::from_be_bytes([addr[12], addr[13]]),
                                u16::from_be_bytes([addr[14], addr[15]]),
                                u16::from_be_bytes([addr[16], addr[17]]),
                                u16::from_be_bytes([addr[18], addr[19]]),
                                u16::from_be_bytes([addr[20], addr[21]]),
                                u16::from_be_bytes([addr[22], addr[23]]),
                            )))
                        }

                        // starts with IPv6 len, but it's not IPv6
                        [0x1C, addr_family, ..] if *addr_family != AF_INET6 as u8 => {
                            warn!(
                                "Expected IPv6 family ({:02X}) but got {:02X} for length 0x1C",
                                AF_INET6, addr_family
                            );
                            Connection::Unknown(format!("Network {:02X}", addr_family))
                        }

                        // starts with IPv4 len, but it's not IPv4
                        [0x10, addr_family, ..] if *addr_family != AF_INET as u8 => {
                            warn!(
                                "Expected IPv4 family ({:02X}) but got {:02X} for length 0x10",
                                AF_INET, addr_family
                            );
                            Connection::Unknown(format!("Network {:02X}", addr_family))
                        }

                        _ => {
                            warn!("Unknown IP address protocol: {:02X}", addr[0]);
                            Connection::Unknown(format!("Network {:02X}", addr[0]))
                        }
                    }
                } else {
                    warn!("Device is network attached, but has no network info");
                    return Err(IdeviceError::UnexpectedResponse(
                        "network device missing NetworkAddress field".into(),
                    ));
                }
            }
            "USB" => Connection::Usb,
            _ => Connection::Unknown(dev.properties.connection_type),
        };
        debug!("Connection type: {connection_type:?}");
        Ok(UsbmuxdDevice {
            connection_type,
            udid: dev.properties.serial_number,
            device_id: dev.device_id,
        })
    }
}
