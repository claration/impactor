//! iOS Mobile Installation Agent (misagent) Client
//!
//! Provides functionality for interacting with the misagent service on iOS devices,
//! which manages provisioning profiles and certificates.

use tracing::warn;

use crate::{Idevice, IdeviceError, IdeviceService, RsdService, obf};

/// Client for interacting with the iOS misagent service
///
/// The misagent service handles:
/// - Installation of provisioning profiles
/// - Removal of provisioning profiles
/// - Querying installed profiles
#[derive(Debug)]
pub struct MisagentClient {
    /// The underlying device connection with established misagent service
    pub idevice: Idevice,
}

impl RsdService for MisagentClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.misagent.shim.remote")
    }

    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, IdeviceError> {
        let mut stream = Idevice::new(stream, "");
        stream.rsd_checkin().await?;
        Ok(Self::new(stream))
    }
}

impl IdeviceService for MisagentClient {
    /// Returns the misagent service name as registered with lockdownd
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.misagent")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl MisagentClient {
    /// Creates a new misagent client from an existing device connection
    ///
    /// # Arguments
    /// * `idevice` - Pre-established device connection
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    /// Installs a provisioning profile on the device
    ///
    /// # Arguments
    /// * `profile` - The provisioning profile data to install
    ///
    /// # Returns
    /// `Ok(())` on successful installation
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The profile is invalid
    /// - Installation is not permitted
    ///
    /// # Example
    /// ```rust
    /// let profile_data = std::fs::read("profile.mobileprovision")?;
    /// client.install(profile_data).await?;
    /// ```
    pub async fn install(&mut self, profile: Vec<u8>) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "MessageType": "Install",
            "Profile": profile,
            "ProfileType": "Provisioning"
        });

        self.idevice.send_plist(req).await?;

        let mut res = self.idevice.read_plist().await?;

        match res.remove("Status") {
            Some(plist::Value::Integer(status)) => {
                if let Some(status) = status.as_unsigned() {
                    if status == 0 {
                        Ok(())
                    } else {
                        Err(IdeviceError::MisagentFailure)
                    }
                } else {
                    warn!("Misagent return status wasn't unsigned");
                    Err(IdeviceError::UnexpectedResponse(
                        "install Status is not an unsigned integer".into(),
                    ))
                }
            }
            _ => {
                warn!("Did not get integer status response");
                Err(IdeviceError::UnexpectedResponse(
                    "missing integer Status in install response".into(),
                ))
            }
        }
    }

    /// Removes a provisioning profile from the device
    ///
    /// # Arguments
    /// * `id` - The UUID of the profile to remove
    ///
    /// # Returns
    /// `Ok(())` on successful removal
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The profile doesn't exist
    /// - Removal is not permitted
    ///
    /// # Example
    /// ```rust
    /// client.remove("asdf").await?;
    /// ```
    pub async fn remove(&mut self, id: &str) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "MessageType": "Remove",
            "ProfileID": id,
            "ProfileType": "Provisioning"
        });

        self.idevice.send_plist(req).await?;

        let mut res = self.idevice.read_plist().await?;

        match res.remove("Status") {
            Some(plist::Value::Integer(status)) => {
                if let Some(status) = status.as_unsigned() {
                    if status == 0 {
                        Ok(())
                    } else {
                        Err(IdeviceError::MisagentFailure)
                    }
                } else {
                    warn!("Misagent return status wasn't unsigned");
                    Err(IdeviceError::UnexpectedResponse(
                        "remove Status is not an unsigned integer".into(),
                    ))
                }
            }
            _ => {
                warn!("Did not get integer status response");
                Err(IdeviceError::UnexpectedResponse(
                    "missing integer Status in remove response".into(),
                ))
            }
        }
    }

    /// Retrieves all provisioning profiles from the device
    ///
    /// # Returns
    /// A vector containing raw profile data for each installed profile
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The response is malformed
    ///
    /// # Example
    /// ```rust
    /// let profiles = client.copy_all().await?;
    /// for profile in profiles {
    ///     println!("Profile size: {} bytes", profile.len());
    /// }
    /// ```
    pub async fn copy_all(&mut self) -> Result<Vec<Vec<u8>>, IdeviceError> {
        let req = crate::plist!({
            "MessageType": "CopyAll",
            "ProfileType": "Provisioning"
        });

        self.idevice.send_plist(req).await?;

        let mut res = self.idevice.read_plist().await?;
        match res.remove("Payload") {
            Some(plist::Value::Array(a)) => {
                let mut res = Vec::new();
                for profile in a {
                    if let Some(profile) = profile.as_data() {
                        res.push(profile.to_vec());
                    } else {
                        warn!("Misagent CopyAll did not return data plists");
                        return Err(IdeviceError::UnexpectedResponse(
                            "CopyAll Payload array contains non-data entry".into(),
                        ));
                    }
                }
                Ok(res)
            }
            _ => {
                warn!("Did not get a payload of provisioning profiles as an array");
                Err(IdeviceError::UnexpectedResponse(
                    "missing Payload array in CopyAll response".into(),
                ))
            }
        }
    }
}
