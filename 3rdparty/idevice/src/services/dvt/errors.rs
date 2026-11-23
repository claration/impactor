// Jackson Coxson

/// Errors specific to the DVT (Developer Tools) protocol
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum DvtError {
    #[error("NSKeyedArchive error")]
    NsKeyedArchiveError(#[from] ns_keyed_archive::ConverterError),
    #[error("Unknown aux value type: {0}")]
    UnknownAuxValueType(u32),
    #[error("unknown channel: {0}")]
    UnknownChannel(u32),
    #[error("disable memory limit failed")]
    DisableMemoryLimitFailed,
}

impl DvtError {
    pub fn sub_code(&self) -> i32 {
        match self {
            Self::NsKeyedArchiveError(_) => 1,
            Self::UnknownAuxValueType(_) => 2,
            Self::UnknownChannel(_) => 3,
            Self::DisableMemoryLimitFailed => 4,
        }
    }
}
