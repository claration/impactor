//! Process Control service client for iOS instruments protocol.
//!
//! This module provides a client for interacting with the process control service
//! on iOS devices through the instruments protocol. It allows launching, killing,
//! and managing processes on the device.
//!
//! # Example
//! ```rust,no_run
//! #[tokio::main]
//! async fn main() -> Result<(), IdeviceError> {
//!     // Create base client (implementation specific)
//!     let mut client = RemoteServerClient::new(your_transport);
//!
//!     // Create process control client
//!     let mut process_control = ProcessControlClient::new(&mut client).await?;
//!
//!     // Launch an app
//!     let pid = process_control.launch_app(
//!         "com.example.app",
//!         None,       // Environment variables
//!         None,       // Arguments
//!         false,      // Start suspended
//!         true        // Kill existing
//!     ).await?;
//!     println!("Launched app with PID: {}", pid);
//!
//!     // Disable memory limits
//!     process_control.disable_memory_limit(pid).await?;
//!
//!     // Kill the app
//!     process_control.kill_app(pid).await?;
//!
//!     Ok(())
//! }
//! ```

use plist::{Dictionary, Value};
use tracing::warn;

use super::errors::DvtError;
use crate::{IdeviceError, ReadWrite, dvt::message::AuxValue, obf};

use super::remote_server::{Channel, RemoteServerClient};

/// Client for process control operations on iOS devices
///
/// Provides methods for launching, killing, and managing processes through the
/// instruments protocol. Each instance maintains its own communication channel.
#[derive(Debug)]
pub struct ProcessControlClient<'a, R: ReadWrite> {
    /// The underlying channel for communication
    channel: Channel<'a, R>,
}

fn parse_u64_value(value: &Value) -> Option<u64> {
    match value {
        Value::Integer(v) => v.as_unsigned(),
        Value::String(s) => s.parse().ok(),
        _ => None,
    }
}

fn extract_ns_error_message(value: &Value) -> Option<String> {
    let dict = match value {
        Value::Dictionary(dict) => dict,
        _ => return None,
    };

    let user_info = match dict.get("NSUserInfo") {
        Some(Value::Array(items)) => items,
        _ => {
            return dict
                .get("NSLocalizedDescription")
                .and_then(Value::as_string)
                .map(ToOwned::to_owned);
        }
    };

    let mut description = dict
        .get("NSLocalizedDescription")
        .and_then(Value::as_string)
        .map(ToOwned::to_owned);
    let mut reason = dict
        .get("NSLocalizedFailureReason")
        .and_then(Value::as_string)
        .map(ToOwned::to_owned);

    for entry in user_info {
        let Value::Dictionary(item) = entry else {
            continue;
        };

        let key = item.get("key").and_then(Value::as_string);
        let value = item.get("value");

        match (key, value) {
            (Some("NSLocalizedDescription"), Some(Value::String(s))) if description.is_none() => {
                description = Some(s.clone());
            }
            (Some("NSLocalizedFailureReason"), Some(Value::String(s))) if reason.is_none() => {
                reason = Some(s.clone());
            }
            (Some("NSUnderlyingError"), Some(v)) => {
                if let Some(message) = extract_ns_error_message(v) {
                    return Some(message);
                }
            }
            _ => {}
        }
    }

    match (description, reason) {
        (Some(description), Some(reason)) => Some(format!("{description}: {reason}")),
        (Some(description), None) => Some(description),
        (None, Some(reason)) => Some(reason),
        (None, None) => None,
    }
}

impl<'a, R: ReadWrite> ProcessControlClient<'a, R> {
    /// Creates a new ProcessControlClient
    ///
    /// # Arguments
    /// * `client` - The base RemoteServerClient to use
    ///
    /// # Returns
    /// * `Ok(ProcessControlClient)` - Connected client instance
    /// * `Err(IdeviceError)` - If channel creation fails
    ///
    /// # Errors
    /// * Propagates errors from channel creation
    pub async fn new(client: &'a mut RemoteServerClient<R>) -> Result<Self, IdeviceError> {
        let channel = client
            .make_channel(obf!("com.apple.instruments.server.services.processcontrol"))
            .await?; // Drop `&mut client` before continuing

        Ok(Self { channel })
    }

    /// Launches an application on the device
    ///
    /// # Arguments
    /// * `bundle_id` - The bundle identifier of the app to launch
    /// * `env_vars` - Optional environment variables dictionary
    /// * `arguments` - Optional launch arguments dictionary
    /// * `start_suspended` - Whether to start the process suspended
    /// * `kill_existing` - Whether to kill existing instances of the app
    ///
    /// # Returns
    /// * `Ok(u64)` - PID of the launched process
    /// * `Err(IdeviceError)` - If launch fails
    ///
    /// # Errors
    /// * `IdeviceError::UnexpectedResponse("unexpected response".into())` if server response is invalid
    /// * Other communication or serialization errors
    pub async fn launch_app(
        &mut self,
        bundle_id: impl Into<String>,
        env_vars: Option<Dictionary>,
        arguments: Option<Dictionary>,
        start_suspended: bool,
        kill_existing: bool,
    ) -> Result<u64, IdeviceError> {
        let method = Value::String(
            "launchSuspendedProcessWithDevicePath:bundleIdentifier:environment:arguments:options:"
                .into(),
        );
        let options = crate::plist!(dict {
           "StartSuspendedKey": start_suspended,
            "KillExisting": kill_existing
        });

        let env_vars = match env_vars {
            Some(e) => e,
            None => Dictionary::new(),
        };
        let arguments = match arguments {
            Some(a) => a,
            None => Dictionary::new(),
        };

        let res = self
            .channel
            .call_method_with_reply(
                Some(method),
                Some(vec![
                    AuxValue::archived_value(""),
                    AuxValue::archived_value(bundle_id.into()),
                    AuxValue::archived_value(env_vars),
                    AuxValue::archived_value(Value::Array(
                        arguments
                            .into_iter()
                            .map(|(_, value)| value)
                            .collect::<Vec<_>>(),
                    )),
                    AuxValue::archived_value(options),
                ]),
            )
            .await?;

        match res.data {
            Some(Value::Integer(p)) => match p.as_unsigned() {
                Some(p) => Ok(p),
                None => {
                    warn!("PID wasn't unsigned");
                    Err(IdeviceError::UnexpectedResponse(
                        "launch response PID was not an unsigned integer".into(),
                    ))
                }
            },
            _ => {
                warn!("Did not get integer response");
                Err(IdeviceError::UnexpectedResponse(
                    "expected integer PID in launch app response".into(),
                ))
            }
        }
    }

    /// Launches a process with fully customisable options.
    ///
    /// This is a lower-level variant of [`launch_app`](Self::launch_app) that
    /// accepts a pre-built `args` array (`Vec<plist::Value>`) and a custom
    /// `options` dictionary (e.g. `{"StartSuspendedKey": false, "ActivateSuspended": true}`).
    ///
    /// # Arguments
    /// * `bundle_id`  - Bundle identifier of the app to launch
    /// * `env`        - Environment variables
    /// * `args`       - Launch arguments (each element already a `plist::Value::String`)
    /// * `options`    - Launch options dictionary
    ///
    /// # Returns
    /// * `Ok(u64)` - PID of the launched process
    pub(crate) async fn launch_with_options(
        &mut self,
        bundle_id: impl Into<String>,
        env: Dictionary,
        args: Vec<Value>,
        options: Dictionary,
    ) -> Result<u64, IdeviceError> {
        let res = self
            .channel
            .call_method_with_reply(
                Some(Value::String(
                    "launchSuspendedProcessWithDevicePath:bundleIdentifier:environment:arguments:options:"
                        .into(),
                )),
                Some(vec![
                    AuxValue::archived_value(Value::String(String::new())), // device_path = ""
                    AuxValue::archived_value(bundle_id.into()),
                    AuxValue::archived_value(Value::Dictionary(env)),
                    AuxValue::archived_value(Value::Array(args)),
                    AuxValue::archived_value(Value::Dictionary(options)),
                ]),
            )
            .await?;
        match res.data {
            Some(v) => parse_u64_value(&v).ok_or_else(|| {
                if let Some(message) = extract_ns_error_message(&v) {
                    warn!("Launch failed: {message}");
                    return IdeviceError::InternalError(message);
                }
                warn!("PID wasn't parseable: {v:?}");
                IdeviceError::UnexpectedResponse("unexpected response".into())
            }),
            _ => {
                warn!("Did not get integer response from launchSuspendedProcess");
                Err(IdeviceError::UnexpectedResponse(
                    "unexpected response".into(),
                ))
            }
        }
    }

    /// Kills a running process
    ///
    /// # Arguments
    /// * `pid` - Process ID to kill
    ///
    /// # Returns
    /// * `Ok(())` - If kill request was sent successfully
    /// * `Err(IdeviceError)` - If communication fails
    ///
    /// # Note
    /// This method doesn't wait for confirmation that the process was killed.
    pub async fn kill_app(&mut self, pid: u64) -> Result<(), IdeviceError> {
        self.channel
            .call_method(
                "killPid:".into(),
                Some(vec![AuxValue::U32(pid as u32)]),
                false,
            )
            .await?;

        Ok(())
    }

    /// Disables memory limits for a process
    ///
    /// # Arguments
    /// * `pid` - Process ID to modify
    ///
    /// # Returns
    /// * `Ok(())` - If memory limits were disabled
    /// * `Err(IdeviceError)` - If operation fails
    ///
    /// # Errors
    /// * `IdeviceError::DisableMemoryLimitFailed` if device reports failure
    /// * `IdeviceError::UnexpectedResponse("unexpected response".into())` for invalid responses
    /// * Other communication errors
    pub async fn disable_memory_limit(&mut self, pid: u64) -> Result<(), IdeviceError> {
        let res = self
            .channel
            .call_method_with_reply(
                "requestDisableMemoryLimitsForPid:".into(),
                Some(vec![AuxValue::U32(pid as u32)]),
            )
            .await?;
        match res.data {
            Some(Value::Boolean(b)) => {
                if b {
                    Ok(())
                } else {
                    warn!("Failed to disable memory limit");
                    Err(DvtError::DisableMemoryLimitFailed.into())
                }
            }
            _ => {
                warn!("Did not receive bool response");
                Err(IdeviceError::UnexpectedResponse(
                    "expected boolean in disable memory limit response".into(),
                ))
            }
        }
    }
}
