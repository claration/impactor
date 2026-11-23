//! iOS Device HouseArrest Service Abstraction
//!
//! The HouseArrest service allows access to the container and Documents directory of apps
//! installed on an iOS device. This is typically used for file transfer and inspection of
//! app-specific data during development or diagnostics.

use crate::{Idevice, IdeviceError, IdeviceService, obf};

use super::afc::AfcClient;

/// Client for interacting with the iOS HouseArrest service
///
/// HouseArrest is used to expose the container or Documents directory of an app to a host machine
/// over AFC (Apple File Conduit).
#[derive(Debug)]
pub struct HouseArrestClient {
    /// The underlying device connection with the HouseArrest service
    pub idevice: Idevice,
}

impl IdeviceService for HouseArrestClient {
    /// Returns the name of the HouseArrest service as registered with lockdownd
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.mobile.house_arrest")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl HouseArrestClient {
    /// Creates a new HouseArrest client from an existing device connection
    ///
    /// # Arguments
    /// * `idevice` - A pre-established device connection with the HouseArrest service
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    /// Requests access to the app's full container (Documents, Library, etc.) over AFC
    ///
    /// # Arguments
    /// * `bundle_id` - The bundle identifier of the target app (e.g., "com.example.MyApp")
    ///
    /// # Returns
    /// An `AfcClient` for accessing the container of the specified app
    ///
    /// # Errors
    /// Returns `IdeviceError` if the request or AFC setup fails
    pub async fn vend_container(
        self,
        bundle_id: impl Into<String>,
    ) -> Result<AfcClient, IdeviceError> {
        let bundle_id = bundle_id.into();
        self.vend(bundle_id, "VendContainer".into()).await
    }

    /// Requests access to the app's Documents directory over AFC.
    /// Note that you can only access the /Documents directory. Permission will be denied
    /// otherwise.
    ///
    /// # Arguments
    /// * `bundle_id` - The bundle identifier of the target app (e.g., "com.example.MyApp")
    ///
    /// # Returns
    /// An `AfcClient` for accessing the Documents directory of the specified app
    ///
    /// # Errors
    /// Returns `IdeviceError` if the request or AFC setup fails
    pub async fn vend_documents(
        self,
        bundle_id: impl Into<String>,
    ) -> Result<AfcClient, IdeviceError> {
        let bundle_id = bundle_id.into();
        self.vend(bundle_id, "VendDocuments".into()).await
    }

    /// Sends a HouseArrest command to expose a specific directory over AFC
    ///
    /// This is an internal method used by `vend_container` and `vend_documents`.
    ///
    /// # Arguments
    /// * `bundle_id` - App bundle identifier
    /// * `cmd` - Command to send ("VendContainer" or "VendDocuments")
    ///
    /// # Returns
    /// A connected `AfcClient` instance
    ///
    /// # Errors
    /// Returns `IdeviceError` if the request or AFC setup fails
    async fn vend(mut self, bundle_id: String, cmd: String) -> Result<AfcClient, IdeviceError> {
        let req = crate::plist!({
            "Command": cmd,
            "Identifier": bundle_id
        });

        self.idevice.send_plist(req).await?;
        self.idevice.read_plist().await?;

        Ok(AfcClient::new(self.idevice))
    }
}

#[cfg(feature = "rsd")]
impl crate::RsdService for HouseArrestClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        crate::obf!("com.apple.mobile.house_arrest.shim.remote")
    }
    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = crate::Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}
