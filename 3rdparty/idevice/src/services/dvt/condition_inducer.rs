//! Condition inducer service - Simulate network/thermal conditions on the device

use plist::Value;

use super::message::AuxValue;
use super::remote_server::{Channel, RemoteServerClient};
use crate::{IdeviceError, ReadWrite, obf};

/// A condition inducer group with a list of profiles
#[derive(Debug, Clone)]
pub struct ConditionInducerGroup {
    pub identifier: String,
    pub profiles: Vec<ConditionProfile>,
}

/// A specific condition profile that can be enabled
#[derive(Debug, Clone)]
pub struct ConditionProfile {
    pub identifier: String,
    pub description: String,
}

/// Client for inducing network/thermal conditions
#[derive(Debug)]
pub struct ConditionInducerClient<'a, R: ReadWrite> {
    channel: Channel<'a, R>,
}

impl<'a, R: ReadWrite> ConditionInducerClient<'a, R> {
    pub async fn new(client: &'a mut RemoteServerClient<R>) -> Result<Self, IdeviceError> {
        let channel = client
            .make_channel(obf!(
                "com.apple.instruments.server.services.ConditionInducer"
            ))
            .await?;
        Ok(Self { channel })
    }

    /// Returns available condition inducers grouped by category
    pub async fn available_conditions(
        &mut self,
    ) -> Result<Vec<ConditionInducerGroup>, IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("availableConditionInducers".into())),
                None,
                true,
            )
            .await?;
        let msg = self.channel.read_message().await?;
        let data = msg
            .data
            .ok_or_else(|| IdeviceError::UnexpectedResponse("expected array".into()))?;
        let arr = data
            .into_array()
            .ok_or_else(|| IdeviceError::UnexpectedResponse("expected array".into()))?;

        let mut groups = Vec::new();
        for item in arr {
            if let Some(dict) = item.into_dictionary() {
                let identifier = dict
                    .get("identifier")
                    .and_then(|v| v.as_string())
                    .unwrap_or("")
                    .to_string();
                let profiles = dict
                    .get("profiles")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .filter_map(|p| {
                        let pd = p.into_dictionary()?;
                        Some(ConditionProfile {
                            identifier: pd
                                .get("identifier")
                                .and_then(|v| v.as_string())
                                .unwrap_or("")
                                .to_string(),
                            description: pd
                                .get("description")
                                .and_then(|v| v.as_string())
                                .unwrap_or("")
                                .to_string(),
                        })
                    })
                    .collect();
                groups.push(ConditionInducerGroup {
                    identifier,
                    profiles,
                });
            }
        }
        Ok(groups)
    }

    /// Enables a specific condition profile
    pub async fn enable_condition(
        &mut self,
        condition_identifier: &str,
        profile_identifier: &str,
    ) -> Result<(), IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String(
                    "enableConditionWithIdentifier:profileIdentifier:".into(),
                )),
                Some(vec![
                    AuxValue::archived_value(Value::String(condition_identifier.to_string())),
                    AuxValue::archived_value(Value::String(profile_identifier.to_string())),
                ]),
                true,
            )
            .await?;
        // Consume reply
        self.channel.read_message().await?;
        Ok(())
    }

    /// Disables the currently active condition
    pub async fn disable_condition(&mut self) -> Result<(), IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("disableActiveCondition".into())),
                None,
                false,
            )
            .await
    }
}
