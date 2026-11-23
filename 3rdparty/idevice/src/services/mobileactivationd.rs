//! mobileactivationd activates iOS devices.
//! This isn't a normal service, as it requires a new connection for each request.
//! As such, this service requires a provider itself, instead of temporary usage of one.

use plist::Dictionary;

use crate::{Idevice, IdeviceError, IdeviceService, lockdown::LockdownClient, obf};

#[derive(Debug)]
pub struct MobileActivationdClient<'a> {
    provider: &'a dyn crate::provider::IdeviceProvider,
}

/// Internal structure for temporary service connections.
/// This struct exists to take advantage of the service trait.
struct MobileActivationdInternal {
    pub idevice: Idevice,
}

impl IdeviceService for MobileActivationdInternal {
    /// Returns the service name as registered with lockdownd
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.mobileactivationd")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl MobileActivationdInternal {
    fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }
}

impl<'a> MobileActivationdClient<'a> {
    pub fn new(provider: &'a dyn crate::provider::IdeviceProvider) -> Self {
        Self { provider }
    }

    pub async fn state(&self) -> Result<String, IdeviceError> {
        if let Ok(res) = self.send_command("GetActivationStateRequest", None).await
            && let Some(v) = res.get("Value").and_then(|x| x.as_string())
        {
            Ok(v.to_string())
        } else {
            let mut lc = LockdownClient::connect(self.provider).await?;
            lc.start_session(&self.provider.get_pairing_file().await?)
                .await?;

            let res = lc.get_value(Some("ActivationState"), None).await?;
            if let Some(v) = res.as_string() {
                Ok(v.to_string())
            } else {
                Err(IdeviceError::UnexpectedResponse(
                    "ActivationState is not a string".into(),
                ))
            }
        }
    }

    pub async fn activated(&self) -> Result<bool, IdeviceError> {
        Ok(self.state().await? == "Activated")
    }

    /// Deactivates the device.
    /// Protocol gives no response on whether it worked or not, so good luck
    pub async fn deactivate(&self) -> Result<(), IdeviceError> {
        self.send_command("DeactivateRequest", None).await?;
        Ok(())
    }

    async fn send_command(
        &self,
        command: impl Into<String>,
        value: Option<&str>,
    ) -> Result<Dictionary, IdeviceError> {
        let mut service = self.service_connect().await?;
        let command = command.into();
        let req = crate::plist!({
            "Command": command,
            "Value":? value,
        });
        service.send_plist(req).await?;
        service.read_plist().await
    }

    async fn service_connect(&self) -> Result<Idevice, IdeviceError> {
        Ok(MobileActivationdInternal::connect(self.provider)
            .await?
            .idevice)
    }
}
