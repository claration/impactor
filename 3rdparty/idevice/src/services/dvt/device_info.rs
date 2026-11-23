// Jackson Coxson
//! Device info service - Query device information via DVT

use plist::Value;

use super::message::AuxValue;
use super::remote_server::{Channel, RemoteServerClient};
use crate::{IdeviceError, ReadWrite, obf};

/// Client for querying device information
#[derive(Debug)]
pub struct DeviceInfoClient<'a, R: ReadWrite> {
    channel: Channel<'a, R>,
}

/// A running process on the device
#[derive(Debug, Clone)]
pub struct RunningProcess {
    pub pid: u32,
    pub name: String,
    pub real_app_name: String,
    pub is_application: bool,
    pub start_page_count: u64,
}

impl<'a, R: ReadWrite> DeviceInfoClient<'a, R> {
    pub async fn new(client: &'a mut RemoteServerClient<R>) -> Result<Self, IdeviceError> {
        let channel = client
            .make_channel(obf!("com.apple.instruments.server.services.deviceinfo"))
            .await?;
        Ok(Self { channel })
    }

    /// Read the reply message
    async fn read_reply(&mut self) -> Result<Value, IdeviceError> {
        let msg = self.channel.read_message().await?;
        msg.data
            .ok_or_else(|| IdeviceError::UnexpectedResponse("no data in reply".into()))
    }

    /// Returns the list of running processes
    pub async fn running_processes(&mut self) -> Result<Vec<RunningProcess>, IdeviceError> {
        self.channel
            .call_method(Some(Value::String("runningProcesses".into())), None, true)
            .await?;
        let data = self.read_reply().await?;
        let arr = data
            .into_array()
            .ok_or_else(|| IdeviceError::UnexpectedResponse("expected array".into()))?;

        let mut result = Vec::new();
        for item in arr {
            if let Some(dict) = item.into_dictionary() {
                let pid = dict
                    .get("pid")
                    .and_then(|v| match v {
                        Value::Integer(i) => i.as_unsigned(),
                        _ => None,
                    })
                    .unwrap_or(0) as u32;
                let name = dict
                    .get("name")
                    .and_then(|v| v.as_string())
                    .unwrap_or("")
                    .to_string();
                let real_app_name = dict
                    .get("realAppName")
                    .and_then(|v| v.as_string())
                    .unwrap_or("")
                    .to_string();
                let is_application = dict
                    .get("isApplication")
                    .and_then(|v| v.as_boolean())
                    .unwrap_or(false);
                let start_page_count = dict
                    .get("startPageCount")
                    .and_then(|v| match v {
                        Value::Integer(i) => i.as_unsigned(),
                        _ => None,
                    })
                    .unwrap_or(0);
                result.push(RunningProcess {
                    pid,
                    name,
                    real_app_name,
                    is_application,
                    start_page_count,
                });
            }
        }
        Ok(result)
    }

    /// Returns the executable name for the given PID
    pub async fn execname_for_pid(&mut self, pid: u32) -> Result<String, IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("execnameForPid:".into())),
                Some(vec![AuxValue::archived_value(Value::Integer(
                    (pid as i64).into(),
                ))]),
                true,
            )
            .await?;
        let data = self.read_reply().await?;
        Ok(data.into_string().unwrap_or_default())
    }

    /// Returns whether the given PID is currently running
    pub async fn is_running_pid(&mut self, pid: u32) -> Result<bool, IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("isRunningPid:".into())),
                Some(vec![AuxValue::archived_value(Value::Integer(
                    (pid as i64).into(),
                ))]),
                true,
            )
            .await?;
        let data = self.read_reply().await?;
        Ok(data.as_boolean().unwrap_or(false))
    }

    /// Returns hardware information about the device
    pub async fn hardware_information(&mut self) -> Result<plist::Dictionary, IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("hardwareInformation".into())),
                None,
                true,
            )
            .await?;
        let data = self.read_reply().await?;
        data.into_dictionary()
            .ok_or_else(|| IdeviceError::UnexpectedResponse("expected dictionary".into()))
    }

    /// Returns network information about the device
    pub async fn network_information(&mut self) -> Result<plist::Dictionary, IdeviceError> {
        self.channel
            .call_method(Some(Value::String("networkInformation".into())), None, true)
            .await?;
        let data = self.read_reply().await?;
        data.into_dictionary()
            .ok_or_else(|| IdeviceError::UnexpectedResponse("expected dictionary".into()))
    }

    /// Returns the mach kernel name
    pub async fn mach_kernel_name(&mut self) -> Result<String, IdeviceError> {
        self.channel
            .call_method(Some(Value::String("machKernelName".into())), None, true)
            .await?;
        let data = self.read_reply().await?;
        Ok(data.into_string().unwrap_or_default())
    }

    /// Returns the list of sysmon process attribute names
    pub async fn sysmon_process_attributes(&mut self) -> Result<Vec<String>, IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("sysmonProcessAttributes".into())),
                None,
                true,
            )
            .await?;
        let data = self.read_reply().await?;
        let arr = data
            .into_array()
            .ok_or_else(|| IdeviceError::UnexpectedResponse("expected array".into()))?;
        Ok(arr.into_iter().filter_map(|v| v.into_string()).collect())
    }

    /// Returns the list of sysmon system attribute names
    pub async fn sysmon_system_attributes(&mut self) -> Result<Vec<String>, IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("sysmonSystemAttributes".into())),
                None,
                true,
            )
            .await?;
        let data = self.read_reply().await?;
        let arr = data
            .into_array()
            .ok_or_else(|| IdeviceError::UnexpectedResponse("expected array".into()))?;
        Ok(arr.into_iter().filter_map(|v| v.into_string()).collect())
    }

    /// Lists directory contents at the given path
    pub async fn directory_listing(&mut self, path: &str) -> Result<Vec<String>, IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("directoryListingForPath:".into())),
                Some(vec![AuxValue::archived_value(Value::String(
                    path.to_string(),
                ))]),
                true,
            )
            .await?;
        let data = self.read_reply().await?;
        let arr = data
            .into_array()
            .ok_or_else(|| IdeviceError::UnexpectedResponse("expected array".into()))?;
        Ok(arr.into_iter().filter_map(|v| v.into_string()).collect())
    }

    /// Returns the username for the given UID
    pub async fn name_for_uid(&mut self, uid: u32) -> Result<String, IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("nameForUID:".into())),
                Some(vec![AuxValue::I64(uid as i64)]),
                true,
            )
            .await?;
        let data = self.read_reply().await?;
        Ok(data.into_string().unwrap_or_default())
    }

    /// Returns the group name for the given GID
    pub async fn name_for_gid(&mut self, gid: u32) -> Result<String, IdeviceError> {
        self.channel
            .call_method(
                Some(Value::String("nameForGID:".into())),
                Some(vec![AuxValue::I64(gid as i64)]),
                true,
            )
            .await?;
        let data = self.read_reply().await?;
        Ok(data.into_string().unwrap_or_default())
    }
}
