// Jackson Coxson
//! TLS-PSK tunnel connect helpers for remote pairing.
//!
//! These functions combine TLS-PSK handshake + CDTunnel handshake into a single call.
//! The CDTunnel protocol itself lives in [`crate::tunnel`].

use tracing::debug;

use crate::{IdeviceError, ReadWrite};

// Re-export for backwards compatibility
pub use crate::tunnel::{CdTunnel, TunnelInfo};

const CDTUNNEL_MAGIC: &[u8] = b"CDTunnel";
const DEFAULT_MTU: u16 = 16000;

/// Wraps a `tokio::net::TcpStream` with TLS-PSK using a pure-Rust implementation
/// and performs the CDTunnel handshake, returning a ready-to-use tunnel.
///
/// `encryption_key` is the key from `RemotePairingClient::encryption_key()`.
///
/// This uses a built-in TLS 1.2 PSK-AES256-CBC-SHA384 implementation with no
/// external TLS library dependency.
pub async fn connect_tls_psk_tunnel_native(
    stream: Box<dyn ReadWrite>,
    encryption_key: &[u8],
) -> Result<CdTunnel<super::tls_psk::TlsPskStream<Box<dyn ReadWrite>>>, IdeviceError> {
    let mut tls_stream = super::tls_psk::tls_psk_handshake(stream, encryption_key).await?;
    debug!("Native TLS-PSK handshake complete");

    // CDTunnel handshake over TLS using the record-level API
    let request = serde_json::json!({
        "type": "clientHandshakeRequest",
        "mtu": DEFAULT_MTU
    });
    let body =
        serde_json::to_vec(&request).map_err(|e| IdeviceError::InternalError(e.to_string()))?;

    let mut pkt = Vec::new();
    pkt.extend_from_slice(CDTUNNEL_MAGIC);
    pkt.extend_from_slice(&(body.len() as u16).to_be_bytes());
    pkt.extend_from_slice(&body);
    tls_stream.write_app_data(&pkt).await?;

    debug!("Sent CDTunnel handshake request via TLS");

    let response_data = tls_stream.read_app_data().await?;
    if response_data.len() < CDTUNNEL_MAGIC.len() + 2 {
        return Err(IdeviceError::UnexpectedResponse(
            "CDTunnel handshake response too short".into(),
        ));
    }
    if &response_data[..CDTUNNEL_MAGIC.len()] != CDTUNNEL_MAGIC {
        return Err(IdeviceError::UnexpectedResponse(
            "CDTunnel handshake response missing magic header".into(),
        ));
    }
    let body_len = u16::from_be_bytes([
        response_data[CDTUNNEL_MAGIC.len()],
        response_data[CDTUNNEL_MAGIC.len() + 1],
    ]) as usize;
    let body_start = CDTUNNEL_MAGIC.len() + 2;
    let response_body = &response_data[body_start..body_start + body_len];

    let response: serde_json::Value = serde_json::from_slice(response_body)
        .map_err(|e| IdeviceError::InternalError(e.to_string()))?;

    debug!("CDTunnel handshake response: {response:#?}");

    let client_params =
        response
            .get("clientParameters")
            .ok_or(IdeviceError::UnexpectedResponse(
                "missing clientParameters in CDTunnel response".into(),
            ))?;

    let client_address = client_params
        .get("address")
        .and_then(|a| a.as_str())
        .ok_or(IdeviceError::UnexpectedResponse(
            "missing client address in CDTunnel response".into(),
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
            "missing server address in CDTunnel response".into(),
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

    Ok(CdTunnel {
        inner: tls_stream,
        info,
    })
}

/// Wraps a `tokio::net::TcpStream` with TLS-PSK using OpenSSL and performs
/// the CDTunnel handshake, returning a ready-to-use tunnel.
///
/// `encryption_key` is the key from `RemotePairingClient::encryption_key()`.
///
/// Requires the `openssl` feature. Consider using [`connect_tls_psk_tunnel_native`]
/// instead, which has no external dependency.
#[cfg(feature = "openssl")]
pub async fn connect_tls_psk_tunnel(
    stream: tokio::net::TcpStream,
    encryption_key: &[u8],
) -> Result<CdTunnel<tokio_openssl::SslStream<tokio::net::TcpStream>>, IdeviceError> {
    use openssl::ssl::{SslConnector, SslMethod, SslVerifyMode};

    let psk = encryption_key.to_vec();

    let mut builder = SslConnector::builder(SslMethod::tls_client())
        .map_err(|e| IdeviceError::InternalError(format!("SslConnector::builder: {e}")))?;

    builder.set_verify(SslVerifyMode::NONE);
    builder
        .set_cipher_list(
            "PSK-AES128-CBC-SHA:PSK-AES256-CBC-SHA:PSK-AES128-CBC-SHA256:PSK-AES256-CBC-SHA384",
        )
        .map_err(|e| IdeviceError::InternalError(format!("set_cipher_list: {e}")))?;
    builder.set_psk_client_callback(move |_ssl, _hint, identity, psk_out| {
        if !identity.is_empty() {
            identity[0] = 0;
        }
        let len = psk.len().min(psk_out.len());
        psk_out[..len].copy_from_slice(&psk[..len]);
        Ok(len)
    });

    builder
        .set_min_proto_version(Some(openssl::ssl::SslVersion::TLS1_2))
        .map_err(|e| IdeviceError::InternalError(e.to_string()))?;
    builder
        .set_max_proto_version(Some(openssl::ssl::SslVersion::TLS1_2))
        .map_err(|e| IdeviceError::InternalError(e.to_string()))?;

    let ssl_connector = builder.build();
    let mut conf = ssl_connector
        .configure()
        .map_err(|e| IdeviceError::InternalError(e.to_string()))?;
    conf.set_verify_hostname(false);
    conf.set_use_server_name_indication(false);
    let ssl = conf
        .into_ssl("localhost")
        .map_err(|e| IdeviceError::InternalError(e.to_string()))?;

    let mut tls_stream = tokio_openssl::SslStream::new(ssl, stream)
        .map_err(|e| IdeviceError::InternalError(e.to_string()))?;

    if let Err(e) = std::pin::Pin::new(&mut tls_stream).connect().await {
        let ssl_errors = openssl::error::ErrorStack::get();
        let msg = format!("TLS-PSK handshake failed: {e} (SSL errors: {ssl_errors:?})");
        tracing::error!("{msg}");
        return Err(IdeviceError::InternalError(msg));
    }

    debug!("TLS-PSK handshake complete");

    CdTunnel::handshake(tls_stream).await
}
