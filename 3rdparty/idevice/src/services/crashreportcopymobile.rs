//! iOS Crash Logs
//!
//! Provides functionality for managing crash logs on a connected iOS device.
//!
//! This module enables clients to list, pull, and remove crash logs via the
//! `CrashReportCopyMobile` service using the AFC protocol. It also includes a
//! function to trigger a flush of crash logs from system storage into the
//! crash reports directory by connecting to the `com.apple.crashreportmover` service.

use tracing::{debug, warn};

use crate::{Idevice, IdeviceError, IdeviceService, afc::AfcClient, lockdown::LockdownClient, obf};

/// Client for managing crash logs on an iOS device.
///
/// This client wraps access to the `com.apple.crashreportcopymobile` service,
/// which exposes crash logs through the Apple File Conduit (AFC).
#[derive(Debug)]
pub struct CrashReportCopyMobileClient {
    /// The underlying AFC client connected to the crash logs directory.
    pub afc_client: AfcClient,
}

impl IdeviceService for CrashReportCopyMobileClient {
    /// Returns the name of the CrashReportCopyMobile service.
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.crashreportcopymobile")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl CrashReportCopyMobileClient {
    /// Creates a new client from an existing AFC-capable device connection.
    ///
    /// # Arguments
    /// * `idevice` - A pre-established connection to the device.
    pub fn new(idevice: Idevice) -> Self {
        Self {
            afc_client: AfcClient::new(idevice),
        }
    }

    /// Lists crash report files in the root of the crash logs directory.
    ///
    /// # Arguments
    /// * `dir_path` - The directory to pull logs from. Default is /
    ///
    /// # Returns
    /// A list of filenames.
    ///
    /// # Errors
    /// Returns `IdeviceError` if listing the directory fails.
    pub async fn ls(&mut self, dir_path: Option<&str>) -> Result<Vec<String>, IdeviceError> {
        let path = dir_path.unwrap_or("/");
        let mut res = self.afc_client.list_dir(path).await?;
        if res.len() > 2 {
            if &res[0] == "." {
                res.swap_remove(0);
            }
            if &res[1] == ".." {
                res.swap_remove(1);
            }
        }

        Ok(res)
    }

    /// Retrieves the contents of a specified crash log file.
    ///
    /// # Arguments
    /// * `log` - Name of the log file to retrieve.
    ///
    /// # Returns
    /// A byte vector containing the file contents.
    ///
    /// # Errors
    /// Returns `IdeviceError` if the file cannot be opened or read.
    pub async fn pull(&mut self, log: impl Into<String>) -> Result<Vec<u8>, IdeviceError> {
        let log = log.into();
        let mut f = self
            .afc_client
            .open(format!("/{log}"), crate::afc::opcode::AfcFopenMode::RdOnly)
            .await?;

        f.read_entire().await
    }

    /// Removes a specified crash log file from the device.
    ///
    /// # Arguments
    /// * `log` - Name of the log file to remove.
    ///
    /// # Errors
    /// Returns `IdeviceError` if the file could not be deleted.
    pub async fn remove(&mut self, log: impl Into<String>) -> Result<(), IdeviceError> {
        let log = log.into();
        self.afc_client.remove(format!("/{log}")).await
    }

    /// Consumes this client and returns the inner AFC client.
    pub fn to_afc_client(self) -> AfcClient {
        self.afc_client
    }
}

const EXPECTED_FLUSH: [u8; 4] = [0x70, 0x69, 0x6E, 0x67]; // 'ping'

/// Triggers a flush of crash logs from system storage.
///
/// This connects to the `com.apple.crashreportmover` service,
/// which moves crash logs into the AFC-accessible directory.
///
/// # Arguments
/// * `provider` - The device provider used for connection and pairing info.
///
/// # Returns
/// `Ok(())` if the service responds with a valid flush indicator.
/// `Err(IdeviceError)` if the service responds with unexpected data
/// or the connection fails.
pub async fn flush_reports(
    provider: &dyn crate::provider::IdeviceProvider,
) -> Result<(), IdeviceError> {
    let mut lockdown = LockdownClient::connect(provider).await?;

    let legacy = lockdown
        .get_value(Some("ProductVersion"), None)
        .await
        .ok()
        .as_ref()
        .and_then(|x| x.as_string())
        .and_then(|x| x.split(".").next())
        .and_then(|x| x.parse::<u8>().ok())
        .map(|x| x < crate::LEGACY_TLS_MAX_MAJOR)
        .unwrap_or(false);

    lockdown
        .start_session(&provider.get_pairing_file().await?)
        .await?;

    let (port, ssl) = lockdown
        .start_service(obf!("com.apple.crashreportmover"))
        .await?;

    let mut idevice = provider.connect(port).await?;
    if ssl {
        idevice
            .start_session(&provider.get_pairing_file().await?, legacy)
            .await?;
    }

    let res = idevice.read_raw(4).await?;
    debug!(
        "Flush reports response: {:?}",
        String::from_utf8_lossy(&res)
    );

    if res[..4] == EXPECTED_FLUSH {
        Ok(())
    } else {
        warn!("crashreportmover sent wrong bytes: {res:02X?}");
        Err(IdeviceError::CrashReportMoverBadResponse(res))
    }
}

#[cfg(feature = "rsd")]
impl crate::RsdService for CrashReportCopyMobileClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        crate::obf!("com.apple.crashreportcopymobile.shim.remote")
    }
    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = crate::Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}
