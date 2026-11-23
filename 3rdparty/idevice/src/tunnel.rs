// Jackson Coxson
//! CDTunnel protocol for establishing IPv6 tunnels to iOS devices.
//!
//! This module is transport-agnostic: the CDTunnel handshake and packet I/O
//! work over any [`ReadWrite`] stream, whether that's a direct USB socket
//! (via CoreDeviceProxy), a TLS-PSK encrypted TCP connection (via remoted),
//! or anything else.

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::debug;

use crate::{IdeviceError, ReadWrite};

const CDTUNNEL_MAGIC: &[u8] = b"CDTunnel";
const IPV6_HEADER_SIZE: usize = 40;
const DEFAULT_MTU: u16 = 16000;

/// Result of the CDTunnel handshake containing network configuration.
#[derive(Debug, Clone)]
pub struct TunnelInfo {
    /// IPv6 address assigned to the host side of the tunnel
    pub client_address: String,
    /// Subnet mask for the tunnel (may be empty if not provided)
    pub netmask: String,
    /// IPv6 address of the device side of the tunnel
    pub server_address: String,
    /// Negotiated MTU for the tunnel
    pub mtu: u16,
    /// RSD port on the device (accessible through the tunnel)
    pub server_rsd_port: u16,
}

/// A CDTunnel connection that carries raw IPv6 packets.
///
/// After handshake, call `send_packet` / `recv_packet` to exchange
/// raw IPv6 packets with the device. These can be fed into jktcp or
/// a TUN device.
#[derive(Debug)]
pub struct CdTunnel<R: ReadWrite> {
    pub(crate) inner: R,
    pub info: TunnelInfo,
}

impl<R: ReadWrite> CdTunnel<R> {
    /// Perform the CDTunnel handshake on an already-connected (and optionally
    /// TLS-wrapped) stream. Returns a tunnel ready for packet I/O.
    pub async fn handshake(mut stream: R) -> Result<Self, IdeviceError> {
        let request = serde_json::json!({
            "type": "clientHandshakeRequest",
            "mtu": DEFAULT_MTU
        });
        let body =
            serde_json::to_vec(&request).map_err(|e| IdeviceError::InternalError(e.to_string()))?;

        stream.write_all(CDTUNNEL_MAGIC).await?;
        stream.write_all(&(body.len() as u16).to_be_bytes()).await?;
        stream.write_all(&body).await?;
        stream.flush().await?;

        debug!("Sent CDTunnel handshake request");

        let mut magic_buf = vec![0u8; CDTUNNEL_MAGIC.len()];
        stream.read_exact(&mut magic_buf).await?;
        if magic_buf != CDTUNNEL_MAGIC {
            return Err(IdeviceError::UnexpectedResponse(
                "invalid CDTunnel magic in handshake response".into(),
            ));
        }

        let mut len_buf = [0u8; 2];
        stream.read_exact(&mut len_buf).await?;
        let response_len = u16::from_be_bytes(len_buf) as usize;

        let mut response_buf = vec![0u8; response_len];
        stream.read_exact(&mut response_buf).await?;

        let response: serde_json::Value = serde_json::from_slice(&response_buf)
            .map_err(|e| IdeviceError::InternalError(e.to_string()))?;

        debug!("CDTunnel handshake response: {response:#?}");

        let client_params =
            response
                .get("clientParameters")
                .ok_or(IdeviceError::UnexpectedResponse(
                    "missing clientParameters in CDTunnel handshake response".into(),
                ))?;

        let client_address = client_params
            .get("address")
            .and_then(|a| a.as_str())
            .ok_or(IdeviceError::UnexpectedResponse(
                "missing client address in CDTunnel handshake response".into(),
            ))?
            .to_string();

        let mtu = client_params
            .get("mtu")
            .and_then(|m| m.as_u64())
            .unwrap_or(1500) as u16;

        let server_address = response
            .get("serverAddress")
            .and_then(|a| a.as_str())
            .ok_or(IdeviceError::UnexpectedResponse(
                "missing serverAddress in CDTunnel handshake response".into(),
            ))?
            .to_string();

        let server_rsd_port = response
            .get("serverRSDPort")
            .and_then(|p| p.as_u64())
            .unwrap_or(0) as u16;

        let info = TunnelInfo {
            client_address,
            netmask: client_params
                .get("netmask")
                .and_then(|n| n.as_str())
                .unwrap_or("")
                .to_string(),
            server_address,
            mtu,
            server_rsd_port,
        };

        debug!("CDTunnel established: {info:?}");

        Ok(Self {
            inner: stream,
            info,
        })
    }

    /// Send a raw IPv6 packet to the device through the tunnel.
    pub async fn send_packet(&mut self, packet: &[u8]) -> Result<(), IdeviceError> {
        self.inner.write_all(packet).await?;
        self.inner.flush().await?;
        Ok(())
    }

    /// Receive a raw IPv6 packet from the device through the tunnel.
    /// Returns the complete IPv6 packet (header + payload).
    pub async fn recv_packet(&mut self) -> Result<Vec<u8>, IdeviceError> {
        let mut header = [0u8; IPV6_HEADER_SIZE];
        self.inner.read_exact(&mut header).await?;

        let payload_len = u16::from_be_bytes([header[4], header[5]]) as usize;

        let mut payload = vec![0u8; payload_len];
        self.inner.read_exact(&mut payload).await?;

        let mut packet = Vec::with_capacity(IPV6_HEADER_SIZE + payload_len);
        packet.extend_from_slice(&header);
        packet.extend_from_slice(&payload);

        Ok(packet)
    }

    /// Consume the tunnel and return the underlying stream.
    pub fn into_inner(self) -> R {
        self.inner
    }
}
