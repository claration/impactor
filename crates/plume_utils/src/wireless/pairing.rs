use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};

use idevice::remote_pairing::{PeerDevice, RemotePairingClient, RpPairingFile, RpPairingSocket};

use crate::Error;

#[derive(Debug, Clone)]
pub struct PairedDevice {
    pub udid: String,
    pub name: String,
    pub model: String,
    pub account_id: String,
}

impl From<&PeerDevice> for PairedDevice {
    fn from(peer: &PeerDevice) -> Self {
        Self {
            udid: peer.remotepairing_udid.clone(),
            name: peer.name.clone(),
            model: peer.model.clone(),
            account_id: peer.account_id.clone(),
        }
    }
}

/// Pairs with the device at `address`, prompting for the PIN it shows on screen.
///
/// `host` is the name the device displays for this computer. It also seeds the
/// pairing file identifier, so keep it stable across pairings.
pub async fn pair<F, Fut>(
    address: SocketAddr,
    host: &str,
    pin_callback: F,
) -> Result<(RpPairingFile, PairedDevice), Error>
where
    F: Fn() -> Fut,
    Fut: Future<Output = String>,
{
    let mut pairing_file = RpPairingFile::generate(host);

    let device = {
        let socket = connect(address).await?;
        let mut client = RemotePairingClient::new(socket, host, &mut pairing_file);

        client
            .connect(
                |_: u8| {
                    let pin = pin_callback();
                    async move { six_byte_pin(pin.await) }
                },
                0u8,
            )
            .await?;
        PairedDevice::from(client.paired_peer_device()?)
    };

    // The keys aren't proven until the device accepts them without a PIN.
    verify(address, host, &mut pairing_file).await?;

    Ok((pairing_file, device))
}

/// Reconnects with an existing pairing file to check the device still trusts it.
pub async fn verify(
    address: SocketAddr,
    host: &str,
    pairing_file: &mut RpPairingFile,
) -> Result<(), Error> {
    let pin_requested = AtomicBool::new(false);

    let socket = connect(address).await?;
    let mut client = RemotePairingClient::new(socket, host, pairing_file);

    let result = client
        .connect(
            |_: u8| {
                pin_requested.store(true, Ordering::Relaxed);
                async { six_byte_pin(String::new()) }
            },
            0u8,
        )
        .await;

    // Being asked for a PIN means the pairing is dead. Checked before the error,
    // which is just the device rejecting the empty PIN above.
    if pin_requested.load(Ordering::Relaxed) {
        return Err(Error::PairingNotAccepted);
    }

    result?;
    Ok(())
}

/// The pairing library slices the first six bytes of the PIN and panics on anything shorter.
fn six_byte_pin(pin: String) -> String {
    if pin.len() >= 6 {
        pin
    } else {
        format!("{pin:0<6}")
    }
}

async fn connect(address: SocketAddr) -> Result<RpPairingSocket<tokio::net::TcpStream>, Error> {
    let stream = tokio::net::TcpStream::connect(address).await?;
    Ok(RpPairingSocket::new(stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_pins_are_padded_to_six_bytes() {
        assert_eq!(six_byte_pin(String::new()), "000000");
        assert_eq!(six_byte_pin("12".to_string()), "120000");
    }

    #[test]
    fn full_pins_are_left_alone() {
        assert_eq!(six_byte_pin("123456".to_string()), "123456");
        assert_eq!(six_byte_pin("1234567".to_string()), "1234567");
    }
}
