#![cfg_attr(docsrs, doc = include_str!("../README.md"))]
#![warn(missing_debug_implementations)]
#![warn(missing_copy_implementations)]
// Jackson Coxson

#[cfg(all(feature = "pair", feature = "rustls"))]
mod ca;
pub mod cursor;
mod obfuscation;
pub mod pairing_file;
pub mod provider;
#[cfg(feature = "remote_pairing")]
pub mod remote_pairing;
#[cfg(feature = "rustls")]
mod sni;
#[cfg(feature = "tunnel_tcp_stack")]
pub mod tcp;
#[cfg(feature = "tss")]
pub mod tss;
#[cfg(feature = "tunneld")]
pub mod tunneld;
#[cfg(feature = "usbmuxd")]
pub mod usbmuxd;
pub mod utils;
#[cfg(feature = "xpc")]
pub mod xpc;

pub mod services;
pub use services::*;
#[cfg(any(feature = "core_device_proxy", feature = "remote_pairing"))]
pub mod tunnel;

#[cfg(feature = "xpc")]
pub use xpc::RemoteXpcClient;

/// Devices whose major iOS version is below this are treated as "legacy":
/// their lockdownd only speaks SSLv3/TLS 1.0 with CBC ciphers, which rustls
/// cannot negotiate. For those, the session handshake goes through OpenSSL
/// (when the `openssl` feature is enabled) with legacy protocols allowed.
pub const LEGACY_TLS_MAX_MAJOR: u8 = 9;

use plist_macro::{plist, pretty_print_dictionary, pretty_print_plist};
use provider::{IdeviceProvider, RsdProvider};
#[cfg(feature = "rustls")]
use rustls::{crypto::CryptoProvider, pki_types::ServerName};
use std::{
    io::{self, BufWriter},
    sync::Arc,
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tracing::{debug, trace};

use crate::services::lockdown::LockdownClient;

/// A trait combining all required characteristics for a device communication socket
///
/// This serves as a convenience trait for any type that can be used as an asynchronous
/// read/write socket for device communication. Combines common async I/O traits with
/// thread safety and debugging requirements.
///
/// Tokio's TcpStream and UnixStream implement this trait.
pub trait ReadWrite: AsyncRead + AsyncWrite + Unpin + Send + Sync + std::fmt::Debug {}

// Blanket implementation for any compatible type
impl<T: AsyncRead + AsyncWrite + Unpin + Send + Sync + std::fmt::Debug> ReadWrite for T {}

/// Interface for services that can be connected to on an iOS device
///
/// Implement this trait to define new services that can be accessed through the
/// device connection protocol.
pub trait IdeviceService: Sized {
    /// Returns the service name as advertised by the device
    fn service_name() -> std::borrow::Cow<'static, str>;

    /// Establishes a connection to this service
    ///
    /// # Arguments
    /// * `provider` - The device provider that can supply connections
    ///
    // From the docs
    // │ │ ├╴  use of `async fn` in public traits is discouraged as auto trait bounds cannot be specified
    // │ │ │    you can suppress this lint if you plan to use the trait only in your own code, or do not care about auto traits like `Send` on the `Future`
    // │ │ │    `#[warn(async_fn_in_trait)]` on by default rustc (async_fn_in_trait) [66, 5]
    #[allow(async_fn_in_trait)]
    async fn connect(provider: &dyn IdeviceProvider) -> Result<Self, IdeviceError> {
        let mut lockdown = LockdownClient::connect(provider).await?;

        #[cfg(feature = "openssl")]
        let legacy = lockdown
            .get_value(Some("ProductVersion"), None)
            .await
            .ok()
            .as_ref()
            .and_then(|x| x.as_string())
            .and_then(|x| x.split(".").next())
            .and_then(|x| x.parse::<u8>().ok())
            .map(|x| x < crate::LEGACY_TLS_MAX_MAJOR)
            .unwrap_or(false);

        #[cfg(not(feature = "openssl"))]
        let legacy = false;

        lockdown
            .start_session(&provider.get_pairing_file().await?)
            .await?;
        // Best-effort fetch UDID for downstream defaults (e.g., MobileBackup2 Target/Source identifiers)
        let udid_value = match lockdown.get_value(Some("UniqueDeviceID"), None).await {
            Ok(v) => v.as_string().map(|s| s.to_string()),
            Err(_) => None,
        };

        let (port, ssl) = lockdown.start_service(Self::service_name()).await?;

        let mut idevice = provider.connect(port).await?;
        if ssl {
            idevice
                .start_session(&provider.get_pairing_file().await?, legacy)
                .await?;
        }

        if let Some(udid) = udid_value {
            idevice.set_udid(udid);
        }

        Self::from_stream(idevice).await
    }

    #[allow(async_fn_in_trait)]
    async fn from_stream(idevice: Idevice) -> Result<Self, IdeviceError>;
}

#[cfg(feature = "rsd")]
pub trait RsdService: Sized {
    fn rsd_service_name() -> std::borrow::Cow<'static, str>;
    fn from_stream(
        stream: Box<dyn ReadWrite>,
    ) -> impl std::future::Future<Output = Result<Self, IdeviceError>> + Send;
    fn connect_rsd(
        provider: &mut impl RsdProvider,
        handshake: &mut rsd::RsdHandshake,
    ) -> impl std::future::Future<Output = Result<Self, IdeviceError>>
    where
        Self: crate::RsdService,
    {
        handshake.connect(provider)
    }
}

/// Type alias for boxed device connection sockets
///
/// Used to enable dynamic dispatch of different connection types while maintaining
/// the required ReadWrite characteristics.
pub type IdeviceSocket = Box<dyn ReadWrite>;

/// Main handle for communicating with an iOS device
///
/// Manages the connection socket and provides methods for common device operations
/// and message exchange.
#[derive(Debug)]
pub struct Idevice {
    /// The underlying connection socket, boxed for dynamic dispatch
    socket: Option<Box<dyn ReadWrite>>,
    /// Unique label identifying this connection
    label: String,
    /// Cached device UDID for convenience in higher-level protocols
    udid: Option<String>,
}

impl Idevice {
    /// Creates a new device connection handle
    ///
    /// # Arguments
    /// * `socket` - The established connection socket
    /// * `label` - Unique identifier for this connection
    pub fn new(socket: Box<dyn ReadWrite>, label: impl Into<String>) -> Self {
        Self {
            socket: Some(socket),
            label: label.into(),
            udid: None,
        }
    }

    pub fn get_socket(self) -> Option<Box<dyn ReadWrite>> {
        self.socket
    }

    /// Sets cached UDID
    pub fn set_udid(&mut self, udid: impl Into<String>) {
        self.udid = Some(udid.into());
    }

    /// Returns cached UDID if available
    pub fn udid(&self) -> Option<&str> {
        self.udid.as_deref()
    }

    /// Queries the device type
    ///
    /// Sends a QueryType request and parses the response
    ///
    /// # Returns
    /// The device type string on success
    ///
    /// # Errors
    /// Returns `IdeviceError` if communication fails or response is invalid
    pub async fn get_type(&mut self) -> Result<String, IdeviceError> {
        let req = plist!({
            "Label": self.label.clone(),
            "Request": "QueryType",
        });
        self.send_plist(req).await?;

        let message: plist::Dictionary = self.read_plist().await?;
        match message.get("Type") {
            Some(m) => Ok(plist::from_value(m)?),
            None => Err(IdeviceError::UnexpectedResponse(
                "missing Type in QueryType response".to_string(),
            )),
        }
    }

    /// Performs RSD (Remote Service Discovery) check-in procedure
    ///
    /// Establishes the basic service connection protocol
    ///
    /// # Errors
    /// Returns `IdeviceError` if the protocol sequence isn't followed correctly
    pub async fn rsd_checkin(&mut self) -> Result<(), IdeviceError> {
        let req = plist!({
            "Label": self.label.clone(),
            "ProtocolVersion": "2",
            "Request": "RSDCheckin",
        });

        self.send_plist(req).await?;
        let res = self.read_plist().await?;
        match res.get("Request").and_then(|x| x.as_string()) {
            Some(r) => {
                if r != "RSDCheckin" {
                    return Err(IdeviceError::UnexpectedResponse(
                        "RSDCheckin request field mismatch".to_string(),
                    ));
                }
            }
            None => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing Request field in RSDCheckin response".to_string(),
                ));
            }
        }

        let res = self.read_plist().await?;
        match res.get("Request").and_then(|x| x.as_string()) {
            Some(r) => {
                if r != "StartService" {
                    return Err(IdeviceError::UnexpectedResponse(
                        "StartService request field mismatch".to_string(),
                    ));
                }
            }
            None => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing Request field in StartService response".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Sends a plist-formatted message to the device
    ///
    /// # Arguments
    /// * `message` - The plist value to send
    ///
    /// # Errors
    /// Returns `IdeviceError` if serialization or transmission fails
    async fn send_plist(&mut self, message: plist::Value) -> Result<(), IdeviceError> {
        if let Some(socket) = &mut self.socket {
            debug!("Sending plist: {}", pretty_print_plist(&message));

            let buf = Vec::new();
            let mut writer = BufWriter::new(buf);
            message.to_writer_xml(&mut writer)?;
            let message = writer.into_inner().unwrap();
            let message = String::from_utf8(message)?;
            let len = message.len() as u32;
            socket.write_all(&len.to_be_bytes()).await?;
            socket.write_all(message.as_bytes()).await?;
            socket.flush().await?;
            Ok(())
        } else {
            Err(IdeviceError::NoEstablishedConnection)
        }
    }

    /// Sends a binary plist-formatted message to the device
    ///
    /// # Arguments
    /// * `message` - The plist value to send
    ///
    /// # Errors
    /// Returns `IdeviceError` if serialization or transmission fails
    async fn send_bplist(&mut self, message: plist::Value) -> Result<(), IdeviceError> {
        if let Some(socket) = &mut self.socket {
            debug!("Sending plist: {}", pretty_print_plist(&message));

            let buf = Vec::new();
            let mut writer = BufWriter::new(buf);
            message.to_writer_binary(&mut writer)?;
            let message = writer.into_inner().unwrap();
            let len = message.len() as u32;
            socket.write_all(&len.to_be_bytes()).await?;
            socket.write_all(&message).await?;
            socket.flush().await?;
            Ok(())
        } else {
            Err(IdeviceError::NoEstablishedConnection)
        }
    }

    /// Sends raw binary data to the device
    ///
    /// # Arguments
    /// * `message` - The bytes to send
    ///
    /// # Errors
    /// Returns `IdeviceError` if transmission fails
    pub async fn send_raw(&mut self, message: &[u8]) -> Result<(), IdeviceError> {
        self.send_raw_with_progress(message, |_| async {}, ()).await
    }

    /// Sends raw binary data via vectored I/O
    ///
    /// # Arguments
    /// * `bufs` - The buffers to send
    ///
    /// # Errors
    /// Returns `IdeviceError` if transmission fails
    pub async fn send_raw_vectored(
        &mut self,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Result<(), IdeviceError> {
        if let Some(socket) = &mut self.socket {
            let mut curr_idx = 0;
            let mut curr_offset = 0;

            while curr_idx < bufs.len() {
                let mut iovec = Vec::new();
                let mut accumulated_len = 0;
                let max_chunk = 1024 * 64;

                // Add partial first slice
                let first_avail = bufs[curr_idx].len() - curr_offset;
                let to_take_first = std::cmp::min(first_avail, max_chunk);
                iovec.push(std::io::IoSlice::new(
                    &bufs[curr_idx][curr_offset..curr_offset + to_take_first],
                ));
                accumulated_len += to_take_first;

                // Add others up to max_chunk
                let mut temp_idx = curr_idx + 1;
                while temp_idx < bufs.len() && accumulated_len < max_chunk {
                    let needed = max_chunk - accumulated_len;
                    let avail = bufs[temp_idx].len();
                    let take = std::cmp::min(avail, needed);
                    iovec.push(std::io::IoSlice::new(&bufs[temp_idx][..take]));
                    accumulated_len += take;
                    temp_idx += 1;
                }

                let n = socket.write_vectored(&iovec).await?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "failed to write whole buffer",
                    )
                    .into());
                }

                // Advance cursor by n
                let mut advanced = n;
                while advanced > 0 && curr_idx < bufs.len() {
                    let available = bufs[curr_idx].len() - curr_offset;
                    if advanced < available {
                        curr_offset += advanced;
                        advanced = 0;
                    } else {
                        advanced -= available;
                        curr_idx += 1;
                        curr_offset = 0;
                    }
                }
            }
            socket.flush().await?;
            Ok(())
        } else {
            Err(IdeviceError::NoEstablishedConnection)
        }
    }

    /// Sends raw binary data with progress callbacks
    ///
    /// # Arguments
    /// * `message` - The bytes to send
    /// * `callback` - Progress callback invoked after each chunk
    /// * `state` - Arbitrary state passed to callback
    ///
    /// # Type Parameters
    /// * `Fut` - Future type returned by callback
    /// * `S` - Type of state passed to callback
    ///
    /// # Errors
    /// Returns `IdeviceError` if transmission fails
    pub async fn send_raw_with_progress<Fut, S>(
        &mut self,
        message: &[u8],
        callback: impl Fn(((usize, usize), S)) -> Fut,
        state: S,
    ) -> Result<(), IdeviceError>
    where
        Fut: std::future::Future<Output = ()>,
        S: Clone,
    {
        if let Some(socket) = &mut self.socket {
            let message_parts = message.chunks(1024 * 64);
            let part_len = message_parts.len() - 1;

            for (i, part) in message_parts.enumerate() {
                trace!("Writing {i}/{part_len}");
                socket.write_all(part).await?;
                callback(((i, part_len), state.clone())).await;
            }
            socket.flush().await?;
            Ok(())
        } else {
            Err(IdeviceError::NoEstablishedConnection)
        }
    }

    /// Reads exactly `len` bytes from the device
    ///
    /// # Arguments
    /// * `len` - Exact number of bytes to read
    ///
    /// # Returns
    /// The received bytes
    ///
    /// # Errors
    /// Returns `IdeviceError` if reading fails or connection is closed prematurely
    pub async fn read_raw(&mut self, len: usize) -> Result<Vec<u8>, IdeviceError> {
        if let Some(socket) = &mut self.socket {
            let mut buf = vec![0; len];
            socket.read_exact(&mut buf).await?;
            Ok(buf)
        } else {
            Err(IdeviceError::NoEstablishedConnection)
        }
    }

    /// Reads up to `max_size` bytes from the device
    ///
    /// # Arguments
    /// * `max_size` - Maximum number of bytes to read
    ///
    /// # Returns
    /// The received bytes (may be shorter than max_size)
    ///
    /// # Errors
    /// Returns `IdeviceError` if reading fails
    pub async fn read_any(&mut self, max_size: u32) -> Result<Vec<u8>, IdeviceError> {
        if let Some(socket) = &mut self.socket {
            let mut buf = vec![0; max_size as usize];
            let len = socket.read(&mut buf).await?;
            Ok(buf[..len].to_vec())
        } else {
            Err(IdeviceError::NoEstablishedConnection)
        }
    }

    /// Reads a plist-formatted message from the device
    ///
    /// # Returns
    /// The parsed plist dictionary
    ///
    /// # Errors
    /// Returns `IdeviceError` if reading, parsing fails, or device reports an error
    async fn read_plist(&mut self) -> Result<plist::Dictionary, IdeviceError> {
        let res = self.read_plist_value().await?;
        let res: plist::Dictionary = plist::from_value(&res)?;
        debug!("Received plist: {}", pretty_print_dictionary(&res));

        if let Some(e) = res.get("Error") {
            let e = match e {
                plist::Value::String(e) => e.to_string(),
                plist::Value::Integer(e) => {
                    if let Some(error_string) = res.get("ErrorString").and_then(|x| x.as_string()) {
                        error_string.to_string()
                    } else {
                        e.to_string()
                    }
                }
                _ => {
                    tracing::error!("Error is not a string or integer from read_plist: {e:?}");
                    return Err(IdeviceError::UnexpectedResponse(
                        "error value is not a string or integer".to_string(),
                    ));
                }
            };
            if let Some(e) = IdeviceError::from_device_error_type(e.as_str(), &res) {
                return Err(e);
            } else {
                let msg =
                    if let Some(desc) = res.get("ErrorDescription").and_then(|x| x.as_string()) {
                        format!("{} ({})", e, desc)
                    } else {
                        e
                    };
                return Err(IdeviceError::UnknownErrorType(msg));
            }
        }
        Ok(res)
    }

    async fn read_plist_value(&mut self) -> Result<plist::Value, IdeviceError> {
        if let Some(socket) = &mut self.socket {
            debug!("Reading response size");
            let mut buf = [0u8; 4];
            socket.read_exact(&mut buf).await?;
            let len = u32::from_be_bytes(buf);
            let mut buf = vec![0; len as usize];
            socket.read_exact(&mut buf).await?;
            let res: plist::Value = plist::from_bytes(&buf)?;
            Ok(res)
        } else {
            Err(IdeviceError::NoEstablishedConnection)
        }
    }

    #[cfg(feature = "syslog_relay")]
    async fn read_until_delim(
        &mut self,
        delimiter: &[u8],
    ) -> Result<Option<bytes::BytesMut>, IdeviceError> {
        if let Some(socket) = &mut self.socket {
            let mut buffer = bytes::BytesMut::with_capacity(1024);
            let mut temp = [0u8; 1024];

            loop {
                let n = socket.read(&mut temp).await?;
                if n == 0 {
                    if buffer.is_empty() {
                        return Ok(None); // EOF and no data
                    } else {
                        return Ok(Some(buffer)); // EOF but return partial data
                    }
                }

                buffer.extend_from_slice(&temp[..n]);

                if let Some(pos) = buffer.windows(delimiter.len()).position(|w| w == delimiter) {
                    let mut line = buffer.split_to(pos + delimiter.len());
                    line.truncate(line.len() - delimiter.len()); // remove delimiter
                    return Ok(Some(line));
                }
            }
        } else {
            Err(IdeviceError::NoEstablishedConnection)
        }
    }

    /// Upgrades the connection to TLS using device pairing credentials
    ///
    /// # Arguments
    /// * `pairing_file` - Contains the device's identity and certificates
    ///
    /// # Errors
    /// Returns `IdeviceError` if TLS handshake fails or credentials are invalid
    pub async fn start_session(
        &mut self,
        pairing_file: &pairing_file::PairingFile,
        legacy: bool,
    ) -> Result<(), IdeviceError> {
        #[cfg(feature = "rustls")]
        {
            #[cfg(not(feature = "openssl"))]
            if legacy {
                tracing::warn!(
                    "Compiled with rustls, but connecting to legacy device! rustls does not support old SSL, this will fail."
                );
            }

            if CryptoProvider::get_default().is_none() {
                // rust-analyzer will choke on this block, don't worry about it
                let crypto_provider: CryptoProvider = {
                    #[cfg(all(feature = "ring", not(feature = "aws-lc")))]
                    {
                        debug!("Using ring crypto backend");
                        rustls::crypto::ring::default_provider()
                    }

                    #[cfg(all(feature = "aws-lc", not(feature = "ring")))]
                    {
                        debug!("Using aws-lc crypto backend");
                        rustls::crypto::aws_lc_rs::default_provider()
                    }

                    #[cfg(not(any(feature = "ring", feature = "aws-lc")))]
                    {
                        compile_error!(
                            "No crypto backend was selected! Specify an idevice feature for a crypto backend"
                        );
                    }

                    #[cfg(all(feature = "ring", feature = "aws-lc"))]
                    {
                        // We can't throw a compile error because it breaks rust-analyzer.
                        // My sanity while debugging the workspace crates are more important.

                        debug!("Using ring crypto backend, because both were passed");
                        tracing::warn!(
                            "Both ring && aws-lc are selected as idevice crypto backends!"
                        );
                        rustls::crypto::ring::default_provider()
                    }
                };

                if let Err(e) = CryptoProvider::install_default(crypto_provider) {
                    // For whatever reason, getting the default provider will return None on iOS at
                    // random. Installing the default provider a second time will return an error, so
                    // we will log it but not propogate it. An issue should be opened with rustls.
                    tracing::error!("Failed to set crypto provider: {e:?}");
                }
            }
            #[cfg(feature = "openssl")]
            if legacy {
                return self.start_legacy_openssl_session(pairing_file).await;
            }

            let config = sni::create_client_config(pairing_file)?;
            let connector = tokio_rustls::TlsConnector::from(Arc::new(config));

            let socket = self.socket.take().unwrap();
            let socket = connector
                .connect(ServerName::try_from("Device").unwrap(), socket)
                .await?;

            self.socket = Some(Box::new(socket));

            Ok(())
        }
        #[cfg(all(feature = "openssl", not(feature = "rustls")))]
        {
            let mut connector =
                openssl::ssl::SslConnector::builder(openssl::ssl::SslMethod::tls())?;
            if legacy {
                connector.set_min_proto_version(Some(openssl::ssl::SslVersion::SSL3))?;
                connector.set_max_proto_version(Some(openssl::ssl::SslVersion::TLS1))?;
                connector.set_cipher_list("ALL:!aNULL:!eNULL:@SECLEVEL=0")?;
                connector.set_options(openssl::ssl::SslOptions::ALLOW_UNSAFE_LEGACY_RENEGOTIATION);
            }

            let mut connector = connector.build().configure()?.into_ssl("ur mom")?;

            connector.set_certificate(&pairing_file.host_certificate)?;
            connector.set_private_key(&pairing_file.host_private_key)?;
            connector.set_verify(openssl::ssl::SslVerifyMode::empty());
            let socket = self.socket.take().unwrap();
            let mut ssl_stream = tokio_openssl::SslStream::new(connector, socket)?;
            std::pin::Pin::new(&mut ssl_stream).connect().await?;
            self.socket = Some(Box::new(ssl_stream));

            Ok(())
        }
    }
}

#[cfg(all(feature = "rustls", feature = "openssl"))]
impl Idevice {
    /// TLS handshake for legacy (pre-iOS 9) devices using OpenSSL, since rustls
    /// only supports TLS 1.2+ with AEAD ciphers. Reuses the rustls-typed pairing file.
    async fn start_legacy_openssl_session(
        &mut self,
        pairing_file: &pairing_file::PairingFile,
    ) -> Result<(), IdeviceError> {
        use openssl::{pkey::PKey, ssl, x509::X509};

        let tls_err = |e: openssl::error::ErrorStack| {
            IdeviceError::LegacyTls(format!("OpenSSL setup failed: {e}"))
        };

        debug!("Using OpenSSL legacy TLS for old device");
        let mut builder = ssl::SslConnector::builder(ssl::SslMethod::tls()).map_err(tls_err)?;
        builder
            .set_min_proto_version(Some(ssl::SslVersion::SSL3))
            .map_err(tls_err)?;
        builder
            .set_max_proto_version(Some(ssl::SslVersion::TLS1))
            .map_err(tls_err)?;
        builder
            .set_cipher_list("ALL:!aNULL:!eNULL:@SECLEVEL=0")
            .map_err(tls_err)?;
        builder.set_options(ssl::SslOptions::ALLOW_UNSAFE_LEGACY_RENEGOTIATION);

        let mut connection = builder
            .build()
            .configure()
            .map_err(tls_err)?
            .into_ssl("Device")
            .map_err(tls_err)?;

        let cert = X509::from_der(pairing_file.host_certificate.as_ref()).map_err(tls_err)?;
        let key = PKey::private_key_from_pem(&pairing_file.host_private_key).map_err(tls_err)?;
        connection.set_certificate(&cert).map_err(tls_err)?;
        connection.set_private_key(&key).map_err(tls_err)?;
        connection.set_verify(ssl::SslVerifyMode::NONE);

        let socket = self
            .socket
            .take()
            .ok_or(IdeviceError::NoEstablishedConnection)?;
        let mut stream = tokio_openssl::SslStream::new(connection, socket).map_err(tls_err)?;
        std::pin::Pin::new(&mut stream)
            .connect()
            .await
            .map_err(|e| IdeviceError::LegacyTls(format!("legacy TLS handshake failed: {e}")))?;
        self.socket = Some(Box::new(stream));
        Ok(())
    }
}

/// Errors specific to the CDTunnel protocol
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum CdTunnelError {
    #[error("CDTunnel packet too short")]
    PacketTooShort,
    #[error("CDTunnel packet invalid magic")]
    InvalidMagic,
    #[error("proclaimed packet size does not match actual size")]
    SizeMismatch,
}

impl CdTunnelError {
    pub fn sub_code(&self) -> i32 {
        match self {
            Self::PacketTooShort => 1,
            Self::InvalidMagic => 2,
            Self::SizeMismatch => 3,
        }
    }
}

/// Errors specific to the heartbeat service
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum HeartbeatError {
    #[error("device went to sleep")]
    SleepyTime,
    #[error("heartbeat timeout")]
    Timeout,
}

impl HeartbeatError {
    pub fn sub_code(&self) -> i32 {
        match self {
            Self::SleepyTime => 1,
            Self::Timeout => 2,
        }
    }
}

/// Comprehensive error type for all device communication failures
#[derive(Error, Debug)]
#[non_exhaustive]
pub enum IdeviceError {
    // 1: I/O and transport errors
    #[error("device socket io failed")]
    Socket(#[from] io::Error),
    #[cfg(feature = "rustls")]
    #[error("PEM parse failed")]
    PemParseFailed(#[from] rustls::pki_types::pem::Error),
    #[cfg(feature = "rustls")]
    #[error("TLS error")]
    Rustls(#[from] rustls::Error),
    #[cfg(all(feature = "openssl", not(feature = "rustls")))]
    #[error("TLS error")]
    Rustls(#[from] openssl::ssl::Error),
    #[cfg(feature = "rustls")]
    #[error("TLS verification build failed")]
    TlsBuilderFailed(#[from] rustls::server::VerifierBuilderError),
    #[cfg(all(feature = "rustls", feature = "openssl"))]
    #[error("legacy TLS error: {0}")]
    LegacyTls(String),
    #[cfg(all(feature = "openssl", not(feature = "rustls")))]
    #[error("TLS verification build failed")]
    TlsBuilderFailed(#[from] openssl::error::ErrorStack),

    // 2: Data format errors
    #[error("io on plist")]
    Plist(#[from] plist::Error),
    #[error("can't convert bytes to utf8")]
    Utf8(#[from] std::string::FromUtf8Error),
    #[error("failed to parse bytes as valid utf8")]
    Utf8Error,
    #[cfg(feature = "core_device_proxy")]
    #[error("JSON serialization failed")]
    Json(#[from] serde_json::Error),
    #[error("cannot parse string as IpAddr")]
    AddrParseError(#[from] std::net::AddrParseError),
    #[error("not enough bytes, expected {1}, got {0}")]
    NotEnoughBytes(usize, usize),
    #[error("integer overflow")]
    IntegerOverflow,
    #[cfg(any(feature = "tss", feature = "tunneld"))]
    #[error("http reqwest error")]
    Reqwest(#[from] reqwest::Error),

    // 3: Protocol/device response errors
    #[error("unexpected response from device: {0}")]
    UnexpectedResponse(String),
    #[error("this request was prohibited")]
    GetProhibited,
    #[error("unknown error `{0}` returned from device")]
    UnknownErrorType(String),
    #[error("internal error")]
    InternalError(String),

    // 4: Connection/session state errors
    #[error("no SSL session is active")]
    SessionInactive,
    #[error("device does not have pairing file")]
    InvalidHostID,
    #[error("no established connection")]
    NoEstablishedConnection,

    // 5: Device state errors
    #[error("not found")]
    NotFound,
    #[error("service not found")]
    ServiceNotFound,
    #[error("device not found")]
    DeviceNotFound,
    #[error("device locked")]
    DeviceLocked,
    #[error("Developer mode is not enabled")]
    DeveloperModeNotEnabled,
    #[error("unsupported watch key")]
    UnsupportedWatchKey,
    #[error("malformed command")]
    MalformedCommand,
    #[error("canceled by user")]
    CanceledByUser,
    #[error("bad build manifest")]
    BadBuildManifest,
    #[error("image not mounted")]
    ImageNotMounted,

    // 6: Pairing errors (lockdown)
    #[cfg(feature = "pair")]
    #[error("pairing trust dialog pending")]
    PairingDialogResponsePending,
    #[cfg(feature = "pair")]
    #[error("user denied pairing trust")]
    UserDeniedPairing,
    #[cfg(feature = "pair")]
    #[error("device is locked")]
    PasswordProtected,

    // 7: FFI-specific errors
    #[error("invalid arguments were passed")]
    FfiInvalidArg,
    #[error("invalid string was passed")]
    FfiInvalidString,
    #[error("buffer passed is too small - needs {0}, got {1}")]
    FfiBufferTooSmall(usize, usize),

    #[cfg(any(
        feature = "debug_proxy",
        all(feature = "afc", feature = "installation_proxy")
    ))]
    #[error("invalid argument passed")]
    InvalidArgument,

    // Service-specific sub-error enums
    #[error(transparent)]
    Heartbeat(#[from] HeartbeatError),
    #[error(transparent)]
    CdTunnel(#[from] CdTunnelError),
    #[cfg(feature = "usbmuxd")]
    #[error(transparent)]
    Usbmuxd(#[from] usbmuxd::errors::UsbmuxdError),
    #[cfg(feature = "remote_pairing")]
    #[error(transparent)]
    RemotePairing(#[from] remote_pairing::errors::RemotePairingError),
    #[cfg(feature = "xpc")]
    #[error(transparent)]
    Xpc(#[from] xpc::errors::XpcError),
    #[cfg(feature = "dvt")]
    #[error(transparent)]
    Dvt(#[from] services::dvt::errors::DvtError),
    #[cfg(feature = "afc")]
    #[error("afc error: {0}")]
    Afc(#[from] afc::errors::AfcError),
    #[cfg(feature = "installation_proxy")]
    #[error(transparent)]
    InstallationProxy(#[from] services::installation_proxy::InstallationProxyError),

    // Feature-gated service errors (single-variant, not worth a sub-enum)
    #[cfg(feature = "misagent")]
    #[error("misagent operation failed")]
    MisagentFailure,
    #[cfg(feature = "crashreportcopymobile")]
    #[error("crash report mover sent the wrong response")]
    CrashReportMoverBadResponse(Vec<u8>),
    #[cfg(feature = "notification_proxy")]
    #[error("notification proxy died")]
    NotificationProxyDeath,
    #[cfg(feature = "installation_proxy")]
    #[error("Application verification failed: {0}")]
    ApplicationVerificationFailed(String),

    #[cfg(feature = "xctest")]
    #[error("application is not installed on the device")]
    AppNotInstalled,

    #[cfg(feature = "xctest")]
    #[error("test runner did not connect within the timeout")]
    TestRunnerTimeout,

    #[cfg(feature = "xctest")]
    #[error("test runner disconnected before the test plan completed")]
    TestRunnerDisconnected,

    #[cfg(feature = "xctest")]
    #[error("xctest session timed out after {0:.1}s")]
    XcTestTimeout(f64),
}

impl IdeviceError {
    /// Converts a device-reported error string to a typed error
    ///
    /// # Arguments
    /// * `e` - The error string from device
    /// * `context` - Full plist context containing additional error details
    ///
    /// # Returns
    /// Some(IdeviceError) if the string maps to a known error type, None otherwise
    fn from_device_error_type(e: &str, context: &plist::Dictionary) -> Option<Self> {
        if e.contains("NSDebugDescription=Canceled by user.") {
            return Some(Self::CanceledByUser);
        } else if e.contains("Developer mode is not enabled.") {
            return Some(Self::DeveloperModeNotEnabled);
        }
        match e {
            "GetProhibited" => Some(Self::GetProhibited),
            "InvalidHostID" => Some(Self::InvalidHostID),
            "SessionInactive" => Some(Self::SessionInactive),
            "DeviceLocked" => Some(Self::DeviceLocked),
            #[cfg(feature = "pair")]
            "PairingDialogResponsePending" => Some(Self::PairingDialogResponsePending),
            #[cfg(feature = "pair")]
            "UserDeniedPairing" => Some(Self::UserDeniedPairing),
            #[cfg(feature = "pair")]
            "PasswordProtected" => Some(Self::PasswordProtected),
            "UnsupportedWatchKey" => Some(Self::UnsupportedWatchKey),
            "MalformedCommand" => Some(Self::MalformedCommand),
            "InternalError" => {
                let detailed_error = context
                    .get("DetailedError")
                    .and_then(|d| d.as_string())
                    .unwrap_or("No context")
                    .to_string();

                if detailed_error.contains("There is no matching entry in the device map for") {
                    Some(Self::ImageNotMounted)
                } else {
                    Some(Self::InternalError(detailed_error))
                }
            }
            #[cfg(feature = "installation_proxy")]
            "ApplicationVerificationFailed" => {
                let msg = context
                    .get("ErrorDescription")
                    .and_then(|x| x.as_string())
                    .unwrap_or("No context")
                    .to_string();
                Some(Self::ApplicationVerificationFailed(msg))
            }
            _ => None,
        }
    }

    /// Returns the top-level error category code for FFI consumers.
    pub fn code(&self) -> i32 {
        match self {
            // 1: I/O and transport
            IdeviceError::Socket(_) => 1,
            #[cfg(feature = "rustls")]
            IdeviceError::PemParseFailed(_) => 2,
            IdeviceError::Rustls(_) => 3,
            IdeviceError::TlsBuilderFailed(_) => 4,
            #[cfg(all(feature = "rustls", feature = "openssl"))]
            IdeviceError::LegacyTls(_) => 3,

            // 5: Data format
            IdeviceError::Plist(_) => 5,
            IdeviceError::Utf8(_) => 6,
            IdeviceError::Utf8Error => 7,
            #[cfg(feature = "core_device_proxy")]
            IdeviceError::Json(_) => 8,
            IdeviceError::AddrParseError(_) => 9,
            IdeviceError::NotEnoughBytes(_, _) => 10,
            IdeviceError::IntegerOverflow => 11,
            #[cfg(any(feature = "tss", feature = "tunneld"))]
            IdeviceError::Reqwest(_) => 12,

            // 13: Protocol/device response
            IdeviceError::UnexpectedResponse(_) => 13,
            IdeviceError::GetProhibited => 14,
            IdeviceError::UnknownErrorType(_) => 15,
            IdeviceError::InternalError(_) => 16,

            // 17: Connection/session state
            IdeviceError::SessionInactive => 17,
            IdeviceError::InvalidHostID => 18,
            IdeviceError::NoEstablishedConnection => 19,

            // 20: Device state
            IdeviceError::NotFound => 20,
            IdeviceError::ServiceNotFound => 21,
            IdeviceError::DeviceNotFound => 22,
            IdeviceError::DeviceLocked => 23,
            IdeviceError::DeveloperModeNotEnabled => 24,
            IdeviceError::UnsupportedWatchKey => 25,
            IdeviceError::MalformedCommand => 26,
            IdeviceError::CanceledByUser => 27,
            IdeviceError::BadBuildManifest => 28,
            IdeviceError::ImageNotMounted => 29,

            // 30: Pairing (lockdown)
            #[cfg(feature = "pair")]
            IdeviceError::PairingDialogResponsePending => 30,
            #[cfg(feature = "pair")]
            IdeviceError::UserDeniedPairing => 31,
            #[cfg(feature = "pair")]
            IdeviceError::PasswordProtected => 32,

            // 33: FFI
            IdeviceError::FfiInvalidArg => 33,
            IdeviceError::FfiInvalidString => 34,
            IdeviceError::FfiBufferTooSmall(_, _) => 35,
            #[cfg(any(
                feature = "debug_proxy",
                all(feature = "afc", feature = "installation_proxy")
            ))]
            IdeviceError::InvalidArgument => 36,

            // 100+: Service sub-error enums
            IdeviceError::Heartbeat(_) => 100,
            IdeviceError::CdTunnel(_) => 101,
            #[cfg(feature = "usbmuxd")]
            IdeviceError::Usbmuxd(_) => 102,
            #[cfg(feature = "remote_pairing")]
            IdeviceError::RemotePairing(_) => 103,
            #[cfg(feature = "xpc")]
            IdeviceError::Xpc(_) => 104,
            #[cfg(feature = "dvt")]
            IdeviceError::Dvt(_) => 105,
            #[cfg(feature = "afc")]
            IdeviceError::Afc(_) => 106,
            #[cfg(feature = "installation_proxy")]
            IdeviceError::InstallationProxy(_) => 107,

            // 200+: Feature-gated single-variant service errors
            #[cfg(feature = "misagent")]
            IdeviceError::MisagentFailure => 200,
            #[cfg(feature = "crashreportcopymobile")]
            IdeviceError::CrashReportMoverBadResponse(_) => 201,
            #[cfg(feature = "notification_proxy")]
            IdeviceError::NotificationProxyDeath => 202,
            #[cfg(feature = "installation_proxy")]
            IdeviceError::ApplicationVerificationFailed(_) => 203,
            #[cfg(feature = "xctest")]
            IdeviceError::AppNotInstalled => 204,
            #[cfg(feature = "xctest")]
            IdeviceError::TestRunnerTimeout => 205,
            #[cfg(feature = "xctest")]
            IdeviceError::TestRunnerDisconnected => 206,
            #[cfg(feature = "xctest")]
            IdeviceError::XcTestTimeout(_) => 207,
        }
    }

    /// Returns the sub-error code within a category, or 0 if there is no sub-error.
    pub fn sub_code(&self) -> i32 {
        match self {
            IdeviceError::Heartbeat(e) => e.sub_code(),
            IdeviceError::CdTunnel(e) => e.sub_code(),
            #[cfg(feature = "usbmuxd")]
            IdeviceError::Usbmuxd(e) => e.sub_code(),
            #[cfg(feature = "remote_pairing")]
            IdeviceError::RemotePairing(e) => e.sub_code(),
            #[cfg(feature = "xpc")]
            IdeviceError::Xpc(e) => e.sub_code(),
            #[cfg(feature = "dvt")]
            IdeviceError::Dvt(e) => e.sub_code(),
            #[cfg(feature = "afc")]
            IdeviceError::Afc(e) => e.sub_code(),
            #[cfg(feature = "installation_proxy")]
            IdeviceError::InstallationProxy(e) => e.sub_code(),
            _ => 0,
        }
    }
}
