use std::collections::HashMap;
use std::time::SystemTime;

use omnisette::{AnisetteConfiguration, AnisetteHeaders};

use crate::Error;

#[derive(Debug, Clone)]
pub struct AnisetteData {
    pub base_headers: HashMap<String, String>,
    pub user_agent: String,
    pub generated_at: SystemTime,
    pub config: AnisetteConfiguration,
}

impl AnisetteData {
    pub async fn new(config: AnisetteConfiguration) -> Result<Self, Error> {
        let mut b = AnisetteHeaders::get_anisette_headers_provider(config.clone())?;
        let mut base_headers = b.provider.get_authentication_headers().await?;
        let user_agent = base_headers
            .remove("User-Agent")
            .unwrap_or_else(|| omnisette::adi_proxy::AKD_USER_AGENT.to_string());

        Ok(AnisetteData {
            base_headers,
            user_agent,
            generated_at: SystemTime::now(),
            config,
        })
    }

    pub fn needs_refresh(&self) -> bool {
        let elapsed = self.generated_at.elapsed().unwrap();
        elapsed.as_secs() > 60
    }

    pub fn is_valid(&self) -> bool {
        let elapsed = self.generated_at.elapsed().unwrap();
        elapsed.as_secs() < 90
    }

    pub async fn refresh(&self) -> Result<Self, crate::Error> {
        Self::new(self.config.clone()).await
    }

    pub fn generate_headers(
        &self,
        cpd: bool,
        client_info: bool,
        app_info: bool,
    ) -> HashMap<String, String> {
        if !self.is_valid() {
            panic!("Invalid data!")
        }

        let mut headers = self.base_headers.clone();
        if !client_info {
            headers.remove("X-Mme-Client-Info");
        }

        if app_info {
            headers.insert(
                "X-Apple-App-Info".to_owned(),
                "com.apple.gs.xcode.auth".to_owned(),
            );
            headers.insert("X-Xcode-Version".to_owned(), "11.2 (11B41)".to_owned());
        }

        if cpd {
            headers.insert("bootstrap".to_owned(), "true".to_owned());
            headers.insert("icscrec".to_owned(), "true".to_owned());
            headers.insert("loc".to_owned(), "en_GB".to_owned());
            headers.insert("pbe".to_owned(), "false".to_owned());
            headers.insert("prkgen".to_owned(), "true".to_owned());
            headers.insert("svct".to_owned(), "iCloud".to_owned());
        }

        headers
    }

    pub fn to_plist(&self, cpd: bool, client_info: bool, app_info: bool) -> plist::Dictionary {
        let mut plist = plist::Dictionary::new();
        for (key, value) in self.generate_headers(cpd, client_info, app_info).iter() {
            plist.insert(key.to_owned(), plist::Value::String(value.to_owned()));
        }

        plist
    }

    pub fn get_header(&self, header: &str) -> Result<String, Error> {
        let headers = self
            .generate_headers(true, true, true)
            .iter()
            .map(|(k, v)| (k.to_lowercase(), v.clone()))
            .collect::<HashMap<String, String>>();

        match headers.get(&header.to_lowercase()) {
            Some(v) => Ok(v.to_string()),
            None => Err(Error::DeveloperSessionRequestFailed),
        }
    }
}

#[cfg(test)]
mod identity_tests {
    use super::*;
    fn synthetic() -> AnisetteData {
        AnisetteData {
            base_headers: HashMap::from([
                (
                    "X-Mme-Client-Info".into(),
                    "<MacTest> <Mac OS X;Test> <com.apple.AuthKit/1 (com.apple.akd/1.0)>".into(),
                ),
                ("X-Apple-I-MD".into(), "AbCdEf+/=".into()),
                ("X-Apple-Locale".into(), "en_US".into()),
            ]),
            user_agent: "akd/1.0 CFNetwork/Test Darwin/Test".into(),
            generated_at: SystemTime::now(),
            config: AnisetteConfiguration::default(),
        }
    }
    #[test]
    fn identity_lookup_preserves_case_sensitive_values() {
        let data = synthetic();
        assert_eq!(data.get_header("X-APPLE-I-MD").unwrap(), "AbCdEf+/=");
        assert_eq!(data.get_header("x-apple-locale").unwrap(), "en_US");
    }
    #[test]
    fn identity_matches_between_srp_and_two_factor_headers() {
        let data = synthetic();
        assert_eq!(
            data.get_header("x-mme-client-info").unwrap(),
            data.generate_headers(false, true, true)["X-Mme-Client-Info"]
        );
        assert_eq!(
            data.generate_headers(false, true, true)["X-Mme-Client-Info"],
            data.base_headers["X-Mme-Client-Info"]
        );
    }
    #[test]
    fn identity_user_agent_is_not_added_to_srp_cpd() {
        let data = synthetic();
        let cpd = data.to_plist(true, false, false);
        assert!(!cpd.contains_key("User-Agent"));
        assert!(!cpd.contains_key("X-Mme-Client-Info"));
        assert_eq!(data.user_agent, "akd/1.0 CFNetwork/Test Darwin/Test");
    }
}
