// Jackson Coxson

use tokio::io::AsyncReadExt;

use crate::{IdeviceError, ReadWrite, RsdService, obf};

impl RsdService for OpenStdioSocketClient {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.coredevice.openstdiosocket")
    }

    async fn from_stream(stream: Box<dyn ReadWrite>) -> Result<Self, IdeviceError> {
        Ok(Self { inner: stream })
    }
}

/// Call ``read_uuid`` to get the UUID. Pass that to app service launch to connect to the stream of
/// the launched app. Inner is exposed to read and write to, using Tokio's AsyncReadExt/AsyncWriteExt
#[derive(Debug)]
pub struct OpenStdioSocketClient {
    pub inner: Box<dyn ReadWrite>,
}

impl OpenStdioSocketClient {
    /// iOS assigns a UUID to a newly opened stream. That UUID is then passed to the launch
    /// parameters of app service to start a stream.
    pub async fn read_uuid(&mut self) -> Result<uuid::Uuid, IdeviceError> {
        let mut buf = [0u8; 16];
        self.inner.read_exact(&mut buf).await?;

        let res = uuid::Uuid::from_bytes(buf);
        Ok(res)
    }
}
