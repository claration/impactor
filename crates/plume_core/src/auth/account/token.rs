use aes::cipher::consts::U16;
use aes_gcm::AesGcm;
use aes_gcm::aes::Aes256;
use aes_gcm::{AeadInOut, Nonce};
use hmac::digest::KeyInit;
use hmac::{Hmac, Mac};
use reqwest::header::{HeaderMap, HeaderValue};

use crate::Error;
use sha2::Sha256;

use crate::auth::account::{check_error, parse_response};
use crate::auth::{
    Account, AppToken, AuthTokenRequest, AuthTokenRequestBody, GSA_ENDPOINT, RequestHeader,
};

impl Account {
    pub async fn get_app_token(&self, app_name: &str) -> Result<AppToken, Error> {
        let spd = self.spd.as_ref().unwrap();
        let dsid = spd.get("adsid").unwrap().as_string().unwrap();
        let auth_token = spd.get("GsIdmsToken").unwrap().as_string().unwrap();

        let valid_anisette = self.get_anisette().await;

        let sk = spd.get("sk").unwrap().as_data().unwrap();
        let c = spd.get("c").unwrap().as_data().unwrap();

        let checksum = Self::create_checksum(&sk.to_vec(), dsid, app_name);

        let mut gsa_headers = HeaderMap::new();
        gsa_headers.insert(
            "Content-Type",
            HeaderValue::from_str("text/x-xml-plist").unwrap(),
        );
        gsa_headers.insert("Accept", HeaderValue::from_str("*/*").unwrap());
        gsa_headers.insert("Connection", HeaderValue::from_static("close"));
        gsa_headers.insert(
            "User-Agent",
            HeaderValue::from_str(&valid_anisette.user_agent).unwrap(),
        );
        gsa_headers.insert(
            "X-MMe-Client-Info",
            HeaderValue::from_str(&valid_anisette.get_header("x-mme-client-info")?).unwrap(),
        );

        let header = RequestHeader {
            version: "1.0.1".to_string(),
        };
        let body = AuthTokenRequestBody {
            cpd: valid_anisette.to_plist(true, false, false),
            app: vec![app_name.to_string()],
            c: plist::Value::Data(c.to_vec()),
            operation: "apptokens".to_owned(),
            t: auth_token.to_string(),
            u: dsid.to_string(),
            checksum: plist::Value::Data(checksum),
        };

        let packet = AuthTokenRequest {
            header: header.clone(),
            request: body,
        };

        let mut buffer = Vec::new();
        plist::to_writer_xml(&mut buffer, &packet)?;
        let buffer = String::from_utf8(buffer).unwrap();

        let res = self
            .client
            .post(GSA_ENDPOINT)
            .headers(gsa_headers.clone())
            .body(buffer)
            .send()
            .await;
        let res = parse_response(res).await?;
        let err_check = check_error(&res);
        if err_check.is_err() {
            return Err(err_check.err().unwrap());
        }

        let encrypted_token = res
            .get("et")
            .ok_or(Error::Parse)?
            .as_data()
            .ok_or(Error::Parse)?;

        if encrypted_token.len() < 3 + 16 + 16 {
            return Err(Error::Parse);
        }
        let header = &encrypted_token[0..3];
        if header != b"XYZ" {
            return Err(Error::AuthSrpWithMessage(
                0,
                "Encrypted token is in an unknown format.".to_string(),
            ));
        }
        let iv = &encrypted_token[3..19];
        let ciphertext_and_tag = &encrypted_token[19..];

        if sk.len() != 32 {
            return Err(Error::Parse);
        }
        if iv.len() != 16 {
            return Err(Error::Parse);
        }

        let key = aes_gcm::Key::<AesGcm<Aes256, U16>>::try_from(sk)?;
        let cipher = AesGcm::<Aes256, U16>::new(&key);
        let nonce = Nonce::<U16>::try_from(iv)?;

        let mut buf = ciphertext_and_tag.to_vec();

        cipher.decrypt_in_place(&nonce, header, &mut buf)?;

        let decrypted_token: plist::Dictionary =
            plist::from_bytes(&buf).map_err(|_| Error::Parse)?;

        let t_val = decrypted_token.get("t").ok_or(Error::Parse)?;
        let app_tokens = t_val.as_dictionary().ok_or(Error::Parse)?;
        let app_token_dict = app_tokens.get(app_name).ok_or(Error::Parse)?;
        let app_token = app_token_dict.as_dictionary().ok_or(Error::Parse)?;
        let token = app_token
            .get("token")
            .and_then(|v| v.as_string())
            .ok_or(Error::Parse)?;

        Ok(AppToken {
            app_tokens: app_tokens.clone(),
            auth_token: token.to_string(),
            app: app_name.to_string(),
        })
    }

    fn create_checksum(session_key: &Vec<u8>, dsid: &str, app_name: &str) -> Vec<u8> {
        Hmac::<Sha256>::new_from_slice(&session_key)
            .unwrap()
            .chain_update("apptokens".as_bytes())
            .chain_update(dsid.as_bytes())
            .chain_update(app_name.as_bytes())
            .finalize()
            .into_bytes()
            .to_vec()
    }
}
