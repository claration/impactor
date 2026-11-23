//! Ticket Signature Server (TSS) Client
//!
//! Provides functionality for interacting with Apple's TSS service to:
//! - Request personalized firmware components
//! - Apply restore request rules for device-specific parameters
//! - Handle cryptographic signing operations

use plist::Value;
use plist_macro::plist_to_xml_bytes;
use tracing::{debug, warn};

use crate::IdeviceError;

/// TSS client version string sent in requests
const TSS_CLIENT_VERSION_STRING: &str = "libauthinstall-1033.0.2";
/// Apple's TSS endpoint URL
const TSS_CONTROLLER_ACTION_URL: &str = "http://gs.apple.com/TSS/controller?action=2";

/// Represents a TSS request to Apple's signing server
#[derive(Debug)]
pub struct TSSRequest {
    /// The underlying plist dictionary containing request parameters
    inner: plist::Dictionary,
}

impl TSSRequest {
    /// Creates a new TSS request with default headers
    ///
    /// Initializes with:
    /// - Host platform info
    /// - Client version string
    /// - Random UUID for request identification
    pub fn new() -> Self {
        let inner = plist_macro::plist!(dict {
            "@HostPlatformInfo": "mac",
            "@VersionInfo": TSS_CLIENT_VERSION_STRING,
            "@UUID": uuid::Uuid::new_v4().to_string().to_uppercase()
        });
        Self { inner }
    }

    /// Inserts a key-value pair into the TSS request
    ///
    /// # Arguments
    /// * `key` - The parameter name
    /// * `val` - The parameter value (will be converted to plist::Value)
    pub fn insert(&mut self, key: impl Into<String>, val: impl Into<Value>) {
        let key = key.into();
        let val = val.into();
        self.inner.insert(key, val);
    }

    /// Sends the TSS request to Apple's servers
    ///
    /// # Returns
    /// The parsed plist response from Apple
    ///
    /// # Errors
    /// Returns `IdeviceError` if:
    /// - The request fails
    /// - The response is malformed
    /// - Apple returns a non-success status
    ///
    /// # Example
    /// ```rust
    /// let mut request = TSSRequest::new();
    /// request.insert("ApBoardID", board_id);
    /// request.insert("ApChipID", chip_id);
    /// let response = request.send().await?;
    /// ```
    pub async fn send(&self) -> Result<plist::Value, IdeviceError> {
        debug!(
            "Sending TSS request: {}",
            crate::pretty_print_dictionary(&self.inner)
        );
        let client = reqwest::Client::new();

        let res = client
            .post(TSS_CONTROLLER_ACTION_URL)
            .header("Cache-Control", "no-cache")
            .header("Content-type", "text/xml; charset=\"utf-8\"")
            .header("User-Agent", "InetURL/1.0")
            .body(plist_to_xml_bytes(&self.inner))
            .send()
            .await?
            .text()
            .await?;

        debug!("Apple responded with {res}");
        let res = res.trim_start_matches("STATUS=0&");
        let res = res.trim_start_matches("MESSAGE=");
        if !res.starts_with("SUCCESS") {
            warn!("TSS responded with non-success value");
            return Err(IdeviceError::UnexpectedResponse(
                "TSS server responded with non-success status".into(),
            ));
        }
        let res = res.split("REQUEST_STRING=").collect::<Vec<&str>>();
        if res.len() < 2 {
            warn!("Response didn't contain a request string");
            return Err(IdeviceError::UnexpectedResponse(
                "TSS response missing REQUEST_STRING".into(),
            ));
        }
        Ok(plist::from_bytes(res[1].as_bytes())?)
    }
}

impl Default for TSSRequest {
    /// Creates a default TSS request (same as `new()`)
    fn default() -> Self {
        Self::new()
    }
}

/// Applies restore request rules to modify input parameters
///
/// # Arguments
/// * `input` - The dictionary to modify based on rules
/// * `parameters` - Device parameters to check conditions against
/// * `rules` - List of rules to apply
///
/// # Process
/// For each rule:
/// 1. Checks all conditions against the parameters
/// 2. If all conditions are met, applies the rule's actions
/// 3. Actions can add, modify or remove parameters
pub fn apply_restore_request_rules(
    input: &mut plist::Dictionary,
    parameters: &plist::Dictionary,
    rules: &Vec<plist::Value>,
) {
    for rule in rules {
        if let plist::Value::Dictionary(rule) = rule {
            let conditions = match rule.get("Conditions") {
                Some(plist::Value::Dictionary(c)) => c,
                _ => {
                    warn!("Conditions doesn't exist or wasn't a dictionary!");
                    continue;
                }
            };

            let mut conditions_fulfilled = true;
            for (key, value) in conditions {
                let value2 = match key.as_str() {
                    "ApRawProductionMode" => parameters.get("ApProductionMode"),
                    "ApCurrentProductionMode" => parameters.get("ApProductionMode"),
                    "ApRawSecurityMode" => parameters.get("ApSecurityMode"),
                    "ApRequiresImage4" => parameters.get("ApSupportsImg4"),
                    "ApDemotionPolicyOverride" => parameters.get("DemotionPolicy"),
                    "ApInRomDFU" => parameters.get("ApInRomDFU"),
                    _ => {
                        warn!("Unhandled key {key}");
                        None
                    }
                };

                if value2.is_none() || value2 != Some(value) {
                    conditions_fulfilled = false;
                    break; // Stop checking other conditions immediately
                }
            }

            if !conditions_fulfilled {
                continue;
            }

            let actions = match rule.get("Actions") {
                Some(plist::Value::Dictionary(a)) => a,
                _ => {
                    warn!("Actions doesn't exist or wasn't a dictionary!");
                    continue;
                }
            };

            for (key, value) in actions {
                // Skip special values (255 typically means "ignore")
                if let Some(i) = value.as_unsigned_integer()
                    && i == 255
                {
                    continue;
                }
                if let Some(i) = value.as_signed_integer()
                    && i == 255
                {
                    continue;
                }

                input.remove(key); // Explicitly remove before inserting
                input.insert(key.to_owned(), value.to_owned());
            }
        } else {
            warn!("Rule wasn't a dictionary");
        }
    }
}
