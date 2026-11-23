//! iOS Mobile Backup 2 Service Client
//!
//! Provides functionality for interacting with the mobilebackup2 service on iOS devices,
//! which allows creating, restoring, and managing device backups.

use plist::Dictionary;
use std::future::Future;
use std::io::{Read, Write};
use std::path::Path;
use std::pin::Pin;
use std::time::SystemTime;
use tokio::io::AsyncReadExt;
use tracing::{debug, warn};

use crate::{Idevice, IdeviceError, IdeviceService, obf};

/// DeviceLink message codes used in MobileBackup2 binary streams
pub const DL_CODE_SUCCESS: u8 = 0x00;
pub const DL_CODE_ERROR_LOCAL: u8 = 0x06;
pub const DL_CODE_ERROR_REMOTE: u8 = 0x0b;
pub const DL_CODE_FILE_DATA: u8 = 0x0c;

/// Metadata for a single directory entry returned by [`BackupDelegate::list_dir`].
#[derive(Debug)]
pub struct DirEntryInfo {
    pub name: String,
    pub is_dir: bool,
    pub is_file: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
}

/// Delegate trait providing host-side storage and platform operations for the
/// mobilebackup2 DeviceLink loop.
///
/// All filesystem-like operations go through this trait so that callers can
/// direct backup data to something other than the local filesystem
/// (e.g. a database, cloud storage, or an in-memory buffer).
///
/// A ready-made [`FsBackupDelegate`] is provided for the common case of
/// reading/writing to the local filesystem via `tokio::fs`.
pub trait BackupDelegate: Send + Sync {
    /// Returns the available disk space in bytes for the volume containing `path`.
    fn get_free_disk_space(&self, path: &Path) -> u64;

    /// Open an existing file for reading.
    #[allow(clippy::type_complexity)]
    fn open_file_read<'a>(
        &'a self,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn Read + Send>, IdeviceError>> + Send + 'a>>;

    /// Create (or truncate) a file for writing.
    #[allow(clippy::type_complexity)]
    fn create_file_write<'a>(
        &'a self,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn Write + Send>, IdeviceError>> + Send + 'a>>;

    /// Recursively create a directory and all parents.
    fn create_dir_all<'a>(
        &'a self,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send + 'a>>;

    /// Remove a path. If it is a directory, remove it recursively.
    fn remove<'a>(
        &'a self,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send + 'a>>;

    /// Rename / move `from` to `to`.
    fn rename<'a>(
        &'a self,
        from: &'a Path,
        to: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send + 'a>>;

    /// Copy a file or directory from `src` to `dst`.
    fn copy<'a>(
        &'a self,
        src: &'a Path,
        dst: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send + 'a>>;

    /// Returns `true` if `path` exists.
    fn exists<'a>(&'a self, path: &'a Path) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

    /// Returns `true` if `path` is a directory.
    fn is_dir<'a>(&'a self, path: &'a Path) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>>;

    /// List the immediate children of `path` with metadata.
    fn list_dir<'a>(
        &'a self,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DirEntryInfo>, IdeviceError>> + Send + 'a>>;

    /// Called after each file is received from the device during backup.
    ///
    /// `file_count` is the running total of files received in the current upload batch.
    fn on_file_received(&self, _path: &str, _file_count: u32) {}

    /// Called periodically during file transfer with byte-level progress.
    ///
    /// - `bytes_done`: total bytes transferred so far in this upload batch
    /// - `bytes_total`: total expected bytes for this batch (0 if unknown)
    /// - `overall_progress`: device-reported overall progress percentage (0.0–100.0),
    ///   or negative if not yet reported
    fn on_progress(&self, _bytes_done: u64, _bytes_total: u64, _overall_progress: f64) {}
}

/// Default [`BackupDelegate`] that reads/writes to the local filesystem via `tokio::fs`.
///
/// Returns `0` for [`get_free_disk_space`](BackupDelegate::get_free_disk_space);
/// override or wrap this if you need real disk-space reporting.
#[derive(Debug, Clone, Copy)]
pub struct FsBackupDelegate;

impl BackupDelegate for FsBackupDelegate {
    fn get_free_disk_space(&self, _path: &Path) -> u64 {
        0
    }

    fn open_file_read<'a>(
        &'a self,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn Read + Send>, IdeviceError>> + Send + 'a>> {
        Box::pin(async move {
            let file = tokio::fs::File::open(path)
                .await
                .map_err(|e| IdeviceError::InternalError(e.to_string()))?;
            let std_file = file.into_std().await;
            Ok(Box::new(std_file) as Box<dyn Read + Send>)
        })
    }

    fn create_file_write<'a>(
        &'a self,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<Box<dyn Write + Send>, IdeviceError>> + Send + 'a>>
    {
        Box::pin(async move {
            let file = tokio::fs::File::create(path)
                .await
                .map_err(|e| IdeviceError::InternalError(e.to_string()))?;
            let std_file = file.into_std().await;
            Ok(Box::new(std_file) as Box<dyn Write + Send>)
        })
    }

    fn create_dir_all<'a>(
        &'a self,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send + 'a>> {
        Box::pin(async move {
            tokio::fs::create_dir_all(path)
                .await
                .map_err(|e| IdeviceError::InternalError(e.to_string()))
        })
    }

    fn remove<'a>(
        &'a self,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send + 'a>> {
        Box::pin(async move {
            let meta = tokio::fs::metadata(path).await;
            match meta {
                Ok(m) if m.is_dir() => tokio::fs::remove_dir_all(path).await,
                _ => tokio::fs::remove_file(path).await,
            }
            .map_err(|e| IdeviceError::InternalError(e.to_string()))
        })
    }

    fn rename<'a>(
        &'a self,
        from: &'a Path,
        to: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send + 'a>> {
        Box::pin(async move {
            tokio::fs::rename(from, to)
                .await
                .map_err(|e| IdeviceError::InternalError(e.to_string()))
        })
    }

    fn copy<'a>(
        &'a self,
        src: &'a Path,
        dst: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send + 'a>> {
        Box::pin(async move {
            let meta = tokio::fs::metadata(src).await;
            if meta.is_ok_and(|m| m.is_dir()) {
                tokio::fs::create_dir_all(dst).await
            } else {
                tokio::fs::copy(src, dst).await.map(|_| ())
            }
            .map_err(|e| IdeviceError::InternalError(e.to_string()))
        })
    }

    fn exists<'a>(&'a self, path: &'a Path) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move { tokio::fs::try_exists(path).await.unwrap_or(false) })
    }

    fn is_dir<'a>(&'a self, path: &'a Path) -> Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            tokio::fs::metadata(path)
                .await
                .map(|m| m.is_dir())
                .unwrap_or(false)
        })
    }

    fn list_dir<'a>(
        &'a self,
        path: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<DirEntryInfo>, IdeviceError>> + Send + 'a>> {
        Box::pin(async move {
            let mut entries = tokio::fs::read_dir(path)
                .await
                .map_err(|e| IdeviceError::InternalError(e.to_string()))?;
            let mut result = Vec::new();
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                let meta = entry.metadata().await.ok();
                result.push(DirEntryInfo {
                    name,
                    is_dir: meta.as_ref().is_some_and(|m| m.is_dir()),
                    is_file: meta.as_ref().is_some_and(|m| m.is_file()),
                    size: meta.as_ref().map_or(0, |m| m.len()),
                    modified: meta.and_then(|m| m.modified().ok()),
                });
            }
            Ok(result)
        })
    }
}

/// Client for interacting with the iOS mobile backup 2 service
///
/// This service provides access to device backup functionality including
/// creating backups, restoring from backups, and managing backup data.
#[derive(Debug)]
pub struct MobileBackup2Client {
    /// The underlying device connection with established mobilebackup2 service
    pub idevice: Idevice,
    /// Protocol version negotiated with the device
    pub protocol_version: f64,
}

impl IdeviceService for MobileBackup2Client {
    /// Returns the mobile backup 2 service name as registered with lockdownd
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.mobilebackup2")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        let mut client = Self::new(idevice);
        // Perform DeviceLink handshake first
        client.dl_version_exchange().await?;
        // Perform version exchange after connection
        client.version_exchange().await?;
        Ok(client)
    }
}

/// Backup message types used in the mobilebackup2 protocol
#[derive(Debug, Clone, Copy)]
pub enum BackupMessageType {
    /// Request to start a backup operation
    BackupMessageTypeBackup,
    /// Request to restore from a backup
    BackupMessageTypeRestore,
    /// Information message
    BackupMessageTypeInfo,
    /// List available backups
    BackupMessageTypeList,
    /// Upload files to backup
    BackupMessageTypeUploadFiles,
    /// Download files from backup
    BackupMessageTypeDownloadFiles,
    /// Clear backup data
    BackupMessageTypeClearBackupData,
    /// Move files in backup
    BackupMessageTypeMoveFiles,
    /// Remove files from backup
    BackupMessageTypeRemoveFiles,
    /// Create directory in backup
    BackupMessageTypeCreateDirectory,
    /// Acquire lock for backup operation
    BackupMessageTypeAcquireLock,
    /// Release lock after backup operation
    BackupMessageTypeReleaseLock,
    /// Copy item in backup
    BackupMessageTypeCopyItem,
    /// Disconnect from service
    BackupMessageTypeDisconnect,
    /// Process message
    BackupMessageTypeProcessMessage,
    /// Get freespace information
    BackupMessageTypeGetFreespace,
    /// Factory info
    BackupMessageTypeFactoryInfo,
    /// Check if backup is encrypted
    BackupMessageTypeCheckBackupEncryption,
}

impl BackupMessageType {
    /// Convert message type to string representation
    pub fn as_str(&self) -> &'static str {
        match self {
            // These map to MobileBackup2 request names per libimobiledevice
            BackupMessageType::BackupMessageTypeBackup => "Backup",
            BackupMessageType::BackupMessageTypeRestore => "Restore",
            BackupMessageType::BackupMessageTypeInfo => "Info",
            BackupMessageType::BackupMessageTypeList => "List",
            // The following are DL control messages and not sent via MessageName
            BackupMessageType::BackupMessageTypeUploadFiles => "DLMessageUploadFiles",
            BackupMessageType::BackupMessageTypeDownloadFiles => "DLMessageDownloadFiles",
            BackupMessageType::BackupMessageTypeClearBackupData => "DLMessageClearBackupData",
            BackupMessageType::BackupMessageTypeMoveFiles => "DLMessageMoveFiles",
            BackupMessageType::BackupMessageTypeRemoveFiles => "DLMessageRemoveFiles",
            BackupMessageType::BackupMessageTypeCreateDirectory => "DLMessageCreateDirectory",
            BackupMessageType::BackupMessageTypeAcquireLock => "DLMessageAcquireLock",
            BackupMessageType::BackupMessageTypeReleaseLock => "DLMessageReleaseLock",
            BackupMessageType::BackupMessageTypeCopyItem => "DLMessageCopyItem",
            BackupMessageType::BackupMessageTypeDisconnect => "DLMessageDisconnect",
            BackupMessageType::BackupMessageTypeProcessMessage => "DLMessageProcessMessage",
            BackupMessageType::BackupMessageTypeGetFreespace => "DLMessageGetFreeDiskSpace",
            BackupMessageType::BackupMessageTypeFactoryInfo => "FactoryInfo",
            BackupMessageType::BackupMessageTypeCheckBackupEncryption => "CheckBackupEncryption",
        }
    }
}

/// Backup information structure
#[derive(Debug, Clone)]
pub struct BackupInfo {
    /// Backup UUID
    pub uuid: String,
    /// Device name
    pub device_name: String,
    /// Display name
    pub display_name: String,
    /// Last backup date
    pub last_backup_date: Option<String>,
    /// Backup version
    pub version: String,
    /// Whether backup is encrypted
    pub is_encrypted: bool,
}

/// High-level builder for restore options so callers don't need to remember raw keys
#[derive(Debug, Clone)]
pub struct RestoreOptions {
    pub reboot: bool,
    pub copy: bool,
    pub preserve_settings: bool,
    pub system_files: bool,
    pub remove_items_not_restored: bool,
    pub password: Option<String>,
}

impl Default for RestoreOptions {
    fn default() -> Self {
        Self {
            reboot: true,
            copy: true,
            preserve_settings: true,
            system_files: false,
            remove_items_not_restored: false,
            password: None,
        }
    }
}

impl RestoreOptions {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn with_reboot(mut self, reboot: bool) -> Self {
        self.reboot = reboot;
        self
    }
    pub fn with_copy(mut self, copy: bool) -> Self {
        self.copy = copy;
        self
    }
    pub fn with_preserve_settings(mut self, preserve: bool) -> Self {
        self.preserve_settings = preserve;
        self
    }
    pub fn with_system_files(mut self, system: bool) -> Self {
        self.system_files = system;
        self
    }
    pub fn with_remove_items_not_restored(mut self, remove: bool) -> Self {
        self.remove_items_not_restored = remove;
        self
    }
    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.password = Some(password.into());
        self
    }

    pub fn to_plist(&self) -> Dictionary {
        crate::plist!(dict {
            "RestoreShouldReboot": self.reboot,
            "RestoreDontCopyBackup": !self.copy,
            "RestorePreserveSettings": self.preserve_settings,
            "RestoreSystemFiles": self.system_files,
            "RemoveItemsNotRestored": self.remove_items_not_restored,
            "Password":? self.password.clone()
        })
    }
}

impl MobileBackup2Client {
    /// Creates a new mobile backup 2 client from an existing device connection
    ///
    /// # Arguments
    /// * `idevice` - Pre-established device connection
    pub fn new(idevice: Idevice) -> Self {
        Self {
            idevice,
            protocol_version: 0.0,
        }
    }

    /// Performs DeviceLink version exchange handshake
    ///
    /// Sequence:
    /// 1) Receive ["DLMessageVersionExchange", major, minor]
    /// 2) Send   ["DLMessageVersionExchange", "DLVersionsOk", 400]
    /// 3) Receive ["DLMessageDeviceReady"]
    async fn dl_version_exchange(&mut self) -> Result<(), IdeviceError> {
        debug!("Starting DeviceLink version exchange");
        // 1) Receive DLMessageVersionExchange
        let (msg, _arr) = self.receive_dl_message().await?;
        if msg != "DLMessageVersionExchange" {
            warn!("Expected DLMessageVersionExchange, got {msg}");
            return Err(IdeviceError::UnexpectedResponse(
                "expected DLMessageVersionExchange during handshake".into(),
            ));
        }

        // 2) Send DLVersionsOk with version 400
        let out = vec![
            plist::Value::String("DLMessageVersionExchange".into()),
            plist::Value::String("DLVersionsOk".into()),
            plist::Value::Integer(400u64.into()),
        ];
        self.send_dl_array(out).await?;

        // 3) Receive DLMessageDeviceReady
        let (msg2, _arr2) = self.receive_dl_message().await?;
        if msg2 != "DLMessageDeviceReady" {
            warn!("Expected DLMessageDeviceReady, got {msg2}");
            return Err(IdeviceError::UnexpectedResponse(
                "expected DLMessageDeviceReady after version exchange".into(),
            ));
        }
        Ok(())
    }

    /// Sends a raw DL array as binary plist
    async fn send_dl_array(&mut self, array: Vec<plist::Value>) -> Result<(), IdeviceError> {
        self.idevice.send_bplist(plist::Value::Array(array)).await
    }

    /// Receives any DL* message and returns (message_tag, full_array_value)
    pub async fn receive_dl_message(&mut self) -> Result<(String, plist::Value), IdeviceError> {
        if let Some(socket) = &mut self.idevice.socket {
            let mut buf = [0u8; 4];
            if let Err(e) = socket.read_exact(&mut buf).await {
                debug!("Failed to read DL message length: {e}");
                return Err(e.into());
            }
            let len = u32::from_be_bytes(buf);
            debug!("Reading DL message body: {len} bytes");
            let mut body = vec![0; len as usize];
            socket.read_exact(&mut body).await?;
            let value: plist::Value = plist::from_bytes(&body)?;
            if let plist::Value::Array(arr) = &value
                && let Some(plist::Value::String(tag)) = arr.first()
            {
                debug!("Received DL message: {tag}");
                return Ok((tag.clone(), value));
            }
            warn!("Invalid DL message format");
            Err(IdeviceError::UnexpectedResponse(
                "invalid DL message format: expected array with string tag".into(),
            ))
        } else {
            Err(IdeviceError::NoEstablishedConnection)
        }
    }

    /// Performs version exchange with the device
    ///
    /// This is required by the mobilebackup2 protocol and must be called
    /// before any other operations.
    ///
    /// # Returns
    /// `Ok(())` on successful version negotiation
    ///
    /// # Errors
    /// Returns `IdeviceError` if version exchange fails
    async fn version_exchange(&mut self) -> Result<(), IdeviceError> {
        debug!("Starting mobilebackup2 version exchange");

        // Send supported protocol versions (matching libimobiledevice)
        let hello_dict = crate::plist!(dict {
            "SupportedProtocolVersions": [
                2.0, 2.1
            ]
        });

        self.send_device_link_message("Hello", Some(hello_dict))
            .await?;

        // Receive response
        let response = self.receive_device_link_message("Response").await?;

        // Check for error
        if let Some(error_code) = response.get("ErrorCode")
            && let Some(code) = error_code.as_unsigned_integer()
            && code != 0
        {
            warn!("Version exchange failed with error code: {code}");
            return Err(IdeviceError::UnexpectedResponse(
                "version exchange failed with non-zero ErrorCode".into(),
            ));
        }

        // Get negotiated protocol version
        if let Some(version) = response.get("ProtocolVersion").and_then(|v| v.as_real()) {
            self.protocol_version = version;
            debug!("Negotiated protocol version: {version}");
        } else {
            warn!("No protocol version in response");
            return Err(IdeviceError::UnexpectedResponse(
                "missing ProtocolVersion in version exchange response".into(),
            ));
        }

        Ok(())
    }

    /// Sends a device link message (DLMessageProcessMessage format)
    ///
    /// This follows the device_link_service protocol used by mobilebackup2
    ///
    /// # Arguments
    /// * `message_name` - The message name (e.g., "Hello", "kBackupMessageTypeInfo")
    /// * `options` - Optional dictionary of options for the message
    ///
    /// # Returns
    /// `Ok(())` on successful message send
    ///
    /// # Errors
    /// Returns `IdeviceError` if communication fails
    async fn send_device_link_message(
        &mut self,
        message_name: &str,
        options: Option<Dictionary>,
    ) -> Result<(), IdeviceError> {
        // Create the actual message dictionary
        let message_dict = crate::plist!(dict {
            "MessageName": message_name,
            :<? options,
        });

        debug!("Sending device link message: {message_name}");
        self.idevice
            .send_bplist(crate::plist!(["DLMessageProcessMessage", message_dict]))
            .await
    }

    /// Receives a device link message and validates the message name
    ///
    /// Arguments
    /// * `expected_message` - The expected message name to validate
    ///
    /// # Returns
    /// The message dictionary on success
    ///
    /// # Errors
    /// Returns `IdeviceError` if communication fails or message name doesn't match
    async fn receive_device_link_message(
        &mut self,
        expected_message: &str,
    ) -> Result<Dictionary, IdeviceError> {
        // Read raw bytes and parse as plist::Value to handle array format
        if let Some(socket) = &mut self.idevice.socket {
            debug!("Reading response size");
            let mut buf = [0u8; 4];
            socket.read_exact(&mut buf).await?;
            let len = u32::from_be_bytes(buf);
            let mut buf = vec![0; len as usize];
            socket.read_exact(&mut buf).await?;
            let response_value: plist::Value = plist::from_bytes(&buf)?;

            // Parse DLMessageProcessMessage format
            if let plist::Value::Array(array) = response_value
                && array.len() >= 2
                && let Some(plist::Value::String(dl_message)) = array.first()
                && let Some(plist::Value::Dictionary(dict)) = array.get(1)
                && dl_message == "DLMessageProcessMessage"
            {
                // Check MessageName if expected
                if !expected_message.is_empty() {
                    if let Some(message_name) = dict.get("MessageName").and_then(|v| v.as_string())
                    {
                        if message_name != expected_message {
                            warn!("Expected message '{expected_message}', got '{message_name}'");
                            return Err(IdeviceError::UnexpectedResponse(
                                "device link MessageName does not match expected".into(),
                            ));
                        }
                    } else {
                        warn!("No MessageName in response");
                        return Err(IdeviceError::UnexpectedResponse(
                            "missing MessageName in DLMessageProcessMessage".into(),
                        ));
                    }
                }
                return Ok(dict.clone());
            }

            warn!("Invalid device link message format");
            Err(IdeviceError::UnexpectedResponse(
                "invalid device link message format".into(),
            ))
        } else {
            Err(IdeviceError::NoEstablishedConnection)
        }
    }

    /// Sends a backup message to the device
    ///
    /// # Arguments
    /// * `message_type` - The type of backup message to send
    /// * `options` - Optional dictionary of options for the message
    ///
    /// # Returns
    /// `Ok(())` on successful message send
    ///
    /// # Errors
    /// Returns `IdeviceError` if communication fails
    async fn send_backup_message(
        &mut self,
        message_type: BackupMessageType,
        options: Option<Dictionary>,
    ) -> Result<(), IdeviceError> {
        self.send_device_link_message(message_type.as_str(), options)
            .await
    }

    /// Sends a MobileBackup2 request with proper envelope and identifiers
    pub async fn send_request(
        &mut self,
        request: &str,
        target_identifier: Option<&str>,
        source_identifier: Option<&str>,
        options: Option<Dictionary>,
    ) -> Result<(), IdeviceError> {
        let dict = crate::plist!(dict {
            "TargetIdentifier":? target_identifier,
            "SourceIdentifier":? source_identifier,
            "Options":? options,
            // Special cases like Unback/EnableCloudBackup are handled by caller if needed
        });
        self.send_device_link_message(request, Some(dict)).await
    }

    /// Sends a DLMessageStatusResponse array
    pub async fn send_status_response(
        &mut self,
        status_code: i64,
        status1: Option<&str>,
        status2: Option<plist::Value>,
    ) -> Result<(), IdeviceError> {
        let arr = vec![
            plist::Value::String("DLMessageStatusResponse".into()),
            plist::Value::Integer(status_code.into()),
            plist::Value::String(status1.unwrap_or("___EmptyParameterString___").into()),
            status2.unwrap_or_else(|| plist::Value::String("___EmptyParameterString___".into())),
        ];
        self.send_dl_array(arr).await
    }

    /// Receives a response from the backup service
    ///
    /// # Returns
    /// The response as a plist Dictionary
    ///
    /// # Errors
    /// Returns `IdeviceError` if communication fails or response is malformed
    async fn receive_backup_response(&mut self) -> Result<Dictionary, IdeviceError> {
        self.receive_device_link_message("").await
    }

    /// Requests device information for backup
    ///
    /// # Returns
    /// A dictionary containing device information
    ///
    /// # Errors
    /// Returns `IdeviceError` if the request fails
    pub async fn request_backup_info(&mut self) -> Result<Dictionary, IdeviceError> {
        // Per protocol use MessageName "Info"
        self.send_backup_message(BackupMessageType::BackupMessageTypeInfo, None)
            .await?;

        let response = self.receive_backup_response().await?;

        // Check for error in response
        if let Some(error) = response.get("ErrorCode") {
            warn!("Backup info request failed with error: {error:?}");
            return Err(IdeviceError::UnexpectedResponse(
                "backup info request returned ErrorCode".into(),
            ));
        }

        Ok(response)
    }

    /// Lists available backups on the device
    ///
    /// # Returns
    /// A vector of backup information
    ///
    /// # Errors
    /// Returns `IdeviceError` if the request fails
    pub async fn list_backups(&mut self) -> Result<Vec<BackupInfo>, IdeviceError> {
        self.send_backup_message(BackupMessageType::BackupMessageTypeList, None)
            .await?;

        let response = self.receive_backup_response().await?;

        // Check for error in response
        if let Some(error) = response.get("ErrorCode") {
            warn!("List backups request failed with error: {error:?}");
            return Err(IdeviceError::UnexpectedResponse(
                "list backups request returned ErrorCode".into(),
            ));
        }

        let mut backups = Vec::new();

        if let Some(plist::Value::Array(backup_list)) = response.get("BackupList") {
            for backup_item in backup_list {
                if let plist::Value::Dictionary(backup_dict) = backup_item {
                    let uuid = backup_dict
                        .get("BackupUUID")
                        .and_then(|v| v.as_string())
                        .unwrap_or_default()
                        .to_string();

                    let device_name = backup_dict
                        .get("DeviceName")
                        .and_then(|v| v.as_string())
                        .unwrap_or_default()
                        .to_string();

                    let display_name = backup_dict
                        .get("DisplayName")
                        .and_then(|v| v.as_string())
                        .unwrap_or_default()
                        .to_string();

                    let last_backup_date = backup_dict
                        .get("LastBackupDate")
                        .and_then(|v| v.as_string())
                        .map(|s| s.to_string());

                    let version = backup_dict
                        .get("Version")
                        .and_then(|v| v.as_string())
                        .unwrap_or("Unknown")
                        .to_string();

                    let is_encrypted = backup_dict
                        .get("IsEncrypted")
                        .and_then(|v| v.as_boolean())
                        .unwrap_or(false);

                    backups.push(BackupInfo {
                        uuid,
                        device_name,
                        display_name,
                        last_backup_date,
                        version,
                        is_encrypted,
                    });
                }
            }
        }

        Ok(backups)
    }

    /// Starts a backup operation
    ///
    /// # Arguments
    /// * `target_identifier` - Optional target identifier for the backup
    /// * `source_identifier` - Optional source identifier for the backup
    /// * `options` - Optional backup options
    ///
    /// # Returns
    /// `Ok(())` on successful backup start
    ///
    /// # Errors
    /// Returns `IdeviceError` if the backup fails to start
    pub async fn start_backup(
        &mut self,
        target_identifier: Option<&str>,
        source_identifier: Option<&str>,
        options: Option<Dictionary>,
    ) -> Result<(), IdeviceError> {
        self.send_request(
            BackupMessageType::BackupMessageTypeBackup.as_str(),
            target_identifier,
            source_identifier,
            options,
        )
        .await?;

        let response = self.receive_backup_response().await?;

        // Check for error in response
        if let Some(error) = response.get("ErrorCode") {
            warn!("Backup start failed with error: {error:?}");
            return Err(IdeviceError::UnexpectedResponse(
                "backup start returned ErrorCode".into(),
            ));
        }

        debug!("Backup started successfully");
        Ok(())
    }

    /// Starts a restore operation
    ///
    /// # Arguments
    /// * `backup_uuid` - UUID of the backup to restore from
    /// * `options` - Optional restore options
    ///
    /// # Returns
    /// `Ok(())` on successful restore start
    ///
    /// # Errors
    /// Returns `IdeviceError` if the restore fails to start
    #[deprecated(
        note = "Use restore_from_path; restore via BackupUUID is not supported by device/mobilebackup2"
    )]
    pub async fn start_restore(
        &mut self,
        _backup_uuid: &str,
        options: Option<Dictionary>,
    ) -> Result<(), IdeviceError> {
        let mut opts = options.unwrap_or_default();
        // Align default restore options with pymobiledevice semantics
        // Caller-specified values (if any) take precedence.
        if !opts.contains_key("RestoreShouldReboot") {
            opts.insert("RestoreShouldReboot".into(), plist::Value::Boolean(true));
        }
        if !opts.contains_key("RestoreDontCopyBackup") {
            // pymobiledevice: copy=True -> RestoreDontCopyBackup=False
            opts.insert("RestoreDontCopyBackup".into(), plist::Value::Boolean(false));
        }
        if !opts.contains_key("RestorePreserveSettings") {
            opts.insert(
                "RestorePreserveSettings".into(),
                plist::Value::Boolean(true),
            );
        }
        if !opts.contains_key("RestoreSystemFiles") {
            opts.insert("RestoreSystemFiles".into(), plist::Value::Boolean(false));
        }
        if !opts.contains_key("RemoveItemsNotRestored") {
            opts.insert(
                "RemoveItemsNotRestored".into(),
                plist::Value::Boolean(false),
            );
        }
        // Avoid borrowing self while sending request
        let target_udid_owned = self.idevice.udid().map(|s| s.to_string());
        let target_udid = target_udid_owned.as_deref();
        self.send_request(
            BackupMessageType::BackupMessageTypeRestore.as_str(),
            // default identifiers to current UDID if available
            target_udid,
            target_udid,
            Some(opts),
        )
        .await?;

        let response = self.receive_backup_response().await?;

        // Check for error in response
        if let Some(error) = response.get("ErrorCode") {
            warn!("Restore start failed with error: {error:?}");
            return Err(IdeviceError::UnexpectedResponse(
                "restore start returned ErrorCode".into(),
            ));
        }

        debug!("Restore started successfully");
        Ok(())
    }

    /// High-level API: Create a backup of the device to a local directory
    ///
    /// - `backup_root` should point to the backup root directory. The device's backup data
    ///   will be stored in `backup_root/<source_identifier>/`.
    /// - If `source_identifier` is None, the current connected device's UDID will be used.
    /// - The backup directory will be created if it does not exist.
    ///
    /// Returns the final response dictionary from the device on success. If the device
    /// reports an error, the dictionary will contain `ErrorCode` and `ErrorDescription`.
    pub async fn backup_from_path(
        &mut self,
        backup_root: &Path,
        source_identifier: Option<&str>,
        options: Option<Dictionary>,
        delegate: &dyn BackupDelegate,
    ) -> Result<Option<Dictionary>, IdeviceError> {
        let target_udid_owned = self.idevice.udid().map(|s| s.to_string());
        let target_udid = target_udid_owned.as_deref();
        let source: &str = match source_identifier {
            Some(s) => s,
            None => target_udid.ok_or(IdeviceError::InvalidHostID)?,
        };

        // Ensure backup subdirectory exists
        let backup_dir = backup_root.join(source);
        let _ = delegate.create_dir_all(&backup_dir).await;

        self.send_request(
            BackupMessageType::BackupMessageTypeBackup.as_str(),
            target_udid,
            Some(source),
            options,
        )
        .await?;

        self.process_dl_loop(backup_root, delegate).await
    }

    /// High-level API: Restore from a local backup directory using DeviceLink file exchange
    ///
    /// - `backup_root` should point to the backup root directory (which contains the `<SourceIdentifier>` subdirectory)
    /// - If `source_identifier` is None, the current connected device's UDID will be used by default
    /// - `options` should be constructed using the `RestoreOptions` builder; if not provided, defaults will be used
    pub async fn restore_from_path(
        &mut self,
        backup_root: &Path,
        source_identifier: Option<&str>,
        options: Option<RestoreOptions>,
        delegate: &dyn BackupDelegate,
    ) -> Result<Option<Dictionary>, IdeviceError> {
        // Take owned UDID to avoid aliasing borrows
        let target_udid_owned = self.idevice.udid().map(|s| s.to_string());
        let target_udid = target_udid_owned.as_deref();
        let source: &str = match source_identifier {
            Some(s) => s,
            None => target_udid.ok_or(IdeviceError::InvalidHostID)?,
        };

        // Simple existence check: backup_root/source must exist
        let backup_dir = backup_root.join(source);
        if !delegate.exists(&backup_dir).await {
            return Err(IdeviceError::NotFound);
        }

        let opts = options.unwrap_or_default().to_plist();
        self.send_request(
            BackupMessageType::BackupMessageTypeRestore.as_str(),
            target_udid,
            Some(source),
            Some(opts),
        )
        .await?;

        self.process_dl_loop(backup_root, delegate).await
    }

    /// Processes the DeviceLink message loop used by backup, restore, and other operations.
    ///
    /// Handles all DL* messages from the device until a `DLMessageProcessMessage` (final
    /// status) or `DLMessageDisconnect` is received.
    async fn process_dl_loop(
        &mut self,
        host_dir: &Path,
        delegate: &dyn BackupDelegate,
    ) -> Result<Option<Dictionary>, IdeviceError> {
        let mut overall_progress: f64 = -1.0;
        loop {
            let (tag, value) = self.receive_dl_message().await?;

            // Extract overall progress from DL messages that carry it
            if let plist::Value::Array(arr) = &value {
                let progress_idx = match tag.as_str() {
                    "DLMessageUploadFiles" => Some(2),
                    "DLMessageDownloadFiles"
                    | "DLMessageMoveFiles"
                    | "DLMessageMoveItems"
                    | "DLMessageRemoveFiles"
                    | "DLMessageRemoveItems" => Some(3),
                    _ => None,
                };
                if let Some(idx) = progress_idx
                    && let Some(plist::Value::Real(p)) = arr.get(idx)
                    && *p > 0.0
                {
                    overall_progress = *p;
                }
            }

            match tag.as_str() {
                "DLMessageDownloadFiles" => {
                    self.handle_download_files(&value, host_dir, delegate)
                        .await?;
                }
                "DLMessageUploadFiles" => {
                    self.handle_upload_files(&value, host_dir, delegate, overall_progress)
                        .await?;
                }
                "DLMessageGetFreeDiskSpace" => {
                    let freespace = delegate.get_free_disk_space(host_dir);
                    self.send_status_response(
                        0,
                        None,
                        Some(plist::Value::Integer(freespace.into())),
                    )
                    .await?;
                }
                "DLContentsOfDirectory" => {
                    let listing = Self::list_directory_contents(&value, host_dir, delegate).await;
                    self.send_status_response(0, None, Some(listing)).await?;
                }
                "DLMessageCreateDirectory" => {
                    if let plist::Value::Array(arr) = &value
                        && let Some(plist::Value::String(dir)) = arr.get(1)
                    {
                        debug!("Creating directory: {dir}");
                    }

                    let status =
                        Self::create_directory_from_message(&value, host_dir, delegate).await;
                    self.send_status_response(status, None, None).await?;
                }
                "DLMessageMoveFiles" | "DLMessageMoveItems" => {
                    let status = Self::move_files_from_message(&value, host_dir, delegate).await;
                    self.send_status_response(
                        status,
                        None,
                        Some(plist::Value::Dictionary(Dictionary::new())),
                    )
                    .await?;
                }
                "DLMessageRemoveFiles" | "DLMessageRemoveItems" => {
                    let status = Self::remove_files_from_message(&value, host_dir, delegate).await;
                    self.send_status_response(
                        status,
                        None,
                        Some(plist::Value::Dictionary(Dictionary::new())),
                    )
                    .await?;
                }
                "DLMessageCopyItem" => {
                    let status = Self::copy_item_from_message(&value, host_dir, delegate).await;
                    self.send_status_response(
                        status,
                        None,
                        Some(plist::Value::Dictionary(Dictionary::new())),
                    )
                    .await?;
                }
                "DLMessageProcessMessage" => {
                    if let plist::Value::Array(arr) = value
                        && let Some(plist::Value::Dictionary(dict)) = arr.get(1)
                    {
                        return Ok(Some(dict.clone()));
                    }
                    return Ok(None);
                }
                "DLMessageDisconnect" => {
                    return Ok(None);
                }
                other => {
                    warn!("Unsupported DL message: {other}");
                    self.send_status_response(-1, Some("Operation not supported"), None)
                        .await?;
                }
            }
        }
    }

    async fn handle_download_files(
        &mut self,
        dl_value: &plist::Value,
        host_dir: &Path,
        delegate: &dyn BackupDelegate,
    ) -> Result<(), IdeviceError> {
        let mut err_any = false;
        if let plist::Value::Array(arr) = dl_value
            && arr.len() >= 2
            && let Some(plist::Value::Array(files)) = arr.get(1)
        {
            for pv in files {
                if let Some(path) = pv.as_string() {
                    debug!("Device requested file: {path}");
                    if let Err(e) = self.send_single_file(host_dir, path, delegate).await {
                        warn!("Failed to send file {path}: {e}");
                        err_any = true;
                    }
                }
            }
        }
        // terminating zero dword
        self.idevice.send_raw(&0u32.to_be_bytes()).await?;
        if err_any {
            self.send_status_response(
                -13,
                Some("Multi status"),
                Some(plist::Value::Dictionary(Dictionary::new())),
            )
            .await
        } else {
            self.send_status_response(0, None, Some(plist::Value::Dictionary(Dictionary::new())))
                .await
        }
    }

    async fn send_single_file(
        &mut self,
        host_dir: &Path,
        rel_path: &str,
        delegate: &dyn BackupDelegate,
    ) -> Result<(), IdeviceError> {
        let full = host_dir.join(rel_path);
        let path_bytes = rel_path.as_bytes().to_vec();
        let nlen = (path_bytes.len() as u32).to_be_bytes();
        self.idevice.send_raw(&nlen).await?;
        self.idevice.send_raw(&path_bytes).await?;

        let mut f = match delegate.open_file_read(&full).await {
            Ok(f) => f,
            Err(e) => {
                // send error
                let desc = e.to_string();
                let size = (desc.len() as u32 + 1).to_be_bytes();
                let mut hdr = Vec::with_capacity(5);
                hdr.extend_from_slice(&size);
                hdr.push(DL_CODE_ERROR_LOCAL);
                self.idevice.send_raw(&hdr).await?;
                self.idevice.send_raw(desc.as_bytes()).await?;
                return Ok(());
            }
        };
        let mut buf = [0u8; 32768];
        loop {
            let read = f.read(&mut buf).unwrap_or(0);
            if read == 0 {
                break;
            }
            let size = ((read as u32) + 1).to_be_bytes();
            let mut hdr = Vec::with_capacity(5);
            hdr.extend_from_slice(&size);
            hdr.push(DL_CODE_FILE_DATA);
            self.idevice.send_raw(&hdr).await?;
            self.idevice.send_raw(&buf[..read]).await?;
        }
        // success trailer
        let mut ok = [0u8; 5];
        ok[..4].copy_from_slice(&1u32.to_be_bytes());
        ok[4] = DL_CODE_SUCCESS;
        self.idevice.send_raw(&ok).await?;
        Ok(())
    }

    async fn handle_upload_files(
        &mut self,
        dl_value: &plist::Value,
        host_dir: &Path,
        delegate: &dyn BackupDelegate,
        overall_progress: f64,
    ) -> Result<(), IdeviceError> {
        let mut file_count: u32 = 0;
        let mut bytes_done: u64 = 0;

        // Extract total expected bytes from DLMessageUploadFiles array index 3
        let bytes_total = if let plist::Value::Array(arr) = dl_value {
            arr.get(3)
                .and_then(|v| v.as_unsigned_integer())
                .unwrap_or(0)
        } else {
            0
        };

        loop {
            // Receive directory name
            let dlen = self.read_be_u32().await?;
            if dlen == 0 {
                break;
            }
            let _dname = self.read_exact_string(dlen as usize).await?;

            // Receive file name
            let flen = self.read_be_u32().await?;
            if flen == 0 {
                break;
            }
            let fname = self.read_exact_string(flen as usize).await?;

            let dst = host_dir.join(&fname);
            if let Some(parent) = dst.parent() {
                let _ = delegate.create_dir_all(parent).await;
            }

            // Read first code+data block
            let mut nlen = self.read_be_u32().await?;
            if nlen == 0 {
                continue;
            }
            let mut code = self.read_one().await?;

            // Remove existing file and create new one
            let _ = delegate.remove(&dst).await;
            let mut file = delegate.create_file_write(&dst).await?;

            // Receive file data blocks
            while code == DL_CODE_FILE_DATA {
                let block_size = (nlen - 1) as usize;
                let data = self.read_exact(block_size).await?;
                file.write_all(&data)
                    .map_err(|e| IdeviceError::InternalError(e.to_string()))?;
                bytes_done += block_size as u64;

                // Read next block header
                nlen = self.read_be_u32().await?;
                if nlen > 0 {
                    code = self.read_one().await?;
                } else {
                    break;
                }
            }

            file_count += 1;
            delegate.on_file_received(&fname, file_count);
            delegate.on_progress(bytes_done, bytes_total, overall_progress);

            // Handle trailing error/status message
            if nlen > 0 && code != DL_CODE_FILE_DATA && code != DL_CODE_SUCCESS {
                // Consume trailing data (error messages, end-of-file markers)
                let _ = self.read_exact((nlen - 1) as usize).await?;
            }
        }

        debug!("Received {file_count} files from device");
        self.send_status_response(0, None, Some(plist::Value::Dictionary(Dictionary::new())))
            .await
    }

    async fn read_be_u32(&mut self) -> Result<u32, IdeviceError> {
        let buf = self.idevice.read_raw(4).await?;
        Ok(u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]))
    }

    async fn read_one(&mut self) -> Result<u8, IdeviceError> {
        let buf = self.idevice.read_raw(1).await?;
        Ok(buf[0])
    }

    async fn read_exact(&mut self, size: usize) -> Result<Vec<u8>, IdeviceError> {
        self.idevice.read_raw(size).await
    }

    async fn read_exact_string(&mut self, size: usize) -> Result<String, IdeviceError> {
        let buf = self.idevice.read_raw(size).await?;
        Ok(String::from_utf8_lossy(&buf).to_string())
    }

    async fn create_directory_from_message(
        dl_value: &plist::Value,
        host_dir: &Path,
        delegate: &dyn BackupDelegate,
    ) -> i64 {
        if let plist::Value::Array(arr) = dl_value
            && arr.len() >= 2
            && let Some(plist::Value::String(dir)) = arr.get(1)
        {
            let path = host_dir.join(dir);
            return match delegate.create_dir_all(&path).await {
                Ok(_) => 0,
                Err(_) => -1,
            };
        }
        -1
    }

    async fn move_files_from_message(
        dl_value: &plist::Value,
        host_dir: &Path,
        delegate: &dyn BackupDelegate,
    ) -> i64 {
        if let plist::Value::Array(arr) = dl_value
            && arr.len() >= 2
            && let Some(plist::Value::Dictionary(map)) = arr.get(1)
        {
            for (from, to_v) in map.iter() {
                if let Some(to) = to_v.as_string() {
                    let old = host_dir.join(from);
                    let newp = host_dir.join(to);
                    if let Some(parent) = newp.parent() {
                        let _ = delegate.create_dir_all(parent).await;
                    }
                    if delegate.rename(&old, &newp).await.is_err() {
                        return -1;
                    }
                }
            }
            return 0;
        }
        -1
    }

    async fn remove_files_from_message(
        dl_value: &plist::Value,
        host_dir: &Path,
        delegate: &dyn BackupDelegate,
    ) -> i64 {
        if let plist::Value::Array(arr) = dl_value
            && arr.len() >= 2
            && let Some(plist::Value::Array(items)) = arr.get(1)
        {
            for it in items {
                if let Some(p) = it.as_string() {
                    let path = host_dir.join(p);
                    if delegate.exists(&path).await && delegate.remove(&path).await.is_err() {
                        return -1;
                    }
                }
            }
            return 0;
        }
        -1
    }

    async fn copy_item_from_message(
        dl_value: &plist::Value,
        host_dir: &Path,
        delegate: &dyn BackupDelegate,
    ) -> i64 {
        if let plist::Value::Array(arr) = dl_value
            && arr.len() >= 3
            && let (Some(plist::Value::String(src)), Some(plist::Value::String(dst))) =
                (arr.get(1), arr.get(2))
        {
            let from = host_dir.join(src);
            let to = host_dir.join(dst);
            if let Some(parent) = to.parent() {
                let _ = delegate.create_dir_all(parent).await;
            }
            return match delegate.copy(&from, &to).await {
                Ok(_) => 0,
                Err(_) => -1,
            };
        }
        -1
    }

    /// Starts a restore using the typed RestoreOptions builder
    #[deprecated(
        note = "Use restore_from_path; restore via BackupUUID is not supported by device/mobilebackup2"
    )]
    pub async fn start_restore_with(
        &mut self,
        _backup_uuid: &str,
        opts: RestoreOptions,
    ) -> Result<(), IdeviceError> {
        let dict = opts.to_plist();
        // Avoid borrowing self during request
        let target_udid_owned = self.idevice.udid().map(|s| s.to_string());
        let target_udid = target_udid_owned.as_deref();
        self.send_request(
            BackupMessageType::BackupMessageTypeRestore.as_str(),
            target_udid,
            target_udid,
            Some(dict),
        )
        .await?;

        let response = self.receive_backup_response().await?;
        if let Some(error) = response.get("ErrorCode") {
            warn!("Restore start failed with error: {error:?}");
            return Err(IdeviceError::UnexpectedResponse(
                "restore start returned ErrorCode".into(),
            ));
        }
        debug!("Restore started successfully");
        Ok(())
    }

    /// Assert a complete backup dir structure exists (Info + Manifest + Status plists)
    async fn assert_backup_exists(
        backup_root: &Path,
        source: &str,
        delegate: &dyn BackupDelegate,
    ) -> Result<(), IdeviceError> {
        let device_dir = backup_root.join(source);
        if delegate.exists(&device_dir.join("Info.plist")).await
            && delegate.exists(&device_dir.join("Manifest.plist")).await
            && delegate.exists(&device_dir.join("Status.plist")).await
        {
            Ok(())
        } else {
            Err(IdeviceError::NotFound)
        }
    }

    /// Assert a backup dir has at least a Manifest.plist (enough for unback/extract)
    async fn assert_backup_has_manifest(
        backup_root: &Path,
        source: &str,
        delegate: &dyn BackupDelegate,
    ) -> Result<(), IdeviceError> {
        let device_dir = backup_root.join(source);
        if delegate.exists(&device_dir.join("Manifest.plist")).await {
            Ok(())
        } else {
            Err(IdeviceError::NotFound)
        }
    }

    /// Get backup information using DeviceLink against a given backup root/source
    pub async fn info_from_path(
        &mut self,
        backup_root: &Path,
        source_identifier: Option<&str>,
        delegate: &dyn BackupDelegate,
    ) -> Result<Dictionary, IdeviceError> {
        let target_udid = self.idevice.udid();
        let source = source_identifier
            .or(target_udid)
            .ok_or(IdeviceError::InvalidHostID)?;
        Self::assert_backup_exists(backup_root, source, delegate).await?;

        let dict = crate::plist!(dict {
            "TargetIdentifier": target_udid.unwrap(),
            "SourceIdentifier":? source_identifier,
        });
        self.send_device_link_message("Info", Some(dict)).await?;

        match self.process_dl_loop(backup_root, delegate).await? {
            Some(res) => Ok(res),
            None => Err(IdeviceError::UnexpectedResponse(
                "info_from_path DL loop returned no response".into(),
            )),
        }
    }

    /// List last backup contents (returns raw response dictionary)
    pub async fn list_from_path(
        &mut self,
        backup_root: &Path,
        source_identifier: Option<&str>,
        delegate: &dyn BackupDelegate,
    ) -> Result<Dictionary, IdeviceError> {
        let target_udid = self.idevice.udid();
        let source = source_identifier
            .or(target_udid)
            .ok_or(IdeviceError::InvalidHostID)?;
        Self::assert_backup_exists(backup_root, source, delegate).await?;

        let dict = crate::plist!(dict {
            "MessageName": "List",
            "TargetIdentifier": target_udid.unwrap(),
            "SourceIdentifier": source,
        });
        self.send_device_link_message("List", Some(dict)).await?;

        match self.process_dl_loop(backup_root, delegate).await? {
            Some(res) => Ok(res),
            None => Err(IdeviceError::UnexpectedResponse(
                "list_from_path DL loop returned no response".into(),
            )),
        }
    }

    /// Unpack a complete backup to the device's original directory hierarchy.
    ///
    /// The device reads the backup manifest and blobs, reassembles the original
    /// files, and streams them back to the host under a `_unback_/` subdirectory.
    ///
    /// **Note:** Apple broke the Unback command in iOS 10+. The device will accept
    /// the request and read the manifest, but then drops the connection before
    /// sending any unpacked files. This only works reliably on iOS 9 and earlier.
    pub async fn unback_from_path(
        &mut self,
        backup_root: &Path,
        password: Option<&str>,
        source_identifier: Option<&str>,
        delegate: &dyn BackupDelegate,
    ) -> Result<(), IdeviceError> {
        let target_udid_owned = self.idevice.udid().map(|s| s.to_string());
        let target_udid = target_udid_owned.as_deref();
        let source: &str = match source_identifier {
            Some(s) => s,
            None => target_udid.ok_or(IdeviceError::InvalidHostID)?,
        };
        Self::assert_backup_has_manifest(backup_root, source, delegate).await?;

        let opts = password.map(|pw| crate::plist!(dict { "Password": pw }));
        self.send_request("Unback", target_udid, Some(source), opts)
            .await?;
        let _ = self.process_dl_loop(backup_root, delegate).await?;
        Ok(())
    }

    /// Extract a single file from a previous backup
    pub async fn extract_from_path(
        &mut self,
        domain_name: &str,
        relative_path: &str,
        backup_root: &Path,
        password: Option<&str>,
        source_identifier: Option<&str>,
        delegate: &dyn BackupDelegate,
    ) -> Result<(), IdeviceError> {
        let target_udid = self.idevice.udid();
        let source = source_identifier
            .or(target_udid)
            .ok_or(IdeviceError::InvalidHostID)?;
        Self::assert_backup_has_manifest(backup_root, source, delegate).await?;
        let dict = crate::plist!(dict {
            "MessageName": "Extract",
            "TargetIdentifier": target_udid.unwrap(),
            "DomainName": domain_name,
            "RelativePath": relative_path,
            "SourceIdentifier": source,
            "Password":? password,
        });
        self.send_device_link_message("Extract", Some(dict)).await?;
        let _ = self.process_dl_loop(backup_root, delegate).await?;
        Ok(())
    }

    /// Change backup password (enable/disable if new/old missing)
    pub async fn change_password_from_path(
        &mut self,
        backup_root: &Path,
        old: Option<&str>,
        new: Option<&str>,
        delegate: &dyn BackupDelegate,
    ) -> Result<(), IdeviceError> {
        let target_udid = self.idevice.udid();
        let dict = crate::plist!(dict {
            "MessageName": "ChangePassword",
            "TargetIdentifier": target_udid.ok_or(IdeviceError::InvalidHostID)?,
            "OldPassword":? old,
            "NewPassword":? new
        });
        self.send_device_link_message("ChangePassword", Some(dict))
            .await?;
        let _ = self.process_dl_loop(backup_root, delegate).await?;
        Ok(())
    }

    /// Erase device via mobilebackup2
    pub async fn erase_device_from_path(
        &mut self,
        backup_root: &Path,
        delegate: &dyn BackupDelegate,
    ) -> Result<(), IdeviceError> {
        let target_udid = self.idevice.udid();
        let dict = crate::plist!(dict {
            "MessageName": "EraseDevice",
            "TargetIdentifier": target_udid.ok_or(IdeviceError::InvalidHostID)?
        });
        self.send_device_link_message("EraseDevice", Some(dict))
            .await?;
        let _ = self.process_dl_loop(backup_root, delegate).await?;
        Ok(())
    }

    /// Gets free space information from the device
    ///
    /// # Returns
    /// Free space in bytes
    ///
    /// # Errors
    /// Returns `IdeviceError` if the request fails
    pub async fn get_freespace(&mut self) -> Result<u64, IdeviceError> {
        // Not a valid host-initiated request in protocol; device asks via DLMessageGetFreeDiskSpace
        Err(IdeviceError::UnexpectedResponse(
            "get_freespace is not a valid host-initiated request".into(),
        ))
    }

    /// Checks if backup encryption is enabled
    ///
    /// # Returns
    /// `true` if backup encryption is enabled, `false` otherwise
    ///
    /// # Errors
    /// Returns `IdeviceError` if the request fails
    pub async fn check_backup_encryption(&mut self) -> Result<bool, IdeviceError> {
        // Not part of host-initiated MB2 protocol; caller should inspect Manifest/lockdown
        Err(IdeviceError::UnexpectedResponse(
            "check_backup_encryption is not a valid host-initiated request".into(),
        ))
    }

    /// Lists the contents of a directory referenced in a `DLContentsOfDirectory` message.
    async fn list_directory_contents(
        dl_value: &plist::Value,
        host_dir: &Path,
        delegate: &dyn BackupDelegate,
    ) -> plist::Value {
        let mut dirlist = Dictionary::new();

        let rel_path = if let plist::Value::Array(arr) = dl_value
            && arr.len() >= 2
            && let Some(plist::Value::String(dir)) = arr.get(1)
        {
            dir.clone()
        } else {
            return plist::Value::Dictionary(dirlist);
        };

        let full_path = host_dir.join(&rel_path);
        if let Ok(entries) = delegate.list_dir(&full_path).await {
            for entry in entries {
                let mut fdict = Dictionary::new();
                let ftype = if entry.is_dir {
                    "DLFileTypeDirectory"
                } else if entry.is_file {
                    "DLFileTypeRegular"
                } else {
                    "DLFileTypeUnknown"
                };
                fdict.insert("DLFileType".into(), plist::Value::String(ftype.into()));
                fdict.insert(
                    "DLFileSize".into(),
                    plist::Value::Integer(entry.size.into()),
                );
                if let Some(mtime) = entry.modified {
                    fdict.insert(
                        "DLFileModificationDate".into(),
                        plist::Value::Date(mtime.into()),
                    );
                }
                dirlist.insert(entry.name, plist::Value::Dictionary(fdict));
            }
        }

        plist::Value::Dictionary(dirlist)
    }

    /// Disconnects from the backup service
    ///
    /// # Returns
    /// `Ok(())` on successful disconnection
    ///
    /// # Errors
    /// Returns `IdeviceError` if disconnection fails
    pub async fn disconnect(&mut self) -> Result<(), IdeviceError> {
        // Send DLMessageDisconnect array per DeviceLink protocol
        let arr = crate::plist!(array [
            "DLMessageDisconnect",
            "___EmptyParameterString___"
        ]);
        self.send_dl_array(arr).await?;
        debug!("Disconnected from backup service");
        Ok(())
    }
}

#[cfg(feature = "rsd")]
impl crate::RsdService for MobileBackup2Client {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        crate::obf!("com.apple.mobilebackup2.shim.remote")
    }
    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = crate::Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        let mut client = Self::new(idevice);
        client.dl_version_exchange().await?;
        client.version_exchange().await?;
        Ok(client)
    }
}
