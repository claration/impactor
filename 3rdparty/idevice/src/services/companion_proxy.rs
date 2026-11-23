//! Companion Proxy is Apple's bridge to connect to the Apple Watch

use tracing::warn;

use crate::{Idevice, IdeviceError, IdeviceService, RsdService, obf};

#[derive(Debug)]
pub struct CompanionProxy {
    idevice: Idevice,
}

#[derive(Debug)]
pub struct CompanionProxyStream {
    proxy: CompanionProxy,
}

impl IdeviceService for CompanionProxy {
    fn service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.companion_proxy")
    }

    async fn from_stream(idevice: Idevice) -> Result<Self, crate::IdeviceError> {
        Ok(Self::new(idevice))
    }
}

impl RsdService for CompanionProxy {
    fn rsd_service_name() -> std::borrow::Cow<'static, str> {
        obf!("com.apple.companion_proxy.shim.remote")
    }

    async fn from_stream(stream: Box<dyn crate::ReadWrite>) -> Result<Self, crate::IdeviceError> {
        let mut idevice = Idevice::new(stream, "");
        idevice.rsd_checkin().await?;
        Ok(Self::new(idevice))
    }
}

impl CompanionProxy {
    pub fn new(idevice: Idevice) -> Self {
        Self { idevice }
    }

    pub async fn get_device_registry(&mut self) -> Result<Vec<String>, IdeviceError> {
        let command = crate::plist!({
            "Command": "GetDeviceRegistry"
        });

        self.idevice.send_plist(command).await?;
        let res = self.idevice.read_plist().await?;
        let list = match res.get("PairedDevicesArray").and_then(|x| x.as_array()) {
            Some(l) => l,
            None => {
                warn!("Didn't get PairedDevicesArray array");
                return Err(IdeviceError::UnexpectedResponse(
                    "missing PairedDevicesArray in device registry response".into(),
                ));
            }
        };

        let mut res = Vec::new();
        for l in list {
            if let plist::Value::String(l) = l {
                res.push(l.to_owned());
            }
        }

        Ok(res)
    }

    pub async fn listen_for_devices(mut self) -> Result<CompanionProxyStream, IdeviceError> {
        let command = crate::plist!({
            "Command": "StartListeningForDevices"
        });
        self.idevice.send_plist(command).await?;

        Ok(CompanionProxyStream { proxy: self })
    }

    pub async fn get_value(
        &mut self,
        udid: impl Into<String>,
        key: impl Into<String>,
    ) -> Result<plist::Value, IdeviceError> {
        let udid = udid.into();
        let key = key.into();
        let command = crate::plist!({
            "Command": "GetValueFromRegistry",
            "GetValueGizmoUDIDKey": udid,
            "GetValueKeyKey": key.clone()
        });
        self.idevice.send_plist(command).await?;
        let mut res = self.idevice.read_plist().await?;
        if let Some(v) = res
            .remove("RetrievedValueDictionary")
            .and_then(|x| x.into_dictionary())
            .and_then(|mut x| x.remove(&key))
        {
            Ok(v)
        } else {
            Err(IdeviceError::NotFound)
        }
    }

    pub async fn start_forwarding_service_port(
        &mut self,
        port: u16,
        service_name: Option<&str>,
        options: Option<plist::Dictionary>,
    ) -> Result<u16, IdeviceError> {
        let command = crate::plist!({
            "Command": "StartForwardingServicePort",
            "GizmoRemotePortNumber": port,
            "IsServiceLowPriority": false,
            "PreferWifi": false,
            "ForwardedServiceName":? service_name,
            :<? options,
        });
        self.idevice.send_plist(command).await?;
        let res = self.idevice.read_plist().await?;
        if let Some(p) = res
            .get("CompanionProxyServicePort")
            .and_then(|x| x.as_unsigned_integer())
        {
            Ok(p as u16)
        } else {
            Err(IdeviceError::UnexpectedResponse(
                "missing CompanionProxyServicePort in forwarding response".into(),
            ))
        }
    }

    pub async fn stop_forwarding_service_port(&mut self, port: u16) -> Result<(), IdeviceError> {
        let command = crate::plist!({
           "Command": "StopForwardingServicePort",
            "GizmoRemotePortNumber": port
        });

        self.idevice.send_plist(command).await?;
        let res = self.idevice.read_plist().await?;
        if let Some(c) = res.get("Command").and_then(|x| x.as_string())
            && (c == "ComandSuccess" || c == "CommandSuccess")
        // Apple you spelled this wrong, adding the right spelling just in case you fix it smh
        {
            Ok(())
        } else {
            Err(IdeviceError::UnexpectedResponse(
                "missing or invalid Command in stop forwarding response".into(),
            ))
        }
    }
}

impl CompanionProxyStream {
    pub async fn next(&mut self) -> Result<plist::Dictionary, IdeviceError> {
        self.proxy.idevice.read_plist().await
    }
}
