//! High-level install/upgrade helpers
//!
//! This module provides convenient wrappers that mirror ideviceinstaller's
//! behavior by uploading a package to `PublicStaging` via AFC and then
//! issuing `Install`/`Upgrade` commands through InstallationProxy.
//!
//! Notes:
//! - The package path used by InstallationProxy must be a path inside the
//!   AFC jail (e.g. `PublicStaging/<name>`)
//! - For `.ipa` files, we upload the whole file to `PublicStaging/<file_name>`
//! - For directories (developer bundles), we recursively mirror the directory
//!   into `PublicStaging/<dir_name>` and pass that directory path.

mod helpers;
use std::path::Path;

use helpers::{InstallPackage, prepare_dir_upload, prepare_file_upload};

use crate::{
    IdeviceError, IdeviceService, provider::IdeviceProvider,
    services::installation_proxy::InstallationProxyClient,
};

#[cfg(feature = "rsd")]
use crate::{RsdService, provider::RsdProvider, rsd};
#[cfg(feature = "rsd")]
use helpers::{prepare_dir_upload_rsd, prepare_file_upload_rsd};

/// Install an application by first uploading the local package and then invoking InstallationProxy.
///
/// - Accepts a local file path or directory path.
/// - `options` is an InstallationProxy ClientOptions dictionary; pass `None` for defaults.
pub async fn install_package<P: AsRef<Path>>(
    provider: &dyn IdeviceProvider,
    local_path: P,
    options: Option<plist::Value>,
) -> Result<(), IdeviceError> {
    install_package_with_callback(provider, local_path, options, |_| async {}, ()).await
}

/// Same as `install_package` but using the RSD transport.
#[cfg(feature = "rsd")]
pub async fn install_package_rsd<P: AsRef<Path>>(
    provider: &mut impl RsdProvider,
    handshake: &mut rsd::RsdHandshake,
    local_path: P,
    options: Option<plist::Value>,
) -> Result<(), IdeviceError> {
    install_package_with_callback_rsd(provider, handshake, local_path, options, |_| async {}, ())
        .await
}

/// Same as `install_package` but providing a callback that receives `(percent_complete, state)`
/// updates while InstallationProxy performs the operation.
pub async fn install_package_with_callback<P: AsRef<Path>, Fut, S>(
    provider: &dyn IdeviceProvider,
    local_path: P,
    options: Option<plist::Value>,
    callback: impl Fn((u64, S)) -> Fut,
    state: S,
) -> Result<(), IdeviceError>
where
    Fut: std::future::Future<Output = ()>,
    S: Clone,
{
    let metadata = tokio::fs::metadata(&local_path).await?;

    if metadata.is_dir() {
        let InstallPackage {
            remote_package_path,
            options,
        } = prepare_dir_upload(provider, local_path, options).await?;
        let mut inst = InstallationProxyClient::connect(provider).await?;

        inst.upgrade_with_callback(remote_package_path, Some(options), callback, state)
            .await
    } else {
        let data = tokio::fs::read(&local_path).await?;
        install_bytes_with_callback(provider, data, options, callback, state).await
    }
}

/// Same as `install_package` but providing a callback that receives `(percent_complete, state)`
/// updates while InstallationProxy performs the operation.
#[cfg(feature = "rsd")]
pub async fn install_package_with_callback_rsd<P: AsRef<Path>, Fut, S>(
    provider: &mut impl RsdProvider,
    handshake: &mut rsd::RsdHandshake,
    local_path: P,
    options: Option<plist::Value>,
    callback: impl Fn((u64, S)) -> Fut,
    state: S,
) -> Result<(), IdeviceError>
where
    Fut: std::future::Future<Output = ()>,
    S: Clone,
{
    let metadata = tokio::fs::metadata(&local_path).await?;

    if metadata.is_dir() {
        let InstallPackage {
            remote_package_path,
            options,
        } = prepare_dir_upload_rsd(provider, handshake, local_path, options).await?;
        let mut inst = InstallationProxyClient::connect_rsd(provider, handshake).await?;

        inst.upgrade_with_callback(remote_package_path, Some(options), callback, state)
            .await
    } else {
        let data = tokio::fs::read(&local_path).await?;
        install_bytes_with_callback_rsd(provider, handshake, data, options, callback, state).await
    }
}

/// Upgrade an application by first uploading the local package and then invoking InstallationProxy.
///
/// - Accepts a local file path or directory path.
/// - `options` is an InstallationProxy ClientOptions dictionary; pass `None` for defaults.
pub async fn upgrade_package<P: AsRef<Path>>(
    provider: &dyn IdeviceProvider,
    local_path: P,
    options: Option<plist::Value>,
) -> Result<(), IdeviceError> {
    upgrade_package_with_callback(provider, local_path, options, |_| async {}, ()).await
}

/// Same as `upgrade_package` but using the RSD transport.
#[cfg(feature = "rsd")]
pub async fn upgrade_package_rsd<P: AsRef<Path>>(
    provider: &mut impl RsdProvider,
    handshake: &mut rsd::RsdHandshake,
    local_path: P,
    options: Option<plist::Value>,
) -> Result<(), IdeviceError> {
    upgrade_package_with_callback_rsd(provider, handshake, local_path, options, |_| async {}, ())
        .await
}

/// Same as `upgrade_package` but providing a callback that receives `(percent_complete, state)`
/// updates while InstallationProxy performs the operation.
pub async fn upgrade_package_with_callback<P: AsRef<Path>, Fut, S>(
    provider: &dyn IdeviceProvider,
    local_path: P,
    options: Option<plist::Value>,
    callback: impl Fn((u64, S)) -> Fut,
    state: S,
) -> Result<(), IdeviceError>
where
    Fut: std::future::Future<Output = ()>,
    S: Clone,
{
    let metadata = tokio::fs::metadata(&local_path).await?;

    if metadata.is_dir() {
        let InstallPackage {
            remote_package_path,
            options,
        } = prepare_dir_upload(provider, local_path, options).await?;
        let mut inst = InstallationProxyClient::connect(provider).await?;

        inst.upgrade_with_callback(remote_package_path, Some(options), callback, state)
            .await
    } else {
        let data = tokio::fs::read(&local_path).await?;
        upgrade_bytes_with_callback(provider, data, options, callback, state).await
    }
}

/// Same as `upgrade_package_with_callback` but using the RSD transport.
#[cfg(feature = "rsd")]
pub async fn upgrade_package_with_callback_rsd<P: AsRef<Path>, Fut, S>(
    provider: &mut impl RsdProvider,
    handshake: &mut rsd::RsdHandshake,
    local_path: P,
    options: Option<plist::Value>,
    callback: impl Fn((u64, S)) -> Fut,
    state: S,
) -> Result<(), IdeviceError>
where
    Fut: std::future::Future<Output = ()>,
    S: Clone,
{
    let metadata = tokio::fs::metadata(&local_path).await?;

    if metadata.is_dir() {
        let InstallPackage {
            remote_package_path,
            options,
        } = prepare_dir_upload_rsd(provider, handshake, local_path, options).await?;
        let mut inst = InstallationProxyClient::connect_rsd(provider, handshake).await?;

        inst.upgrade_with_callback(remote_package_path, Some(options), callback, state)
            .await
    } else {
        let data = tokio::fs::read(&local_path).await?;
        upgrade_bytes_with_callback_rsd(provider, handshake, data, options, callback, state).await
    }
}

/// Install an application from raw bytes by first uploading them to `PublicStaging` and then
/// invoking InstallationProxy `Install`.
///
/// - `remote_name` determines the remote filename under `PublicStaging`.
/// - `options` is an InstallationProxy ClientOptions dictionary; pass `None` for defaults.
pub async fn install_bytes(
    provider: &dyn IdeviceProvider,
    data: impl AsRef<[u8]>,
    options: Option<plist::Value>,
) -> Result<(), IdeviceError> {
    install_bytes_with_callback(provider, data, options, |_| async {}, ()).await
}

/// Same as `install_bytes` but using the RSD transport.
#[cfg(feature = "rsd")]
pub async fn install_bytes_rsd(
    provider: &mut impl RsdProvider,
    handshake: &mut rsd::RsdHandshake,
    data: impl AsRef<[u8]>,
    options: Option<plist::Value>,
) -> Result<(), IdeviceError> {
    install_bytes_with_callback_rsd(provider, handshake, data, options, |_| async {}, ()).await
}

/// Same as `install_bytes` but providing a callback that receives `(percent_complete, state)`
/// updates while InstallationProxy performs the install operation.
///
/// Tip:
/// - When embedding assets into the binary, you can pass `include_bytes!("path/to/app.ipa")`
///   as the `data` argument and choose a desired `remote_name` (e.g. `"MyApp.ipa"`).
pub async fn install_bytes_with_callback<Fut, S>(
    provider: &dyn IdeviceProvider,
    data: impl AsRef<[u8]>,
    options: Option<plist::Value>,
    callback: impl Fn((u64, S)) -> Fut,
    state: S,
) -> Result<(), IdeviceError>
where
    Fut: std::future::Future<Output = ()>,
    S: Clone,
{
    let InstallPackage {
        remote_package_path,
        options,
    } = prepare_file_upload(provider, data, options).await?;
    let mut inst = InstallationProxyClient::connect(provider).await?;

    inst.install_with_callback(remote_package_path, Some(options), callback, state)
        .await
}

/// Same as `install_bytes_with_callback` but using the RSD transport.
#[cfg(feature = "rsd")]
pub async fn install_bytes_with_callback_rsd<Fut, S>(
    provider: &mut impl RsdProvider,
    handshake: &mut rsd::RsdHandshake,
    data: impl AsRef<[u8]>,
    options: Option<plist::Value>,
    callback: impl Fn((u64, S)) -> Fut,
    state: S,
) -> Result<(), IdeviceError>
where
    Fut: std::future::Future<Output = ()>,
    S: Clone,
{
    let InstallPackage {
        remote_package_path,
        options,
    } = prepare_file_upload_rsd(provider, handshake, data, options).await?;
    let mut inst = InstallationProxyClient::connect_rsd(provider, handshake).await?;

    inst.install_with_callback(remote_package_path, Some(options), callback, state)
        .await
}

/// Upgrade an application from raw bytes by first uploading them to `PublicStaging` and then
/// invoking InstallationProxy `Upgrade`.
///
/// - `remote_name` determines the remote filename under `PublicStaging`.
/// - `options` is an InstallationProxy ClientOptions dictionary; pass `None` for defaults.
pub async fn upgrade_bytes(
    provider: &dyn IdeviceProvider,
    data: impl AsRef<[u8]>,
    options: Option<plist::Value>,
) -> Result<(), IdeviceError> {
    upgrade_bytes_with_callback(provider, data, options, |_| async {}, ()).await
}

/// Same as `upgrade_bytes` but using the RSD transport.
#[cfg(feature = "rsd")]
pub async fn upgrade_bytes_rsd(
    provider: &mut impl RsdProvider,
    handshake: &mut rsd::RsdHandshake,
    data: impl AsRef<[u8]>,
    options: Option<plist::Value>,
) -> Result<(), IdeviceError> {
    upgrade_bytes_with_callback_rsd(provider, handshake, data, options, |_| async {}, ()).await
}

/// Same as `upgrade_bytes` but providing a callback that receives `(percent_complete, state)`
/// updates while InstallationProxy performs the upgrade operation.
///
/// Tip:
/// - When embedding assets into the binary, you can pass `include_bytes!("path/to/app.ipa")`
///   as the `data` argument and choose a desired `remote_name` (e.g. `"MyApp.ipa"`).
pub async fn upgrade_bytes_with_callback<Fut, S>(
    provider: &dyn IdeviceProvider,
    data: impl AsRef<[u8]>,
    options: Option<plist::Value>,
    callback: impl Fn((u64, S)) -> Fut,
    state: S,
) -> Result<(), IdeviceError>
where
    Fut: std::future::Future<Output = ()>,
    S: Clone,
{
    let InstallPackage {
        remote_package_path,
        options,
    } = prepare_file_upload(provider, data, options).await?;
    let mut inst = InstallationProxyClient::connect(provider).await?;

    inst.upgrade_with_callback(remote_package_path, Some(options), callback, state)
        .await
}

/// Same as `upgrade_bytes_with_callback` but using the RSD transport.
#[cfg(feature = "rsd")]
pub async fn upgrade_bytes_with_callback_rsd<Fut, S>(
    provider: &mut impl RsdProvider,
    handshake: &mut rsd::RsdHandshake,
    data: impl AsRef<[u8]>,
    options: Option<plist::Value>,
    callback: impl Fn((u64, S)) -> Fut,
    state: S,
) -> Result<(), IdeviceError>
where
    Fut: std::future::Future<Output = ()>,
    S: Clone,
{
    let InstallPackage {
        remote_package_path,
        options,
    } = prepare_file_upload_rsd(provider, handshake, data, options).await?;
    let mut inst = InstallationProxyClient::connect_rsd(provider, handshake).await?;

    inst.upgrade_with_callback(remote_package_path, Some(options), callback, state)
        .await
}
