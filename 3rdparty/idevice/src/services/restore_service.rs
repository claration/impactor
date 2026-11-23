//! Restore Service

use plist::Dictionary;
use tracing::warn;

use crate::{IdeviceError, ReadWrite, RemoteXpcClient, RsdService, obf};

/// Client for interacting with the Restore Service
#[derive(Debug)]
pub struct RestoreServiceClient {
    /// The underlying device connection with established Restore Service service
    pub stream: RemoteXpcClient<Box<dyn ReadWrite>>,
}

impl RsdService for RestoreServiceClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.RestoreRemoteServices.restoreserviced")
    }

    async fn from_stream(stream: Box<dyn ReadWrite>) -> Result<Self, IdeviceError> {
        Self::new(stream).await
    }
}

impl RestoreServiceClient {
    /// Creates a new Restore Service client a socket connection,
    /// and connects to the RemoteXPC service.
    ///
    /// # Arguments
    /// * `idevice` - Pre-established device connection
    pub async fn new(stream: Box<dyn ReadWrite>) -> Result<Self, IdeviceError> {
        let mut stream = RemoteXpcClient::new(stream).await?;
        stream.do_handshake().await?;
        Ok(Self { stream })
    }

    /// Enter recovery
    pub async fn enter_recovery(&mut self) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "command": "recovery"
        });

        self.stream.send_object(req, true).await?;

        let res = self.stream.recv().await?;
        let mut res = match res {
            plist::Value::Dictionary(d) => d,
            _ => {
                warn!("Did not receive dictionary response from XPC");
                return Err(IdeviceError::UnexpectedResponse(
                    "enter_recovery XPC response is not a dictionary".into(),
                ));
            }
        };

        match res.remove("result") {
            Some(plist::Value::String(r)) => {
                if r == "success" {
                    Ok(())
                } else {
                    warn!("Failed to enter recovery");
                    Err(IdeviceError::UnexpectedResponse(
                        "enter_recovery result was not success".into(),
                    ))
                }
            }
            _ => {
                warn!("XPC dictionary did not contain result");
                Err(IdeviceError::UnexpectedResponse(
                    "missing result in enter_recovery response".into(),
                ))
            }
        }
    }

    /// Reboot
    pub async fn reboot(&mut self) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "command": "reboot"
        });
        self.stream.send_object(req, true).await?;

        let res = self.stream.recv().await?;
        let mut res = match res {
            plist::Value::Dictionary(d) => d,
            _ => {
                warn!("Did not receive dictionary response from XPC");
                return Err(IdeviceError::UnexpectedResponse(
                    "reboot XPC response is not a dictionary".into(),
                ));
            }
        };

        match res.remove("result") {
            Some(plist::Value::String(r)) => {
                if r == "success" {
                    Ok(())
                } else {
                    warn!("Failed to enter recovery");
                    Err(IdeviceError::UnexpectedResponse(
                        "reboot result was not success".into(),
                    ))
                }
            }
            _ => {
                warn!("XPC dictionary did not contain result");
                Err(IdeviceError::UnexpectedResponse(
                    "missing result in reboot response".into(),
                ))
            }
        }
    }

    /// Get preflightinfo
    pub async fn get_preflightinfo(&mut self) -> Result<Dictionary, IdeviceError> {
        let req = crate::plist!({
            "command": "getpreflightinfo"
        });
        self.stream.send_object(req, true).await?;

        let res = self.stream.recv().await?;
        let mut res = match res {
            plist::Value::Dictionary(d) => d,
            _ => {
                warn!("Did not receive dictionary response from XPC");
                return Err(IdeviceError::UnexpectedResponse(
                    "getpreflightinfo XPC response is not a dictionary".into(),
                ));
            }
        };

        let res = match res.remove("preflightinfo") {
            Some(plist::Value::Dictionary(i)) => i,
            _ => {
                warn!("XPC dictionary did not contain preflight info");
                return Err(IdeviceError::UnexpectedResponse(
                    "missing preflightinfo in response".into(),
                ));
            }
        };

        Ok(res)
    }

    /// Get nonces
    /// Doesn't seem to work
    pub async fn get_nonces(&mut self) -> Result<Dictionary, IdeviceError> {
        let req = crate::plist!({
            "command": "getnonces"
        });
        self.stream.send_object(req, true).await?;

        let res = self.stream.recv().await?;
        let mut res = match res {
            plist::Value::Dictionary(d) => d,
            _ => {
                warn!("Did not receive dictionary response from XPC");
                return Err(IdeviceError::UnexpectedResponse(
                    "getnonces XPC response is not a dictionary".into(),
                ));
            }
        };

        let res = match res.remove("nonces") {
            Some(plist::Value::Dictionary(i)) => i,
            _ => {
                warn!("XPC dictionary did not contain nonces");
                return Err(IdeviceError::UnexpectedResponse(
                    "missing nonces in response".into(),
                ));
            }
        };

        Ok(res)
    }

    /// Get app parameters
    /// Doesn't seem to work
    pub async fn get_app_parameters(&mut self) -> Result<Dictionary, IdeviceError> {
        let req = crate::plist!({
            "command": "getappparameters"
        });
        self.stream.send_object(req, true).await?;

        let res = self.stream.recv().await?;
        let mut res = match res {
            plist::Value::Dictionary(d) => d,
            _ => {
                warn!("Did not receive dictionary response from XPC");
                return Err(IdeviceError::UnexpectedResponse(
                    "getappparameters XPC response is not a dictionary".into(),
                ));
            }
        };

        let res = match res.remove("appparameters") {
            Some(plist::Value::Dictionary(i)) => i,
            _ => {
                warn!("XPC dictionary did not contain parameters");
                return Err(IdeviceError::UnexpectedResponse(
                    "missing appparameters in response".into(),
                ));
            }
        };

        Ok(res)
    }

    /// Restores the language
    /// Doesn't seem to work
    pub async fn restore_lang(&mut self, language: impl Into<String>) -> Result<(), IdeviceError> {
        let language = language.into();

        let req = crate::plist!({
            "command": "restorelang",
            "argument": language,
        });
        self.stream.send_object(req, true).await?;

        let res = self.stream.recv().await?;
        let mut res = match res {
            plist::Value::Dictionary(d) => d,
            _ => {
                warn!("Did not receive dictionary response from XPC");
                return Err(IdeviceError::UnexpectedResponse(
                    "restorelang XPC response is not a dictionary".into(),
                ));
            }
        };

        match res.remove("result") {
            Some(plist::Value::String(r)) => {
                if r == "success" {
                    Ok(())
                } else {
                    warn!("Failed to restore language");
                    Err(IdeviceError::UnexpectedResponse(
                        "restorelang result was not success".into(),
                    ))
                }
            }
            _ => {
                warn!("XPC dictionary did not contain result");
                Err(IdeviceError::UnexpectedResponse(
                    "missing result in restorelang response".into(),
                ))
            }
        }
    }
}
