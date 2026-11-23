//! Diagnostics Relay

use crate::{Idevice, IdeviceError, IdeviceService, obf};

/// Client for interacting with the Diagnostics Relay
#[derive(Debug)]
pub struct DiagnosticsRelayClient {
    /// The underlying device connection with established service
    pub idevice: Idevice,
}

impl IdeviceService for DiagnosticsRelayClient {
    /// Returns the service name as registered with lockdownd
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.mobile.diagnostics_relay")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl DiagnosticsRelayClient {
    /// Creates a new client from an existing device connection
    ///
    /// # Arguments
    /// * `idevice` - Pre-established device connection
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    /// Requests data from the IO registry
    ///
    /// # Arguments
    /// * `current_plane` - The plane to request the tree as
    /// * `entry_name` - The entry to get
    /// * `entry_class` - The class to filter by
    ///
    /// # Returns
    /// A plist of the tree on success
    pub async fn ioregistry(
        &mut self,
        current_plane: Option<&str>,
        entry_name: Option<&str>,
        entry_class: Option<&str>,
    ) -> Result<Option<plist::Dictionary>, IdeviceError> {
        let req = crate::plist!({
            "Request": "IORegistry",
            "CurrentPlane":? current_plane,
            "EntryName":? entry_name,
            "EntryClass":? entry_class,
        });
        self.idevice.send_plist(req).await?;
        let mut res = self.idevice.read_plist().await?;

        match res.get("Status").and_then(|x| x.as_string()) {
            Some("Success") => {}
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing or non-Success Status in IORegistry response".into(),
                ));
            }
        }

        let res = res
            .remove("Diagnostics")
            .and_then(|x| x.into_dictionary())
            .and_then(|mut x| x.remove("IORegistry"))
            .and_then(|x| x.into_dictionary());

        Ok(res)
    }

    /// Requests MobileGestalt information from the device
    ///
    /// # Arguments
    /// * `keys` - Optional list of specific keys to request. If None, requests all available keys
    ///
    /// # Returns
    /// A dictionary containing the requested MobileGestalt information
    pub async fn mobilegestalt(
        &mut self,
        keys: Option<Vec<String>>,
    ) -> Result<Option<plist::Dictionary>, IdeviceError> {
        let req = crate::plist!({
            "Request": "MobileGestalt",
            "MobileGestaltKeys":? keys,
        });
        self.idevice.send_plist(req).await?;
        let mut res = self.idevice.read_plist().await?;

        match res.get("Status").and_then(|x| x.as_string()) {
            Some("Success") => {}
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing or non-Success Status in MobileGestalt response".into(),
                ));
            }
        }

        let res = res.remove("Diagnostics").and_then(|x| x.into_dictionary());

        Ok(res)
    }

    /// Requests gas gauge information from the device
    ///
    /// # Returns
    /// A dictionary containing gas gauge (battery) information
    pub async fn gasguage(&mut self) -> Result<Option<plist::Dictionary>, IdeviceError> {
        let req = crate::plist!({
            "Request": "GasGauge"
        });
        self.idevice.send_plist(req).await?;
        let mut res = self.idevice.read_plist().await?;

        match res.get("Status").and_then(|x| x.as_string()) {
            Some("Success") => {}
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing or non-Success Status in GasGauge response".into(),
                ));
            }
        }

        let res = res.remove("Diagnostics").and_then(|x| x.into_dictionary());

        Ok(res)
    }

    /// Requests NAND information from the device
    ///
    /// # Returns
    /// A dictionary containing NAND flash information
    pub async fn nand(&mut self) -> Result<Option<plist::Dictionary>, IdeviceError> {
        let req = crate::plist!({
            "Request": "NAND"
        });

        self.idevice.send_plist(req).await?;
        let mut res = self.idevice.read_plist().await?;

        match res.get("Status").and_then(|x| x.as_string()) {
            Some("Success") => {}
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing or non-Success Status in NAND response".into(),
                ));
            }
        }

        let res = res.remove("Diagnostics").and_then(|x| x.into_dictionary());

        Ok(res)
    }

    /// Requests all available diagnostics information
    ///
    /// # Returns
    /// A dictionary containing all diagnostics information
    pub async fn all(&mut self) -> Result<Option<plist::Dictionary>, IdeviceError> {
        let req = crate::plist!({
            "Request": "All"
        });

        self.idevice.send_plist(req).await?;
        let mut res = self.idevice.read_plist().await?;

        match res.get("Status").and_then(|x| x.as_string()) {
            Some("Success") => {}
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing or non-Success Status in All diagnostics response".into(),
                ));
            }
        }

        let res = res.remove("Diagnostics").and_then(|x| x.into_dictionary());

        Ok(res)
    }

    /// Restarts the device
    ///
    /// # Returns
    /// Result indicating success or failure
    pub async fn restart(&mut self) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "Request": "Restart",
        });

        self.idevice.send_plist(req).await?;
        let res = self.idevice.read_plist().await?;

        match res.get("Status").and_then(|x| x.as_string()) {
            Some("Success") => Ok(()),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing or non-Success Status in Restart response".into(),
            )),
        }
    }

    /// Shuts down the device
    ///
    /// # Returns
    /// Result indicating success or failure
    pub async fn shutdown(&mut self) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "Request": "Shutdown"
        });

        self.idevice.send_plist(req).await?;
        let res = self.idevice.read_plist().await?;

        match res.get("Status").and_then(|x| x.as_string()) {
            Some("Success") => Ok(()),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing or non-Success Status in Shutdown response".into(),
            )),
        }
    }

    /// Puts the device to sleep
    ///
    /// # Returns
    /// Result indicating success or failure
    pub async fn sleep(&mut self) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "Request": "Sleep"
        });

        self.idevice.send_plist(req).await?;
        let res = self.idevice.read_plist().await?;

        match res.get("Status").and_then(|x| x.as_string()) {
            Some("Success") => Ok(()),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing or non-Success Status in Sleep response".into(),
            )),
        }
    }

    /// Requests WiFi diagnostics from the device
    pub async fn wifi(&mut self) -> Result<Option<plist::Dictionary>, IdeviceError> {
        let req = crate::plist!({
            "Request": "WiFi"
        });

        self.idevice.send_plist(req).await?;
        let mut res = self.idevice.read_plist().await?;

        match res.get("Status").and_then(|x| x.as_string()) {
            Some("Success") => {}
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing or non-Success Status in WiFi response".into(),
                ));
            }
        }

        let res = res.remove("Diagnostics").and_then(|x| x.into_dictionary());

        Ok(res)
    }

    /// Sends Goodbye request signaling end of communication
    pub async fn goodbye(&mut self) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "Request": "Goodbye"
        });

        self.idevice.send_plist(req).await?;
        let res = self.idevice.read_plist().await?;
        match res.get("Status").and_then(|x| x.as_string()) {
            Some("Success") => Ok(()),
            Some("UnknownRequest") => Err(IdeviceError::UnexpectedResponse(
                "Goodbye request returned UnknownRequest".into(),
            )),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing or non-Success Status in Goodbye response".into(),
            )),
        }
    }
}

#[cfg(feature = "rsd")]
impl crate::RsdService for DiagnosticsRelayClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        crate::obf!("com.apple.mobile.diagnostics_relay.shim.remote")
    }
    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = crate::Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}
