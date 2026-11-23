//! Energy Monitor service client for iOS instruments protocol

use crate::{
    IdeviceError, ReadWrite,
    dvt::message::AuxValue,
    dvt::remote_server::{Channel, RemoteServerClient},
    obf,
};
use plist::{Dictionary, Value};

#[derive(Debug)]
pub struct EnergyMonitorClient<'a, R: ReadWrite> {
    channel: Channel<'a, R>,
}

impl<'a, R: ReadWrite> EnergyMonitorClient<'a, R> {
    pub async fn new(client: &'a mut RemoteServerClient<R>) -> Result<Self, IdeviceError> {
        let channel = client
            .make_channel(obf!("com.apple.xcode.debug-gauge-data-providers.Energy"))
            .await?;
        Ok(Self { channel })
    }

    pub async fn start_sampling(&mut self, pids: &[u32]) -> Result<(), IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("startSamplingForPIDs:".into())),
                Some(vec![Self::archive_pids(pids)]),
                false,
            )
            .await
    }

    pub async fn stop_sampling(&mut self, pids: &[u32]) -> Result<(), IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("stopSamplingForPIDs:".into())),
                Some(vec![Self::archive_pids(pids)]),
                false,
            )
            .await
    }

    /// Returns raw NSKeyedArchive bytes for the response, since the response uses
    /// NSDictionary with NSNumber (integer) keys which plist::Dictionary doesn't support.
    pub async fn sample_attributes(&mut self, pids: &[u32]) -> Result<Vec<u8>, IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("sampleAttributes:forPIDs:".into())),
                Some(vec![
                    AuxValue::archived_value(Value::Dictionary(Dictionary::new())),
                    Self::archive_pids(pids),
                ]),
                true,
            )
            .await?;
        let msg = self.channel.read_message().await?;
        msg.raw_data
            .ok_or_else(|| IdeviceError::UnexpectedResponse("expected energy data bytes".into()))
    }

    fn archive_pids(pids: &[u32]) -> AuxValue {
        AuxValue::archived_value(Value::Array(
            pids.iter()
                .map(|p| Value::Integer((*p as i64).into()))
                .collect(),
        ))
    }
}

/// Parse an NSKeyedArchive binary plist that may contain NSDictionary with integer keys.
/// Returns a plist::Value::Dictionary where integer keys are converted to strings.
pub fn decode_nka_int_keys(bytes: &[u8]) -> Result<Value, IdeviceError> {
    let archive = plist::Value::from_reader(std::io::Cursor::new(bytes))
        .map_err(|e| IdeviceError::UnexpectedResponse(format!("plist parse error: {e}")))?;

    let dict = archive.as_dictionary().ok_or_else(|| {
        IdeviceError::UnexpectedResponse("NSKeyedArchive root is not a dict".into())
    })?;

    let objects = dict
        .get("$objects")
        .and_then(|v| v.as_array())
        .ok_or_else(|| IdeviceError::UnexpectedResponse("missing $objects".into()))?;

    let top = dict
        .get("$top")
        .and_then(|v| v.as_dictionary())
        .ok_or_else(|| IdeviceError::UnexpectedResponse("missing $top".into()))?;

    let root_uid = top
        .get("root")
        .and_then(uid_index)
        .ok_or_else(|| IdeviceError::UnexpectedResponse("missing $top.root UID".into()))?;

    decode_nka_object(objects, root_uid)
}

fn uid_index(v: &Value) -> Option<usize> {
    match v {
        Value::Uid(uid) => Some(uid.get() as usize),
        _ => None,
    }
}

fn decode_nka_object(objects: &[Value], idx: usize) -> Result<Value, IdeviceError> {
    let obj = objects.get(idx).ok_or_else(|| {
        IdeviceError::UnexpectedResponse(format!("UID index {idx} out of bounds"))
    })?;

    match obj {
        Value::String(s) if s == "$null" => Ok(Value::Dictionary(Dictionary::new())),
        Value::Dictionary(d) => {
            let class_uid = d.get("$class").and_then(uid_index);
            if let Some(cuid) = class_uid {
                let class_obj = objects.get(cuid).and_then(|v| v.as_dictionary());
                let class_name = class_obj
                    .and_then(|d| d.get("$classname"))
                    .and_then(|v| v.as_string())
                    .unwrap_or("");

                match class_name {
                    // Match both public and internal iOS class names
                    s if s.contains("Dictionary") => {
                        let keys = d.get("NS.keys").and_then(|v| v.as_array());
                        let vals = d.get("NS.objects").and_then(|v| v.as_array());
                        if let (Some(keys), Some(vals)) = (keys, vals) {
                            let mut result = Dictionary::new();
                            for (k, v) in keys.iter().zip(vals.iter()) {
                                if let (Some(ki), Some(vi)) = (uid_index(k), uid_index(v)) {
                                    let key_val = decode_nka_object(objects, ki)?;
                                    let val_val = decode_nka_object(objects, vi)?;
                                    let key_str = match &key_val {
                                        Value::String(s) => s.clone(),
                                        Value::Integer(i) => i.to_string(),
                                        Value::Real(f) => f.to_string(),
                                        _ => format!("{key_val:?}"),
                                    };
                                    result.insert(key_str, val_val);
                                }
                            }
                            return Ok(Value::Dictionary(result));
                        }
                        Ok(Value::Dictionary(Dictionary::new()))
                    }
                    // NSSet/NSMutableSet also uses NS.objects (unordered, treat as array)
                    s if s.contains("Array") || s.contains("Set") => {
                        let items = d.get("NS.objects").and_then(|v| v.as_array());
                        if let Some(items) = items {
                            let mut result = Vec::new();
                            for item in items {
                                if let Some(i) = uid_index(item) {
                                    result.push(decode_nka_object(objects, i)?);
                                }
                            }
                            return Ok(Value::Array(result));
                        }
                        Ok(Value::Array(Vec::new()))
                    }
                    _ => Ok(obj.clone()),
                }
            } else {
                Ok(obj.clone())
            }
        }
        Value::Uid(uid) => decode_nka_object(objects, uid.get() as usize),
        other => Ok(other.clone()),
    }
}

#[derive(Debug, Clone, Copy)]
pub struct EnergySample {
    pub pid: u32,
    pub timestamp: i64,
    pub total_energy: f64,
    pub cpu_energy: f64,
    pub gpu_energy: f64,
    pub networking_energy: f64,
    pub display_energy: f64,
    pub location_energy: f64,
    pub appstate_energy: f64,
}

impl EnergySample {
    /// Parse energy samples from the raw NSKeyedArchive bytes returned by sample_attributes.
    /// The response is NSDictionary<NSNumber(pid), NSDictionary<String, id>>.
    pub fn from_bytes(bytes: &[u8]) -> Result<Vec<Self>, IdeviceError> {
        let value = decode_nka_int_keys(bytes)?;
        let outer = value.as_dictionary().ok_or_else(|| {
            IdeviceError::UnexpectedResponse("energy response is not a dict".into())
        })?;

        let mut samples = Vec::new();
        for (pid_str, inner_val) in outer {
            let pid: u32 = pid_str.parse().unwrap_or(0);
            let inner = match inner_val.as_dictionary() {
                Some(d) => d,
                None => continue,
            };

            let get_f64 = |key: &str| -> f64 {
                inner
                    .get(key)
                    .and_then(|v| match v {
                        Value::Real(f) => Some(*f),
                        Value::Integer(i) => i.as_signed().map(|i| i as f64),
                        _ => None,
                    })
                    .unwrap_or(0.0)
            };

            let get_i64 = |key: &str| -> i64 {
                inner
                    .get(key)
                    .and_then(|v| match v {
                        Value::Integer(i) => i.as_signed(),
                        Value::Real(f) => Some(*f as i64),
                        _ => None,
                    })
                    .unwrap_or(0)
            };

            samples.push(EnergySample {
                pid,
                timestamp: get_i64("kIDEGaugeSecondsSinceInitialQueryKey"),
                total_energy: get_f64("energy.cost"),
                cpu_energy: get_f64("energy.cpu.cost"),
                gpu_energy: get_f64("energy.gpu.cost"),
                networking_energy: get_f64("energy.networking.cost"),
                display_energy: get_f64("energy.display.cost"),
                location_energy: get_f64("energy.location.cost"),
                appstate_energy: get_f64("energy.appstate.cost"),
            });
        }

        Ok(samples)
    }
}
