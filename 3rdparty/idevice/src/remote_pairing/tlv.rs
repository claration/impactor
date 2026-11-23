// Jackson Coxson

use super::errors::RemotePairingError;

// from pym3
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PairingDataComponentType {
    Method = 0x00,
    Identifier = 0x01,
    Salt = 0x02,
    PublicKey = 0x03,
    Proof = 0x04,
    EncryptedData = 0x05,
    State = 0x06,
    ErrorResponse = 0x07,
    RetryDelay = 0x08,
    Certificate = 0x09,
    Signature = 0x0a,
    Permissions = 0x0b,
    FragmentData = 0x0c,
    FragmentLast = 0x0d,
    SessionId = 0x0e,
    Ttl = 0x0f,
    ExtraData = 0x10,
    Info = 0x11,
    Acl = 0x12,
    Flags = 0x13,
    ValidationData = 0x14,
    MfiAuthToken = 0x15,
    MfiProductType = 0x16,
    SerialNumber = 0x17,
    MfiAuthTokenUuid = 0x18,
    AppFlags = 0x19,
    OwnershipProof = 0x1a,
    SetupCodeType = 0x1b,
    ProductionData = 0x1c,
    AppInfo = 0x1d,
    Separator = 0xff,
}

#[derive(Debug, Clone)]
pub struct TLV8Entry {
    pub tlv_type: PairingDataComponentType,
    pub data: Vec<u8>,
}

pub fn collect_component_data(
    entries: &[TLV8Entry],
    component: PairingDataComponentType,
) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in entries {
        if entry.tlv_type == component {
            out.extend_from_slice(&entry.data);
        }
    }
    out
}

pub fn contains_component(entries: &[TLV8Entry], component: PairingDataComponentType) -> bool {
    entries.iter().any(|entry| entry.tlv_type == component)
}

pub fn serialize_tlv8(entries: &[TLV8Entry]) -> Vec<u8> {
    let mut out = Vec::new();
    for entry in entries {
        out.push(entry.tlv_type as u8);
        out.push(entry.data.len() as u8);
        out.extend(&entry.data);
    }
    out
}

pub fn deserialize_tlv8(input: &[u8]) -> Result<Vec<TLV8Entry>, RemotePairingError> {
    let mut index = 0;
    let mut result = Vec::new();

    while index + 2 <= input.len() {
        let type_byte = input[index];
        let length = input[index + 1] as usize;
        index += 2;

        if index + length > input.len() {
            return Err(RemotePairingError::MalformedTlv);
        }

        let data = input[index..index + length].to_vec();
        index += length;

        let tlv_type = PairingDataComponentType::try_from(type_byte)
            .map_err(|_| RemotePairingError::UnknownTlv(type_byte))?;

        result.push(TLV8Entry { tlv_type, data });
    }

    Ok(result)
}

impl TryFrom<u8> for PairingDataComponentType {
    type Error = u8;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        use PairingDataComponentType::*;
        Ok(match value {
            0x00 => Method,
            0x01 => Identifier,
            0x02 => Salt,
            0x03 => PublicKey,
            0x04 => Proof,
            0x05 => EncryptedData,
            0x06 => State,
            0x07 => ErrorResponse,
            0x08 => RetryDelay,
            0x09 => Certificate,
            0x0a => Signature,
            0x0b => Permissions,
            0x0c => FragmentData,
            0x0d => FragmentLast,
            0x0e => SessionId,
            0x0f => Ttl,
            0x10 => ExtraData,
            0x11 => Info,
            0x12 => Acl,
            0x13 => Flags,
            0x14 => ValidationData,
            0x15 => MfiAuthToken,
            0x16 => MfiProductType,
            0x17 => SerialNumber,
            0x18 => MfiAuthTokenUuid,
            0x19 => AppFlags,
            0x1a => OwnershipProof,
            0x1b => SetupCodeType,
            0x1c => ProductionData,
            0x1d => AppInfo,
            0xff => Separator,
            other => return Err(other),
        })
    }
}
