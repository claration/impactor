use crate::{Idevice, IdeviceError, IdeviceService, obf};

#[derive(Debug)]
pub struct LocationSimulationService {
    idevice: Idevice,
}

impl IdeviceService for LocationSimulationService {
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.dt.simulatelocation")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl LocationSimulationService {
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    pub async fn clear(&mut self) -> Result<(), IdeviceError> {
        let message: [u8; 4] = [0x00, 0x00, 0x00, 0x01];
        self.idevice.send_raw(&message).await?;
        Ok(())
    }

    pub async fn set(&mut self, latitude: &str, longtiude: &str) -> Result<(), IdeviceError> {
        let message: [u8; 4] = [0x00, 0x00, 0x00, 0x00];
        let latitude_len = latitude.len() as u32;
        let longtiude_len = longtiude.len() as u32;
        let latitude_bytes = [&latitude_len.to_be_bytes(), latitude.as_bytes()].concat();
        let longitude_bytes = [&longtiude_len.to_be_bytes(), longtiude.as_bytes()].concat();
        let data = [&message[..], &latitude_bytes[..], &longitude_bytes[..]].concat();
        self.idevice.send_raw(data.as_slice()).await?;
        Ok(())
    }
}
