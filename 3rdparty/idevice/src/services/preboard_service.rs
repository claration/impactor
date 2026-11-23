//! Abstraction for preboard

use crate::{Idevice, IdeviceError, IdeviceService, RsdService, obf};

/// Client for interacting with the preboard service on the device.
#[derive(Debug)]
pub struct PreboardServiceClient {
    /// The underlying device connection with established service
    pub idevice: Idevice,
}

impl IdeviceService for PreboardServiceClient {
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.preboardservice_v2")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl RsdService for PreboardServiceClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.preboardservice_v2.shim.remote")
    }

    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}

impl PreboardServiceClient {
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    pub async fn create_stashbag(&mut self, manifest: &[u8]) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "Command": "CreateStashbag",
            "Manifest": manifest
        });
        self.idevice.send_plist(req).await?;
        let res = self.idevice.read_plist().await?;
        if let Some(res) = res.get("ShowDialog").and_then(|x| x.as_boolean()) {
            if !res {
                tracing::warn!("ShowDialog is not true");
                return Err(IdeviceError::UnexpectedResponse(
                    "ShowDialog is false in CreateStashbag response".into(),
                ));
            }
        } else {
            tracing::warn!("No ShowDialog in response from service");
            return Err(IdeviceError::UnexpectedResponse(
                "missing ShowDialog in CreateStashbag response".into(),
            ));
        }

        self.idevice.read_plist().await?;
        Ok(())
    }

    pub async fn commit_stashbag(&mut self, manifest: &[u8]) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "Command": "CommitStashbag",
            "Manifest": manifest
        });
        self.idevice.send_plist(req).await?;
        self.idevice.read_plist().await?;
        Ok(())
    }

    pub async fn clear_system_token(&mut self) -> Result<(), IdeviceError> {
        todo!()
    }
}
