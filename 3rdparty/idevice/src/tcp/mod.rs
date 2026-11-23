// Jackson Coxson

pub use jktcp::adapter;
pub use jktcp::handle;
pub use jktcp::packets;
pub use jktcp::stream;

use crate::{ReadWrite, provider::RsdProvider};

impl RsdProvider for handle::AdapterHandle {
    async fn connect_to_service_port(
        &mut self,
        port: u16,
    ) -> Result<Box<dyn ReadWrite>, crate::IdeviceError> {
        let s = self.connect(port).await?;
        Ok(Box::new(s))
    }
}
