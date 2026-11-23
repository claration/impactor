//! iOS Image Mounter Client
//!
//! Provides functionality for mounting disk images on iOS devices, including:
//! - Developer disk images
//! - Personalized images
//! - Cryptex images
//!
//! Handles the complete workflow from uploading images to mounting them with proper signatures.

use tracing::debug;

#[cfg(feature = "rsd")]
use crate::RsdService;
use crate::{Idevice, IdeviceError, IdeviceService, obf};
use sha2::{Digest, Sha384};

#[cfg(feature = "tss")]
use crate::tss::TSSRequest;

/// Client for interacting with the iOS mobile image mounter service
///
/// Manages mounted images on the device.
///
/// # Important Note
/// A lockdown client must be established and queried after establishing a mounter client,
/// or the device will stop responding to requests.
#[derive(Debug)]
pub struct ImageMounter {
    /// The underlying device connection with established image mounter service
    idevice: Idevice,
}

impl IdeviceService for ImageMounter {
    /// Returns the image mounter service name as registered with lockdownd
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.mobile.mobile_image_mounter")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl ImageMounter {
    /// Creates a new image mounter client from an existing device connection
    ///
    /// # Arguments
    /// * `idevice` - Pre-established device connection
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    /// Retrieves a list of currently mounted devices
    ///
    /// # Returns
    /// A vector of plist values describing mounted devices
    ///
    /// # Errors
    /// Returns `IdeviceError` if communication fails or response is malformed
    pub async fn copy_devices(&mut self) -> Result<Vec<plist::Value>, IdeviceError> {
        let req = crate::plist!({
            "Command": "CopyDevices"
        });
        self.idevice.send_plist(req).await?;
        let mut res = self.idevice.read_plist().await?;

        match res.remove("EntryList") {
            Some(plist::Value::Array(i)) => Ok(i),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing EntryList array in CopyDevices response".into(),
            )),
        }
    }

    /// Looks up an image by type and returns its signature
    ///
    /// # Arguments
    /// * `image_type` - The type of image to lookup (e.g., "Developer")
    ///
    /// # Returns
    /// The image signature if found
    ///
    /// # Errors
    /// Returns `IdeviceError::NotFound` if image doesn't exist
    pub async fn lookup_image(
        &mut self,
        image_type: impl Into<&str>,
    ) -> Result<Vec<u8>, IdeviceError> {
        let image_type = image_type.into();
        let req = crate::plist!({
            "Command": "LookupImage",
            "ImageType": image_type
        });
        self.idevice.send_plist(req).await?;

        let res = self.idevice.read_plist().await?;
        match res
            .get("ImageSignature")
            .and_then(|x| x.as_array())
            .and_then(|x| x.first())
        {
            Some(plist::Value::Data(signature)) => Ok(signature.clone()),
            _ => Err(IdeviceError::NotFound),
        }
    }

    /// Uploads an image to the device
    ///
    /// # Arguments
    /// * `image_type` - Type of image being uploaded
    /// * `image` - The image data
    /// * `signature` - Signature for the image
    ///
    /// # Errors
    /// Returns `IdeviceError` if upload fails
    pub async fn upload_image(
        &mut self,
        image_type: impl Into<String>,
        image: &[u8],
        signature: Vec<u8>,
    ) -> Result<(), IdeviceError> {
        self.upload_image_with_progress(image_type, image, signature, |_| async {}, ())
            .await
    }

    /// Uploads an image with progress callbacks
    ///
    /// # Arguments
    /// * `image_type` - Type of image being uploaded
    /// * `image` - The image data
    /// * `signature` - Signature for the image
    /// * `callback` - Progress callback
    /// * `state` - State to pass to callback
    ///
    /// # Type Parameters
    /// * `Fut` - Future type returned by callback
    /// * `S` - Type of state passed to callback
    ///
    /// # Errors
    /// Returns `IdeviceError` if upload fails
    pub async fn upload_image_with_progress<Fut, S>(
        &mut self,
        image_type: impl Into<String>,
        image: &[u8],
        signature: Vec<u8>,
        callback: impl Fn(((usize, usize), S)) -> Fut,
        state: S,
    ) -> Result<(), IdeviceError>
    where
        Fut: std::future::Future<Output = ()>,
        S: Clone,
    {
        let image_type = image_type.into();
        let image_size = match u64::try_from(image.len()) {
            Ok(i) => i,
            Err(e) => {
                tracing::error!("Could not parse image size as u64: {e:?}");
                return Err(IdeviceError::UnexpectedResponse(
                    "image size exceeds u64 range".into(),
                ));
            }
        };

        let req = crate::plist!({
            "Command": "ReceiveBytes",
            "ImageType": image_type,
            "ImageSize": image_size,
            "ImageSignature": signature,
        });
        self.idevice.send_plist(req).await?;

        let res = self.idevice.read_plist().await?;
        match res.get("Status") {
            Some(plist::Value::String(s)) => {
                if s.as_str() != "ReceiveBytesAck" {
                    tracing::error!("Received bad response to SendBytes: {s:?}");
                    return Err(IdeviceError::UnexpectedResponse(
                        "expected ReceiveBytesAck Status in upload response".into(),
                    ));
                }
            }
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing Status in ReceiveBytes response".into(),
                ));
            }
        }

        debug!("Sending image bytes");
        self.idevice
            .send_raw_with_progress(image, callback, state)
            .await?;

        let res = self.idevice.read_plist().await?;
        match res.get("Status") {
            Some(plist::Value::String(s)) => {
                if s.as_str() != "Complete" {
                    tracing::error!("Image send failure: {s:?}");
                    return Err(IdeviceError::UnexpectedResponse(
                        "expected Complete Status after image upload".into(),
                    ));
                }
            }
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing Status after image upload".into(),
                ));
            }
        }

        Ok(())
    }

    /// Mounts an image on the device
    ///
    /// # Arguments
    /// * `image_type` - Type of image to mount
    /// * `signature` - Signature for the image
    /// * `trust_cache` - Optional trust cache data
    /// * `info_plist` - Optional info plist for the image
    ///
    /// # Errors
    /// Returns `IdeviceError` if mounting fails
    pub async fn mount_image(
        &mut self,
        image_type: impl Into<String>,
        signature: Vec<u8>,
        trust_cache: Option<Vec<u8>>,
        info_plist: Option<plist::Value>,
    ) -> Result<(), IdeviceError> {
        let image_type = image_type.into();

        let req = crate::plist!({
            "Command": "MountImage",
            "ImageType": image_type,
            "ImageSignature": signature,
            "ImageTrustCache":? trust_cache,
            "ImageInfoPlist":? info_plist,
        });
        self.idevice.send_plist(req).await?;

        let res = self.idevice.read_plist().await?;

        match res.get("Status") {
            Some(plist::Value::String(s)) => {
                if s.as_str() != "Complete" {
                    tracing::error!("Image send failure: {s:?}");
                    return Err(IdeviceError::UnexpectedResponse(
                        "expected Complete Status in MountImage response".into(),
                    ));
                }
            }
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing Status in MountImage response".into(),
                ));
            }
        }

        Ok(())
    }

    /// Unmounts an image at the specified path
    ///
    /// # Arguments
    /// * `mount_path` - Path where image is mounted:
    ///   - `/Developer` for pre-iOS 17 developer images
    ///   - `/System/Developer` for personalized images
    ///
    /// # Errors
    /// Returns `IdeviceError` if unmounting fails
    pub async fn unmount_image(
        &mut self,
        mount_path: impl Into<String>,
    ) -> Result<(), IdeviceError> {
        let mount_path = mount_path.into();
        let req = crate::plist!({
            "Command": "UnmountImage",
            "MountPath": mount_path,
        });
        self.idevice.send_plist(req).await?;

        let res = self.idevice.read_plist().await?;
        match res.get("Status") {
            Some(plist::Value::String(s)) if s.as_str() == "Complete" => Ok(()),
            _ => Err(IdeviceError::UnexpectedResponse(
                "expected Complete Status in UnmountImage response".into(),
            )),
        }
    }

    /// Queries the personalization manifest from the device
    ///
    /// # Important
    /// On failure, the socket must be closed and reestablished.
    ///
    /// # Arguments
    /// * `image_type` - Type of image to query manifest for
    /// * `signature` - Signature of the image
    ///
    /// # Returns
    /// The personalization manifest data
    ///
    /// # Errors
    /// Returns `IdeviceError` if query fails
    pub async fn query_personalization_manifest(
        &mut self,
        image_type: impl Into<String>,
        signature: Vec<u8>,
    ) -> Result<Vec<u8>, IdeviceError> {
        let image_type = image_type.into();

        let req = crate::plist!({
            "Command": "QueryPersonalizationManifest",
            "PersonalizedImageType": image_type.clone(),
            "ImageType": image_type,
            "ImageSignature": signature
        });
        self.idevice.send_plist(req).await?;

        let mut res = self.idevice.read_plist().await?;
        match res.remove("ImageSignature") {
            Some(plist::Value::Data(i)) => Ok(i),
            _ => Err(IdeviceError::NotFound),
        }
    }

    /// Queries the developer mode status of the device
    ///
    /// # Returns
    /// `true` if developer mode is enabled, `false` otherwise
    ///
    /// # Errors
    /// Returns `IdeviceError` if query fails
    pub async fn query_developer_mode_status(&mut self) -> Result<bool, IdeviceError> {
        let req = crate::plist!({
            "Command": "QueryDeveloperModeStatus"
        });
        self.idevice.send_plist(req).await?;

        let res = self.idevice.read_plist().await?;
        match res.get("DeveloperModeStatus") {
            Some(plist::Value::Boolean(status)) => Ok(*status),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing DeveloperModeStatus boolean in response".into(),
            )),
        }
    }

    /// Queries the nonce value from the device
    ///
    /// # Arguments
    /// * `personalized_image_type` - Optional image type to get nonce for
    ///
    /// # Returns
    /// The nonce value
    ///
    /// # Errors
    /// Returns `IdeviceError` if query fails
    pub async fn query_nonce(
        &mut self,
        personalized_image_type: Option<&str>,
    ) -> Result<Vec<u8>, IdeviceError> {
        let req = crate::plist!({
            "Command": "QueryNonce",
            "PersonalizedImageType":? personalized_image_type,
        });
        self.idevice.send_plist(req).await?;

        let res = self.idevice.read_plist().await?;
        match res.get("PersonalizationNonce") {
            Some(plist::Value::Data(nonce)) => Ok(nonce.clone()),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing PersonalizationNonce data in response".into(),
            )),
        }
    }

    /// Queries personalization identifiers from the device
    ///
    /// # Arguments
    /// * `image_type` - Optional image type to get identifiers for
    ///
    /// # Returns
    /// Dictionary of personalization identifiers
    ///
    /// # Errors
    /// Returns `IdeviceError` if query fails
    pub async fn query_personalization_identifiers(
        &mut self,
        image_type: Option<&str>,
    ) -> Result<plist::Dictionary, IdeviceError> {
        let req = crate::plist!({
            "Command": "QueryPersonalizationIdentifiers",
            "PersonalizedImageType":? image_type,
        });
        self.idevice.send_plist(req).await?;

        let res = self.idevice.read_plist().await?;
        match res.get("PersonalizationIdentifiers") {
            Some(plist::Value::Dictionary(identifiers)) => Ok(identifiers.clone()),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing PersonalizationIdentifiers dictionary in response".into(),
            )),
        }
    }

    /// Rolls the personalization nonce on the device
    ///
    /// # Errors
    /// Returns `IdeviceError` if operation fails
    pub async fn roll_personalization_nonce(&mut self) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "Command": "RollPersonalizationNonce"
        });
        self.idevice.send_plist(req).await?;

        Ok(())
    }

    /// Rolls the cryptex nonce on the device
    ///
    /// # Errors
    /// Returns `IdeviceError` if operation fails
    pub async fn roll_cryptex_nonce(&mut self) -> Result<(), IdeviceError> {
        let req = crate::plist!({
            "Command": "RollCryptexNonce"
        });
        self.idevice.send_plist(req).await?;

        Ok(())
    }

    /// Mounts a developer disk image
    ///
    /// # Arguments
    /// * `image` - The developer disk image data
    /// * `signature` - Signature for the image
    ///
    /// # Errors
    /// Returns `IdeviceError` if mounting fails
    pub async fn mount_developer(
        &mut self,
        image: &[u8],
        signature: Vec<u8>,
    ) -> Result<(), IdeviceError> {
        self.upload_image("Developer", image, signature.clone())
            .await?;
        self.mount_image("Developer", signature, None, None).await?;

        Ok(())
    }

    #[cfg(feature = "tss")]
    /// Mounts a personalized image with automatic manifest handling
    ///
    /// # Arguments
    /// * `provider` - Device connection provider (used for reconnection if needed)
    /// * `image` - The image data
    /// * `trust_cache` - Trust cache data
    /// * `build_manifest` - Build manifest data
    /// * `info_plist` - Optional info plist for the image
    /// * `unique_chip_id` - Device's unique chip ID
    ///
    /// # Errors
    /// Returns `IdeviceError` if mounting fails
    pub async fn mount_personalized(
        &mut self,
        provider: &dyn crate::provider::IdeviceProvider,
        image: Vec<u8>,
        trust_cache: Vec<u8>,
        build_manifest: &[u8],
        info_plist: Option<plist::Value>,
        unique_chip_id: u64,
    ) -> Result<(), IdeviceError> {
        self.mount_personalized_with_callback(
            provider,
            image,
            trust_cache,
            build_manifest,
            info_plist,
            unique_chip_id,
            |_| async {},
            (),
        )
        .await
    }

    #[cfg(feature = "tss")]
    /// Mounts a personalized image with progress callbacks
    ///
    /// # Important
    /// This may close the socket on failure, requiring reconnection.
    ///
    /// # Arguments
    /// * `provider` - Device connection provider
    /// * `image` - The image data
    /// * `trust_cache` - Trust cache data
    /// * `build_manifest` - Build manifest data
    /// * `info_plist` - Optional info plist for the image
    /// * `unique_chip_id` - Device's unique chip ID
    /// * `callback` - Progress callback
    /// * `state` - State to pass to callback
    ///
    /// # Type Parameters
    /// * `Fut` - Future type returned by callback
    /// * `S` - Type of state passed to callback
    ///
    /// # Errors
    /// Returns `IdeviceError` if mounting fails
    #[allow(clippy::too_many_arguments)]
    pub async fn mount_personalized_with_callback<Fut, S>(
        &mut self,
        provider: &dyn crate::provider::IdeviceProvider,
        image: Vec<u8>,
        trust_cache: Vec<u8>,
        build_manifest: &[u8],
        info_plist: Option<plist::Value>,
        unique_chip_id: u64,
        callback: impl Fn(((usize, usize), S)) -> Fut,
        state: S,
    ) -> Result<(), IdeviceError>
    where
        Fut: std::future::Future<Output = ()>,
        S: Clone,
    {
        // Try to fetch personalization manifest
        let mut hasher = Sha384::new();
        hasher.update(&image);
        let image_hash = hasher.finalize();
        let manifest = match self
            .query_personalization_manifest("DeveloperDiskImage", image_hash.to_vec())
            .await
        {
            Ok(manifest) => manifest,
            Err(e) => {
                debug!("Device didn't contain a manifest: {e:?}, fetching from TSS");

                // On failure, the socket closes. Open a new one.
                self.idevice = Self::connect(provider).await?.idevice;

                // Get manifest from TSS
                let manifest_dict: plist::Dictionary = plist::from_bytes(build_manifest)?;
                self.get_manifest_from_tss(&manifest_dict, unique_chip_id)
                    .await?
            }
        };

        debug!("Uploading image");
        self.upload_image_with_progress("Personalized", &image, manifest.clone(), callback, state)
            .await?;

        debug!("Mounting image");
        self.mount_image("Personalized", manifest, Some(trust_cache), info_plist)
            .await?;

        Ok(())
    }

    /// Mounts a personalized image with automatic manifest handling
    ///
    /// # Arguments
    /// * `provider` - Device connection provider (used for reconnection if needed)
    /// * `image` - The image data
    /// * `trust_cache` - Trust cache data
    /// * `build_manifest` - Build manifest data
    /// * `info_plist` - Optional info plist for the image
    /// * `unique_chip_id` - Device's unique chip ID
    ///
    /// # Errors
    /// Returns `IdeviceError` if mounting fails
    #[cfg(all(feature = "tss", feature = "rsd"))]
    #[allow(clippy::too_many_arguments)]
    pub async fn mount_personalized_rsd(
        &mut self,
        provider: &mut impl crate::provider::RsdProvider,
        handshake: &mut crate::rsd::RsdHandshake,
        image: Vec<u8>,
        trust_cache: Vec<u8>,
        build_manifest: &[u8],
        info_plist: Option<plist::Value>,
        unique_chip_id: u64,
    ) -> Result<(), IdeviceError> {
        self.mount_personalized_with_callback_rsd(
            provider,
            handshake,
            image,
            trust_cache,
            build_manifest,
            info_plist,
            unique_chip_id,
            |_| async {},
            (),
        )
        .await
    }

    /// Mounts a personalized image with progress callbacks
    ///
    /// # Important
    /// This may close the socket on failure, requiring reconnection.
    ///
    /// # Arguments
    /// * `provider` - Device connection provider
    /// * `image` - The image data
    /// * `trust_cache` - Trust cache data
    /// * `build_manifest` - Build manifest data
    /// * `info_plist` - Optional info plist for the image
    /// * `unique_chip_id` - Device's unique chip ID
    /// * `callback` - Progress callback
    /// * `state` - State to pass to callback
    ///
    /// # Type Parameters
    /// * `Fut` - Future type returned by callback
    /// * `S` - Type of state passed to callback
    ///
    /// # Errors
    /// Returns `IdeviceError` if mounting fails
    #[cfg(all(feature = "tss", feature = "rsd"))]
    #[allow(clippy::too_many_arguments)]
    pub async fn mount_personalized_with_callback_rsd<Fut, S>(
        &mut self,
        provider: &mut impl crate::provider::RsdProvider,
        handshake: &mut crate::rsd::RsdHandshake,
        image: Vec<u8>,
        trust_cache: Vec<u8>,
        build_manifest: &[u8],
        info_plist: Option<plist::Value>,
        unique_chip_id: u64,
        callback: impl Fn(((usize, usize), S)) -> Fut,
        state: S,
    ) -> Result<(), IdeviceError>
    where
        Fut: std::future::Future<Output = ()>,
        S: Clone,
    {
        // Try to fetch personalization manifest
        let mut hasher = Sha384::new();
        hasher.update(&image);
        let image_hash = hasher.finalize();
        let manifest = match self
            .query_personalization_manifest("DeveloperDiskImage", image_hash.to_vec())
            .await
        {
            Ok(manifest) => manifest,
            Err(e) => {
                debug!("Device didn't contain a manifest: {e:?}, fetching from TSS");

                // On failure, the socket closes. Open a new one.
                self.idevice = Self::connect_rsd(provider, handshake).await?.idevice;

                // Get manifest from TSS
                let manifest_dict: plist::Dictionary = plist::from_bytes(build_manifest)?;
                self.get_manifest_from_tss(&manifest_dict, unique_chip_id)
                    .await?
            }
        };

        debug!("Uploading image");
        self.upload_image_with_progress("Personalized", &image, manifest.clone(), callback, state)
            .await?;

        debug!("Mounting image");
        self.mount_image("Personalized", manifest, Some(trust_cache), info_plist)
            .await?;

        Ok(())
    }

    #[cfg(feature = "tss")]
    /// Retrieves a personalization manifest from Apple's TSS server
    ///
    /// # Arguments
    /// * `build_manifest` - Build manifest dictionary
    /// * `unique_chip_id` - Device's unique chip ID
    ///
    /// # Returns
    /// The manifest data
    ///
    /// # Errors
    /// Returns `IdeviceError` if manifest retrieval fails
    pub async fn get_manifest_from_tss(
        &mut self,
        build_manifest: &plist::Dictionary,
        unique_chip_id: u64,
    ) -> Result<Vec<u8>, IdeviceError> {
        use tracing::{debug, warn};

        let mut request = TSSRequest::new();

        let personalization_identifiers = self.query_personalization_identifiers(None).await?;
        for (key, val) in &personalization_identifiers {
            if key.starts_with("Ap,") {
                request.insert(key, val.clone());
            }
        }

        let board_id = match personalization_identifiers.get("BoardId") {
            Some(plist::Value::Integer(b)) => match b.as_unsigned() {
                Some(b) => b,
                None => {
                    return Err(IdeviceError::UnexpectedResponse(
                        "BoardId is not an unsigned integer".into(),
                    ));
                }
            },
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing BoardId in personalization identifiers".into(),
                ));
            }
        };
        let chip_id = match personalization_identifiers.get("ChipID") {
            Some(plist::Value::Integer(b)) => match b.as_unsigned() {
                Some(b) => b,
                None => {
                    return Err(IdeviceError::UnexpectedResponse(
                        "ChipID is not an unsigned integer".into(),
                    ));
                }
            },
            _ => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing ChipID in personalization identifiers".into(),
                ));
            }
        };

        request.insert("@ApImg4Ticket", true);
        request.insert("@BBTicket", true);
        request.insert("ApBoardID", board_id);
        request.insert("ApChipID", chip_id);
        request.insert("ApECID", unique_chip_id);
        request.insert(
            "ApNonce",
            plist::Value::Data(self.query_nonce(Some("DeveloperDiskImage")).await?),
        );
        request.insert("ApProductionMode", true);
        request.insert("ApSecurityDomain", 1);
        request.insert("ApSecurityMode", true);
        request.insert("SepNonce", plist::Value::Data(vec![0; 20]));
        request.insert("UID_MODE", false);

        let identities = match build_manifest.get("BuildIdentities") {
            Some(plist::Value::Array(i)) => i,
            _ => {
                return Err(IdeviceError::BadBuildManifest);
            }
        };
        let mut build_identity = None;
        for id in identities {
            let id = match id {
                plist::Value::Dictionary(id) => id,
                _ => {
                    debug!("build identity wasn't a dictionary");
                    continue;
                }
            };

            let ap_board_id = match id.get("ApBoardID") {
                Some(plist::Value::String(a)) => a,
                _ => {
                    debug!("Build identity contained no ApBoardID");
                    continue;
                }
            };
            let ap_board_id = match u64::from_str_radix(ap_board_id.trim_start_matches("0x"), 16) {
                Ok(a) => a,
                Err(_) => {
                    debug!("Could not parse {ap_board_id} as usize");
                    continue;
                }
            };
            if ap_board_id != board_id {
                continue;
            }
            let ap_chip_id = match id.get("ApChipID") {
                Some(plist::Value::String(a)) => a,
                _ => {
                    debug!("Build identity contained no ApChipID");
                    continue;
                }
            };
            let ap_chip_id = match u64::from_str_radix(ap_chip_id.trim_start_matches("0x"), 16) {
                Ok(a) => a,
                Err(_) => {
                    debug!("Could not parse {ap_board_id} as usize");
                    continue;
                }
            };
            if ap_chip_id != chip_id {
                continue;
            }
            build_identity = Some(id.to_owned());
            break;
        }

        let build_identity = match build_identity {
            Some(b) => b,
            None => {
                return Err(IdeviceError::BadBuildManifest);
            }
        };

        let manifest = match build_identity.get("Manifest") {
            Some(plist::Value::Dictionary(m)) => m,
            _ => {
                return Err(IdeviceError::BadBuildManifest);
            }
        };

        let parameters = crate::plist!(dict {
            "ApProductionMode": true,
            "ApSecurityMode": 1,
            "ApSecurityMode": true,
            "ApSupportsImg4": true
        });

        for (key, manifest_item) in manifest {
            println!("{key}, {manifest_item:?}");
            let manifest_item = match manifest_item {
                plist::Value::Dictionary(m) => m,
                _ => {
                    debug!("Manifest item wasn't a dictionary");
                    continue;
                }
            };
            if manifest_item.get("Info").is_none() {
                debug!("Manifest item didn't contain info");
                continue;
            }

            match manifest_item.get("Trusted") {
                Some(plist::Value::Boolean(t)) => {
                    if !t {
                        debug!("Info item isn't trusted");
                        continue;
                    }
                }
                _ => {
                    debug!("Info didn't contain trusted bool");
                    continue;
                }
            }

            let mut tss_entry = manifest_item.clone();
            tss_entry.remove("Info");

            if let Some(info) = manifest
                .get("LoadableTrustCache")
                .and_then(|l| l.as_dictionary())
                .and_then(|l| l.get("Info"))
                .and_then(|i| i.as_dictionary())
                && let Some(plist::Value::Array(rules)) = info.get("RestoreRequestRules")
            {
                crate::tss::apply_restore_request_rules(&mut tss_entry, &parameters, rules);
            }

            if manifest_item.get("Digest").is_none() {
                tss_entry.insert("Digest".into(), plist::Value::Data(vec![]));
            }

            request.insert(key, tss_entry);
        }
        let res = request.send().await?;
        let mut res = match res {
            plist::Value::Dictionary(r) => r,
            _ => {
                warn!("Apple returned a non-dictionary plist");
                return Err(IdeviceError::UnexpectedResponse(
                    "TSS response is not a dictionary".into(),
                ));
            }
        };

        match res.remove("ApImg4Ticket") {
            Some(plist::Value::Data(d)) => Ok(d),
            _ => {
                warn!("TSS response didn't contain ApImg4Ticket data");
                Err(IdeviceError::UnexpectedResponse(
                    "missing ApImg4Ticket data in TSS response".into(),
                ))
            }
        }
    }
}

#[cfg(feature = "rsd")]
impl crate::RsdService for ImageMounter {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        crate::obf!("com.apple.mobile.mobile_image_mounter.shim.remote")
    }
    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = crate::Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}
