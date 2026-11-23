//! iOS Device Connection Providers
//!
//! Provides abstractions for establishing connections to iOS devices through different
//! transport mechanisms (TCP, USB, etc.).

use std::{future::Future, pin::Pin};

use std::net::IpAddr;
use std::net::{SocketAddr, SocketAddrV6};

#[cfg(feature = "tcp")]
use tokio::net::TcpStream;

use crate::{Idevice, IdeviceError, ReadWrite, pairing_file::PairingFile};

#[cfg(feature = "usbmuxd")]
use crate::usbmuxd::UsbmuxdAddr;

/// Trait for providers that can establish connections to iOS devices
///
/// This is an async trait that abstracts over different connection methods
/// (TCP, USB, etc.).
pub trait IdeviceProvider: Unpin + Send + Sync + std::fmt::Debug {
    /// Establishes a connection to the specified port on the device
    ///
    /// # Arguments
    /// * `port` - The port number to connect to
    ///
    /// # Returns
    /// A future that resolves to an `Idevice` connection handle
    fn connect(
        &self,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Idevice, IdeviceError>> + Send>>;

    /// Returns a label identifying this provider/connection
    fn label(&self) -> &str;

    /// Retrieves the pairing file needed for secure communication
    ///
    /// # Returns
    /// A future that resolves to the device's `PairingFile`
    fn get_pairing_file(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<PairingFile, IdeviceError>> + Send>>;
}

pub trait RsdProvider: Unpin + Send + Sync + std::fmt::Debug {
    fn connect_to_service_port(
        &mut self,
        port: u16,
    ) -> impl std::future::Future<Output = Result<Box<dyn ReadWrite>, IdeviceError>> + Send;
}

/// TCP-based device connection provider
#[cfg(feature = "tcp")]
#[derive(Debug)]
pub struct TcpProvider {
    /// IP address of the device
    pub addr: std::net::IpAddr,
    /// An optional scope ID may be provided for IPv6 addresses.
    pub scope_id: Option<u32>,
    /// Pairing file for secure communication
    pub pairing_file: PairingFile,
    /// Label identifying this connection
    pub label: String,
}

#[cfg(feature = "tcp")]
impl IdeviceProvider for TcpProvider {
    /// Connects to the device over TCP
    ///
    /// # Arguments
    /// * `port` - The TCP port to connect to
    ///
    /// # Returns
    /// An `Idevice` wrapped in a future
    fn connect(
        &self,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Idevice, IdeviceError>> + Send>> {
        let addr = self.addr;
        let label = self.label.clone();
        let scope_id = self.scope_id.unwrap_or(0);
        Box::pin(async move {
            let socket_addr = match addr {
                IpAddr::V4(_) => SocketAddr::new(addr, port),
                IpAddr::V6(ipv6) => SocketAddr::V6(SocketAddrV6::new(ipv6, port, 0, scope_id)),
            };
            let stream = TcpStream::connect(socket_addr).await?;
            Ok(Idevice::new(Box::new(stream), label))
        })
    }

    /// Returns the connection label
    fn label(&self) -> &str {
        &self.label
    }

    /// Returns the pairing file (cloned from the provider)
    fn get_pairing_file(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<PairingFile, IdeviceError>> + Send>> {
        let pairing_file = self.pairing_file.clone();
        Box::pin(async move { Ok(pairing_file) })
    }
}

/// USB-based device connection provider using usbmuxd
#[cfg(feature = "usbmuxd")]
#[derive(Debug, Clone)]
pub struct UsbmuxdProvider {
    /// USB connection address
    pub addr: UsbmuxdAddr,
    /// Connection tag/identifier
    pub tag: u32,
    /// Device UDID
    pub udid: String,
    /// Device ID
    pub device_id: u32,
    /// Connection label
    pub label: String,
}

#[cfg(feature = "usbmuxd")]
impl IdeviceProvider for UsbmuxdProvider {
    /// Connects to the device over USB via usbmuxd
    ///
    /// # Arguments
    /// * `port` - The port number to connect to on the device
    ///
    /// # Returns
    /// An `Idevice` wrapped in a future
    fn connect(
        &self,
        port: u16,
    ) -> Pin<Box<dyn Future<Output = Result<Idevice, IdeviceError>> + Send>> {
        let addr = self.addr.clone();
        let tag = self.tag;
        let device_id = self.device_id;
        let label = self.label.clone();

        Box::pin(async move {
            let usbmuxd = addr.connect(tag).await?;
            usbmuxd.connect_to_device(device_id, port, &label).await
        })
    }

    /// Returns the connection label
    fn label(&self) -> &str {
        &self.label
    }

    /// Retrieves the pairing record from usbmuxd
    fn get_pairing_file(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<PairingFile, IdeviceError>> + Send>> {
        let addr = self.addr.clone();
        let tag = self.tag;
        let udid = self.udid.clone();

        Box::pin(async move {
            let mut usbmuxd = addr.connect(tag).await?;
            usbmuxd.get_pair_record(&udid).await
        })
    }
}

#[cfg(feature = "tcp")]
impl RsdProvider for std::net::IpAddr {
    async fn connect_to_service_port(
        &mut self,
        port: u16,
    ) -> Result<Box<dyn ReadWrite>, IdeviceError> {
        Ok(Box::new(
            tokio::net::TcpStream::connect((*self, port)).await?,
        ))
    }
}
