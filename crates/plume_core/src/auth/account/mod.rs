mod login;
mod token;
mod two_factor_auth;

use aes::cipher::BlockModeDecrypt;
use cbc::cipher::{KeyIvInit, block_padding::Pkcs7};
use hmac::{Hmac, KeyInit, Mac};
use reqwest::Response;
use sha2::Sha256;
use srp::ClientVerifier;

use crate::Error;

pub async fn parse_response(
    res: Result<Response, reqwest::Error>,
) -> Result<plist::Dictionary, Error> {
    let res = res?.text().await?;
    let res: plist::Dictionary = plist::from_bytes(res.as_bytes())?;
    let res: plist::Value = res.get("Response").unwrap().to_owned();
    match res {
        plist::Value::Dictionary(dict) => Ok(dict),
        _ => Err(crate::Error::Parse),
    }
}

pub fn check_error(res: &plist::Dictionary) -> Result<(), Error> {
    let res = match res.get("Status") {
        Some(plist::Value::Dictionary(d)) => d,
        _ => &res,
    };

    if res.get("ec").unwrap().as_signed_integer().unwrap() != 0 {
        return Err(Error::AuthSrpWithMessage(
            res.get("ec").unwrap().as_signed_integer().unwrap().into(),
            res.get("em").unwrap().as_string().unwrap().to_owned(),
        ));
    }

    Ok(())
}

pub fn decrypt_cbc(usr: &ClientVerifier<Sha256>, data: &[u8]) -> Vec<u8> {
    let extra_data_key = create_session_key(usr, "extra data key:");
    let extra_data_iv = create_session_key(usr, "extra data iv:");
    let extra_data_iv = &extra_data_iv[..16];

    cbc::Decryptor::<aes::Aes256>::new_from_slices(&extra_data_key, extra_data_iv)
        .unwrap()
        .decrypt_padded_vec::<Pkcs7>(&data)
        .unwrap()
}

/// Sends one of the GSA login packets, retrying across the local VPN proxy
/// and a direct connection until Apple answers with a real plist. Apple's edge
/// intermittently denies non-Apple clients with an HTML page (503/401); those
/// are retried and alternated between routes, while real service replies (any
/// ec value) are returned immediately.
pub(crate) async fn post_gsa_retry(
    default_client: &reqwest::Client,
    url: &str,
    headers: reqwest::header::HeaderMap,
    body: Vec<u8>,
) -> Result<plist::Dictionary, Error> {
    let mut attempt: u32 = 0;
    let mut last_report: String;
    loop {
        attempt += 1;
        let client = if attempt % 2 == 1 {
            match crate::system_proxy_url() {
                Some(proxy_url) => crate::client_with_proxy(Some(&proxy_url))
                    .unwrap_or_else(|_| default_client.clone()),
                None => default_client.clone(),
            }
        } else {
            crate::client_with_proxy(None).unwrap_or_else(|_| default_client.clone())
        };
        match client
            .post(url)
            .headers(headers.clone())
            .body(body.clone())
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                match parse_response(Ok(resp)).await {
                    Ok(dict) => return Ok(dict),
                    Err(err) => last_report = format!("HTTP {status}: {err}"),
                }
            }
            Err(err) => last_report = format!("connection error: {err}"),
        }
        if attempt >= 90 {
            return Err(Error::AuthSrpWithMessage(
                0,
                format!("Apple denied every attempt (VPN and direct). Last: {last_report}"),
            ));
        }
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
    }
}

pub fn create_session_key(usr: &ClientVerifier<Sha256>, name: &str) -> Vec<u8> {
    Hmac::<Sha256>::new_from_slice(&usr.key())
        .unwrap()
        .chain_update(name.as_bytes())
        .finalize()
        .into_bytes()
        .to_vec()
}
