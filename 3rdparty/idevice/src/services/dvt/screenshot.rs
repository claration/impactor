//! Screenshot service client for iOS instruments protocol.
//!
//! This module provides a client for interacting with the screenshot service
//! on iOS devices through the instruments protocol. It allows taking screenshots from the device.
//!

use plist::Value;

use crate::{
    IdeviceError, ReadWrite,
    dvt::remote_server::{Channel, RemoteServerClient},
    obf,
};

/// Client for take screenshot operations on iOS devices
///
/// Provides methods for take screnn_shot through the
/// instruments protocol. Each instance maintains its own communication channel.
#[derive(Debug)]
pub struct ScreenshotClient<'a, R: ReadWrite> {
    /// The underlying channel for communication
    channel: Channel<'a, R>,
}

impl<'a, R: ReadWrite> ScreenshotClient<'a, R> {
    /// Creates a new ScreenshotClient
    ///
    /// # Arguments
    /// * `client` - The base RemoteServerClient to use
    ///
    /// # Returns
    /// * `Ok(ScreenshotClient)` - Connected client instance
    /// * `Err(IdeviceError)` - If channel creation fails
    ///
    /// # Errors
    /// * Propagates errors from channel creation
    pub async fn new(client: &'a mut RemoteServerClient<R>) -> Result<Self, IdeviceError> {
        let channel = client
            .make_channel(obf!("com.apple.instruments.server.services.screenshot"))
            .await?; // Drop `&mut client` before continuing

        Ok(Self { channel })
    }

    /// Take screenshot from the device
    ///
    /// # Returns
    /// * `Ok(Vec<u8>)` - the bytes of the screenshot
    /// * `Err(IdeviceError)` - If communication fails
    ///
    /// # Errors
    /// * `IdeviceError::UnexpectedResponse` if server response is invalid
    /// * Other communication or serialization errors
    pub async fn take_screenshot(&mut self) -> Result<Vec<u8>, IdeviceError> {
        let method = Value::String("takeScreenshot".into());

        self.channel.call_method(Some(method), None, true).await?;

        let msg = self.channel.read_message().await?;
        match msg.data {
            Some(Value::Data(data)) => Ok(data),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing screenshot data in DVT response".into(),
            )),
        }
    }
}
