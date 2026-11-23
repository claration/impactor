//! Tunneld Client Implementation
//!
//! Provides functionality for interacting with pymobiledevice3's tunneld service,
//! which creates network tunnels to iOS devices over USB.

use std::{collections::HashMap, net::SocketAddr};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::warn;

use crate::IdeviceError;

/// Default port number for the tunneld service
pub const DEFAULT_PORT: u16 = 49151;

/// Represents a device connected through tunneld
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunneldDevice {
    /// Network interface name
    pub interface: String,
    /// Tunnel IP address
    #[serde(rename = "tunnel-address")]
    pub tunnel_address: String,
    /// Tunnel port number
    #[serde(rename = "tunnel-port")]
    pub tunnel_port: u16,
}

/// Retrieves all devices currently connected through tunneld
///
/// # Arguments
/// * `socket` - Socket address of the tunneld service (typically localhost with DEFAULT_PORT)
///
/// # Returns
/// A HashMap mapping device UDIDs to their tunnel information
///
/// # Errors
/// Returns `IdeviceError` if:
/// - The HTTP request fails
/// - The response format is invalid
/// - JSON parsing fails
///
/// # Example
/// ```rust
/// let host = SocketAddr::new(IpAddr::from_str("127.0.0.1").unwrap(), DEFAULT_PORT);
/// let devices = get_tunneld_devices(host).await?;
/// for (udid, device) in devices {
///     println!("Device {} is available at {}:{}",
///         udid, device.tunnel_address, device.tunnel_port);
/// }
/// ```
pub async fn get_tunneld_devices(
    socket: SocketAddr,
) -> Result<HashMap<String, TunneldDevice>, IdeviceError> {
    // Make HTTP GET request to tunneld endpoint
    let res: Value = reqwest::get(format!("http://{socket}"))
        .await?
        .json()
        .await?;

    // Verify response is a JSON object
    let res = match res.as_object() {
        Some(r) => r,
        None => {
            warn!("tunneld return type wasn't a dictionary");
            return Err(IdeviceError::UnexpectedResponse(
                "tunneld response was not a JSON object".into(),
            ));
        }
    };

    // Parse each device entry
    let mut to_return = HashMap::new();
    for (udid, v) in res.into_iter() {
        let mut v: Vec<TunneldDevice> = match serde_json::from_value(v.clone()) {
            Ok(v) => v,
            Err(e) => {
                warn!("Failed to parse tunneld results as vector of struct: {e:?}");
                continue;
            }
        };

        if v.is_empty() {
            warn!("Device had no entries");
            continue;
        }

        to_return.insert(udid.clone(), v.remove(0));
    }

    Ok(to_return)
}

#[cfg(test)]
mod tests {
    use std::{net::IpAddr, str::FromStr};

    use super::*;

    /// Test case for verifying tunneld device listing
    #[tokio::test]
    async fn test_get_tunneld_devices() {
        let host = SocketAddr::new(IpAddr::from_str("127.0.0.1").unwrap(), DEFAULT_PORT);
        match get_tunneld_devices(host).await {
            Ok(devices) => println!("Found tunneld devices: {devices:#?}"),
            Err(e) => println!("Error querying tunneld: {e}"),
        }
    }
}
