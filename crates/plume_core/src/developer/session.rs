use omnisette::AnisetteConfiguration;
use reqwest::header::HeaderName;
use std::sync::Arc;
use tokio::sync::Mutex;

use plist::{Dictionary, Value};
use reqwest::Client;
use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;
use uuid::Uuid;

use crate::Error;

use crate::auth::Account;
use crate::auth::anisette_data::AnisetteData;
use crate::developer::qh::QHResponseMeta;
use crate::developer::v1::V1ErrorResponse;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeveloperDeviceType {
    #[default]
    Ios,
    Tvos,
}

impl From<Option<&str>> for DeveloperDeviceType {
    fn from(product_type: Option<&str>) -> Self {
        match product_type {
            Some(product_type) if product_type.starts_with("AppleTV") => Self::Tvos,
            _ => Self::Ios,
        }
    }
}

pub struct DeveloperSession {
    anisette: Arc<Mutex<AnisetteData>>,
    client: Client,
    adsid: String,          // from grandslam's SPD "adsid"
    xcode_gs_token: String, // requested from spd initially // com.apple.gs.xcode.auth
    device_type: DeveloperDeviceType,
}

impl DeveloperSession {
    pub async fn using_account(account: Account) -> Result<Self, Error> {
        let adsid = account
            .spd
            .as_ref()
            .unwrap()
            .get("adsid")
            .unwrap()
            .as_string()
            .unwrap();
        let xcode_gs_token = account
            .get_app_token("com.apple.gs.xcode.auth")
            .await?
            .auth_token;

        Ok(DeveloperSession {
            anisette: account.anisette.clone(),
            client: account.client.clone(),
            adsid: adsid.into(),
            xcode_gs_token,
            device_type: DeveloperDeviceType::default(),
        })
    }

    pub async fn new(
        adsid: String,
        xcode_gs_token: String,
        config: AnisetteConfiguration,
    ) -> Result<Self, Error> {
        let anisette = AnisetteData::new(config).await?;
        Self::new_with_anisette(adsid, xcode_gs_token, Arc::new(Mutex::new(anisette))).await
    }

    pub async fn new_with_anisette(
        adsid: String,
        xcode_gs_token: String,
        anisette: Arc<Mutex<AnisetteData>>,
    ) -> Result<Self, Error> {
        let client = crate::client()?;

        let s = Self {
            anisette,
            client,
            adsid,
            xcode_gs_token,
            device_type: DeveloperDeviceType::default(),
        };

        // we test the session by listing teams
        // if this fails, the session is invalid (obviously)
        s.qh_list_teams().await?;

        Ok(s)
    }

    pub fn adsid(&self) -> &String {
        &self.adsid
    }

    pub fn xcode_gs_token(&self) -> &String {
        &self.xcode_gs_token
    }

    pub fn set_device_type(&mut self, device_type: DeveloperDeviceType) {
        self.device_type = device_type;
    }
}

impl DeveloperSession {
    pub async fn qh_send_request(
        &self,
        url: &str,
        body: Option<Dictionary>,
    ) -> Result<Dictionary, Error> {
        let mut headers = HeaderMap::new();
        headers.insert("Content-Type", HeaderValue::from_static("text/x-xml-plist"));
        headers.insert("Accept", HeaderValue::from_static("text/x-xml-plist"));
        self.insert_identity_headers(&mut headers).await;
        self.insert_anisette_headers(&mut headers).await;

        let mut body = body.unwrap_or_default();
        body.insert(
            "requestId".into(),
            Value::String(Uuid::new_v4().to_string().to_uppercase()),
        );
        if self.device_type == DeveloperDeviceType::Tvos {
            body.insert("subPlatform".into(), Value::String("tvOS".into()));
        }

        let request_builder = self.client.post(url).headers(headers);

        let mut buffer = Vec::new();
        plist::to_writer_xml(&mut buffer, &body)?;

        log::debug!("QH Request to {}: {:?}", url, body);

        let response = request_builder.body(buffer).send().await?;
        let response_bytes = response.bytes().await?;
        let response_dict: Dictionary = plist::from_bytes(&response_bytes)?;

        log::debug!("QH Response from {}: {:?}", url, response_dict);

        let response_meta: QHResponseMeta =
            plist::from_value(&Value::Dictionary(response_dict.clone()))?;

        if response_meta.result_code.as_signed().unwrap_or(0) != 0 {
            return Err(response_meta.to_error(url.to_string()));
        }

        Ok(response_dict)
    }

    pub async fn v1_send_request(
        &self,
        url: &str,
        body: Option<serde_json::Value>,
        request_type: Option<RequestType>,
    ) -> Result<serde_json::Value, Error> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Content-Type",
            HeaderValue::from_static("application/vnd.api+json"),
        );
        headers.insert(
            "Accept",
            HeaderValue::from_static("application/json, text/plain, */*"),
        );
        headers.insert(
            "X-Requested-With",
            HeaderValue::from_static("XMLHttpRequest"),
        );
        self.insert_identity_headers(&mut headers).await;
        if let Some(RequestType::Get) = request_type {
            headers.insert("X-HTTP-Method-Override", HeaderValue::from_static("GET"));
        }
        self.insert_anisette_headers(&mut headers).await;

        let mut request_builder = match request_type {
            Some(RequestType::Patch) => self.client.patch(url).headers(headers.clone()),
            Some(RequestType::Post) | _ if body.is_some() => {
                self.client.post(url).headers(headers.clone())
            }
            _ => self.client.get(url).headers(headers.clone()),
        };

        log::debug!("V1 Request to {}: {:?}", url, &body);

        if let Some(body) = body {
            request_builder = request_builder.json(&body);
        }

        let response = request_builder.send().await?;
        let response_text = response.text().await?;

        log::debug!("V1 Response from {}: {}", url, response_text);

        let response_json: serde_json::Value = serde_json::from_str(&response_text)?;

        if let Ok(errors) = serde_json::from_value::<V1ErrorResponse>(response_json.clone()) {
            return Err(errors.errors[0].to_error(url.to_string()));
        }

        Ok(response_json)
    }

    // TODO: this can be deduplicated as well, for reuse in `fn build_2fa_headers`
    async fn insert_identity_headers(&self, headers: &mut HeaderMap) {
        headers.insert("Accept-Language", HeaderValue::from_static("en-us"));
        headers.insert("User-Agent", HeaderValue::from_static("Xcode"));
        headers.insert(
            "X-Apple-I-Identity-Id",
            HeaderValue::from_str(&self.adsid).unwrap(),
        );
        headers.insert(
            "X-Apple-GS-Token",
            HeaderValue::from_str(&self.xcode_gs_token).unwrap(),
        );
    }

    async fn insert_anisette_headers(&self, headers: &mut HeaderMap) {
        let valid_anisette = self.get_anisette().await;
        for (k, v) in valid_anisette.generate_headers(false, true, true) {
            headers.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(&v).unwrap(),
            );
        }
        if let Ok(locale) = valid_anisette.get_header("x-apple-locale") {
            headers.insert("X-Apple-Locale", HeaderValue::from_str(&locale).unwrap());
        }
    }

    // TODO: deduplicate?
    pub async fn get_anisette(&self) -> AnisetteData {
        let mut locked = self.anisette.lock().await;
        if locked.needs_refresh() {
            *locked = locked.refresh().await.unwrap();
        }
        locked.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestType {
    Get,
    Post,
    Patch,
}
