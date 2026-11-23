//! CoreDeviceProxy service for iOS's CoreDeviceProxy daemon.
//!
//! This service starts an L3 (TUN) tunnel for "trusted" services introduced
//! in iOS 17. Over USB it connects to `com.apple.internal.devicecompute.CoreDeviceProxy`;
//! over the network the same CDTunnel protocol runs over TLS-PSK.
//!
//! The CDTunnel framing and handshake are implemented in [`crate::remote_pairing::tunnel`].
//!
//! # Features
//! - `tunnel_tcp_stack`: Enables software TCP/IP tunnel creation using jktcp.

use crate::tunnel::{CdTunnel, TunnelInfo};
use crate::{Idevice, IdeviceError, IdeviceService, ReadWrite, obf};

/// A high-level client for the `com.apple.internal.devicecompute.CoreDeviceProxy` service.
///
/// Wraps a [`CdTunnel`] established over the USB CoreDeviceProxy service connection.
pub struct CoreDeviceProxy {
    /// The underlying CDTunnel carrying raw IPv6 packets.
    tunnel: CdTunnel<Box<dyn ReadWrite>>,
}

impl std::fmt::Debug for CoreDeviceProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreDeviceProxy")
            .field("info", &self.tunnel.info)
            .finish()
    }
}

impl IdeviceService for CoreDeviceProxy {
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.internal.devicecompute.CoreDeviceProxy")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, IdeviceError> {
        Self::new(idevice).await
    }
}

impl CoreDeviceProxy {
    /// Constructs a new `CoreDeviceProxy` by performing a CDTunnel handshake.
    pub async fn new(mut idevice: Idevice) -> Result<Self, IdeviceError> {
        let socket = idevice
            .socket
            .take()
            .ok_or(IdeviceError::NoEstablishedConnection)?;
        let tunnel = CdTunnel::handshake(socket).await?;
        Ok(Self { tunnel })
    }

    /// Returns the tunnel info (addresses, MTU, RSD port) from the handshake.
    pub fn tunnel_info(&self) -> &TunnelInfo {
        &self.tunnel.info
    }

    /// Sends a raw data packet through the tunnel.
    pub async fn send(&mut self, data: &[u8]) -> Result<(), IdeviceError> {
        self.tunnel.send_packet(data).await
    }

    /// Receives a raw IPv6 packet from the tunnel.
    pub async fn recv(&mut self) -> Result<Vec<u8>, IdeviceError> {
        self.tunnel.recv_packet().await
    }

    /// Consumes the proxy and returns the inner `CdTunnel`.
    pub fn into_tunnel(self) -> CdTunnel<Box<dyn ReadWrite>> {
        self.tunnel
    }

    /// Creates a software-based TCP tunnel adapter using jktcp.
    #[cfg(feature = "tunnel_tcp_stack")]
    pub fn create_software_tunnel(self) -> Result<crate::tcp::adapter::Adapter, IdeviceError> {
        let our_ip = self
            .tunnel
            .info
            .client_address
            .parse::<std::net::IpAddr>()?;
        let their_ip = self
            .tunnel
            .info
            .server_address
            .parse::<std::net::IpAddr>()?;
        // The inner stream is Box<dyn crate::ReadWrite> but jktcp wants Box<dyn jktcp::ReadWrite>.
        // Both traits have the same bounds, and IdeviceSocket implements both.
        // Re-box through the jktcp trait.
        let mtu = self.tunnel.info.mtu as usize;
        let stream: Box<dyn crate::ReadWrite> = self.tunnel.into_inner();
        let mut adapter = crate::tcp::adapter::Adapter::new(Box::new(stream), our_ip, their_ip);
        adapter.set_mss(mtu.saturating_sub(60));
        Ok(adapter)
    }
}
