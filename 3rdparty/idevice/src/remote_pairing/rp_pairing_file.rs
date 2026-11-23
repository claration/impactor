// Jackson Coxson

use std::path::Path;

use ed25519_dalek::{SigningKey, VerifyingKey};
use plist::Dictionary;
use plist_macro::plist_to_xml_bytes;
use rsa::rand_core::OsRng;
use serde::de::Error;
use tracing::{debug, warn};

use crate::IdeviceError;

#[derive(Clone)]
pub struct RpPairingFile {
    pub e_private_key: SigningKey,
    pub e_public_key: VerifyingKey,
    pub identifier: String,
    pub alt_irk: Option<Vec<u8>>,
}

impl RpPairingFile {
    /// Returns the Ed25519 public key bytes (32 bytes).
    pub fn public_key_bytes(&self) -> Vec<u8> {
        self.e_public_key.to_bytes().to_vec()
    }

    /// Returns the Ed25519 private key bytes (32 bytes).
    pub fn private_key_bytes(&self) -> Vec<u8> {
        self.e_private_key.to_bytes().to_vec()
    }

    /// Returns the identifier string.
    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    /// Returns the `alt_irk` bytes (16 bytes).
    pub fn alt_irk(&self) -> Option<&[u8]> {
        self.alt_irk.as_deref()
    }

    pub fn generate(sending_host: &str) -> Self {
        // Ed25519 private key (persistent signing key)
        let ed25519_private_key = SigningKey::generate(&mut OsRng);
        let ed25519_public_key = VerifyingKey::from(&ed25519_private_key);

        let identifier =
            uuid::Uuid::new_v3(&uuid::Uuid::NAMESPACE_DNS, sending_host.as_bytes()).to_string();

        Self {
            e_private_key: ed25519_private_key,
            e_public_key: ed25519_public_key,
            identifier,
            alt_irk: None,
        }
    }

    pub(crate) fn recreate_signing_keys(&mut self) {
        let ed25519_private_key = SigningKey::generate(&mut OsRng);
        let ed25519_public_key = VerifyingKey::from(&ed25519_private_key);
        self.e_public_key = ed25519_public_key;
        self.e_private_key = ed25519_private_key;
        self.alt_irk = None;
    }

    /// Serialize to XML plist bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut dict = plist::Dictionary::new();
        dict.insert(
            "public_key".into(),
            plist::Value::Data(self.e_public_key.to_bytes().to_vec()),
        );
        dict.insert(
            "private_key".into(),
            plist::Value::Data(self.e_private_key.to_bytes().to_vec()),
        );
        dict.insert(
            "identifier".into(),
            plist::Value::String(self.identifier.clone()),
        );
        if let Some(irk) = &self.alt_irk {
            dict.insert("alt_irk".into(), plist::Value::Data(irk.clone()));
        }
        plist_to_xml_bytes(&dict)
    }

    /// Parse from plist bytes (XML or binary).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IdeviceError> {
        let mut p: Dictionary = plist::from_bytes(bytes)?;
        debug!("Read dictionary for rppairingfile: {p:#?}");

        let public_key = match p
            .remove("public_key")
            .and_then(|x| x.into_data())
            .filter(|x| x.len() == 32)
            .and_then(|x| VerifyingKey::from_bytes(&x[..32].try_into().unwrap()).ok())
        {
            Some(p) => p,
            None => {
                warn!("plist did not contain valid public key bytes");
                return Err(IdeviceError::Plist(plist::Error::missing_field(
                    "public_key",
                )));
            }
        };

        let private_key = match p
            .remove("private_key")
            .and_then(|x| x.into_data())
            .filter(|x| x.len() == 32)
        {
            Some(p) => SigningKey::from_bytes(&p.try_into().unwrap()),
            None => {
                warn!("plist did not contain valid private key bytes");
                return Err(IdeviceError::Plist(plist::Error::missing_field(
                    "private_key",
                )));
            }
        };

        let identifier = match p.remove("identifier").and_then(|x| x.into_string()) {
            Some(i) => i,
            None => {
                warn!("plist did not contain identifier");
                return Err(IdeviceError::Plist(plist::Error::missing_field(
                    "identifier",
                )));
            }
        };

        let alt_irk = match p.remove("alt_irk").and_then(|x| x.into_data()) {
            Some(irk) => Some(irk),
            None => {
                warn!("plist did not contain alt_irk");
                None
            }
        };

        Ok(Self {
            e_private_key: private_key,
            e_public_key: public_key,
            identifier,
            alt_irk,
        })
    }

    pub async fn write_to_file(&self, path: impl AsRef<Path>) -> Result<(), IdeviceError> {
        tokio::fs::write(path, self.to_bytes()).await?;
        Ok(())
    }

    pub async fn read_from_file(path: impl AsRef<Path>) -> Result<Self, IdeviceError> {
        let bytes = tokio::fs::read(path).await?;
        Self::from_bytes(&bytes)
    }
}

impl std::fmt::Debug for RpPairingFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RpPairingFile")
            .field("e_public_key", &self.e_public_key)
            .field("identifier", &self.identifier)
            .field("alt_irk", &self.alt_irk)
            .finish()
    }
}
