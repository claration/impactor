use std::hash::Hasher;

use base64::Engine;
use siphasher::sip::SipHasher;

use crate::IdeviceError;

use super::{opack, tlv};

#[derive(Debug, Clone)]
pub struct PeerDevice {
    /// peer identifier, same as the identifier returned in the `verifyManualPairing` response
    pub account_id: String,
    /// altIRK: 16-byte
    pub alt_irk: Vec<u8>,
    /// Device's model identifier, e.g. "iPhone14,4"
    pub model: String,
    /// Device's name
    pub name: String,
    /// Device's Unique Device Identifier
    pub remotepairing_udid: String,
}

impl PeerDevice {
    /// Validates a `_remotepairing._tcp` mDNS `authTag`.
    ///
    /// - `alt_irk`: 16-byte mDNS identity key (`alt_irk`) stored in the pairing file
    /// - `service_identifier`: the service identifier from the mDNS TXT record
    /// - `auth_tag`: 6-byte auth tag from the mDNS TXT record
    ///
    /// Computes `SipHash-2-4(key=alt_irk, msg=service_identifier)` and compares the
    /// first 6 bytes of the 8-byte LE output (reversed) against `auth_tag`.
    pub fn validate_auth_tag(alt_irk: &[u8], service_identifier: &str, auth_tag: &str) -> bool {
        let bytes = match base64::engine::general_purpose::STANDARD.decode(auth_tag) {
            Ok(b) => b,
            Err(_) => return false,
        };
        if bytes.len() != 6 {
            return false;
        }
        let Ok(alt_irk) = <&[u8; 16]>::try_from(alt_irk) else {
            return false;
        };
        compute_auth_tag(alt_irk, service_identifier) == bytes.as_slice()
    }

    pub fn try_from_info_dictionary(dict: &plist::Dictionary) -> Result<Self, IdeviceError> {
        let alt_irk = required_data_field(dict, "altIRK")?;
        if alt_irk.len() != 16 {
            return Err(IdeviceError::UnexpectedResponse(format!(
                "invalid altIRK length in peer device info: expected 16 bytes, got {}",
                alt_irk.len()
            )));
        }

        Ok(Self {
            account_id: required_string_field(dict, "accountID")?,
            alt_irk,
            model: required_string_field(dict, "model")?,
            name: required_string_field(dict, "name")?,
            remotepairing_udid: required_string_field(dict, "remotepairing_udid")?,
        })
    }
}

fn required_string_field(dict: &plist::Dictionary, key: &str) -> Result<String, IdeviceError> {
    dict.get(key)
        .and_then(|value| value.as_string())
        .map(str::to_string)
        .ok_or(IdeviceError::UnexpectedResponse(format!(
            "missing string field `{key}` in peer device info"
        )))
}

fn required_data_field(dict: &plist::Dictionary, key: &str) -> Result<Vec<u8>, IdeviceError> {
    dict.get(key)
        .and_then(|value| value.as_data())
        .map(|value| value.to_vec())
        .ok_or(IdeviceError::UnexpectedResponse(format!(
            "missing data field `{key}` in peer device info"
        )))
}

fn parse_info_dictionary_from_tlv(
    entries: &[tlv::TLV8Entry],
) -> Result<plist::Dictionary, IdeviceError> {
    if tlv::contains_component(entries, tlv::PairingDataComponentType::ErrorResponse) {
        return Err(IdeviceError::UnexpectedResponse(
            "TLV error response in pair record save".into(),
        ));
    }

    let info = tlv::collect_component_data(entries, tlv::PairingDataComponentType::Info);
    if info.is_empty() {
        return Err(IdeviceError::UnexpectedResponse(
            "missing info payload in pair record response".into(),
        ));
    }

    let info_plist = opack::opack_to_plist(&info).map_err(|e| {
        IdeviceError::UnexpectedResponse(format!(
            "failed to parse OPACK info payload from pair record response: {e}"
        ))
    })?;

    let info_dict = info_plist
        .as_dictionary()
        .ok_or(IdeviceError::UnexpectedResponse(
            "info OPACK payload is not a dictionary".into(),
        ))?;

    Ok(info_dict.to_owned())
}

pub(super) fn parse_peer_device_from_tlv(
    entries: &[tlv::TLV8Entry],
) -> Result<PeerDevice, IdeviceError> {
    let info = parse_info_dictionary_from_tlv(entries)?;
    PeerDevice::try_from_info_dictionary(&info)
}

/// Computes the 6-byte mDNS `authTag` for the given `alt_irk` and `service_identifier`.
///
/// Algorithm: `SipHash-2-4(key=alt_irk, msg=service_identifier)` → take 8-byte LE output,
/// return the first 6 bytes in **reverse** order.
pub(super) fn compute_auth_tag(alt_irk: &[u8; 16], service_identifier: &str) -> [u8; 6] {
    let k0 = u64::from_le_bytes(alt_irk[..8].try_into().unwrap());
    let k1 = u64::from_le_bytes(alt_irk[8..16].try_into().unwrap());
    let mut sip = SipHasher::new_with_keys(k0, k1);
    sip.write(service_identifier.as_bytes());
    let output = sip.finish().to_le_bytes();
    let mut tag = [0u8; 6];
    for i in 0..6 {
        tag[i] = output[5 - i];
    }
    tag
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_peer_device_dictionary() -> plist::Dictionary {
        let mut dict = plist::Dictionary::new();
        dict.insert(
            "accountID".into(),
            plist::Value::String("test-account".into()),
        );
        dict.insert("altIRK".into(), plist::Value::Data(vec![0xAB; 16]));
        dict.insert("model".into(), plist::Value::String("AppleTV11,1".into()));
        dict.insert("name".into(), plist::Value::String("Living Room".into()));
        dict.insert(
            "remotepairing_udid".into(),
            plist::Value::String("00008110-001A2B3C00000000".into()),
        );
        dict
    }

    #[test]
    fn peer_device_requires_all_fields() {
        let mut dict = sample_peer_device_dictionary();
        dict.remove("model");

        let err = PeerDevice::try_from_info_dictionary(&dict).unwrap_err();

        assert!(
            matches!(err, IdeviceError::UnexpectedResponse(message) if message.contains("model"))
        );
    }

    #[test]
    fn peer_device_parses_required_fields() {
        let dict = sample_peer_device_dictionary();

        let peer_device = PeerDevice::try_from_info_dictionary(&dict).unwrap();

        assert_eq!(peer_device.account_id, "test-account");
        assert_eq!(peer_device.alt_irk, vec![0xAB; 16]);
        assert_eq!(peer_device.model, "AppleTV11,1");
        assert_eq!(peer_device.name, "Living Room");
        assert_eq!(peer_device.remotepairing_udid, "00008110-001A2B3C00000000");
    }

    #[test]
    fn parse_peer_device_from_tlv_reads_info_payload() {
        let info = plist::Value::Dictionary(sample_peer_device_dictionary());
        let entries = vec![tlv::TLV8Entry {
            tlv_type: tlv::PairingDataComponentType::Info,
            data: opack::plist_to_opack(&info),
        }];

        let peer_device = parse_peer_device_from_tlv(&entries).unwrap();

        assert_eq!(peer_device.name, "Living Room");
        assert_eq!(peer_device.alt_irk.len(), 16);
    }

    #[test]
    fn validate_auth_tag_returns_true_for_correct_tag() {
        let alt_irk = base64::engine::general_purpose::STANDARD
            .decode("Mgp6ZGPzXM2ku9br46vsiw==")
            .unwrap();
        let service_identifier = "2BE6E510-0325-4365-923E-B14C6F57DB3A";
        let auth_tag = "kXjlTr2l";
        assert!(PeerDevice::validate_auth_tag(
            &alt_irk,
            service_identifier,
            auth_tag
        ));
    }
}
