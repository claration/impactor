//! iOS Device Heartbeat Service Abstraction
//!
//! iOS automatically closes service connections if there is no heartbeat client connected and
//! responding.

use crate::{HeartbeatError, Idevice, IdeviceError, IdeviceService, obf};

/// Client for interacting with the iOS device heartbeat service
///
/// The heartbeat service provides a keep-alive mechanism and can notify when
/// the device enters sleep mode or disconnects.
/// Note that a running heartbeat client is required to access other services on the device.
/// Implements the standard "Marco-Polo" protocol
/// where the host sends "Polo" in response to the device's "Marco".
#[derive(Debug)]
pub struct HeartbeatClient {
    /// The underlying device connection with established heartbeat service
    pub idevice: Idevice,
}

impl IdeviceService for HeartbeatClient {
    /// Returns the heartbeat service name as registered with lockdownd
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.mobile.heartbeat")
    }
    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl HeartbeatClient {
    /// Creates a new heartbeat client from an existing device connection
    ///
    /// # Arguments
    /// * `idevice` - Pre-established device connection
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    /// Waits for and processes a "Marco" message from the device
    ///
    /// This will either:
    /// - Return the heartbeat interval if received
    /// - Return a timeout error if no message received in time
    /// - Return a sleep notification if device is going to sleep
    ///
    /// # Arguments
    /// * `interval` - Timeout in seconds to wait for message
    ///
    /// # Returns
    /// The heartbeat interval in seconds if successful
    ///
    /// # Errors
    /// - `HeartbeatTimeout` if no message received before interval
    /// - `HeartbeatSleepyTime` if device is going to sleep
    /// - `UnexpectedResponse` for malformed messages
    pub async fn get_marco(&mut self, interval: u64) -> Result<u64, IdeviceError> {
        // Get a plist or wait for the interval
        let rec = tokio::select! {
            rec = self.idevice.read_plist() => rec?,
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(interval)) => {
                return Err(HeartbeatError::Timeout.into())
            }
        };
        match rec.get("Interval") {
            Some(plist::Value::Integer(interval)) => {
                if let Some(interval) = interval.as_unsigned() {
                    Ok(interval)
                } else {
                    Err(IdeviceError::UnexpectedResponse(
                        "heartbeat Interval is not a valid unsigned integer".into(),
                    ))
                }
            }
            _ => match rec.get("Command") {
                Some(plist::Value::String(command)) => {
                    if command.as_str() == "SleepyTime" {
                        Err(HeartbeatError::SleepyTime.into())
                    } else {
                        Err(IdeviceError::UnexpectedResponse(format!(
                            "unexpected heartbeat Command: {command}"
                        )))
                    }
                }
                _ => Err(IdeviceError::UnexpectedResponse(
                    "heartbeat response missing both Interval and Command".into(),
                )),
            },
        }
    }

    /// Sends a "Polo" response to the device
    ///
    /// This acknowledges receipt of a "Marco" message and maintains
    /// the connection keep-alive.
    ///
    /// # Errors
    /// Returns `IdeviceError` if the message fails to send
    pub async fn send_polo(&mut self) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "Command": "Polo"
        });
        self.idevice.send_plist(req).await?;
        Ok(())
    }
}

#[cfg(feature = "rsd")]
impl crate::RsdService for HeartbeatClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        crate::obf!("com.apple.mobile.heartbeat.shim.remote")
    }
    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = crate::Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}
