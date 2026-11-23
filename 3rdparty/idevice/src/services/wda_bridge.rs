//! Localhost bridge for WebDriverAgent HTTP and MJPEG endpoints.
//!
//! This module exposes device-side WDA ports as dynamic localhost URLs so GUI
//! clients (for example Tauri/React) can consume them as ordinary HTTP
//! endpoints without hard-coding host ports.

use std::{net::SocketAddr, sync::Arc};

use tokio::{
    io::copy_bidirectional,
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tracing::{debug, warn};

use crate::{IdeviceError, provider::IdeviceProvider};

use super::wda::{DEFAULT_WDA_MJPEG_PORT, DEFAULT_WDA_PORT, WdaPorts};

/// Localhost URLs assigned to a running WDA bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WdaBridgeEndpoints {
    /// Device UDID when it can be resolved from the pairing file.
    pub udid: Option<String>,
    /// Local URL forwarding to the device-side WDA HTTP endpoint.
    pub wda_url: String,
    /// Local URL forwarding to the device-side MJPEG endpoint.
    pub mjpeg_url: String,
    /// Local ports bound on the host.
    pub local_ports: WdaPorts,
    /// Original device-side ports.
    pub device_ports: WdaPorts,
}

#[derive(Debug)]
struct TcpPortForward {
    local_addr: SocketAddr,
    task: JoinHandle<()>,
}

impl TcpPortForward {
    async fn start(
        provider: Arc<dyn IdeviceProvider>,
        device_port: u16,
        label: &'static str,
    ) -> Result<Self, IdeviceError> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let local_addr = listener.local_addr()?;
        let provider_label = provider.label().to_string();

        let task = tokio::spawn(async move {
            loop {
                let (mut client, client_addr) = match listener.accept().await {
                    Ok(connection) => connection,
                    Err(error) => {
                        warn!("[{}] localhost bridge accept failed: {}", label, error);
                        break;
                    }
                };

                let provider = provider.clone();
                let provider_label = provider_label.clone();
                tokio::spawn(async move {
                    debug!(
                        "[{}] bridging {} -> {}:{}",
                        label, client_addr, provider_label, device_port
                    );

                    let device = match provider.connect(device_port).await {
                        Ok(device) => device,
                        Err(error) => {
                            warn!(
                                "[{}] failed to connect to device port {}: {}",
                                label, device_port, error
                            );
                            return;
                        }
                    };

                    let mut device_socket = match device.get_socket() {
                        Some(socket) => socket,
                        None => {
                            warn!(
                                "[{}] failed to extract device socket for port {}",
                                label, device_port
                            );
                            return;
                        }
                    };

                    if let Err(error) = proxy_connection(&mut client, device_socket.as_mut()).await
                    {
                        debug!(
                            "[{}] bridge connection {} -> {} closed with error: {}",
                            label, client_addr, device_port, error
                        );
                    }
                });
            }
        });

        Ok(Self { local_addr, task })
    }

    fn local_port(&self) -> u16 {
        self.local_addr.port()
    }
}

impl Drop for TcpPortForward {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Dynamic localhost bridge for a single device's WDA endpoints.
#[derive(Debug)]
pub struct WdaBridge {
    endpoints: WdaBridgeEndpoints,
    wda_forward: TcpPortForward,
    mjpeg_forward: TcpPortForward,
}

impl WdaBridge {
    /// Starts localhost forwarding for the default WDA HTTP and MJPEG ports.
    pub async fn start(provider: Arc<dyn IdeviceProvider>) -> Result<Self, IdeviceError> {
        Self::start_with_ports(
            provider,
            WdaPorts {
                http: DEFAULT_WDA_PORT,
                mjpeg: DEFAULT_WDA_MJPEG_PORT,
            },
        )
        .await
    }

    /// Starts localhost forwarding for custom device-side WDA ports.
    pub async fn start_with_ports(
        provider: Arc<dyn IdeviceProvider>,
        device_ports: WdaPorts,
    ) -> Result<Self, IdeviceError> {
        let udid = provider
            .get_pairing_file()
            .await
            .ok()
            .and_then(|pairing| pairing.udid);
        let wda_forward =
            TcpPortForward::start(provider.clone(), device_ports.http, "wda-http").await?;
        let mjpeg_forward =
            TcpPortForward::start(provider, device_ports.mjpeg, "wda-mjpeg").await?;

        let local_ports = WdaPorts {
            http: wda_forward.local_port(),
            mjpeg: mjpeg_forward.local_port(),
        };

        let endpoints = bridge_endpoints(udid, local_ports, device_ports);

        Ok(Self {
            endpoints,
            wda_forward,
            mjpeg_forward,
        })
    }

    /// Returns the resolved localhost endpoints.
    pub fn endpoints(&self) -> &WdaBridgeEndpoints {
        &self.endpoints
    }

    /// Returns the localhost WDA HTTP URL.
    pub fn wda_url(&self) -> &str {
        &self.endpoints.wda_url
    }

    /// Returns the localhost MJPEG URL.
    pub fn mjpeg_url(&self) -> &str {
        &self.endpoints.mjpeg_url
    }

    /// Stops the localhost bridge by consuming the handle.
    ///
    /// Dropping the bridge aborts the underlying accept loops, so an explicit
    /// shutdown method is only a convenience wrapper over normal drop
    /// semantics.
    pub fn shutdown(self) {
        let WdaBridge {
            endpoints: _,
            wda_forward,
            mjpeg_forward,
        } = self;
        drop(wda_forward);
        drop(mjpeg_forward);
    }
}

fn bridge_endpoints(
    udid: Option<String>,
    local_ports: WdaPorts,
    device_ports: WdaPorts,
) -> WdaBridgeEndpoints {
    WdaBridgeEndpoints {
        udid,
        wda_url: format!("http://127.0.0.1:{}", local_ports.http),
        mjpeg_url: format!("http://127.0.0.1:{}", local_ports.mjpeg),
        local_ports,
        device_ports,
    }
}

async fn proxy_connection(
    client: &mut TcpStream,
    device: &mut dyn crate::ReadWrite,
) -> Result<(), IdeviceError> {
    let _ = copy_bidirectional(client, device).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{WdaPorts, bridge_endpoints};

    #[test]
    fn bridge_endpoints_use_local_ports_in_urls() {
        let endpoints = bridge_endpoints(
            Some("test-udid".into()),
            WdaPorts {
                http: 38100,
                mjpeg: 39100,
            },
            WdaPorts {
                http: 8100,
                mjpeg: 9100,
            },
        );

        assert_eq!(endpoints.udid.as_deref(), Some("test-udid"));
        assert_eq!(endpoints.wda_url, "http://127.0.0.1:38100");
        assert_eq!(endpoints.mjpeg_url, "http://127.0.0.1:39100");
        assert_eq!(endpoints.local_ports.http, 38100);
        assert_eq!(endpoints.local_ports.mjpeg, 39100);
        assert_eq!(endpoints.device_ports.http, 8100);
        assert_eq!(endpoints.device_ports.mjpeg, 9100);
    }
}
