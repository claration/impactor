use std::str::FromStr;

use crate::Error;
use base64::{Engine, engine::general_purpose};
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};

use crate::auth::{Account, AuthenticationExtras, LoginState, PhoneNumber, VerifyBody, VerifyCode};

impl Account {
    pub async fn send_2fa_to_devices(&self) -> Result<LoginState, Error> {
        let headers = self.build_2fa_headers(false).await;

        let res = self
            .client
            .get("https://gsa.apple.com/auth/verify/trusteddevice")
            .headers(headers)
            .send()
            .await?;

        let status_code = res.status();

        if !status_code.is_success() {
            return Err(Error::AuthSrpWithMessage(
                status_code.as_u16() as i64,
                "Failed to send 2FA to devices".to_string(),
            ));
        }

        return Ok(LoginState::Needs2FAVerification);
    }

    pub async fn send_sms_2fa_to_devices(&self, phone_id: u32) -> Result<LoginState, Error> {
        let headers = self.build_2fa_headers(true).await;

        let body = VerifyBody {
            phone_number: PhoneNumber { id: phone_id },
            mode: "sms".to_string(),
            security_code: None,
        };

        let res = self
            .client
            .put("https://gsa.apple.com/auth/verify/phone")
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        let status_code = res.status();

        if !status_code.is_success() {
            return Err(Error::AuthSrpWithMessage(
                status_code.as_u16() as i64,
                "Failed to send SMS 2FA to devices".to_string(),
            ));
        }

        return Ok(LoginState::NeedsSMS2FAVerification(body));
    }

    pub async fn get_auth_extras(&self) -> Result<AuthenticationExtras, Error> {
        let headers = self.build_2fa_headers(true);

        let req = self
            .client
            .get("https://gsa.apple.com/auth")
            .headers(headers.await)
            .header("Accept", "application/json")
            .send()
            .await?;
        let status = req.status().as_u16();
        parse_auth_extras_response(status, &req.bytes().await?)
    }

    pub async fn verify_2fa(&self, code: String) -> Result<LoginState, Error> {
        let headers = self.build_2fa_headers(false);
        let res = self
            .client
            .get("https://gsa.apple.com/grandslam/GsService2/validate")
            .headers(headers.await)
            .header(
                HeaderName::from_str("security-code").unwrap(),
                HeaderValue::from_str(&code).unwrap(),
            )
            .send()
            .await?;

        let res: plist::Dictionary = plist::from_bytes(res.text().await?.as_bytes())?;

        super::check_error(&res)?;

        Ok(LoginState::NeedsLogin)
    }

    pub async fn verify_sms_2fa(
        &self,
        code: String,
        mut body: VerifyBody,
    ) -> Result<LoginState, Error> {
        let headers = self.build_2fa_headers(true).await;
        body.security_code = Some(VerifyCode { code });
        let res = self
            .client
            .post("https://gsa.apple.com/auth/verify/phone/securitycode")
            .headers(headers)
            .json(&body)
            .send()
            .await?;

        let status_code = res.status();

        // TODO: 423 http code may occur, in this case we to ask for sending
        // last code sent (unlikely it would even work), or try again later
        if !status_code.is_success() {
            return Err(Error::Bad2faCode);
        }

        Ok(LoginState::NeedsLogin)
    }

    async fn build_2fa_headers(&self, sms: bool) -> HeaderMap {
        let spd = self.spd.as_ref().unwrap();
        let dsid = spd.get("adsid").unwrap().as_string().unwrap();
        let token = spd.get("GsIdmsToken").unwrap().as_string().unwrap();

        let identity_token = general_purpose::STANDARD.encode(format!("{}:{}", dsid, token));

        let mut headers = HeaderMap::new();
        let valid_anisette = self.get_anisette().await;
        for (k, v) in valid_anisette.generate_headers(false, true, true) {
            headers.insert(
                HeaderName::from_bytes(k.as_bytes()).unwrap(),
                HeaderValue::from_str(&v).unwrap(),
            );
        }
        if !sms {
            headers.insert("Content-Type", HeaderValue::from_static("text/x-xml-plist"));
            headers.insert("Accept", HeaderValue::from_static("text/x-xml-plist"));
        } else {
            headers.insert("Content-Type", HeaderValue::from_static("application/json"));
            headers.insert("Accept", HeaderValue::from_static("application/json"));
        }
        headers.insert(
            "User-Agent",
            HeaderValue::from_str(&valid_anisette.user_agent).unwrap(),
        );
        headers.insert("Connection", HeaderValue::from_static("close"));
        headers.insert("Accept-Language", HeaderValue::from_static("en-us"));
        headers.append(
            "X-Apple-Identity-Token",
            HeaderValue::from_str(&identity_token).unwrap(),
        );

        if let Ok(locale) = valid_anisette.get_header("x-apple-locale") {
            headers.insert("Loc", HeaderValue::from_str(&locale).unwrap());
        }

        headers
    }
}

// Preserve HTTP failures even when the response body is HTML or empty.
fn parse_auth_extras_response(status: u16, body: &[u8]) -> Result<AuthenticationExtras, Error> {
    if !(200..300).contains(&status) {
        return Err(Error::AuthSrpWithMessage(
            status.into(),
            "Failed to retrieve authentication options".into(),
        ));
    }
    parse_auth_extras(status, serde_json::from_slice(body)?)
}

fn parse_auth_extras(status: u16, value: serde_json::Value) -> Result<AuthenticationExtras, Error> {
    let flag = |name: &str| {
        value
            .get("securityCode")
            .and_then(|v| v.get(name))
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
    };
    if flag("securityCodeLocked")
        || (status != 201 && (flag("securityCodeCooldown") || flag("tooManyCodesSent")))
    {
        return Err(Error::AuthSrpWithMessage(
            0,
            "SMS verification is locked or delivery is cooling down".into(),
        ));
    }
    let mut extras: AuthenticationExtras = serde_json::from_value(value.clone())?;
    if status == 201 {
        // Prefer Apple's selected challenge number. A sole returned number is unambiguous.
        let selected = match value
            .get("phoneNumber")
            .or_else(|| value.get("trustedPhoneNumber"))
        {
            Some(number) => Some(
                number
                    .get("id")
                    .and_then(|id| id.as_u64())
                    .and_then(|id| u32::try_from(id).ok())
                    .ok_or_else(|| {
                        Error::AuthSrpWithMessage(0, "Invalid selected SMS challenge number".into())
                    })?,
            ),
            None => None,
        };
        let id = selected
            .or_else(|| match extras.trusted_phone_numbers.as_slice() {
                [number] => Some(number.id),
                _ => None,
            })
            .ok_or_else(|| {
                Error::AuthSrpWithMessage(0, "Missing selected SMS challenge number".into())
            })?;
        if value
            .get("mode")
            .and_then(|v| v.as_str())
            .is_some_and(|m| m != "sms")
        {
            return Err(Error::AuthSrpWithMessage(
                0,
                "Unsupported non-SMS authentication challenge".into(),
            ));
        }
        extras.new_state = Some(LoginState::NeedsSMS2FAVerification(VerifyBody {
            phone_number: PhoneNumber { id },
            mode: "sms".into(),
            security_code: None,
        }));
    }
    Ok(extras)
}

#[cfg(test)]
mod sms_options_tests {
    use super::*;
    #[test]
    fn lookup_http_error_precedes_body_decoding() {
        for body in [b"<html>Forbidden</html>".as_slice(), b"".as_slice()] {
            assert!(matches!(
                parse_auth_extras_response(403, body),
                Err(Error::AuthSrpWithMessage(403, _))
            ));
        }
    }
    #[test]
    fn successful_lookup_with_invalid_json_fails() {
        assert!(parse_auth_extras_response(200, b"not json").is_err());
    }
    #[test]
    fn ambiguous_created_response_does_not_guess_a_number() {
        let numbers = [7, 8].map(|id| {
            serde_json::json!({
                "id": id, "numberWithDialCode": "masked", "lastTwoDigits": "00", "pushMode": "sms"
            })
        });
        assert!(
            parse_auth_extras(201, serde_json::json!({"trustedPhoneNumbers": numbers})).is_err()
        );
    }
    #[test]
    fn invalid_explicit_selection_is_not_replaced_by_the_sole_number() {
        let number = serde_json::json!({"id": 7, "numberWithDialCode": "masked", "lastTwoDigits": "00", "pushMode": "sms"});
        assert!(
            parse_auth_extras(
                201,
                serde_json::json!({
                    "trustedPhoneNumbers": [number], "phoneNumber": {"id": "invalid"}
                })
            )
            .is_err()
        );
    }
    #[test]
    fn locked_verification_and_non_sms_challenges_stop() {
        assert!(parse_auth_extras(201, serde_json::json!({"trustedPhoneNumbers": [], "phoneNumber": {"id": 7}, "securityCode": {"securityCodeLocked": true}})).is_err());
        assert!(parse_auth_extras(201, serde_json::json!({"trustedPhoneNumbers": [], "phoneNumber": {"id": 7}, "mode": "voice"})).is_err());
    }
    #[test]
    fn locked_or_cooling_down_responses_cannot_request_delivery() {
        for name in [
            "securityCodeLocked",
            "securityCodeCooldown",
            "tooManyCodesSent",
        ] {
            let mut value = serde_json::json!({"trustedPhoneNumbers": [], "securityCode": {}});
            value["securityCode"][name] = serde_json::Value::Bool(true);
            assert!(parse_auth_extras(200, value).is_err());
        }
    }
    #[test]
    fn delivery_cooldown_still_allows_an_already_sent_code() {
        let value = serde_json::json!({"trustedPhoneNumbers": [], "phoneNumber": {"id": 9}, "securityCode": {"securityCodeCooldown": true}});
        assert!(parse_auth_extras(201, value).unwrap().new_state.is_some());
    }
    #[test]
    fn created_response_preserves_selected_challenge() {
        let extras = parse_auth_extras(
            201,
            serde_json::json!({
                "trustedPhoneNumbers": [], "phoneNumber": {"id": 9}, "mode": "sms"
            }),
        )
        .unwrap();
        assert!(
            matches!(extras.new_state, Some(LoginState::NeedsSMS2FAVerification(ref b)) if b.phone_number.id == 9)
        );
    }
    #[test]
    fn created_response_without_challenge_number_is_an_error() {
        assert!(parse_auth_extras(201, serde_json::json!({"trustedPhoneNumbers": []})).is_err());
    }
    #[test]
    fn lookup_response_does_not_claim_code_was_sent() {
        assert!(
            parse_auth_extras(200, serde_json::json!({"trustedPhoneNumbers": []}))
                .unwrap()
                .new_state
                .is_none()
        );
    }
}
