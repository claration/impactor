// Jackson Coxson

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u64)]
pub enum AfcOpcode {
    Status = 0x00000001,
    Data = 0x00000002,          // Data
    ReadDir = 0x00000003,       // ReadDir
    ReadFile = 0x00000004,      // ReadFile
    WriteFile = 0x00000005,     // WriteFile
    WritePart = 0x00000006,     // WritePart
    Truncate = 0x00000007,      // TruncateFile
    RemovePath = 0x00000008,    // RemovePath
    MakeDir = 0x00000009,       // MakeDir
    GetFileInfo = 0x0000000a,   // GetFileInfo
    GetDevInfo = 0x0000000b,    // GetDeviceInfo
    WriteFileAtom = 0x0000000c, // WriteFileAtomic (tmp file+rename)
    FileOpen = 0x0000000d,      // FileRefOpen
    FileOpenRes = 0x0000000e,   // FileRefOpenResult
    Read = 0x0000000f,          // FileRefRead
    Write = 0x00000010,         // FileRefWrite
    FileSeek = 0x00000011,      // FileRefSeek
    FileTell = 0x00000012,      // FileRefTell
    FileTellRes = 0x00000013,   // FileRefTellResult
    FileClose = 0x00000014,     // FileRefClose
    FileSetSize = 0x00000015,   // FileRefSetFileSize (ftruncate)
    GetConInfo = 0x00000016,    // GetConnectionInfo
    SetConOptions = 0x00000017, // SetConnectionOptions
    RenamePath = 0x00000018,    // RenamePath
    SetFsBs = 0x00000019,       // SetFSBlockSize (0x800000)
    SetSocketBs = 0x0000001A,   // SetSocketBlockSize (0x800000)
    FileLock = 0x0000001B,      // FileRefLock
    MakeLink = 0x0000001C,      // MakeLink
    SetFileTime = 0x0000001E,   // Set st_mtime
    RemovePathAndContents = 0x00000022,
}

#[repr(u64)]
#[derive(Clone, Copy, Debug)]
pub enum AfcFopenMode {
    RdOnly = 0x00000001,   // r   O_RDONLY
    Rw = 0x00000002,       // r+  O_RDWR   | O_CREAT
    WrOnly = 0x00000003,   // w   O_WRONLY | O_CREAT  | O_TRUNC
    Wr = 0x00000004,       // w+  O_RDWR   | O_CREAT  | O_TRUNC
    Append = 0x00000005,   // a   O_WRONLY | O_APPEND | O_CREAT
    RdAppend = 0x00000006, // a+  O_RDWR   | O_APPEND | O_CREAT
}

#[repr(u64)]
#[derive(Clone, Copy, Debug)]
pub enum LinkType {
    Hardlink = 0x00000001,
    Symlink = 0x00000002,
}

impl TryFrom<u64> for AfcOpcode {
    type Error = ();

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            0x00000001 => Ok(Self::Status),
            0x00000002 => Ok(Self::Data),
            0x00000003 => Ok(Self::ReadDir),
            0x00000004 => Ok(Self::ReadFile),
            0x00000005 => Ok(Self::WriteFile),
            0x00000006 => Ok(Self::WritePart),
            0x00000007 => Ok(Self::Truncate),
            0x00000008 => Ok(Self::RemovePath),
            0x00000009 => Ok(Self::MakeDir),
            0x0000000a => Ok(Self::GetFileInfo),
            0x0000000b => Ok(Self::GetDevInfo),
            0x0000000c => Ok(Self::WriteFileAtom),
            0x0000000d => Ok(Self::FileOpen),
            0x0000000e => Ok(Self::FileOpenRes),
            0x0000000f => Ok(Self::Read),
            0x00000010 => Ok(Self::Write),
            0x00000011 => Ok(Self::FileSeek),
            0x00000012 => Ok(Self::FileTell),
            0x00000013 => Ok(Self::FileTellRes),
            0x00000014 => Ok(Self::FileClose),
            0x00000015 => Ok(Self::FileSetSize),
            0x00000016 => Ok(Self::GetConInfo),
            0x00000017 => Ok(Self::SetConOptions),
            0x00000018 => Ok(Self::RenamePath),
            0x00000019 => Ok(Self::SetFsBs),
            0x0000001A => Ok(Self::SetSocketBs),
            0x0000001B => Ok(Self::FileLock),
            0x0000001C => Ok(Self::MakeLink),
            0x0000001E => Ok(Self::SetFileTime),
            _ => Err(()),
        }
    }
}
