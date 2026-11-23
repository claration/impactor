// Jackson Coxson

/// Errors specific to the XPC/HTTP2 protocol layer
#[derive(thiserror::Error, Debug)]
#[non_exhaustive]
pub enum XpcError {
    #[error("unknown http frame type: {0}")]
    UnknownFrame(u8),
    #[error("unknown http setting type: {0}")]
    UnknownHttpSetting(u16),
    #[error("uninitialized stream ID")]
    UninitializedStreamId,
    #[error("unknown XPC type: {0}")]
    UnknownXpcType(u32),
    #[error("malformed XPC message")]
    MalformedXpc,
    #[error("invalid XPC magic")]
    InvalidXpcMagic,
    #[error("unexpected XPC version")]
    UnexpectedXpcVersion,
    #[error("invalid C string")]
    InvalidCString,
    #[error("stream reset")]
    HttpStreamReset,
    #[error("go away packet received: {0}")]
    HttpGoAway(String),
}

impl XpcError {
    pub fn sub_code(&self) -> i32 {
        match self {
            Self::UnknownFrame(_) => 1,
            Self::UnknownHttpSetting(_) => 2,
            Self::UninitializedStreamId => 3,
            Self::UnknownXpcType(_) => 4,
            Self::MalformedXpc => 5,
            Self::InvalidXpcMagic => 6,
            Self::UnexpectedXpcVersion => 7,
            Self::InvalidCString => 8,
            Self::HttpStreamReset => 9,
            Self::HttpGoAway(_) => 10,
        }
    }
}
