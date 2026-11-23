//! iOS Device SyslogRelay Service Abstraction

use crate::{Idevice, IdeviceError, IdeviceService, obf};

/// Client for interacting with the iOS device SyslogRelay service
#[derive(Debug)]
pub struct SyslogRelayClient {
    /// The underlying device connection with established SyslogRelay service
    pub idevice: Idevice,
}

impl IdeviceService for SyslogRelayClient {
    /// Returns the SyslogRelay service name as registered with lockdownd
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.syslog_relay")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl SyslogRelayClient {
    /// Creates a new SyslogRelay client from an existing device connection
    ///
    /// # Arguments
    /// * `idevice` - Pre-established device connection
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    /// Get the next log from the relay
    ///
    /// # Returns
    /// A string containing the log
    ///
    /// # Errors
    /// UnexpectedResponse if the service sends an EOF
    pub async fn next(&mut self) -> Result<String, IdeviceError> {
        let res = self.idevice.read_until_delim(b"\n\x00").await?;
        match res {
            Some(res) => Ok(String::from_utf8_lossy(&res).to_string()),
            None => Err(IdeviceError::UnexpectedResponse(
                "syslog relay returned EOF".into(),
            )),
        }
    }
}

#[cfg(feature = "rsd")]
impl crate::RsdService for SyslogRelayClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        crate::obf!("com.apple.syslog_relay.shim.remote")
    }
    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = crate::Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}
