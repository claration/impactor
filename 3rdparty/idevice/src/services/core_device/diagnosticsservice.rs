// Jackson Coxson

use std::pin::Pin;

use futures::Stream;
use tracing::warn;

use crate::{IdeviceError, ReadWrite, RsdService, obf};

impl RsdService for DiagnostisServiceClient<Box<dyn ReadWrite>> {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.coredevice.diagnosticsservice")
    }

    async fn from_stream(stream: Box<dyn ReadWrite>) -> Result<Self, IdeviceError> {
        Ok(Self {
            inner: super::CoreDeviceServiceClient::new(stream).await?,
        })
    }
}

#[derive(Debug)]
pub struct DiagnostisServiceClient<R: ReadWrite> {
    inner: super::CoreDeviceServiceClient<R>,
}

pub struct SysdiagnoseResponse<'a> {
    pub preferred_filename: String,
    pub stream: Pin<Box<dyn Stream<Item = Result<Vec<u8>, IdeviceError>> + 'a>>,
    pub expected_length: usize,
}

impl<R: ReadWrite> DiagnostisServiceClient<R> {
    pub async fn capture_sysdiagnose<'a>(
        &'a mut self,
        dry_run: bool,
    ) -> Result<SysdiagnoseResponse<'a>, IdeviceError> {
        let req = crate::plist!({
            "options": {
                "collectFullLogs": true
            },
            "isDryRun": dry_run
        })
        .into_dictionary()
        .unwrap();

        let res = self
            .inner
            .invoke_with_plist("com.apple.coredevice.feature.capturesysdiagnose", req)
            .await?;

        if let Some(len) = res
            .as_dictionary()
            .and_then(|x| x.get("fileTransfer"))
            .and_then(|x| x.as_dictionary())
            .and_then(|x| x.get("expectedLength"))
            .and_then(|x| x.as_unsigned_integer())
            && let Some(name) = res
                .as_dictionary()
                .and_then(|x| x.get("preferredFilename"))
                .and_then(|x| x.as_string())
        {
            Ok(SysdiagnoseResponse {
                stream: Box::pin(self.inner.inner.iter_file_chunks(len as usize, 0)),
                preferred_filename: name.to_string(),
                expected_length: len as usize,
            })
        } else {
            warn!("Did not get expected responses from RemoteXPC");
            Err(IdeviceError::UnexpectedResponse(
                "missing fileTransfer/expectedLength or preferredFilename in sysdiagnose response"
                    .into(),
            ))
        }
    }
}

impl std::fmt::Debug for SysdiagnoseResponse<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SysdiagnoseResponse")
            .field("preferred_filename", &self.preferred_filename)
            .field("expected_length", &self.expected_length)
            .finish()
    }
}
