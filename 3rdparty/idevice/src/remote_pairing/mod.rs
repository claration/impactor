//! Remote Pairing

use crate::IdeviceError;
use base64::Engine as _;
use errors::RemotePairingError;

use chacha20poly1305::{
    ChaCha20Poly1305, Key, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use ed25519_dalek::Signature;
use hkdf::Hkdf;
use idevice_srp::{client::SrpClient, groups::G_3072};
use plist_macro::plist;
use plist_macro::{PlistConvertible, PlistExt};
use rand::Rng as _;
use rsa::{rand_core::OsRng, signature::SignerMut};
use serde::Serialize;
use sha2::Sha512;
use tracing::{debug, warn};
use x25519_dalek::{EphemeralSecret, PublicKey as X25519PublicKey};

pub mod errors;
mod opack;
mod peer_device;
mod rp_pairing_file;
mod socket;
pub mod tls_psk;
mod tlv;
pub mod tunnel;

// export
pub use peer_device::PeerDevice;
pub use rp_pairing_file::RpPairingFile;
pub use socket::{RpPairingSocket, RpPairingSocketProvider};
#[cfg(feature = "openssl")]
pub use tunnel::connect_tls_psk_tunnel;
pub use tunnel::{CdTunnel, TunnelInfo, connect_tls_psk_tunnel_native};

const RPPAIRING_MAGIC: &[u8] = b"RPPairing";
const WIRE_PROTOCOL_VERSION: u8 = 19;

pub struct RemotePairingClient<'a, R: RpPairingSocketProvider> {
    inner: R,
    sequence_number: usize,
    encrypted_sequence_number: u64,
    pairing_file: &'a mut RpPairingFile,
    sending_host: String,

    /// The shared secret from X25519 (pair-verify) or SRP (initial pairing).
    /// Used as PSK for the TLS tunnel and to derive per-message encryption keys.
    encryption_key: Vec<u8>,

    client_cipher: ChaCha20Poly1305,
    server_cipher: ChaCha20Poly1305,

    paired_peer_device: Option<PeerDevice>,
}

impl<'a, R: RpPairingSocketProvider> RemotePairingClient<'a, R> {
    pub fn new(inner: R, sending_host: &str, pairing_file: &'a mut RpPairingFile) -> Self {
        // Initial ciphers derived from Ed25519 key; will be re-derived from
        // the actual encryption_key once pair-verify or pairing completes.
        let initial_key = pairing_file.e_private_key.as_bytes().to_vec();
        let (client_cipher, server_cipher) = Self::derive_main_ciphers(&initial_key);

        Self {
            inner,
            sequence_number: 0,
            encrypted_sequence_number: 0,
            pairing_file,
            sending_host: sending_host.to_string(),
            encryption_key: initial_key,
            client_cipher,
            server_cipher,
            paired_peer_device: None,
        }
    }

    fn derive_main_ciphers(key: &[u8]) -> (ChaCha20Poly1305, ChaCha20Poly1305) {
        let hk = Hkdf::<sha2::Sha512>::new(None, key);
        let mut okm = [0u8; 32];
        hk.expand(b"ClientEncrypt-main", &mut okm).unwrap();
        let client_cipher = ChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(&okm));

        let hk = Hkdf::<sha2::Sha512>::new(None, key);
        let mut okm = [0u8; 32];
        hk.expand(b"ServerEncrypt-main", &mut okm).unwrap();
        let server_cipher = ChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(&okm));

        (client_cipher, server_cipher)
    }

    /// Returns the encryption key established during pairing.
    /// This is used as TLS-PSK for tunnel connections.
    pub fn encryption_key(&self) -> &[u8] {
        &self.encryption_key
    }

    pub async fn connect<Fut, S>(
        &mut self,
        pin_callback: impl Fn(S) -> Fut,
        state: S,
    ) -> Result<(), IdeviceError>
    where
        Fut: std::future::Future<Output = String>,
    {
        self.attempt_pair_verify().await?;

        if self.validate_pairing().await.is_err() {
            self.pair(pin_callback, state).await?;
        }
        Ok(())
    }

    /// Returns peer device info captured during this client's successful `pair()` flow.
    pub fn paired_peer_device(&self) -> Result<&PeerDevice, IdeviceError> {
        self.paired_peer_device
            .as_ref()
            .ok_or(IdeviceError::UnexpectedResponse(
                "paired peer device info is only available after a successful pair() call".into(),
            ))
    }

    pub async fn validate_pairing(&mut self) -> Result<(), IdeviceError> {
        let x_private_key = EphemeralSecret::random_from_rng(OsRng);
        let x_public_key = X25519PublicKey::from(&x_private_key);

        let pairing_data = tlv::serialize_tlv8(&[
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::State,
                data: vec![0x01],
            },
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::PublicKey,
                data: x_public_key.to_bytes().to_vec(),
            },
        ]);
        let pairing_data = R::serialize_bytes(&pairing_data);
        self.send_pairing_data(plist!({
            "data": pairing_data,
            "kind": "verifyManualPairing",
            "startNewSession": true
        }))
        .await?;
        debug!("Waiting for response from verifyManualPairing");

        let pairing_data = self.receive_pairing_data().await?;

        let data = match R::deserialize_bytes(pairing_data) {
            Some(d) => d,
            None => {
                return Err(IdeviceError::UnexpectedResponse(
                    "failed to deserialize pair-verify response bytes".into(),
                ));
            }
        };

        let data = tlv::deserialize_tlv8(&data)?;

        if data
            .iter()
            .any(|x| x.tlv_type == tlv::PairingDataComponentType::ErrorResponse)
        {
            self.send_pair_verified_failed().await?;
            return Err(RemotePairingError::PairVerifyFailed.into());
        }

        let device_public_key = match data
            .iter()
            .find(|x| x.tlv_type == tlv::PairingDataComponentType::PublicKey)
        {
            Some(d) => d,
            None => {
                warn!("No public key in TLV data");
                return Err(IdeviceError::UnexpectedResponse(
                    "missing public key in pair-verify TLV data".into(),
                ));
            }
        };
        let peer_pub_bytes: [u8; 32] = match device_public_key.data.as_slice().try_into() {
            Ok(d) => d,
            Err(_) => {
                warn!("Device public key isn't the expected size");
                return Err(IdeviceError::NotEnoughBytes(
                    32,
                    device_public_key.data.len(),
                ));
            }
        };
        let device_public_key = x25519_dalek::PublicKey::from(peer_pub_bytes);
        let shared_secret = x_private_key.diffie_hellman(&device_public_key);

        // Save the raw shared secret as the encryption key for tunnel PSK
        self.encryption_key = shared_secret.as_bytes().to_vec();

        // Derive encryption key with HKDF-SHA512
        let hk =
            Hkdf::<sha2::Sha512>::new(Some(b"Pair-Verify-Encrypt-Salt"), shared_secret.as_bytes());

        let mut okm = [0u8; 32];
        hk.expand(b"Pair-Verify-Encrypt-Info", &mut okm).unwrap();

        // ChaCha20Poly1305 AEAD cipher
        let cipher = ChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(&okm));

        let ed25519_signing_key = &mut self.pairing_file.e_private_key;

        let mut signbuf = Vec::with_capacity(32 + self.pairing_file.identifier.len() + 32);
        signbuf.extend_from_slice(x_public_key.as_bytes()); // 32 bytes
        signbuf.extend_from_slice(self.pairing_file.identifier.as_bytes()); // variable
        signbuf.extend_from_slice(device_public_key.as_bytes()); // 32 bytes

        let signature: Signature = ed25519_signing_key.sign(&signbuf);

        let plaintext = vec![
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::Identifier,
                data: self.pairing_file.identifier.as_bytes().to_vec(),
            },
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::Signature,
                data: signature.to_vec(),
            },
        ];
        let plaintext = tlv::serialize_tlv8(&plaintext);
        let nonce = Nonce::from_slice(b"\x00\x00\x00\x00PV-Msg03"); // 12-byte nonce
        let ciphertext = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: &plaintext,
                    aad: &[],
                },
            )
            .expect("encryption should not fail");

        let msg = vec![
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::State,
                data: [0x03].to_vec(),
            },
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::EncryptedData,
                data: ciphertext,
            },
        ];

        debug!("Waiting for signbuf response");
        self.send_pairing_data(plist! ({
            "data": R::serialize_bytes(&tlv::serialize_tlv8(&msg)),
            "kind": "verifyManualPairing",
            "startNewSession": false
        }))
        .await?;
        let res = self.receive_pairing_data().await?;

        let data = match R::deserialize_bytes(res) {
            Some(d) => d,
            None => {
                return Err(IdeviceError::UnexpectedResponse(
                    "failed to deserialize pair-verify signature response bytes".into(),
                ));
            }
        };
        let data = tlv::deserialize_tlv8(&data)?;
        debug!("Verify TLV: {data:#?}");

        // Check if the device responded with an error (which is expected for a new pairing)
        if data
            .iter()
            .any(|x| x.tlv_type == tlv::PairingDataComponentType::ErrorResponse)
        {
            debug!(
                "Verification failed, device reported an error. This is expected for a new pairing."
            );
            self.send_pair_verified_failed().await?;
            // Return a specific error to the caller.
            return Err(RemotePairingError::PairVerifyFailed.into());
        }

        // Re-derive main encryption ciphers from the X25519 shared secret
        let (cc, sc) = Self::derive_main_ciphers(&self.encryption_key);
        self.client_cipher = cc;
        self.server_cipher = sc;

        Ok(())
    }

    pub async fn send_pair_verified_failed(&mut self) -> Result<(), IdeviceError> {
        self.inner
            .send_plain(
                plist!({
                    "event": {
                        "_0": {
                            "pairVerifyFailed": {}
                        }
                    }
                }),
                self.sequence_number,
            )
            .await?;
        self.sequence_number += 1;
        Ok(())
    }

    pub async fn attempt_pair_verify(&mut self) -> Result<plist::Value, IdeviceError> {
        debug!("Sending attemptPairVerify");
        self.inner
            .send_plain(
                plist!({
                    "request": {
                        "_0": {
                            "handshake": {
                                "_0": {
                                    "hostOptions": {
                                        "attemptPairVerify": true
                                    },
                                    "wireProtocolVersion": plist::Value::Integer(WIRE_PROTOCOL_VERSION.into()),
                                }
                            }
                        }
                    }
                }),
                self.sequence_number,
            )
            .await?;
        self.sequence_number += 1;

        debug!("Waiting for attemptPairVerify response");
        let response = self.inner.recv_plain().await?;

        let response = response
            .as_dictionary()
            .and_then(|x| x.get("response"))
            .and_then(|x| x.as_dictionary())
            .and_then(|x| x.get("_1"))
            .and_then(|x| x.as_dictionary())
            .and_then(|x| x.get("handshake"))
            .and_then(|x| x.as_dictionary())
            .and_then(|x| x.get("_0"));

        match response {
            Some(v) => Ok(v.to_owned()),
            None => Err(IdeviceError::UnexpectedResponse(
                "missing handshake response in attemptPairVerify".into(),
            )),
        }
    }

    pub async fn pair<Fut, S>(
        &mut self,
        pin_callback: impl Fn(S) -> Fut,
        state: S,
    ) -> Result<(), IdeviceError>
    where
        Fut: std::future::Future<Output = String>,
    {
        let (salt, public_key, pin) = self.request_pair_consent(pin_callback, state).await?;
        let key = self.init_srp_context(&salt, &public_key, &pin).await?;
        let tlv = self.save_pair_record_on_peer(&key).await?;
        let peer_device = peer_device::parse_peer_device_from_tlv(&tlv)?;

        self.pairing_file.alt_irk = Some(peer_device.alt_irk.clone());
        self.paired_peer_device = Some(peer_device);

        Ok(())
    }

    /// Returns salt and public key and pin
    async fn request_pair_consent<Fut, S>(
        &mut self,
        pin_callback: impl Fn(S) -> Fut,
        state: S,
    ) -> Result<(Vec<u8>, Vec<u8>, String), IdeviceError>
    where
        Fut: std::future::Future<Output = String>,
    {
        let tlv = tlv::serialize_tlv8(&[
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::Method,
                data: vec![0x00],
            },
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::State,
                data: vec![0x01],
            },
        ]);
        let tlv = R::serialize_bytes(&tlv);
        self.send_pairing_data(plist!({
            "data": tlv,
            "kind": "setupManualPairing",
            "sendingHost": &self.sending_host,
            "startNewSession": true
        }))
        .await?;

        let response = self.inner.recv_plain().await?;
        let response = match response
            .get_by("event")
            .and_then(|x| x.get_by("_0"))
            .and_then(|x| x.as_dictionary())
        {
            Some(r) => r,
            None => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing event._0 in pair consent response".into(),
                ));
            }
        };

        let mut pin = None;

        let pairing_data = match if let Some(err) = response.get("pairingRejectedWithError") {
            let context = err
                .get_by("wrappedError")
                .and_then(|x| x.get_by("userInfo"))
                .and_then(|x| x.get_by("NSLocalizedDescription"))
                .and_then(|x| x.as_string())
                .map(|x| x.to_string());
            return Err(RemotePairingError::PairingRejected(context.unwrap_or_default()).into());
        } else if response.get("awaitingUserConsent").is_some() {
            pin = Some("000000".to_string());
            Some(self.receive_pairing_data().await?)
        } else {
            // On Apple TV, we can get the pin now
            response
                .get_by("pairingData")
                .and_then(|x| x.get_by("_0"))
                .and_then(|x| x.get_by("data"))
                .map(|x| x.to_owned())
        } {
            Some(p) => p,
            None => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing pairing data in pair consent response".into(),
                ));
            }
        };

        let tlv = tlv::deserialize_tlv8(&match R::deserialize_bytes(pairing_data) {
            Some(t) => t,
            None => {
                return Err(IdeviceError::UnexpectedResponse(
                    "failed to deserialize pairing data bytes in pair consent".into(),
                ));
            }
        })?;
        debug!("Received pairingData response: {tlv:#?}");

        let mut salt = Vec::new();
        let mut public_key = Vec::new();
        for t in tlv {
            match t.tlv_type {
                tlv::PairingDataComponentType::Salt => {
                    salt = t.data;
                }
                tlv::PairingDataComponentType::PublicKey => {
                    public_key.extend(t.data);
                }
                tlv::PairingDataComponentType::ErrorResponse => {
                    warn!("Pairing data contained error response");
                    return Err(IdeviceError::UnexpectedResponse(
                        "pairing data contained error response during pair consent".into(),
                    ));
                }
                _ => {
                    continue;
                }
            }
        }

        let pin = match pin {
            Some(p) => p,
            None => pin_callback(state).await,
        };

        if salt.is_empty() || public_key.is_empty() {
            warn!("Pairing data did not contain salt or public key");
            return Err(IdeviceError::UnexpectedResponse(
                "pairing data missing salt or public key".into(),
            ));
        }

        Ok((salt, public_key, pin))
    }

    /// Returns the encryption key
    async fn init_srp_context(
        &mut self,
        salt: &[u8],
        public_key: &[u8],
        pin: &str,
    ) -> Result<Vec<u8>, IdeviceError> {
        let client = SrpClient::<Sha512>::new(
            &G_3072, // PRIME_3072 + generator
        );

        let mut a_private = [0u8; 32];
        rand::rng().fill_bytes(&mut a_private);

        let a_public = client.compute_public_ephemeral(&a_private);

        let verifier = match client.process_reply(
            &a_private,
            "Pair-Setup".as_bytes(),
            &pin.as_bytes()[..6],
            salt,
            public_key,
            false,
        ) {
            Ok(v) => v,
            Err(e) => {
                warn!("SRP verifier creation failed: {e:?}");
                return Err(RemotePairingError::SrpAuthFailed.into());
            }
        };

        let client_proof = verifier.proof();

        let tlv = tlv::serialize_tlv8(&[
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::State,
                data: vec![0x03],
            },
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::PublicKey,
                data: a_public[..254].to_vec(),
            },
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::PublicKey,
                data: a_public[254..].to_vec(),
            },
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::Proof,
                data: client_proof.to_vec(),
            },
        ]);
        let tlv = R::serialize_bytes(&tlv);

        self.send_pairing_data(plist!({
            "data": tlv,
            "kind": "setupManualPairing",
            "sendingHost": &self.sending_host,
            "startNewSession": false,

        }))
        .await?;

        let response = self.receive_pairing_data().await?;
        let response = tlv::deserialize_tlv8(&match R::deserialize_bytes(response.to_owned()) {
            Some(r) => r,
            None => {
                return Err(IdeviceError::UnexpectedResponse(
                    "failed to deserialize SRP proof response bytes".into(),
                ));
            }
        })?;

        debug!("Proof response: {response:#?}");

        let proof = match response
            .iter()
            .find(|x| x.tlv_type == tlv::PairingDataComponentType::Proof)
        {
            Some(p) => &p.data,
            None => {
                warn!("Proof response did not contain server proof");
                return Err(IdeviceError::UnexpectedResponse(
                    "missing server proof in SRP response".into(),
                ));
            }
        };

        match verifier.verify_server(proof) {
            Ok(_) => Ok(verifier.key().to_vec()),
            Err(e) => {
                warn!("Server auth failed: {e:?}");
                Err(RemotePairingError::SrpAuthFailed.into())
            }
        }
    }

    async fn save_pair_record_on_peer(
        &mut self,
        encryption_key: &[u8],
    ) -> Result<Vec<tlv::TLV8Entry>, IdeviceError> {
        let salt = b"Pair-Setup-Encrypt-Salt";
        let info = b"Pair-Setup-Encrypt-Info";

        let hk = Hkdf::<Sha512>::new(Some(salt), encryption_key);
        let mut setup_encryption_key = [0u8; 32];
        hk.expand(info, &mut setup_encryption_key)
            .expect("HKDF expand failed");

        // Save the SRP session key as the encryption key
        self.encryption_key = encryption_key.to_vec();

        self.pairing_file.recreate_signing_keys();
        {
            // Re-derive main ciphers from the SRP session key
            let (cc, sc) = Self::derive_main_ciphers(encryption_key);
            self.client_cipher = cc;
            self.server_cipher = sc;
        }

        let hk = Hkdf::<Sha512>::new(Some(b"Pair-Setup-Controller-Sign-Salt"), encryption_key);

        let mut signbuf = Vec::with_capacity(32 + self.pairing_file.identifier.len() + 32);

        let mut hkdf_out = [0u8; 32];
        hk.expand(b"Pair-Setup-Controller-Sign-Info", &mut hkdf_out)
            .expect("HKDF expand failed");

        signbuf.extend_from_slice(&hkdf_out);

        signbuf.extend_from_slice(self.pairing_file.identifier.as_bytes());
        signbuf.extend_from_slice(self.pairing_file.e_public_key.as_bytes());

        let signature = self.pairing_file.e_private_key.sign(&signbuf);

        let device_info = crate::plist!({
            "altIRK": b"\xe9\xe8-\xc0jIykVoT\x00\x19\xb1\xc7{".to_vec(),
            "btAddr": "11:22:33:44:55:66",
            "mac": b"\x11\x22\x33\x44\x55\x66".to_vec(),
            "remotepairing_serial_number": "AAAAAAAAAAAA",
            "accountID": self.pairing_file.identifier.as_str(),
            "model": "computer-model",
            "name": self.sending_host.as_str()
        });
        let device_info = opack::plist_to_opack(&device_info);

        let tlv = tlv::serialize_tlv8(&[
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::Identifier,
                data: self.pairing_file.identifier.as_bytes().to_vec(),
            },
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::PublicKey,
                data: self.pairing_file.e_public_key.to_bytes().to_vec(),
            },
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::Signature,
                data: signature.to_vec(),
            },
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::Info,
                data: device_info,
            },
        ]);

        let key = Key::from_slice(&setup_encryption_key); // 32 bytes
        let cipher = ChaCha20Poly1305::new(key);

        let nonce = Nonce::from_slice(b"\x00\x00\x00\x00PS-Msg05"); // 12 bytes

        let plaintext = &tlv;

        let ciphertext = match cipher.encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad: b"",
            },
        ) {
            Ok(c) => c,
            Err(e) => {
                warn!("Chacha encryption failed: {e:?}");
                return Err(RemotePairingError::ChachaEncryption(e).into());
            }
        };
        debug!("ciphertext len: {}", ciphertext.len());

        let tlv = tlv::serialize_tlv8(&[
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::EncryptedData,
                data: ciphertext[..254].to_vec(),
            },
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::EncryptedData,
                data: ciphertext[254..].to_vec(),
            },
            tlv::TLV8Entry {
                tlv_type: tlv::PairingDataComponentType::State,
                data: vec![0x05],
            },
        ]);
        let tlv = R::serialize_bytes(&tlv);

        debug!("Sending encrypted data");
        self.send_pairing_data(plist!({
            "data": tlv,
            "kind": "setupManualPairing",
            "sendingHost": &self.sending_host,
            "startNewSession": false,
        }))
        .await?;

        debug!("Waiting for encrypted data");
        let response = match R::deserialize_bytes(self.receive_pairing_data().await?) {
            Some(r) => r,
            None => {
                warn!("Pairing data response was not deserializable");
                return Err(IdeviceError::UnexpectedResponse(
                    "failed to deserialize pair record response bytes".into(),
                ));
            }
        };

        let tlv = tlv::deserialize_tlv8(&response)?;

        let mut encrypted_data = Vec::new();
        for t in tlv {
            match t.tlv_type {
                tlv::PairingDataComponentType::EncryptedData => encrypted_data.extend(t.data),
                tlv::PairingDataComponentType::ErrorResponse => {
                    warn!("TLV contained error response");
                    return Err(IdeviceError::UnexpectedResponse(
                        "TLV error response in pair record save".into(),
                    ));
                }
                _ => {}
            }
        }

        let nonce = Nonce::from_slice(b"\x00\x00\x00\x00PS-Msg06");

        let plaintext = cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &encrypted_data,
                    aad: b"",
                },
            )
            .expect("decryption failure!");

        let tlv = tlv::deserialize_tlv8(&plaintext)?;

        debug!("Decrypted plaintext TLV: {tlv:?}");
        Ok(tlv)
    }

    /// Send an encrypted request and receive an encrypted response.
    /// Used for post-pairing RPCs like creating tunnel listeners.
    pub async fn send_receive_encrypted_request(
        &mut self,
        request: plist::Value,
    ) -> Result<plist::Value, IdeviceError> {
        let plaintext = serde_json::to_vec(
            &plist::to_value(&request).map_err(|e| IdeviceError::InternalError(e.to_string()))?,
        )
        .map_err(|e| IdeviceError::InternalError(e.to_string()))?;

        // Build nonce: 8-byte LE sequence number + 4 zero bytes
        let mut nonce_bytes = [0u8; 12];
        nonce_bytes[..8].copy_from_slice(&self.encrypted_sequence_number.to_le_bytes());
        let nonce = Nonce::from_slice(&nonce_bytes);

        let ciphertext = self
            .client_cipher
            .encrypt(
                nonce,
                Payload {
                    msg: &plaintext,
                    aad: b"",
                },
            )
            .map_err(|e| IdeviceError::RemotePairing(RemotePairingError::ChachaEncryption(e)))?;

        self.inner
            .send_encrypted(ciphertext, self.sequence_number)
            .await?;
        self.sequence_number += 1;

        // Receive encrypted response
        let response = self.inner.recv_plain().await?;
        let encrypted_data = response
            .get_by("message")
            .and_then(|m| m.get_by("streamEncrypted"))
            .and_then(|s| s.get_by("_0"))
            .and_then(|d| {
                // Could be bytes directly or base64
                R::deserialize_bytes(d.to_owned())
            })
            .ok_or(IdeviceError::UnexpectedResponse(
                "missing encrypted data in streamEncrypted response".into(),
            ))?;

        let decrypted = self
            .server_cipher
            .decrypt(
                nonce,
                Payload {
                    msg: &encrypted_data,
                    aad: b"",
                },
            )
            .map_err(|e| IdeviceError::RemotePairing(RemotePairingError::ChachaEncryption(e)))?;

        self.encrypted_sequence_number += 1;

        let value: plist::Value = serde_json::from_slice(&decrypted)
            .map_err(|e| IdeviceError::InternalError(e.to_string()))?;

        // Extract response._1
        let result = value
            .get_by("response")
            .and_then(|r| r.get_by("_1"))
            .cloned()
            .ok_or(IdeviceError::UnexpectedResponse(
                "missing response._1 in encrypted response".into(),
            ))?;

        Ok(result)
    }

    /// Send a request to create a TCP tunnel listener on the device.
    /// Returns the port the device is listening on.
    pub async fn create_tcp_listener(&mut self) -> Result<u16, IdeviceError> {
        let request = plist!({
            "request": {
                "_0": {
                    "createListener": {
                        "key": base64::engine::general_purpose::STANDARD.encode(&self.encryption_key),
                        "transportProtocolType": "tcp"
                    }
                }
            }
        });

        let response = self.send_receive_encrypted_request(request).await?;
        debug!("createListener response: {response:#?}");

        let port = response
            .get_by("createListener")
            .and_then(|c| c.get_by("port"))
            .and_then(|p| p.as_unsigned_integer())
            .ok_or(IdeviceError::UnexpectedResponse(
                "missing port in createListener response".into(),
            ))?;

        Ok(port as u16)
    }

    async fn send_pairing_data(
        &mut self,
        pairing_data: impl Serialize + PlistConvertible,
    ) -> Result<(), IdeviceError> {
        self.inner
            .send_plain(
                plist!({
                    "event": {
                        "_0": {
                            "pairingData": {
                                "_0": pairing_data
                            }
                        }
                    }
                }),
                self.sequence_number,
            )
            .await?;

        self.sequence_number += 1;
        Ok(())
    }

    async fn receive_pairing_data(&mut self) -> Result<plist::Value, IdeviceError> {
        let response = self.inner.recv_plain().await?;

        let response = match response.get_by("event").and_then(|x| x.get_by("_0")) {
            Some(r) => r,
            None => {
                return Err(IdeviceError::UnexpectedResponse(
                    "missing event._0 in pairing data response".into(),
                ));
            }
        };

        if let Some(data) = response
            .get_by("pairingData")
            .and_then(|x| x.get_by("_0"))
            .and_then(|x| x.get_by("data"))
        {
            Ok(data.to_owned())
        } else if let Some(err) = response.get_by("pairingRejectedWithError") {
            let context = err
                .get_by("wrappedError")
                .and_then(|x| x.get_by("userInfo"))
                .and_then(|x| x.get_by("NSLocalizedDescription"))
                .and_then(|x| x.as_string())
                .map(|x| x.to_string());
            Err(RemotePairingError::PairingRejected(context.unwrap_or_default()).into())
        } else {
            Err(IdeviceError::UnexpectedResponse(
                "pairing data response contained neither data nor rejection".into(),
            ))
        }
    }
}

impl<R: RpPairingSocketProvider> std::fmt::Debug for RemotePairingClient<'_, R> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RemotePairingClient")
            .field("inner", &self.inner)
            .field("sequence_number", &self.sequence_number)
            .field("pairing_file", &self.pairing_file)
            .field("sending_host", &self.sending_host)
            .finish()
    }
}
