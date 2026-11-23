// Jackson Coxson

#[derive(thiserror::Error, Debug, PartialEq)]
#[non_exhaustive]
#[repr(C)]
pub enum AfcError {
    Success = 0,
    UnknownError = 1,
    OpHeaderInvalid = 2,
    NoResources = 3,
    ReadError = 4,
    WriteError = 5,
    UnknownPacketType = 6,
    InvalidArg = 7,
    ObjectNotFound = 8,
    ObjectIsDir = 9,
    PermDenied = 10,
    ServiceNotConnected = 11,
    OpTimeout = 12,
    TooMuchData = 13,
    EndOfData = 14,
    OpNotSupported = 15,
    ObjectExists = 16,
    ObjectBusy = 17,
    NoSpaceLeft = 18,
    OpWouldBlock = 19,
    IoError = 20,
    OpInterrupted = 21,
    OpInProgress = 22,
    InternalError = 23,
    MuxError = 30,
    NoMem = 31,
    NotEnoughData = 32,
    DirNotEmpty = 33,
    UnknownOpcode = 34,
    InvalidMagic = 35,
    MissingAttribute = 36,
}

impl std::fmt::Display for AfcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let description = match self {
            AfcError::Success => "Success",
            AfcError::UnknownError => "Unknown error",
            AfcError::OpHeaderInvalid => "Operation header invalid",
            AfcError::NoResources => "No resources available",
            AfcError::ReadError => "Read error",
            AfcError::WriteError => "Write error",
            AfcError::UnknownPacketType => "Unknown packet type",
            AfcError::InvalidArg => "Invalid argument",
            AfcError::ObjectNotFound => "Object not found",
            AfcError::ObjectIsDir => "Object is a directory",
            AfcError::PermDenied => "Permission denied",
            AfcError::ServiceNotConnected => "Service not connected",
            AfcError::OpTimeout => "Operation timed out",
            AfcError::TooMuchData => "Too much data",
            AfcError::EndOfData => "End of data",
            AfcError::OpNotSupported => "Operation not supported",
            AfcError::ObjectExists => "Object already exists",
            AfcError::ObjectBusy => "Object is busy",
            AfcError::NoSpaceLeft => "No space left",
            AfcError::OpWouldBlock => "Operation would block",
            AfcError::IoError => "I/O error",
            AfcError::OpInterrupted => "Operation interrupted",
            AfcError::OpInProgress => "Operation in progress",
            AfcError::InternalError => "Internal error",
            AfcError::MuxError => "Multiplexer error",
            AfcError::NoMem => "Out of memory",
            AfcError::NotEnoughData => "Not enough data",
            AfcError::DirNotEmpty => "Directory not empty",
            AfcError::UnknownOpcode => "Unknown AFC opcode",
            AfcError::InvalidMagic => "Invalid AFC magic",
            AfcError::MissingAttribute => "Missing file attribute",
        };
        write!(f, "{description}")
    }
}

impl From<u64> for AfcError {
    fn from(value: u64) -> Self {
        match value {
            0 => Self::Success,
            1 => Self::UnknownError,
            2 => Self::OpHeaderInvalid,
            3 => Self::NoResources,
            4 => Self::ReadError,
            5 => Self::WriteError,
            6 => Self::UnknownPacketType,
            7 => Self::InvalidArg,
            8 => Self::ObjectNotFound,
            9 => Self::ObjectIsDir,
            10 => Self::PermDenied,
            11 => Self::ServiceNotConnected,
            12 => Self::OpTimeout,
            13 => Self::TooMuchData,
            14 => Self::EndOfData,
            15 => Self::OpNotSupported,
            16 => Self::ObjectExists,
            17 => Self::ObjectBusy,
            18 => Self::NoSpaceLeft,
            19 => Self::OpWouldBlock,
            20 => Self::IoError,
            21 => Self::OpInterrupted,
            22 => Self::OpInProgress,
            23 => Self::InternalError,
            30 => Self::MuxError,
            31 => Self::NoMem,
            32 => Self::NotEnoughData,
            33 => Self::DirNotEmpty,
            34 => Self::UnknownOpcode,
            35 => Self::InvalidMagic,
            36 => Self::MissingAttribute,
            _ => Self::UnknownError, // fallback for unknown codes
        }
    }
}

impl AfcError {
    pub fn sub_code(&self) -> i32 {
        match self {
            Self::Success => 0,
            Self::UnknownError => 1,
            Self::OpHeaderInvalid => 2,
            Self::NoResources => 3,
            Self::ReadError => 4,
            Self::WriteError => 5,
            Self::UnknownPacketType => 6,
            Self::InvalidArg => 7,
            Self::ObjectNotFound => 8,
            Self::ObjectIsDir => 9,
            Self::PermDenied => 10,
            Self::ServiceNotConnected => 11,
            Self::OpTimeout => 12,
            Self::TooMuchData => 13,
            Self::EndOfData => 14,
            Self::OpNotSupported => 15,
            Self::ObjectExists => 16,
            Self::ObjectBusy => 17,
            Self::NoSpaceLeft => 18,
            Self::OpWouldBlock => 19,
            Self::IoError => 20,
            Self::OpInterrupted => 21,
            Self::OpInProgress => 22,
            Self::InternalError => 23,
            Self::MuxError => 24,
            Self::NoMem => 25,
            Self::NotEnoughData => 26,
            Self::DirNotEmpty => 27,
            Self::UnknownOpcode => 28,
            Self::InvalidMagic => 29,
            Self::MissingAttribute => 30,
        }
    }
}
