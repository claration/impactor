// Jackson Coxson

use crate::{IdeviceError, ReadWrite, RemoteXpcClient, RsdService, obf};

impl RsdService for InstallcoordinationProxy<Box<dyn ReadWrite>> {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.remote.installcoordination_proxy")
    }

    async fn from_stream(stream: Box<dyn ReadWrite>) -> Result<Self, IdeviceError> {
        let mut client = RemoteXpcClient::new(stream).await?;
        client.do_handshake().await?;
        Ok(Self { inner: client })
    }
}

#[derive(Debug)]
pub struct InstallcoordinationProxy<R: ReadWrite> {
    inner: RemoteXpcClient<R>,
}

impl<R: ReadWrite> InstallcoordinationProxy<R> {
    // TODO: implement 2 missing functions
    //
    // # REVERT STASH
    // Revert Stashed App (RequestType: 2)
    // This request rolls back an application to a previously "stashed" version, which is typically done after a failed update.
    //
    // Handler: _handleRevertStashMessage_forRemoteConnection_
    //
    // RequestType: 2
    // ProtocolVersion: 1
    // BundleID: The bundle identifier of the app to revert.
    //
    // Action: The service creates an IXSRemoteReverter object, which calls the IXAppInstallCoordinator to perform the revert. It responds with a success or failure message.
    //
    // # INSTALL
    // This is the most complex request. It tells the service to install a new application.
    // Purpose: To stream an application binary from a client and install it on the device.
    //
    // Handler: _handleInstallBeginMessage_forRemoteConnection_
    // RequestType: 1
    // ProtocolVersion: 1
    // AssetSize: The total size of the app binary.
    // AssetStreamFD: A file descriptor from which the service will read the app binary.
    // RemoteInstallOptions: A dictionary containing all the app's metadata, like:
    //
    // {
    // BundleID: (String) The application's bundle identifier (e.g., com.example.myapp).
    // LocalizedName: (String) The display name of the app.
    // InstallMode: (uint64) An enum specifying the installation mode (e.g., full install, update).
    // Importance: (uint64) An enum defining the install's priority. 1 for "user" and 2 for "system".
    // InstallableType: (uint64) Specifies the type of content being installed (e.g., app, system component).
    // StoreMetadata: (Dictionary) A dictionary containing App Store metadata.
    // SINFData: (Data) The legacy iTunes Sinf data for DRM.
    // ProvisioningProfiles: (Array of Data) An array of provisioning profiles to install alongside the app.
    // IconData: (Data, Optional) The raw data for the application's icon.
    // IconDataType: (uint64, Optional) An enum specifying the format of the IconData
    // }
    //
    // Action: The service creates an IXSRemoteInstaller object. It reads the app data from the file descriptor and passes it to the system's IXAppInstallCoordinator to handle the installation, placeholder creation, and data management. It sends back progress updates and a final completion message.

    pub async fn uninstall_app(&mut self, bundle_id: &str) -> Result<(), IdeviceError> {
        let req = crate::xpc!({
            "RequestVersion": 1u64,
            "ProtocolVersion": 1u64,
            "RequestType": 3u64,
            "BundleID": bundle_id,
        });

        self.inner.send_object(req, true).await?;
        let res = self.inner.recv_root().await?; // it responds on the root??

        match res
            .as_dictionary()
            .and_then(|x| x.get("Success"))
            .and_then(|x| x.as_boolean())
        {
            Some(true) => Ok(()),
            _ => Err(IdeviceError::UnexpectedResponse(
                "missing Success boolean in uninstall response".into(),
            )),
        }
    }

    pub async fn query_app_path(&mut self, bundle_id: &str) -> Result<String, IdeviceError> {
        let req = crate::xpc!({
            "RequestVersion": 1u64,
            "ProtocolVersion": 1u64,
            "RequestType": 4u64,
            "BundleID": bundle_id,
        });

        self.inner.send_object(req, true).await?;
        let res = self.inner.recv_root().await?; // it responds on the root??

        match res
            .as_dictionary()
            .and_then(|x| x.get("InstallPath"))
            .and_then(|x| x.as_dictionary())
            .and_then(|x| x.get("com.apple.CFURL.string"))
            .and_then(|x| x.as_string())
        {
            Some(s) => Ok(s.to_string()),
            None => Err(IdeviceError::UnexpectedResponse(
                "missing InstallPath in query_app_path response".into(),
            )),
        }
    }
}
