use async_zip::base::read::seek::ZipFileReader;
use futures::AsyncReadExt as _;
use plist_macro::plist;
use std::{io::Cursor, path::Path};
use tokio::io::{AsyncBufRead, AsyncSeek, BufReader};

use crate::{
    IdeviceError, IdeviceService,
    afc::{AfcClient, opcode::AfcFopenMode},
    installation_proxy::InstallationProxyError,
    provider::IdeviceProvider,
};

#[cfg(feature = "rsd")]
use crate::{RsdService, provider::RsdProvider, rsd};

pub const PUBLIC_STAGING: &str = "PublicStaging";

pub const IPCC_REMOTE_FILE: &str = "idevice.ipcc";

pub const IPA_REMOTE_FILE: &str = "idevice.ipa";

/// Result of a prepared upload, containing the remote path to use in Install/Upgrade
pub struct InstallPackage {
    /// Path inside the AFC jail for InstallationProxy `PackagePath`
    pub remote_package_path: String,

    // Each package type has a special option that has to be passed
    pub options: plist::Value,
}

/// Represent the type of package being installed.
pub enum PackageType {
    Ipcc, // Carrier bundle package
    // an IPA package needs the build id to be installed
    Ipa(String), // iOS app package
    Unknown,
}

impl PackageType {
    pub fn get_remote_file(&self) -> Result<&'static str, IdeviceError> {
        match self {
            Self::Ipcc => Ok(IPCC_REMOTE_FILE),
            Self::Ipa(_) => Ok(IPA_REMOTE_FILE),
            Self::Unknown => {
                Err(InstallationProxyError::OperationFailed("invalid package".into()).into())
            }
        }
    }
}

/// Ensure `PublicStaging` exists on device via AFC
pub async fn ensure_public_staging(afc: &mut AfcClient) -> Result<(), IdeviceError> {
    // Try to stat and if it fails, create directory
    match afc.get_file_info(PUBLIC_STAGING).await {
        Ok(_) => Ok(()),
        Err(_) => afc.mk_dir(PUBLIC_STAGING).await,
    }
}

// Get the bundle id of a package by looping through it's files and looking inside of the
// `Info.plist`
pub async fn get_bundle_id<T>(file: &mut T) -> Result<String, IdeviceError>
where
    T: AsyncBufRead + AsyncSeek + Unpin,
{
    let mut zip_file = ZipFileReader::with_tokio(file)
        .await
        .map_err(InstallationProxyError::from)?;

    for i in 0..zip_file.file().entries().len() {
        let mut entry_reader = zip_file
            .reader_with_entry(i)
            .await
            .map_err(InstallationProxyError::from)?;
        let entry = entry_reader.entry();

        let inner_file_path = entry
            .filename()
            .as_str()
            .map_err(|_| IdeviceError::Utf8Error)?
            .trim_end_matches('/');

        let path_segments_count = inner_file_path.split('/').count();

        // there's multiple `Info.plist` files, we only need the one that's in the root of the
        // package
        //
        //                           1             2              3
        // which is in this case: Playload -> APP_NAME.app -> Info.plist
        if inner_file_path.ends_with("Info.plist") && path_segments_count == 3 {
            let mut info_plist_bytes = Vec::new();
            entry_reader.read_to_end(&mut info_plist_bytes).await?;

            let info_plist: plist::Value = plist::from_bytes(&info_plist_bytes)?;

            if let Some(bundle_id) = info_plist
                .as_dictionary()
                .and_then(|dict| dict.get("CFBundleIdentifier"))
                .and_then(|v| v.as_string())
            {
                return Ok(bundle_id.to_string());
            }
        }
    }

    Err(IdeviceError::NotFound)
}

/// Determines the type of package based on its content (IPA or IPCC).
pub async fn determine_package_type<P: AsRef<[u8]>>(
    package: &P,
) -> Result<PackageType, IdeviceError> {
    let mut package_cursor = BufReader::new(Cursor::new(package.as_ref()));

    let mut archive = ZipFileReader::with_tokio(&mut package_cursor)
        .await
        .map_err(InstallationProxyError::from)?;

    // the first index is the first folder name, which is probably `Payload`
    //
    // we need the folder inside of that `Payload`, which has an extension that we can
    // determine the type of the package from it, hence the second index
    let inside_folder = archive
        .reader_with_entry(1)
        .await
        .map_err(InstallationProxyError::from)?;

    let folder_name = inside_folder
        .entry()
        .filename()
        .as_str()
        .map_err(|_| IdeviceError::Utf8Error)?
        .split('/')
        .nth(1)
        // only if the package does not have anything inside of the `Payload` folder
        .ok_or(InstallationProxyError::from(
            async_zip::error::ZipError::EntryIndexOutOfBounds,
        ))?
        .to_string();

    let bundle_id = get_bundle_id(&mut package_cursor).await?;

    if folder_name.ends_with(".bundle") {
        Ok(PackageType::Ipcc)
    } else if folder_name.ends_with(".app") {
        Ok(PackageType::Ipa(bundle_id))
    } else {
        Ok(PackageType::Unknown)
    }
}

/// Upload a single file to a destination path on device using AFC
pub async fn afc_upload_file<F: AsRef<[u8]>>(
    afc: &mut AfcClient,
    file: F,
    remote_path: &str,
) -> Result<(), IdeviceError> {
    let mut fd = afc.open(remote_path, AfcFopenMode::WrOnly).await?;
    fd.write_entire(file.as_ref()).await?;
    fd.close().await
}

/// Recursively upload a directory to device via AFC (mirror contents)
pub async fn afc_upload_dir(
    afc: &mut AfcClient,
    local_dir: &Path,
    remote_dir: &str,
) -> Result<(), IdeviceError> {
    use std::collections::VecDeque;
    afc.mk_dir(remote_dir).await.ok();

    let mut queue: VecDeque<(std::path::PathBuf, String)> = VecDeque::new();
    queue.push_back((local_dir.to_path_buf(), remote_dir.to_string()));

    while let Some((cur_local, cur_remote)) = queue.pop_front() {
        let mut rd = tokio::fs::read_dir(&cur_local).await?;
        while let Some(entry) = rd.next_entry().await? {
            let meta = entry.metadata().await?;
            let name = entry.file_name();
            let name = name.to_string_lossy().into_owned();
            if name == "." || name == ".." {
                continue;
            }
            let child_local = entry.path();
            let child_remote = format!("{cur_remote}/{name}");
            if meta.is_dir() {
                afc.mk_dir(&child_remote).await.ok();
                queue.push_back((child_local, child_remote));
            } else if meta.is_file() {
                afc_upload_file(afc, tokio::fs::read(&child_local).await?, &child_remote).await?;
            }
        }
    }
    Ok(())
}

/// Upload a file to `PublicStaging` and return its InstallationProxy path
async fn upload_file_to_public_staging<P: AsRef<[u8]>>(
    provider: &dyn IdeviceProvider,
    file: P,
) -> Result<InstallPackage, IdeviceError> {
    // Connect to AFC via the generic service connector
    let mut afc = AfcClient::connect(provider).await?;

    ensure_public_staging(&mut afc).await?;

    let file = file.as_ref();

    let package_type = determine_package_type(&file).await?;

    let remote_path = format!("{PUBLIC_STAGING}/{}", package_type.get_remote_file()?);

    afc_upload_file(&mut afc, file, &remote_path).await?;

    let options = match package_type {
        PackageType::Ipcc => plist!({"PackageType": "CarrierBundle"}),
        PackageType::Ipa(build_id) => plist!({"CFBundleIdentifier": build_id}),
        PackageType::Unknown => plist!({}),
    };

    Ok(InstallPackage {
        remote_package_path: remote_path,
        options,
    })
}

/// Upload a file to `PublicStaging` over RSD and return its InstallationProxy path
#[cfg(feature = "rsd")]
async fn upload_file_to_public_staging_rsd<P: AsRef<[u8]>>(
    provider: &mut impl RsdProvider,
    handshake: &mut rsd::RsdHandshake,
    file: P,
) -> Result<InstallPackage, IdeviceError> {
    let mut afc = AfcClient::connect_rsd(provider, handshake).await?;

    ensure_public_staging(&mut afc).await?;

    let file = file.as_ref();

    let package_type = determine_package_type(&file).await?;

    let remote_path = format!("{PUBLIC_STAGING}/{}", package_type.get_remote_file()?);

    afc_upload_file(&mut afc, file, &remote_path).await?;

    let options = match package_type {
        PackageType::Ipcc => plist!({"PackageType": "CarrierBundle"}),
        PackageType::Ipa(build_id) => plist!({"CFBundleIdentifier": build_id}),
        PackageType::Unknown => plist!({}),
    };

    Ok(InstallPackage {
        remote_package_path: remote_path,
        options,
    })
}

/// Recursively Upload a directory of file to `PublicStaging`
async fn upload_dir_to_public_staging<P: AsRef<Path>>(
    provider: &dyn IdeviceProvider,
    file: P,
) -> Result<InstallPackage, IdeviceError> {
    let mut afc = AfcClient::connect(provider).await?;

    ensure_public_staging(&mut afc).await?;

    let file = file.as_ref();
    let remote_folder_name = file
        .iter()
        .next_back()
        .map(|x| x.to_string_lossy().to_string())
        .unwrap_or(IPA_REMOTE_FILE.to_string());

    let remote_path = format!("{PUBLIC_STAGING}/{remote_folder_name}");

    afc_upload_dir(&mut afc, file, &remote_path).await?;

    Ok(InstallPackage {
        remote_package_path: remote_path,
        options: plist!({"PackageType": "Developer"}),
    })
}

/// Recursively upload a directory to `PublicStaging` over RSD.
#[cfg(feature = "rsd")]
async fn upload_dir_to_public_staging_rsd<P: AsRef<Path>>(
    provider: &mut impl RsdProvider,
    handshake: &mut rsd::RsdHandshake,
    file: P,
) -> Result<InstallPackage, IdeviceError> {
    let mut afc = AfcClient::connect_rsd(provider, handshake).await?;

    ensure_public_staging(&mut afc).await?;

    let file = file.as_ref();
    let remote_folder_name = file
        .iter()
        .next_back()
        .map(|x| x.to_string_lossy().to_string())
        .unwrap_or(IPA_REMOTE_FILE.to_string());

    let remote_path = format!("{PUBLIC_STAGING}/{remote_folder_name}");

    afc_upload_dir(&mut afc, file, &remote_path).await?;

    Ok(InstallPackage {
        remote_package_path: remote_path,
        options: plist!({"PackageType": "Developer"}),
    })
}

pub async fn prepare_file_upload(
    provider: &dyn IdeviceProvider,
    data: impl AsRef<[u8]>,
    caller_options: Option<plist::Value>,
) -> Result<InstallPackage, IdeviceError> {
    let InstallPackage {
        remote_package_path,
        options,
    } = upload_file_to_public_staging(provider, data).await?;
    let full_options = plist!({
        :<? caller_options,
        :< options,
    });

    Ok(InstallPackage {
        remote_package_path,
        options: full_options,
    })
}

#[cfg(feature = "rsd")]
pub async fn prepare_file_upload_rsd(
    provider: &mut impl RsdProvider,
    handshake: &mut rsd::RsdHandshake,
    data: impl AsRef<[u8]>,
    caller_options: Option<plist::Value>,
) -> Result<InstallPackage, IdeviceError> {
    let InstallPackage {
        remote_package_path,
        options,
    } = upload_file_to_public_staging_rsd(provider, handshake, data).await?;
    let full_options = plist!({
        :<? caller_options,
        :< options,
    });

    Ok(InstallPackage {
        remote_package_path,
        options: full_options,
    })
}

pub async fn prepare_dir_upload(
    provider: &dyn IdeviceProvider,
    local_path: impl AsRef<Path>,
    caller_options: Option<plist::Value>,
) -> Result<InstallPackage, IdeviceError> {
    let InstallPackage {
        remote_package_path,
        options,
    } = upload_dir_to_public_staging(provider, &local_path).await?;

    let full_options = plist!({
        :<? caller_options,
        :< options,
    });

    Ok(InstallPackage {
        remote_package_path,
        options: full_options,
    })
}

#[cfg(feature = "rsd")]
pub async fn prepare_dir_upload_rsd(
    provider: &mut impl RsdProvider,
    handshake: &mut rsd::RsdHandshake,
    local_path: impl AsRef<Path>,
    caller_options: Option<plist::Value>,
) -> Result<InstallPackage, IdeviceError> {
    let InstallPackage {
        remote_package_path,
        options,
    } = upload_dir_to_public_staging_rsd(provider, handshake, &local_path).await?;

    let full_options = plist!({
        :<? caller_options,
        :< options,
    });

    Ok(InstallPackage {
        remote_package_path,
        options: full_options,
    })
}
