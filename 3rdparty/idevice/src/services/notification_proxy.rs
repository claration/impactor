//! iOS Device Notification Proxy Service
//!
//! Based on libimobiledevice's notification_proxy implementation
//!
//! Common notification identifiers:
//! Full list: include/libimobiledevice/notification_proxy.h
//!
//! - Notifications that can be sent (PostNotification):
//!   - `com.apple.itunes-mobdev.syncWillStart`           - Sync will start
//!   - `com.apple.itunes-mobdev.syncDidStart`            - Sync started
//!   - `com.apple.itunes-mobdev.syncDidFinish`           - Sync finished
//!   - `com.apple.itunes-mobdev.syncLockRequest`         - Request sync lock
//!
//! - Notifications that can be observed (ObserveNotification):
//!   - `com.apple.itunes-client.syncCancelRequest`       - Cancel sync request
//!   - `com.apple.itunes-client.syncSuspendRequest`      - Suspend sync
//!   - `com.apple.itunes-client.syncResumeRequest`       - Resume sync
//!   - `com.apple.mobile.lockdown.phone_number_changed`  - Phone number changed
//!   - `com.apple.mobile.lockdown.device_name_changed`   - Device name changed
//!   - `com.apple.mobile.lockdown.timezone_changed`      - Timezone changed
//!   - `com.apple.mobile.lockdown.trusted_host_attached` - Trusted host attached
//!   - `com.apple.mobile.lockdown.host_detached`         - Host detached
//!   - `com.apple.mobile.lockdown.host_attached`         - Host attached
//!   - `com.apple.mobile.lockdown.registration_failed`   - Registration failed
//!   - `com.apple.mobile.lockdown.activation_state`      - Activation state
//!   - `com.apple.mobile.lockdown.brick_state`           - Brick state
//!   - `com.apple.mobile.lockdown.disk_usage_changed`    - Disk usage (iOS 4.0+)
//!   - `com.apple.mobile.data_sync.domain_changed`       - Data sync domain changed
//!   - `com.apple.mobile.application_installed`          - App installed
//!   - `com.apple.mobile.application_uninstalled`        - App uninstalled

use std::pin::Pin;

use futures::Stream;
use tracing::warn;

use crate::{HeartbeatError, Idevice, IdeviceError, IdeviceService, obf};

/// Client for interacting with the iOS notification proxy service
///
/// The notification proxy service provides a mechanism to observe and post
/// system notifications.
///
/// Use `observe_notification` to register for events, then `receive_notification`
/// to wait for them.
#[derive(Debug)]
pub struct NotificationProxyClient {
    /// The underlying device connection with established notification_proxy service
    pub idevice: Idevice,
}

impl IdeviceService for NotificationProxyClient {
    /// Returns the notification proxy service name as registered with lockdownd
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.mobile.notification_proxy")
    }
    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl NotificationProxyClient {
    /// Creates a new notification proxy client from an existing device connection
    ///
    /// # Arguments
    /// * `idevice` - Pre-established device connection
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    /// Posts a notification to the device
    ///
    /// # Arguments
    /// * `notification_name` - Name of the notification to post
    ///
    /// # Errors
    /// Returns `IdeviceError` if the notification fails to send
    pub async fn post_notification(
        &mut self,
        notification_name: impl Into<String>,
    ) -> Result<(), IdeviceError> {
        let request = crate::plist!({
            "Command": "PostNotification",
            "Name": notification_name.into()
        });
        self.idevice.send_plist(request).await
    }

    /// Registers to observe a specific notification
    ///
    /// After calling this, use `receive_notification` to wait for events.
    ///
    /// # Arguments
    /// * `notification_name` - Name of the notification to observe
    ///
    /// # Errors
    /// Returns `IdeviceError` if the registration fails
    pub async fn observe_notification(
        &mut self,
        notification_name: impl Into<String>,
    ) -> Result<(), IdeviceError> {
        let request = crate::plist!({
            "Command": "ObserveNotification",
            "Name": notification_name.into()
        });
        self.idevice.send_plist(request).await
    }

    /// Registers to observe multiple notifications at once
    ///
    /// # Arguments
    /// * `notification_names` - Slice of notification names to observe
    ///
    /// # Errors
    /// Returns `IdeviceError` if any registration fails
    pub async fn observe_notifications(
        &mut self,
        notification_names: &[&str],
    ) -> Result<(), IdeviceError> {
        for name in notification_names {
            self.observe_notification(*name).await?;
        }
        Ok(())
    }

    /// Waits for and receives the next notification from the device
    ///
    /// # Returns
    /// The name of the received notification
    ///
    /// # Errors
    /// - `NotificationProxyDeath` if the proxy connection died
    /// - `UnexpectedResponse` if the response format is invalid
    pub async fn receive_notification(&mut self) -> Result<String, IdeviceError> {
        let response = self.idevice.read_plist().await?;

        match response.get("Command").and_then(|c| c.as_string()) {
            Some("RelayNotification") => match response.get("Name").and_then(|n| n.as_string()) {
                Some(name) => Ok(name.to_string()),
                None => Err(IdeviceError::UnexpectedResponse(
                    "missing Name in RelayNotification".into(),
                )),
            },
            Some("ProxyDeath") => {
                warn!("NotificationProxy died!");
                Err(IdeviceError::NotificationProxyDeath)
            }
            _ => Err(IdeviceError::UnexpectedResponse(
                "unexpected Command in notification response".into(),
            )),
        }
    }

    /// Waits for a notification with a timeout
    ///
    /// # Arguments
    /// * `interval` - Timeout in seconds to wait for a notification
    ///
    /// # Returns
    /// The name of the received notification
    ///
    /// # Errors
    /// - `NotificationProxyDeath` if the proxy connection died
    /// - `UnexpectedResponse` if the response format is invalid
    /// - `HeartbeatTimeout` if no notification received before interval
    pub async fn receive_notification_with_timeout(
        &mut self,
        interval: u64,
    ) -> Result<String, IdeviceError> {
        tokio::select! {
            result = self.receive_notification() => result,
            _ = tokio::time::sleep(tokio::time::Duration::from_secs(interval)) => {
                Err(HeartbeatError::Timeout.into())
            }
        }
    }

    /// Continuous stream of notifications.
    pub fn into_stream(
        mut self,
    ) -> Pin<Box<dyn Stream<Item = Result<String, IdeviceError>> + Send>> {
        Box::pin(async_stream::try_stream! {
            loop {
                let response = self.idevice.read_plist().await?;

                match response.get("Command").and_then(|c| c.as_string()) {
                    Some("RelayNotification") => {
                        match response.get("Name").and_then(|n| n.as_string()) {
                            Some(name) => yield name.to_string(),
                            None => Err(IdeviceError::UnexpectedResponse("missing Name in RelayNotification stream".into()))?,
                        }
                    }
                    Some("ProxyDeath") => {
                        warn!("NotificationProxy died!");
                        Err(IdeviceError::NotificationProxyDeath)?;
                    }
                    _ => Err(IdeviceError::UnexpectedResponse("unexpected Command in notification stream".into()))?,
                }
            }
        })
    }

    /// Shuts down the notification proxy connection
    ///
    /// # Errors
    /// Returns `IdeviceError` if the shutdown command fails to send
    pub async fn shutdown(&mut self) -> Result<(), IdeviceError> {
        let request = crate::plist!({
            "Command": "Shutdown"
        });
        self.idevice.send_plist(request).await?;
        // Best-effort: wait for ProxyDeath ack
        let _ = self.idevice.read_plist().await;
        Ok(())
    }
}

#[cfg(feature = "rsd")]
impl crate::RsdService for NotificationProxyClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        crate::obf!("com.apple.mobile.notification_proxy.shim.remote")
    }
    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = crate::Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}
