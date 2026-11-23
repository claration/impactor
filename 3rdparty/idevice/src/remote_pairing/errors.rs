// Jackson Coxson

/// Errors specific to remote pairing operations
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum RemotePairingError {
    #[error("Unknown TLV: {0}")]
    UnknownTlv(u8),
    #[error("Malformed TLV")]
    MalformedTlv,
    #[error("Pairing rejected: {0}")]
    PairingRejected(String),
    #[cfg(feature = "remote_pairing")]
    #[error("Base64 decode error")]
    Base64DecodeError(#[from] base64::DecodeError),
    #[error("Pair verify failed")]
    PairVerifyFailed,
    #[error("SRP auth failed")]
    SrpAuthFailed,
    #[cfg(feature = "remote_pairing")]
    #[error("Chacha encryption error")]
    ChachaEncryption(chacha20poly1305::Error),
}

impl RemotePairingError {
    pub fn sub_code(&self) -> i32 {
        match self {
            Self::UnknownTlv(_) => 1,
            Self::MalformedTlv => 2,
            Self::PairingRejected(_) => 3,
            #[cfg(feature = "remote_pairing")]
            Self::Base64DecodeError(_) => 4,
            Self::PairVerifyFailed => 5,
            Self::SrpAuthFailed => 6,
            #[cfg(feature = "remote_pairing")]
            Self::ChachaEncryption(_) => 7,
        }
    }
}
