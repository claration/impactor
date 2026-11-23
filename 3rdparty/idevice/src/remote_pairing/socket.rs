// Jackson Coxson

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use plist_macro::{plist, pretty_print_plist};
use serde::Serialize;
use serde_json::json;
use std::{fmt::Debug, pin::Pin};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, warn};

use crate::{
    IdeviceError, ReadWrite, RemoteXpcClient, remote_pairing::RPPAIRING_MAGIC, xpc::XPCObject,
};

pub trait RpPairingSocketProvider: Debug {
    fn send_plain(
        &mut self,
        value: impl Serialize,
        seq: usize,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send + '_>>;

    fn send_encrypted(
        &mut self,
        ciphertext: Vec<u8>,
        seq: usize,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send + '_>>;

    fn recv_plain<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<plist::Value, IdeviceError>> + Send + 'a>>;

    /// rppairing uses b64, while RemoteXPC uses raw bytes just fine
    fn serialize_bytes(b: &[u8]) -> plist::Value;
    fn deserialize_bytes(v: plist::Value) -> Option<Vec<u8>>;
}

#[derive(Debug)]
pub struct RpPairingSocket<R: ReadWrite> {
    pub inner: R,
}

impl<R: ReadWrite> RpPairingSocket<R> {
    pub fn new(socket: R) -> Self {
        Self { inner: socket }
    }

    async fn send_rppairing(&mut self, value: impl Serialize) -> Result<(), IdeviceError> {
        let value = serde_json::to_string(&value)?;
        let x = value.as_bytes();

        self.inner.write_all(RPPAIRING_MAGIC).await?;
        self.inner
            .write_all(&(x.len() as u16).to_be_bytes())
            .await?;
        self.inner.write_all(x).await?;
        Ok(())
    }
}

impl<R: ReadWrite> RpPairingSocketProvider for RpPairingSocket<R> {
    fn send_plain(
        &mut self,
        value: impl Serialize,
        seq: usize,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send + '_>> {
        let v = json!({
            "message": {"plain": {"_0": value}},
            "originatedBy": "host",
            "sequenceNumber": seq
        });

        Box::pin(async move {
            self.send_rppairing(v).await?;
            Ok(())
        })
    }

    fn send_encrypted(
        &mut self,
        ciphertext: Vec<u8>,
        seq: usize,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send + '_>> {
        let v = json!({
            "message": {"streamEncrypted": {"_0": B64.encode(&ciphertext)}},
            "originatedBy": "host",
            "sequenceNumber": seq
        });

        Box::pin(async move {
            self.send_rppairing(v).await?;
            Ok(())
        })
    }

    fn recv_plain<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<plist::Value, IdeviceError>> + Send + 'a>> {
        Box::pin(async move {
            self.inner
                .read_exact(&mut vec![0u8; RPPAIRING_MAGIC.len()])
                .await?;

            let mut packet_len_bytes = [0u8; 2];
            self.inner.read_exact(&mut packet_len_bytes).await?;
            let packet_len = u16::from_be_bytes(packet_len_bytes);

            let mut value = vec![0u8; packet_len as usize];
            self.inner.read_exact(&mut value).await?;

            let value: serde_json::Value = serde_json::from_slice(&value)?;

            // Try plain first, then return the whole message dict for encrypted
            if let Some(v) = value
                .get("message")
                .and_then(|x| x.get("plain"))
                .and_then(|x| x.get("_0"))
            {
                Ok(plist::to_value(v).unwrap())
            } else {
                // Return the full message for encrypted handling
                Ok(plist::to_value(&value).unwrap())
            }
        })
    }

    fn serialize_bytes(b: &[u8]) -> plist::Value {
        plist!(B64.encode(b))
    }

    fn deserialize_bytes(v: plist::Value) -> Option<Vec<u8>> {
        if let plist::Value::String(v) = v {
            B64.decode(v).ok()
        } else {
            None
        }
    }
}

impl<R: ReadWrite> RpPairingSocketProvider for RemoteXpcClient<R> {
    fn send_plain(
        &mut self,
        value: impl Serialize,
        seq: usize,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send + '_>> {
        let value: plist::Value = plist::to_value(&value).expect("plist assert failed");
        let value: XPCObject = value.into();

        let v = crate::xpc!({
            "mangledTypeName": "RemotePairing.ControlChannelMessageEnvelope",
            "value": {
                "message": {"plain": {"_0": value}},
                "originatedBy": "host",
                "sequenceNumber": seq as u64
            }
        });
        debug!("Sending XPC: {v:#?}");

        Box::pin(async move {
            self.send_object(v, true).await?;
            Ok(())
        })
    }

    fn send_encrypted(
        &mut self,
        ciphertext: Vec<u8>,
        seq: usize,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdeviceError>> + Send + '_>> {
        let v = crate::xpc!({
            "mangledTypeName": "RemotePairing.ControlChannelMessageEnvelope",
            "value": {
                "message": {"streamEncrypted": {"_0": ciphertext}},
                "originatedBy": "host",
                "sequenceNumber": seq as u64
            }
        });

        Box::pin(async move {
            self.send_object(v, true).await?;
            Ok(())
        })
    }

    fn recv_plain<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<plist::Value, IdeviceError>> + Send + 'a>> {
        Box::pin(async move {
            let msg = self.recv_root().await.unwrap();
            debug!("Received RemoteXPC {}", pretty_print_plist(&msg));
            let msg = msg.into_dictionary().and_then(|mut x| x.remove("value"));

            let msg = match msg {
                Some(v) => v,
                None => {
                    return Err(IdeviceError::UnexpectedResponse(
                        "missing value field in RemoteXPC message".into(),
                    ));
                }
            };

            // Try plain first
            if let Some(plain) = msg
                .as_dictionary()
                .and_then(|x| x.get("message"))
                .and_then(|x| x.as_dictionary())
                .and_then(|x| x.get("plain"))
                .and_then(|x| x.as_dictionary())
                .and_then(|x| x.get("_0"))
                .cloned()
            {
                return Ok(plain);
            }

            // Return the whole value dict for encrypted handling
            Ok(msg)
        })
    }

    fn serialize_bytes(b: &[u8]) -> plist::Value {
        plist::Value::Data(b.to_owned())
    }

    fn deserialize_bytes(v: plist::Value) -> Option<Vec<u8>> {
        if let plist::Value::Data(v) = v {
            Some(v)
        } else {
            warn!("Non-data passed to rppairingsocket::deserialize_bytes for RemoteXPC provider");
            None
        }
    }
}
