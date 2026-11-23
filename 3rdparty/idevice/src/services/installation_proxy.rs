//! iOS Installation Proxy Service Client
//!
//! Provides functionality for interacting with the installation_proxy service on iOS devices,
//! which allows querying and managing installed applications.

use std::collections::HashMap;

use plist::Dictionary;
use tracing::warn;

use crate::{Idevice, IdeviceError, IdeviceService, obf};

/// Errors specific to installation proxy operations
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum InstallationProxyError {
    #[error("installation proxy operation failed: {0}")]
    OperationFailed(String),
    #[error("malformed package archive: {0}")]
    MalformedPackageArchive(#[from] async_zip::error::ZipError),
}

impl InstallationProxyError {
    pub fn sub_code(&self) -> i32 {
        match self {
            Self::OperationFailed(_) => 1,
            Self::MalformedPackageArchive(_) => 2,
        }
    }
}

/// Client for interacting with the iOS installation proxy service
///
/// This service provides access to information about installed applications
/// and can perform application management operations.
#[derive(Debug)]
pub struct InstallationProxyClient {
    /// The underlying device connection with established installation_proxy service
    pub idevice: Idevice,
}

impl IdeviceService for InstallationProxyClient {
    /// Returns the installation proxy service name as registered with lockdownd
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.mobile.installation_proxy")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl InstallationProxyClient {
    /// Creates a new installation proxy client from an existing device connection
    ///
    /// # Arguments
    /// * `idevice` - Pre-established device connection
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    /// Retrieves information about installed applications
    ///
    /// # Arguments
    /// * `application_type` - Optional filter for application type:
    ///   - "System" for system applications
    ///   - "User" for user-installed applications
    ///   - "Any" for all applications (default)
    /// * `bundle_identifiers` - Optional list of specific bundle IDs to query
    ///
    /// # Returns
    /// A HashMap mapping bundle identifiers to application information plist values
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The response is malformed
    /// - The service returns an error
    ///
    /// # Example
    /// ```rust
    /// let apps = client.get_apps(Some("User".to_string()), None).await?;
    /// for (bundle_id, info) in apps {
    ///     println!("{}: {:?}", bundle_id, info);
    /// }
    /// ```
    pub async fn get_apps(
        &mut self,
        application_type: Option<&str>,
        bundle_identifiers: Option<Vec<String>>,
    ) -> Result<HashMap<String, plist::Value>, IdeviceError> {
        let application_type = application_type.unwrap_or("Any");

        let req = crate::plist!({
            "Command": "Lookup",
            "ClientOptions": {
                "ApplicationType": application_type,
                "BundleIDs":? bundle_identifiers,
            }
        });
        self.idevice.send_plist(req).await?;

        let mut res = self.idevice.read_plist().await?;
        match res.remove("LookupResult") {
            Some(plist::Value::Dictionary(res)) => {
                Ok(res.into_iter().collect::<HashMap<String, plist::Value>>())
            }
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing LookupResult dictionary in response".into(),
            )),
        }
    }

    /// Installs an application package on the device
    ///
    /// # Arguments
    /// * `package_path` - Path to the .ipa package in the AFC jail (device's installation directory)
    /// * `options` - Optional installation options as a plist dictionary
    ///
    /// # Returns
    /// `Ok(())` on successful installation
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The installation fails
    /// - The service returns an error
    ///
    /// # Note
    /// The package_path should be relative to the AFC jail root
    pub async fn install(
        &mut self,
        package_path: impl Into<String>,
        options: Option<plist::Value>,
    ) -> Result<(), IdeviceError> {
        self.install_with_callback(package_path, options, |_| async {}, ())
            .await
    }

    /// Installs an application package on the device
    ///
    /// # Arguments
    /// * `package_path` - Path to the .ipa package in the AFC jail (device's installation directory)
    /// * `options` - Optional installation options as a plist dictionary
    /// * `callback` - Progress callback that receives (percent_complete, state)
    /// * `state` - State to pass to the callback
    ///
    /// # Returns
    /// `Ok(())` on successful installation
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The installation fails
    /// - The service returns an error
    ///
    /// # Note
    /// The package_path should be relative to the AFC jail root
    pub async fn install_with_callback<Fut, S>(
        &mut self,
        package_path: impl Into<String>,
        options: Option<plist::Value>,
        callback: impl Fn((u64, S)) -> Fut,
        state: S,
    ) -> Result<(), IdeviceError>
    where
        Fut: std::future::Future<Output = ()>,
        S: Clone,
    {
        let package_path = package_path.into();
        let options = options.unwrap_or(plist::Value::Dictionary(Dictionary::new()));

        let command = crate::plist!({
            "Command": "Install",
            "ClientOptions": options,
            "PackagePath": package_path,
        });

        self.idevice.send_plist(command).await?;

        self.watch_completion(callback, state).await
    }

    /// Upgrades an existing application on the device
    ///
    /// # Arguments
    /// * `package_path` - Path to the .ipa package in the AFC jail (device's installation directory)
    /// * `options` - Optional upgrade options as a plist dictionary
    ///
    /// # Returns
    /// `Ok(())` on successful upgrade
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The upgrade fails
    /// - The service returns an error
    pub async fn upgrade(
        &mut self,
        package_path: impl Into<String>,
        options: Option<plist::Value>,
    ) -> Result<(), IdeviceError> {
        self.upgrade_with_callback(package_path, options, |_| async {}, ())
            .await
    }

    /// Upgrades an existing application on the device
    ///
    /// # Arguments
    /// * `package_path` - Path to the .ipa package in the AFC jail (device's installation directory)
    /// * `options` - Optional upgrade options as a plist dictionary
    /// * `callback` - Progress callback that receives (percent_complete, state)
    /// * `state` - State to pass to the callback
    ///
    /// # Returns
    /// `Ok(())` on successful upgrade
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The upgrade fails
    /// - The service returns an error
    pub async fn upgrade_with_callback<Fut, S>(
        &mut self,
        package_path: impl Into<String>,
        options: Option<plist::Value>,
        callback: impl Fn((u64, S)) -> Fut,
        state: S,
    ) -> Result<(), IdeviceError>
    where
        Fut: std::future::Future<Output = ()>,
        S: Clone,
    {
        let package_path = package_path.into();
        let options = options.unwrap_or(plist::Value::Dictionary(Dictionary::new()));

        let command = crate::plist!({
            "Command": "Upgrade",
            "ClientOptions": options,
            "PackagePath": package_path,
        });

        self.idevice.send_plist(command).await?;

        self.watch_completion(callback, state).await
    }

    /// Uninstalls an application from the device
    ///
    /// # Arguments
    /// * `bundle_id` - Bundle identifier of the application to uninstall
    /// * `options` - Optional uninstall options as a plist dictionary
    ///
    /// # Returns
    /// `Ok(())` on successful uninstallation
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The uninstallation fails
    /// - The service returns an error
    pub async fn uninstall(
        &mut self,
        bundle_id: impl Into<String>,
        options: Option<plist::Value>,
    ) -> Result<(), IdeviceError> {
        self.uninstall_with_callback(bundle_id, options, |_| async {}, ())
            .await
    }

    /// Uninstalls an application from the device
    ///
    /// # Arguments
    /// * `bundle_id` - Bundle identifier of the application to uninstall
    /// * `options` - Optional uninstall options as a plist dictionary
    /// * `callback` - Progress callback that receives (percent_complete, state)
    /// * `state` - State to pass to the callback
    ///
    /// # Returns
    /// `Ok(())` on successful uninstallation
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The uninstallation fails
    /// - The service returns an error
    pub async fn uninstall_with_callback<Fut, S>(
        &mut self,
        bundle_id: impl Into<String>,
        options: Option<plist::Value>,
        callback: impl Fn((u64, S)) -> Fut,
        state: S,
    ) -> Result<(), IdeviceError>
    where
        Fut: std::future::Future<Output = ()>,
        S: Clone,
    {
        let bundle_id = bundle_id.into();
        let options = options.unwrap_or(plist::Value::Dictionary(Dictionary::new()));

        let command = crate::plist!({
            "Command": "Uninstall",
            "ApplicationIdentifier": bundle_id,
            "ClientOptions": options,
        });

        self.idevice.send_plist(command).await?;

        self.watch_completion(callback, state).await
    }

    /// Checks if the device capabilities match the required capabilities
    ///
    /// # Arguments
    /// * `capabilities` - List of required capabilities as plist values
    /// * `options` - Optional check options as a plist dictionary
    ///
    /// # Returns
    /// `true` if all capabilities are supported, `false` otherwise
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The service returns an error
    pub async fn check_capabilities_match(
        &mut self,
        capabilities: Vec<plist::Value>,
        options: Option<plist::Value>,
    ) -> Result<bool, IdeviceError> {
        let options = options.unwrap_or(plist::Value::Dictionary(Dictionary::new()));

        let command = crate::plist!({
            "Command": "CheckCapabilitiesMatch",
            "ClientOptions": options,
            "Capabilities": capabilities
        });

        self.idevice.send_plist(command).await?;
        let mut res = self.idevice.read_plist().await?;

        if let Some(caps) = res.remove("LookupResult").and_then(|x| x.as_boolean()) {
            Ok(caps)
        } else {
            Err(IdeviceError::UnexpectedResponse(
                "missing LookupResult boolean in CheckCapabilitiesMatch response".into(),
            ))
        }
    }

    /// Browses installed applications on the device
    ///
    /// # Arguments
    /// * `options` - Optional browse options as a plist dictionary
    ///
    /// # Returns
    /// A vector of plist values representing application information
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The service returns an error
    ///
    /// # Note
    /// This method streams application information in chunks and collects them into a single vector
    pub async fn browse(
        &mut self,
        options: Option<plist::Value>,
    ) -> Result<Vec<plist::Value>, IdeviceError> {
        let options = options.unwrap_or(plist::Value::Dictionary(Dictionary::new()));

        let command = crate::plist!({
            "Command": "Browse",
            "ClientOptions": options,
        });

        self.idevice.send_plist(command).await?;

        let mut values = Vec::new();
        loop {
            let mut res = self.idevice.read_plist().await?;

            if let Some(list) = res.remove("CurrentList").and_then(|x| x.into_array()) {
                for v in list.into_iter() {
                    values.push(v);
                }
            } else {
                warn!("browse didn't contain current list");
                break;
            }

            if let Some(status) = res.get("Status").and_then(|x| x.as_string())
                && status == "Complete"
            {
                break;
            }
        }
        Ok(values)
    }

    /// Watches for operation completion and handles progress callbacks
    ///
    /// # Arguments
    /// * `callback` - Optional progress callback that receives (percent_complete, state)
    /// * `state` - Optional state to pass to the callback
    ///
    /// # Returns
    /// `Ok(())` when the operation completes successfully
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The operation fails
    /// - The service returns an error
    async fn watch_completion<Fut, S>(
        &mut self,
        callback: impl Fn((u64, S)) -> Fut,
        state: S,
    ) -> Result<(), IdeviceError>
    where
        Fut: std::future::Future<Output = ()>,
        S: Clone,
    {
        loop {
            let mut res = self.idevice.read_plist().await?;

            if let Some(e) = res.remove("ErrorDescription").and_then(|x| x.into_string()) {
                return Err(InstallationProxyError::OperationFailed(e.to_string()).into());
            }

            if let Some(c) = res
                .remove("PercentComplete")
                .and_then(|x| x.as_unsigned_integer())
            {
                callback((c, state.clone())).await;
            }

            if let Some(c) = res.remove("Status").and_then(|x| x.into_string())
                && c == "Complete"
            {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(feature = "rsd")]
impl crate::RsdService for InstallationProxyClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        crate::obf!("com.apple.mobile.installation_proxy.shim.remote")
    }
    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = crate::Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}
