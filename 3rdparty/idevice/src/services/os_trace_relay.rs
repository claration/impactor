//! iOS Device OsTraceRelay Service Abstraction
//! Note that there are unknown fields that will hopefully be filled in the future.
//! Huge thanks to pymobiledevice3 for the struct implementation
//! https://github.com/doronz88/pymobiledevice3/blob/master/pymobiledevice3/services/os_trace.py

use chrono::{DateTime, NaiveDateTime};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::{Idevice, IdeviceError, IdeviceService, obf};

/// Client for interacting with the iOS device OsTraceRelay service
#[derive(Debug)]
pub struct OsTraceRelayClient {
    /// The underlying device connection with established OsTraceRelay service
    pub idevice: Idevice,
}

impl IdeviceService for OsTraceRelayClient {
    /// Returns the OsTraceRelay service name as registered with lockdownd
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.os_trace_relay")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self { idevice })
    }
}

/// An initialized client for receiving logs
#[derive(Debug)]
pub struct OsTraceRelayReceiver {
    inner: OsTraceRelayClient,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OsTraceLog {
    pub pid: u32,
    pub timestamp: NaiveDateTime,
    pub level: LogLevel,
    pub image_name: String,
    pub filename: String,
    pub message: String,
    pub label: Option<SyslogLabel>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyslogLabel {
    pub subsystem: String,
    pub category: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Copy)]
pub enum LogLevel {
    Notice = 0,
    Info = 1,
    Debug = 2,
    Error = 10,
    Fault = 11,
}

impl OsTraceRelayClient {
    /// Starts the stream of logs from the relay
    ///
    /// # Arguments
    /// * `pid` - An optional pid to stream logs from
    pub async fn start_trace(
        mut self,
        pid: Option<u32>,
    ) -> Result<OsTraceRelayReceiver, IdeviceError> {
        let pid = match pid {
            Some(p) => p as i64,
            None => -1,
        };
        let req = crate::plist!({
            "Request": "StartActivity",
            "Pid": pid,
            "MessageFilter": 65_535,
            "StreamFlags": 60
        });

        self.idevice.send_bplist(req).await?;

        // Read a single byte
        self.idevice.read_raw(1).await?;

        // Result
        let res = self.idevice.read_plist().await?;

        match res.get("Status").and_then(|x| x.as_string()) {
            Some(r) => {
                if r == "RequestSuccessful" {
                    Ok(OsTraceRelayReceiver { inner: self })
                } else {
                    Err(IdeviceError::UnexpectedResponse(
                        "Status was not RequestSuccessful in StartActivity response".into(),
                    ))
                }
            }
            None => Err(IdeviceError::UnexpectedResponse(
                "missing Status in StartActivity response".into(),
            )),
        }
    }

    /// Get the list of available PIDs
    pub async fn get_pid_list(&mut self) -> Result<Vec<u64>, IdeviceError> {
        let req = crate::plist!({
            "Request": "PidList"
        });

        self.idevice.send_bplist(req).await?;

        // Read a single byte
        self.idevice.read_raw(1).await?;

        // Result
        let res = self.idevice.read_plist().await?;

        // Device returns { "Payload": { "<pid>": { "ProcessName": "..." }, ... } }
        // where the PIDs are the string keys of the Payload dictionary.
        if let Some(payload) = res.get("Payload").and_then(|x| x.as_dictionary()) {
            payload
                .keys()
                .map(|k| {
                    k.parse::<u64>().map_err(|_| {
                        IdeviceError::UnexpectedResponse(format!(
                            "PidList Payload key is not a valid PID: {k}"
                        ))
                    })
                })
                .collect()
        } else {
            Err(IdeviceError::UnexpectedResponse(
                "missing Payload dictionary in PidList response".into(),
            ))
        }
    }

    /// Create a log archive and write it to the provided writer
    pub async fn create_archive<W: tokio::io::AsyncWrite + Unpin>(
        &mut self,
        out: &mut W,
        size_limit: Option<u64>,
        age_limit: Option<u64>,
        start_time: Option<u64>,
    ) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "Request": "CreateArchive",
            "SizeLimit":? size_limit,
            "AgeLimit":? age_limit,
            "StartTime":? start_time,
        });

        self.idevice.send_bplist(req).await?;

        // Read a single byte
        if self.idevice.read_raw(1).await?[0] != 1 {
            return Err(IdeviceError::UnexpectedResponse(
                "expected leading byte 0x01 in CreateArchive response".into(),
            ));
        }

        // Check status
        let res = self.idevice.read_plist().await?;
        match res.get("Status").and_then(|x| x.as_string()) {
            Some("RequestSuccessful") => {}
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "Status was not RequestSuccessful in CreateArchive response".into(),
                ));
            }
        }

        // Read archive data
        loop {
            match self.idevice.read_raw(1).await {
                Ok(data) if data[0] == 0x03 => {
                    let length_bytes = self.idevice.read_raw(4).await?;
                    let length = u32::from_le_bytes([
                        length_bytes[0],
                        length_bytes[1],
                        length_bytes[2],
                        length_bytes[3],
                    ]);
                    let data = self.idevice.read_raw(length as usize).await?;
                    out.write_all(&data).await?;
                }
                Err(IdeviceError::Socket(_)) => break,
                _ => {
                    return Err(IdeviceError::UnexpectedResponse(
                        "unexpected data format in archive stream".into(),
                    ));
                }
            }
        }

        Ok(())
    }
}

impl OsTraceRelayReceiver {
    /// Get the next log from the relay
    ///
    /// # Returns
    /// A string containing the log
    ///
    /// # Errors
    /// UnexpectedResponse if the service sends an EOF
    pub async fn next(&mut self) -> Result<OsTraceLog, IdeviceError> {
        // Read 0x02, at the beginning of each packet
        if self.inner.idevice.read_raw(1).await?[0] != 0x02 {
            return Err(IdeviceError::UnexpectedResponse(
                "expected leading byte 0x02 at start of log packet".into(),
            ));
        }

        // Read the len of the packet
        let pl = self.inner.idevice.read_raw(4).await?;
        let packet_length = u32::from_le_bytes([pl[0], pl[1], pl[2], pl[3]]);

        let packet = self.inner.idevice.read_raw(packet_length as usize).await?;

        // 9 bytes of padding
        let packet = &packet[9..];

        // Parse PID (4 bytes)
        let pid = u32::from_le_bytes([packet[0], packet[1], packet[2], packet[3]]);
        let packet = &packet[4..];

        // Skip 42 unknown bytes
        let packet = &packet[42..];

        // Parse timestamp (seconds + microseconds)
        let seconds = u32::from_le_bytes([packet[0], packet[1], packet[2], packet[3]]);
        let packet = &packet[8..]; // skip 4 bytes padding after seconds
        let microseconds = u32::from_le_bytes([packet[0], packet[1], packet[2], packet[3]]);
        let packet = &packet[4..];

        // Skip 1 byte padding
        let packet = &packet[1..];

        // Parse log level
        let log_level = packet[0];
        let log_level: LogLevel = log_level.try_into()?;
        let packet = &packet[1..];

        // Skip 38 unknown bytes
        let packet = &packet[38..];

        // Parse string sizes
        let image_name_size = u16::from_le_bytes([packet[0], packet[1]]) as usize;
        let packet = &packet[2..];
        let message_size = u16::from_le_bytes([packet[0], packet[1]]) as usize;
        let packet = &packet[2..];

        // Skip 6 bytes
        let packet = &packet[6..];

        // Parse subsystem and category sizes
        let subsystem_size =
            u32::from_le_bytes([packet[0], packet[1], packet[2], packet[3]]) as usize;
        let packet = &packet[4..];
        let category_size =
            u32::from_le_bytes([packet[0], packet[1], packet[2], packet[3]]) as usize;
        let packet = &packet[4..];

        // Skip 4 bytes
        let packet = &packet[4..];

        // Parse filename (null-terminated string)
        let filename_end =
            packet
                .iter()
                .position(|&b| b == 0)
                .ok_or(IdeviceError::UnexpectedResponse(
                    "filename not null-terminated in log packet".into(),
                ))?;
        let filename = String::from_utf8_lossy(&packet[..filename_end]).into_owned();
        let packet = &packet[filename_end + 1..];

        // Parse image name
        let image_name_bytes = &packet[..image_name_size];
        let image_name =
            String::from_utf8_lossy(&image_name_bytes[..image_name_bytes.len() - 1]).into_owned();
        let packet = &packet[image_name_size..];

        // Parse message
        let message_bytes = &packet[..message_size];
        let message =
            String::from_utf8_lossy(&message_bytes[..message_bytes.len() - 1]).into_owned();
        let packet = &packet[message_size..];

        // Parse label if subsystem and category exist
        let label = if subsystem_size > 0 && category_size > 0 && !packet.is_empty() {
            let subsystem_bytes = &packet[..subsystem_size];
            let subsystem =
                String::from_utf8_lossy(&subsystem_bytes[..subsystem_bytes.len() - 1]).into_owned();
            let packet = &packet[subsystem_size..];

            let category_bytes = &packet[..category_size];
            let category =
                String::from_utf8_lossy(&category_bytes[..category_bytes.len() - 1]).into_owned();

            Some(SyslogLabel {
                subsystem,
                category,
            })
        } else {
            None
        };

        let timestamp = match DateTime::from_timestamp(seconds as i64, microseconds) {
            Some(t) => t.naive_local(),
            None => {
                return Err(IdeviceError::UnexpectedResponse(
                    "invalid timestamp in log packet".into(),
                ));
            }
        };

        Ok(OsTraceLog {
            pid,
            timestamp,
            level: log_level,
            image_name,
            filename,
            message,
            label,
        })
    }
}

impl TryFrom<u8> for LogLevel {
    type Error = IdeviceError;

    fn try_from(value: u8) -> Result<Self, IdeviceError> {
        Ok(match value {
            0 => Self::Notice,
            1 => Self::Info,
            2 => Self::Debug,
            0x10 => Self::Error,
            0x11 => Self::Fault,
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "unknown log level byte value".into(),
                ));
            }
        })
    }
}

#[cfg(feature = "rsd")]
impl crate::RsdService for OsTraceRelayClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        crate::obf!("com.apple.os_trace_relay.shim.remote")
    }
    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = crate::Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self { idevice })
    }
}
