//! SpringBoard Services Client
//!
//! Provides functionality for interacting with the SpringBoard services on iOS devices,
//! which manages home screen and app icon related operations.

use crate::{Idevice, IdeviceError, IdeviceService, obf, utils::plist::truncate_dates_to_seconds};

/// Orientation of the device
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum InterfaceOrientation {
    /// Orientation is unknown or cannot be determined
    Unknown = 0,
    /// Portrait mode (normal vertical)
    Portrait = 1,
    /// Portrait mode upside down
    PortraitUpsideDown = 2,
    /// Landscape with home button on the right (notch to the left)
    LandscapeRight = 3,
    /// Landscape with home button on the left (notch to the right)
    LandscapeLeft = 4,
}

/// Client for interacting with the iOS SpringBoard services
///
/// This service provides access to home screen and app icon functionality,
/// such as retrieving app icons.
#[derive(Debug)]
pub struct SpringBoardServicesClient {
    /// The underlying device connection with established SpringBoard services
    pub idevice: Idevice,
}

impl IdeviceService for SpringBoardServicesClient {
    /// Returns the SpringBoard services name as registered with lockdownd
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.springboardservices")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl SpringBoardServicesClient {
    /// Creates a new SpringBoard services client from an existing device connection
    ///
    /// # Arguments
    /// * `idevice` - Pre-established device connection
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    /// Retrieves the PNG icon data for a specified app
    ///
    /// # Arguments
    /// * `bundle_identifier` - The bundle identifier of the app (e.g., "com.apple.Maps")
    ///
    /// # Returns
    /// The raw PNG data of the app icon
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The app doesn't exist
    /// - The response is malformed
    ///
    /// # Example
    /// ```rust
    /// let icon_data = client.get_icon_pngdata("com.apple.Maps".to_string()).await?;
    /// std::fs::write("maps_icon.png", icon_data)?;
    /// ```
    pub async fn get_icon_pngdata(
        &mut self,
        bundle_identifier: String,
    ) -> Result<Vec<u8>, IdeviceError> {
        let req = crate::plist!({
            "command": "getIconPNGData",
            "bundleId": bundle_identifier,
        });
        self.idevice.send_plist(req).await?;

        let mut res = self.idevice.read_plist().await?;
        match res.remove("pngData") {
            Some(plist::Value::Data(res)) => Ok(res),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing pngData in icon response".into(),
            )),
        }
    }

    /// Retrieves the current icon state from the device
    ///
    /// The icon state contains the layout and organization of all apps on the home screen,
    /// including folder structures and icon positions. This is a read-only operation.
    ///
    /// # Arguments
    /// * `format_version` - Optional format version string for the icon state format
    ///
    /// # Returns
    /// A plist Value containing the complete icon state structure
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The response is malformed
    ///
    /// # Example
    /// ```rust
    /// use idevice::services::springboardservices::SpringBoardServicesClient;
    ///
    /// let mut client = SpringBoardServicesClient::connect(&provider).await?;
    /// let icon_state = client.get_icon_state(None).await?;
    /// println!("Icon state: {:?}", icon_state);
    /// ```
    ///
    /// # Notes
    /// This method successfully reads the home screen layout on all iOS versions.
    pub async fn get_icon_state(
        &mut self,
        format_version: Option<&str>,
    ) -> Result<plist::Value, IdeviceError> {
        let req = crate::plist!({
            "command": "getIconState",
            "formatVersion":? format_version,
        });

        self.idevice.send_plist(req).await?;
        let mut res = self.idevice.read_plist_value().await?;

        // Some devices may return an error dictionary instead of icon state.
        // Detect this and surface it as an UnexpectedResponse, similar to get_icon_pngdata.
        if let plist::Value::Dictionary(ref dict) = res
            && (dict.contains_key("error") || dict.contains_key("Error"))
        {
            return Err(IdeviceError::UnexpectedResponse(
                "device returned error in icon state response".into(),
            ));
        }

        truncate_dates_to_seconds(&mut res);

        Ok(res)
    }

    /// Sets the icon state on the device
    ///
    /// This method allows you to modify the home screen layout by providing a new icon state.
    /// The icon state structure should match the format returned by `get_icon_state`.
    ///
    /// # Arguments
    /// * `icon_state` - A plist Value containing the complete icon state structure
    ///
    /// # Returns
    /// Ok(()) if the icon state was successfully set
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The icon state format is invalid
    /// - The device rejects the new layout
    ///
    /// # Example
    /// ```rust
    /// use idevice::services::springboardservices::SpringBoardServicesClient;
    ///
    /// let mut client = SpringBoardServicesClient::connect(&provider).await?;
    /// let mut icon_state = client.get_icon_state(None).await?;
    ///
    /// // Modify the icon state (e.g., swap two icons)
    /// // ... modify icon_state ...
    ///
    /// client.set_icon_state(icon_state).await?;
    /// println!("Icon state updated successfully");
    /// ```
    ///
    /// # Notes    
    /// - Changes take effect immediately
    /// - The device may validate the icon state structure before applying
    /// - Invalid icon states will be rejected by the device
    pub async fn set_icon_state(&mut self, icon_state: plist::Value) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "command": "setIconState",
            "iconState": icon_state,
        });

        self.idevice.send_plist(req).await?;
        Ok(())
    }

    /// Sets the icon state with a specific format version
    ///
    /// This is similar to `set_icon_state` but allows specifying a format version.
    ///
    /// # Arguments
    /// * `icon_state` - A plist Value containing the complete icon state structure
    /// * `format_version` - Optional format version string
    ///
    /// # Returns
    /// Ok(()) if the icon state was successfully set
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The icon state format is invalid
    /// - The device rejects the new layout
    pub async fn set_icon_state_with_version(
        &mut self,
        icon_state: plist::Value,
        format_version: Option<&str>,
    ) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "command": "setIconState",
            "iconState": icon_state,
            "formatVersion":? format_version,
        });

        self.idevice.send_plist(req).await?;
        Ok(())
    }

    /// Gets the home screen wallpaper preview as PNG data
    ///
    /// This gets a rendered preview of the home screen wallpaper.
    ///
    /// # Returns
    /// The raw PNG data of the home screen wallpaper preview
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The device rejects the request
    /// - The image is malformed/corupted
    ///
    /// # Example
    /// ```rust
    /// let wallpaper = client.get_home_screen_wallpaper_preview_pngdata().await?;
    /// std::fs::write("home.png", wallpaper)?;
    /// ```
    pub async fn get_home_screen_wallpaper_preview_pngdata(
        &mut self,
    ) -> Result<Vec<u8>, IdeviceError> {
        let req = crate::plist!({
            "command": "getWallpaperPreviewImage",
            "wallpaperName": "homescreen",
        });
        self.idevice.send_plist(req).await?;

        let mut res = self.idevice.read_plist().await?;
        match res.remove("pngData") {
            Some(plist::Value::Data(res)) => Ok(res),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing pngData in home screen wallpaper response".into(),
            )),
        }
    }

    /// Gets the lock screen wallpaper preview as PNG data
    ///
    /// This gets a rendered preview of the lock screen wallpaper.
    ///
    /// # Returns
    /// The raw PNG data of the lock screen wallpaper preview
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The device rejects the request
    /// - The image is malformed/corupted
    ///
    /// # Example
    /// ```rust
    /// let wallpaper = client.get_lock_screen_wallpaper_preview_pngdata().await?;
    /// std::fs::write("lock.png", wallpaper)?;
    /// ```
    pub async fn get_lock_screen_wallpaper_preview_pngdata(
        &mut self,
    ) -> Result<Vec<u8>, IdeviceError> {
        let req = crate::plist!({
            "command": "getWallpaperPreviewImage",
            "wallpaperName": "lockscreen",
        });
        self.idevice.send_plist(req).await?;

        let mut res = self.idevice.read_plist().await?;
        match res.remove("pngData") {
            Some(plist::Value::Data(res)) => Ok(res),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing pngData in lock screen wallpaper response".into(),
            )),
        }
    }

    /// Gets the current interface orientation of the device
    ///
    /// This gets which way the device is currently facing
    ///
    /// # Returns
    /// The current `InterfaceOrientation` of the device
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The device doesn't support this command
    /// - The response format is unexpected
    ///
    /// # Example
    /// ```rust
    /// let orientation = client.get_interface_orientation().await?;
    /// println!("Device orientation: {:?}", orientation);
    /// ```
    pub async fn get_interface_orientation(
        &mut self,
    ) -> Result<InterfaceOrientation, IdeviceError> {
        let req = crate::plist!({
            "command": "getInterfaceOrientation",
        });
        self.idevice.send_plist(req).await?;

        let res = self.idevice.read_plist().await?;
        let orientation_value = res
            .get("interfaceOrientation")
            .and_then(|v| v.as_unsigned_integer())
            .ok_or(IdeviceError::UnexpectedResponse(
                "missing interfaceOrientation in response".into(),
            ))?;

        let orientation = match orientation_value {
            1 => InterfaceOrientation::Portrait,
            2 => InterfaceOrientation::PortraitUpsideDown,
            3 => InterfaceOrientation::LandscapeRight,
            4 => InterfaceOrientation::LandscapeLeft,
            _ => InterfaceOrientation::Unknown,
        };

        Ok(orientation)
    }

    /// Gets the home screen icon layout metrics
    ///
    /// Returns icon spacing, size, and positioning information
    ///
    /// # Returns
    /// A `plist::Dictionary` containing the icon layout metrics
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - Communication fails
    /// - The response is malformed
    ///
    /// # Example
    /// ```rust
    /// let metrics = client.get_homescreen_icon_metrics().await?;
    /// println!("{:?}", metrics);
    /// ```
    pub async fn get_homescreen_icon_metrics(&mut self) -> Result<plist::Dictionary, IdeviceError> {
        let req = crate::plist!({
            "command": "getHomeScreenIconMetrics",
        });
        self.idevice.send_plist(req).await?;

        let res = self.idevice.read_plist().await?;
        Ok(res)
    }
}

#[cfg(feature = "rsd")]
impl crate::RsdService for SpringBoardServicesClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        crate::obf!("com.apple.springboardservices.shim.remote")
    }
    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = crate::Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}
