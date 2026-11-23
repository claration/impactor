//! USB Multiplexing Daemon (usbmuxd) Client
//!
//! Provides functionality for interacting with the usbmuxd service which manages
//! connections to iOS devices over USB and network and pairing files

use std::{
    net::{AddrParseError, IpAddr, SocketAddr},
    pin::Pin,
    str::FromStr,
};

#[cfg(not(unix))]
use std::net::{Ipv4Addr, SocketAddrV4};

use futures::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, warn};

use crate::{
    Idevice, IdeviceError, ReadWrite, pairing_file::PairingFile, provider::UsbmuxdProvider,
    usbmuxd::des::DeviceListResponse,
};

use errors::UsbmuxdError;

mod des;
pub mod errors;
mod raw_packet;

/// Represents the connection type of a device
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    /// Connected via USB
    Usb,
    /// Connected via network with specific IP address
    Network(IpAddr),
    /// Unknown connection type with description
    Unknown(String),
}

/// Represents a device connected through usbmuxd
#[derive(Debug, Clone)]
pub struct UsbmuxdDevice {
    /// How the device is connected
    pub connection_type: Connection,
    /// Unique Device Identifier
    pub udid: String,
    /// usbmuxd-assigned device ID
    pub device_id: u32,
}

/// Listen events from the socket
#[derive(Debug, Clone)]
pub enum UsbmuxdListenEvent {
    Connected(UsbmuxdDevice),
    /// The mux ID
    Disconnected(u32),
}

/// Active connection to the usbmuxd service
#[derive(Debug)]
pub struct UsbmuxdConnection {
    socket: Box<dyn ReadWrite>,
    tag: u32,
}

/// Address of the usbmuxd service
#[allow(missing_copy_implementations)]
#[derive(Clone, Debug)]
pub enum UsbmuxdAddr {
    /// Unix domain socket path (Unix systems only)
    #[cfg(unix)]
    UnixSocket(String),
    /// TCP socket address
    TcpSocket(SocketAddr),
}

impl UsbmuxdAddr {
    /// Default TCP port for usbmuxd
    pub const DEFAULT_PORT: u16 = 27015;
    /// Default Unix socket path for usbmuxd
    pub const SOCKET_FILE: &'static str = "/var/run/usbmuxd";

    /// Connects to the usbmuxd service
    ///
    /// # Returns
    /// A boxed transport stream
    ///
    /// # Errors
    /// Returns `IdeviceError` if connection fails
    pub async fn to_socket(&self) -> Result<Box<dyn ReadWrite>, IdeviceError> {
        Ok(match self {
            #[cfg(unix)]
            Self::UnixSocket(addr) => Box::new(tokio::net::UnixStream::connect(addr).await?),
            Self::TcpSocket(addr) => Box::new(tokio::net::TcpStream::connect(addr).await?),
        })
    }

    /// Creates a new usbmuxd connection
    ///
    /// # Arguments
    /// * `tag` - Connection tag/identifier
    ///
    /// # Returns
    /// A connected `UsbmuxdConnection`
    pub async fn connect(&self, tag: u32) -> Result<UsbmuxdConnection, IdeviceError> {
        let socket = self.to_socket().await?;
        Ok(UsbmuxdConnection::new(socket, tag))
    }

    /// Creates a UsbmuxdAddr from environment variable
    ///
    /// Checks `USBMUXD_SOCKET_ADDRESS` environment variable, falls back to default
    ///
    /// # Returns
    /// Configured UsbmuxdAddr or parse error
    pub fn from_env_var() -> Result<Self, AddrParseError> {
        Ok(match std::env::var("USBMUXD_SOCKET_ADDRESS") {
            Ok(var) => {
                #[cfg(unix)]
                if var.contains(':') {
                    Self::TcpSocket(SocketAddr::from_str(&var)?)
                } else {
                    Self::UnixSocket(var)
                }
                #[cfg(not(unix))]
                Self::TcpSocket(SocketAddr::from_str(&var)?)
            }
            Err(_) => Self::default(),
        })
    }
}

impl Default for UsbmuxdAddr {
    /// Creates default usbmuxd address based on platform:
    /// - Unix: Uses default socket path
    /// - Non-Unix: Uses localhost TCP port
    fn default() -> Self {
        #[cfg(not(unix))]
        {
            Self::TcpSocket(SocketAddr::V4(SocketAddrV4::new(
                Ipv4Addr::new(127, 0, 0, 1),
                Self::DEFAULT_PORT,
            )))
        }
        #[cfg(unix)]
        Self::UnixSocket(Self::SOCKET_FILE.to_string())
    }
}

impl UsbmuxdConnection {
    /// Binary PLIST protocol version
    pub const BINARY_PLIST_VERSION: u32 = 0;
    /// XML PLIST protocol version
    pub const XML_PLIST_VERSION: u32 = 1;

    /// Result message type
    pub const RESULT_MESSAGE_TYPE: u32 = 1;
    /// PLIST message type
    pub const PLIST_MESSAGE_TYPE: u32 = 8;

    /// Creates a default usbmuxd connection
    ///
    /// Uses default address based on platform
    ///
    /// # Returns
    /// Connected `UsbmuxdConnection` or error
    pub async fn default() -> Result<Self, IdeviceError> {
        let socket = UsbmuxdAddr::default().to_socket().await?;

        Ok(Self {
            socket: Box::new(socket),
            tag: 0,
        })
    }

    /// Creates a new usbmuxd connection
    ///
    /// # Arguments
    /// * `socket` - The transport stream
    /// * `tag` - Connection tag/identifier
    pub fn new(socket: Box<dyn ReadWrite>, tag: u32) -> Self {
        Self { socket, tag }
    }

    /// Lists all connected devices
    ///
    /// # Returns
    /// Vector of connected devices
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - Response is malformed
    /// - Device info is incomplete
    pub async fn get_devices(&mut self) -> Result<Vec<UsbmuxdDevice>, IdeviceError> {
        let mut req = plist::Dictionary::new();
        req.insert("MessageType".into(), "ListDevices".into());
        req.insert("ClientVersionString".into(), "idevice-rs".into());
        req.insert("kLibUSBMuxVersion".into(), 3.into());
        self.write_plist(req).await?;
        let res = self.read_plist().await?;
        let res = plist::to_value(&res)?;
        let res = plist::from_value::<des::ListDevicesResponse>(&res)?;

        let devs = res
            .device_list
            .into_iter()
            .flat_map(|x| x.into_usbmuxd_dev())
            .collect::<Vec<UsbmuxdDevice>>();

        Ok(devs)
    }

    /// Gets a specific device by UDID
    ///
    /// # Arguments
    /// * `udid` - The device UDID to find
    ///
    /// # Returns
    /// The matching device or error if not found
    pub async fn get_device(&mut self, udid: &str) -> Result<UsbmuxdDevice, IdeviceError> {
        let devices = self.get_devices().await?;
        match devices.into_iter().find(|x| x.udid == udid) {
            Some(d) => Ok(d),
            None => Err(IdeviceError::DeviceNotFound),
        }
    }

    /// Gets the pairing record for a device
    ///
    /// # Arguments
    /// * `udid` - The device UDID
    ///
    /// # Returns
    /// The pairing file or error
    pub async fn get_pair_record(&mut self, udid: &str) -> Result<PairingFile, IdeviceError> {
        debug!("Getting pair record for {udid}");
        let mut req = plist::Dictionary::new();
        req.insert("MessageType".into(), "ReadPairRecord".into());
        req.insert("PairRecordID".into(), udid.into());
        self.write_plist(req).await?;
        let res = self.read_plist().await?;

        match res.get("PairRecordData") {
            Some(plist::Value::Data(d)) => PairingFile::from_bytes(d),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing PairRecordData in pair record response".into(),
            )),
        }
    }

    /// Gets the BUID
    ///
    /// # Returns
    /// The BUID string or error
    pub async fn get_buid(&mut self) -> Result<String, IdeviceError> {
        let mut req = plist::Dictionary::new();
        req.insert("MessageType".into(), "ReadBUID".into());
        self.write_plist(req).await?;
        let mut res = self.read_plist().await?;

        match res.remove("BUID") {
            Some(plist::Value::String(s)) => Ok(s),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing BUID string in ReadBUID response".into(),
            )),
        }
    }

    /// Connects to a service on the device
    ///
    /// # Arguments
    /// * `device_id` - usbmuxd device ID
    /// * `port` - TCP port to connect to (host byte order)
    /// * `label` - Connection label
    ///
    /// # Returns
    /// An `Idevice` connection or error
    pub async fn connect_to_device(
        mut self,
        device_id: u32,
        port: u16,
        label: impl Into<String>,
    ) -> Result<Idevice, IdeviceError> {
        debug!("Connecting to device {device_id} on port {port}");
        let port = port.to_be();

        let mut req = plist::Dictionary::new();
        req.insert("MessageType".into(), "Connect".into());
        req.insert("DeviceID".into(), device_id.into());
        req.insert("PortNumber".into(), port.into());
        self.write_plist(req).await?;
        match self.read_plist().await?.get("Number") {
            Some(plist::Value::Integer(i)) => match i.as_unsigned() {
                Some(0) => Ok(Idevice::new(self.socket, label)),
                Some(1) => Err(UsbmuxdError::BadCommand.into()),
                Some(2) => Err(UsbmuxdError::BadDevice.into()),
                Some(3) => Err(UsbmuxdError::ConnectionRefused.into()),
                Some(6) => Err(UsbmuxdError::BadVersion.into()),
                _ => Err(IdeviceError::UnexpectedResponse(
                    "unknown usbmuxd connect error code".into(),
                )),
            },
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing Number field in usbmuxd connect response".into(),
            )),
        }
    }

    /// Tells usbmuxd to save the pairing record in its storage
    ///
    /// # Arguments
    /// * `device_id` - usbmuxd device ID
    /// * `udid` - the device UDID/serial
    /// * `pair_record` - a serialized plist of the pair record
    pub async fn save_pair_record(
        &mut self,
        udid: &str,
        pair_record: Vec<u8>,
    ) -> Result<(), IdeviceError> {
        let req = crate::plist!(dict {
            "MessageType": "SavePairRecord",
            "PairRecordData": pair_record,
            "PairRecordID": udid,
        });
        self.write_plist(req).await?;
        let res = self.read_plist().await?;
        match res.get("Number").and_then(|x| x.as_unsigned_integer()) {
            Some(0) => Ok(()),
            _ => Err(IdeviceError::UnexpectedResponse(
                "SavePairRecord did not return success".into(),
            )),
        }
    }

    pub async fn listen<'a>(
        &'a mut self,
    ) -> Result<
        Pin<Box<dyn Stream<Item = Result<UsbmuxdListenEvent, IdeviceError>> + 'a>>,
        IdeviceError,
    > {
        let req = crate::plist!(dict {
            "MessageType": "Listen",
        });
        self.write_plist(req).await?;

        // First, read the handshake response to confirm the "Listen" request was successful
        let res = self.read_plist().await?;
        match res.get("Number").and_then(|x| x.as_unsigned_integer()) {
            Some(0) => {
                // Success, now create the stream
                let stream = futures::stream::try_unfold(self, |conn| async move {
                    // This loop is to skip non-Attach/Detach messages
                    loop {
                        // Read the next packet. This will propagate IO errors.
                        let msg = conn.read_plist().await?;

                        if let Some(plist::Value::String(s)) = msg.get("MessageType") {
                            match s.as_str() {
                                "Attached" => {
                                    if let Ok(props) = plist::from_value::<DeviceListResponse>(
                                        &plist::Value::Dictionary(msg),
                                    ) {
                                        let dev: UsbmuxdDevice = match props.into_usbmuxd_dev() {
                                            Ok(d) => d,
                                            Err(e) => {
                                                warn!(
                                                    "Failed to convert props into usbmuxd device: {e:?}"
                                                );
                                                continue;
                                            }
                                        };

                                        let res = UsbmuxdListenEvent::Connected(dev);

                                        // Yield the device and the next state
                                        return Ok(Some((res, conn)));
                                    } else {
                                        warn!(
                                            "Received malformed message during listen (no device props and ID)"
                                        );
                                    }
                                }
                                "Detached" => {
                                    // Log it and continue the loop to wait for the next message
                                    if let Some(id) =
                                        msg.get("DeviceID").and_then(|v| v.as_unsigned_integer())
                                    {
                                        let res = UsbmuxdListenEvent::Disconnected(id as u32);
                                        return Ok(Some((res, conn)));
                                    } else {
                                        debug!("Device detached (unknown ID)");
                                    }
                                    // Continue loop
                                }
                                _ => {
                                    // Unexpected message type, log and continue
                                    warn!("Received unexpected message type during listen: {}", s);
                                    // Continue loop
                                }
                            }
                        } else {
                            // Malformed message, log and continue
                            warn!("Received malformed message during listen (no MessageType)");
                            // Continue loop
                        }
                    }
                });

                // Box and Pin the stream
                Ok(Box::pin(stream))
            }
            _ => {
                // "Listen" request failed
                Err(IdeviceError::UnexpectedResponse(
                    "usbmuxd Listen request did not return success".into(),
                ))
            }
        }
    }

    /// Writes a PLIST message to usbmuxd
    async fn write_plist(&mut self, req: plist::Dictionary) -> Result<(), IdeviceError> {
        let raw = raw_packet::RawPacket::new(
            req,
            Self::XML_PLIST_VERSION,
            Self::PLIST_MESSAGE_TYPE,
            self.tag,
        );

        let raw: Vec<u8> = raw.into();
        self.socket.write_all(&raw).await?;

        Ok(())
    }

    /// Reads a PLIST message from usbmuxd
    async fn read_plist(&mut self) -> Result<plist::Dictionary, IdeviceError> {
        let mut header_buffer = [0; 16];
        self.socket.read_exact(&mut header_buffer).await?;

        // We are safe to unwrap as it only panics if the buffer isn't 4
        let packet_size = u32::from_le_bytes(header_buffer[..4].try_into().unwrap()) - 16;
        debug!("Reading {packet_size} bytes from muxer");

        let mut body_buffer = vec![0; packet_size as usize];
        self.socket.read_exact(&mut body_buffer).await?;

        let res = plist::from_bytes(&body_buffer)?;
        debug!("Read from muxer: {}", crate::pretty_print_dictionary(&res));

        Ok(res)
    }
}

impl UsbmuxdDevice {
    /// Creates a provider for this device
    ///
    /// # Arguments
    /// * `addr` - usbmuxd address
    /// * `tag` - Connection tag
    /// * `label` - Connection label
    ///
    /// # Returns
    /// Configured `UsbmuxdProvider`
    pub fn to_provider(&self, addr: UsbmuxdAddr, label: impl Into<String>) -> UsbmuxdProvider {
        let label = label.into();

        UsbmuxdProvider {
            addr,
            tag: self.device_id,
            udid: self.udid.clone(),
            device_id: self.device_id,
            label,
        }
    }
}
