//! Application listing service - List installed applications on the device

use plist::{Dictionary, Value};

use super::message::AuxValue;
use super::remote_server::{Channel, RemoteServerClient};
use crate::{IdeviceError, ReadWrite, obf};

/// Client for listing installed applications
#[derive(Debug)]
pub struct ApplicationListingClient<'a, R: ReadWrite> {
    channel: Channel<'a, R>,
}

impl<'a, R: ReadWrite> ApplicationListingClient<'a, R> {
    pub async fn new(client: &'a mut RemoteServerClient<R>) -> Result<Self, IdeviceError> {
        let channel = client
            .make_channel(obf!(
                "com.apple.instruments.server.services.device.applictionListing"
            ))
            .await?;
        Ok(Self { channel })
    }

    /// Returns the list of installed applications with their attributes
    pub async fn installed_applications(&mut self) -> Result<Vec<Dictionary>, IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String(
                    "installedApplicationsMatching:registerUpdateToken:".into(),
                )),
                Some(vec![
                    AuxValue::archived_value(Value::Dictionary(Dictionary::new())),
                    AuxValue::archived_value(Value::String(String::new())),
                ]),
                true,
            )
            .await?;
        let msg = self.channel.read_message().await?;
        let data = msg
            .data
            .ok_or_else(|| IdeviceError::UnexpectedResponse("expected application list".into()))?;

        let arr = data
            .into_array()
            .ok_or_else(|| IdeviceError::UnexpectedResponse("expected array".into()))?;
        Ok(arr
            .into_iter()
            .filter_map(|v| v.into_dictionary())
            .collect())
    }
}
