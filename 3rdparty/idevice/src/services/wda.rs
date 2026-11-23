//! Minimal WebDriverAgent bootstrap client over direct device connections.
//!
//! This client talks to WDA on the device port directly through
//! [`crate::provider::IdeviceProvider`], so parallel automation across many
//! devices does not require binding unique localhost ports per device.
//!
//! The API intentionally remains library-first and currently covers session
//! bootstrap plus the most common WDA interactions. It is not yet a full
//! long-lived WebDriver transport and currently assumes simple HTTP JSON
//! request/response flows.

use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use tokio::time::{Instant, sleep, timeout};

use crate::{Idevice, IdeviceError, provider::IdeviceProvider};

/// Default WDA HTTP port on the device.
pub const DEFAULT_WDA_PORT: u16 = 8100;

/// Default MJPEG streaming port used by many WDA builds.
pub const DEFAULT_WDA_MJPEG_PORT: u16 = 9100;

/// Poll interval used while waiting for WDA to begin responding.
const WDA_READY_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Device-side ports exposed by a WDA runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WdaPorts {
    /// Device port for the HTTP WebDriver endpoint.
    pub http: u16,
    /// Device port for the MJPEG stream endpoint.
    pub mjpeg: u16,
}

impl Default for WdaPorts {
    fn default() -> Self {
        Self {
            http: DEFAULT_WDA_PORT,
            mjpeg: DEFAULT_WDA_MJPEG_PORT,
        }
    }
}

/// Minimal WDA bootstrap client bound to a specific device provider.
///
/// This type intentionally opens a fresh direct device connection per request
/// to keep the transport simple and independent per device.
#[derive(Debug)]
pub struct WdaClient<'a> {
    provider: &'a dyn IdeviceProvider,
    ports: WdaPorts,
    timeout: Duration,
    session_id: Option<String>,
}

impl<'a> WdaClient<'a> {
    /// Creates a WDA client using the default device-side ports.
    pub fn new(provider: &'a dyn IdeviceProvider) -> Self {
        Self {
            provider,
            ports: WdaPorts::default(),
            timeout: Duration::from_secs(10),
            session_id: None,
        }
    }

    /// Overrides the device-side WDA ports.
    pub fn with_ports(mut self, ports: WdaPorts) -> Self {
        self.ports = ports;
        self
    }

    /// Overrides the per-request timeout.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Returns the configured device-side ports.
    pub fn ports(&self) -> WdaPorts {
        self.ports
    }

    /// Returns the currently tracked WDA session id, if one exists.
    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    /// Fetches `/status` from the WDA HTTP endpoint.
    pub async fn status(&self) -> Result<Value, IdeviceError> {
        self.request_json("GET", "/status", None).await
    }

    /// Waits until WDA begins responding on its HTTP endpoint.
    ///
    /// This uses a modest polling interval to avoid hammering usbmux/device
    /// connects when many devices are starting up in parallel.
    pub async fn wait_until_ready(
        &self,
        timeout_duration: Duration,
    ) -> Result<Value, IdeviceError> {
        let deadline = Instant::now() + timeout_duration;
        loop {
            match self.status().await {
                Ok(status) => return Ok(status),
                Err(_) if Instant::now() < deadline => {
                    sleep(WDA_READY_POLL_INTERVAL).await;
                }
                Err(error) => return Err(error),
            }
        }
    }

    /// Starts a WDA session and returns the session id.
    pub async fn start_session(&mut self, bundle_id: Option<&str>) -> Result<String, IdeviceError> {
        let mut caps = serde_json::Map::new();
        if let Some(bundle_id) = bundle_id {
            caps.insert("bundleId".into(), Value::String(bundle_id.to_owned()));
        }

        let mut capabilities = serde_json::Map::new();
        capabilities.insert("alwaysMatch".into(), Value::Object(caps.clone()));

        let payload = Value::Object(serde_json::Map::from_iter([
            ("capabilities".into(), Value::Object(capabilities)),
            ("desiredCapabilities".into(), Value::Object(caps)),
        ]));

        let response = self
            .request_json("POST", "/session", Some(&payload))
            .await?;
        let session_id = Self::extract_session_id(&response)?;
        self.session_id = Some(session_id.clone());
        Ok(session_id)
    }

    /// Finds a single element and returns its WDA element id.
    pub async fn find_element(
        &self,
        using: &str,
        value: &str,
        session_id: Option<&str>,
    ) -> Result<String, IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let response = self
            .request_json(
                "POST",
                &format!("/session/{session_id}/element"),
                Some(&json!({ "using": using, "value": value })),
            )
            .await?;
        Self::extract_element_id(Self::value_field(&response)?)
    }

    /// Finds multiple elements and returns their WDA element ids.
    pub async fn find_elements(
        &self,
        using: &str,
        value: &str,
        session_id: Option<&str>,
    ) -> Result<Vec<String>, IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let response = self
            .request_json(
                "POST",
                &format!("/session/{session_id}/elements"),
                Some(&json!({ "using": using, "value": value })),
            )
            .await?;
        let values =
            Self::value_field(&response)?
                .as_array()
                .ok_or(IdeviceError::UnexpectedResponse(
                    "unexpected response".into(),
                ))?;
        values.iter().map(Self::extract_element_id).collect()
    }

    /// Clicks an element by its WDA element id.
    pub async fn click(
        &self,
        element_id: &str,
        session_id: Option<&str>,
    ) -> Result<(), IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        self.request_json(
            "POST",
            &format!("/session/{session_id}/element/{element_id}/click"),
            Some(&json!({})),
        )
        .await?;
        Ok(())
    }

    /// Returns a raw attribute value for an element.
    pub async fn element_attribute(
        &self,
        element_id: &str,
        name: &str,
        session_id: Option<&str>,
    ) -> Result<Value, IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let response = self
            .request_json(
                "GET",
                &format!("/session/{session_id}/element/{element_id}/attribute/{name}"),
                None,
            )
            .await?;
        Ok(Self::value_field(&response)?.clone())
    }

    /// Returns the element text-like value as a string when WDA provides it.
    pub async fn element_text(
        &self,
        element_id: &str,
        session_id: Option<&str>,
    ) -> Result<String, IdeviceError> {
        self.element_attribute(element_id, "value", session_id)
            .await?
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            ))
    }

    /// Returns the element bounds rectangle.
    pub async fn element_rect(
        &self,
        element_id: &str,
        session_id: Option<&str>,
    ) -> Result<Value, IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let response = self
            .request_json(
                "GET",
                &format!("/session/{session_id}/element/{element_id}/rect"),
                None,
            )
            .await?;
        Ok(Self::value_field(&response)?.clone())
    }

    /// Returns whether an element is displayed.
    pub async fn element_displayed(
        &self,
        element_id: &str,
        session_id: Option<&str>,
    ) -> Result<bool, IdeviceError> {
        self.element_bool_state(element_id, "displayed", session_id)
            .await
    }

    /// Returns whether an element is enabled.
    pub async fn element_enabled(
        &self,
        element_id: &str,
        session_id: Option<&str>,
    ) -> Result<bool, IdeviceError> {
        self.element_bool_state(element_id, "enabled", session_id)
            .await
    }

    /// Returns whether an element is selected.
    pub async fn element_selected(
        &self,
        element_id: &str,
        session_id: Option<&str>,
    ) -> Result<bool, IdeviceError> {
        self.element_bool_state(element_id, "selected", session_id)
            .await
    }

    /// Presses a hardware button through WDA if the current server supports it.
    pub async fn press_button(
        &self,
        name: &str,
        session_id: Option<&str>,
    ) -> Result<(), IdeviceError> {
        let normalized = normalize_wda_button_name(name);
        let payload = json!({ "name": normalized });

        if let Some(session_id) = session_id.or(self.session_id()) {
            match self
                .request_json(
                    "POST",
                    &format!("/session/{session_id}/wda/pressButton"),
                    Some(&payload),
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(IdeviceError::UnknownErrorType(message)) if message.contains("404") => {}
                Err(error) => return Err(error),
            }

            if self.try_keys_endpoint(session_id, &normalized).await? {
                return Ok(());
            }
        }

        if normalized == "home" {
            self.request_json("POST", "/wda/homescreen", Some(&json!({})))
                .await?;
            return Ok(());
        }

        Err(IdeviceError::UnknownErrorType(
            "WDA does not support pressButton or keys endpoints".into(),
        ))
    }

    /// Unlocks the device via WDA.
    pub async fn unlock(&self, session_id: Option<&str>) -> Result<(), IdeviceError> {
        if let Some(session_id) = session_id.or(self.session_id()) {
            match self
                .request_json(
                    "POST",
                    &format!("/session/{session_id}/wda/unlock"),
                    Some(&json!({})),
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(IdeviceError::UnknownErrorType(message)) if message.contains("404") => {}
                Err(error) => return Err(error),
            }
        }

        self.request_json("POST", "/wda/unlock", Some(&json!({})))
            .await?;
        Ok(())
    }

    /// Returns the current UI source tree as XML.
    pub async fn source(&self, session_id: Option<&str>) -> Result<String, IdeviceError> {
        let path = match session_id.or(self.session_id()) {
            Some(session_id) => format!("/session/{session_id}/source"),
            None => "/source".to_owned(),
        };
        let response = self.request_json("GET", &path, None).await?;
        Self::value_field(&response)?
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            ))
    }

    /// Returns a PNG screenshot as raw bytes.
    pub async fn screenshot(&self, session_id: Option<&str>) -> Result<Vec<u8>, IdeviceError> {
        let path = match session_id.or(self.session_id()) {
            Some(session_id) => format!("/session/{session_id}/screenshot"),
            None => "/screenshot".to_owned(),
        };
        let response = self.request_json("GET", &path, None).await?;
        let value =
            Self::value_field(&response)?
                .as_str()
                .ok_or(IdeviceError::UnexpectedResponse(
                    "unexpected response".into(),
                ))?;
        STANDARD
            .decode(value)
            .map_err(|_| IdeviceError::UnexpectedResponse("unexpected response".into()))
    }

    /// Returns the current window size payload from WDA.
    pub async fn window_size(&self, session_id: Option<&str>) -> Result<Value, IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let response = self
            .request_json("GET", &format!("/session/{session_id}/window/size"), None)
            .await?;
        Ok(Self::value_field(&response)?.clone())
    }

    /// Sends text input to the currently focused element.
    pub async fn send_keys(
        &self,
        text: &str,
        session_id: Option<&str>,
    ) -> Result<(), IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let payload = json!({
            "value": text.chars().map(|ch| ch.to_string()).collect::<Vec<_>>()
        });

        match self
            .request_json(
                "POST",
                &format!("/session/{session_id}/wda/keys"),
                Some(&payload),
            )
            .await
        {
            Ok(_) => Ok(()),
            Err(IdeviceError::UnknownErrorType(message)) if message.contains("404") => {
                self.request_json(
                    "POST",
                    &format!("/session/{session_id}/keys"),
                    Some(&payload),
                )
                .await?;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    /// Swipes from one coordinate to another.
    pub async fn swipe(
        &self,
        start_x: i64,
        start_y: i64,
        end_x: i64,
        end_y: i64,
        duration: f64,
        session_id: Option<&str>,
    ) -> Result<(), IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        self.request_json(
            "POST",
            &format!("/session/{session_id}/wda/dragfromtoforduration"),
            Some(&json!({
                "fromX": start_x,
                "fromY": start_y,
                "toX": end_x,
                "toY": end_y,
                "duration": duration,
            })),
        )
        .await?;
        Ok(())
    }

    /// Performs a tap gesture on the screen or relative to an element.
    pub async fn tap(
        &self,
        x: Option<f64>,
        y: Option<f64>,
        element_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<(), IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        match self
            .execute_gesture("tap", x, y, element_id, None, Some(session_id))
            .await
        {
            Ok(()) => Ok(()),
            Err(IdeviceError::UnknownErrorType(message)) if message.contains("status=404") => {
                let (tap_x, tap_y) = self
                    .resolve_gesture_coordinates(x, y, element_id, session_id)
                    .await?;
                self.perform_tap_actions(session_id, tap_x, tap_y, 1).await
            }
            Err(error) => Err(error),
        }
    }

    /// Performs a double-tap gesture on the screen or relative to an element.
    pub async fn double_tap(
        &self,
        x: Option<f64>,
        y: Option<f64>,
        element_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<(), IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        match self
            .execute_gesture("doubleTap", x, y, element_id, None, Some(session_id))
            .await
        {
            Ok(()) => Ok(()),
            Err(IdeviceError::UnknownErrorType(message)) if message.contains("status=404") => {
                let (tap_x, tap_y) = self
                    .resolve_gesture_coordinates(x, y, element_id, session_id)
                    .await?;
                self.perform_tap_actions(session_id, tap_x, tap_y, 2).await
            }
            Err(error) => Err(error),
        }
    }

    /// Performs a long-press gesture on the screen or relative to an element.
    pub async fn touch_and_hold(
        &self,
        duration: f64,
        x: Option<f64>,
        y: Option<f64>,
        element_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<(), IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        match self
            .execute_gesture(
                "touchAndHold",
                x,
                y,
                element_id,
                Some(duration),
                Some(session_id),
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(IdeviceError::UnknownErrorType(message)) if message.contains("status=404") => {
                let (hold_x, hold_y) = self
                    .resolve_gesture_coordinates(x, y, element_id, session_id)
                    .await?;
                self.perform_touch_and_hold_actions(session_id, hold_x, hold_y, duration)
                    .await
            }
            Err(error) => Err(error),
        }
    }

    /// Scrolls the current view or an element using a WDA mobile command.
    ///
    /// Typical directions are `up`, `down`, `left`, and `right`.
    pub async fn scroll(
        &self,
        direction: Option<&str>,
        name: Option<&str>,
        predicate_string: Option<&str>,
        to_visible: Option<bool>,
        element_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<(), IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let mut payload = serde_json::Map::new();

        if let Some(direction) = direction {
            payload.insert("direction".into(), Value::String(direction.to_owned()));
        }
        if let Some(name) = name {
            payload.insert("name".into(), Value::String(name.to_owned()));
        }
        if let Some(predicate_string) = predicate_string {
            payload.insert(
                "predicateString".into(),
                Value::String(predicate_string.to_owned()),
            );
        }
        if let Some(to_visible) = to_visible {
            payload.insert("toVisible".into(), Value::Bool(to_visible));
        }
        if let Some(element_id) = element_id {
            payload.insert("elementId".into(), Value::String(element_id.to_owned()));
        }

        self.execute_mobile_method(session_id, "scroll", Value::Object(payload))
            .await?;
        Ok(())
    }

    /// Returns the current viewport rectangle if the server exposes it.
    pub async fn viewport_rect(&self, session_id: Option<&str>) -> Result<Value, IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let response = self
            .execute_mobile_method(
                session_id,
                "viewportRect",
                Value::Object(Default::default()),
            )
            .await?;
        Ok(Self::value_field(&response)?.clone())
    }

    /// Returns the current orientation if the server exposes it.
    pub async fn orientation(&self, session_id: Option<&str>) -> Result<String, IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let response = self
            .request_json("GET", &format!("/session/{session_id}/orientation"), None)
            .await?;
        Self::value_field(&response)?
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            ))
    }

    /// Launches or activates an application via WDA.
    pub async fn launch_app(
        &self,
        bundle_id: &str,
        arguments: Option<&[String]>,
        environment: Option<&serde_json::Map<String, Value>>,
        session_id: Option<&str>,
    ) -> Result<Value, IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let mut payload = serde_json::Map::new();
        payload.insert("bundleId".into(), Value::String(bundle_id.to_owned()));
        if let Some(arguments) = arguments {
            payload.insert(
                "arguments".into(),
                Value::Array(arguments.iter().cloned().map(Value::String).collect()),
            );
        }
        if let Some(environment) = environment {
            payload.insert("environment".into(), Value::Object(environment.clone()));
        }
        let response = self
            .execute_mobile_method(session_id, "launchApp", Value::Object(payload))
            .await?;
        Ok(Self::value_field(&response)?.clone())
    }

    /// Activates an already running application.
    pub async fn activate_app(
        &self,
        bundle_id: &str,
        session_id: Option<&str>,
    ) -> Result<Value, IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let response = self
            .execute_mobile_method(session_id, "activateApp", json!({ "bundleId": bundle_id }))
            .await?;
        Ok(Self::value_field(&response)?.clone())
    }

    /// Terminates an application and returns the WDA result.
    pub async fn terminate_app(
        &self,
        bundle_id: &str,
        session_id: Option<&str>,
    ) -> Result<bool, IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let response = self
            .execute_mobile_method(session_id, "terminateApp", json!({ "bundleId": bundle_id }))
            .await?;
        Self::value_field(&response)?
            .as_bool()
            .ok_or(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            ))
    }

    /// Queries the XCTest application state for the given bundle id.
    pub async fn query_app_state(
        &self,
        bundle_id: &str,
        session_id: Option<&str>,
    ) -> Result<i64, IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let response = self
            .execute_mobile_method(
                session_id,
                "queryAppState",
                json!({ "bundleId": bundle_id }),
            )
            .await?;
        Self::value_field(&response)?
            .as_i64()
            .ok_or(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            ))
    }

    /// Backgrounds the current app for the given number of seconds.
    ///
    /// A negative value means background without restoring.
    pub async fn background_app(
        &self,
        seconds: Option<f64>,
        session_id: Option<&str>,
    ) -> Result<Value, IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let payload = match seconds {
            Some(seconds) => json!({ "seconds": seconds }),
            None => json!({}),
        };
        let response = self
            .execute_mobile_method(session_id, "backgroundApp", payload)
            .await?;
        Ok(Self::value_field(&response)?.clone())
    }

    /// Returns whether the device is currently locked.
    pub async fn is_locked(&self, session_id: Option<&str>) -> Result<bool, IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let response = self
            .execute_mobile_method(session_id, "isLocked", Value::Object(Default::default()))
            .await?;
        Self::value_field(&response)?
            .as_bool()
            .ok_or(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            ))
    }

    /// Deletes a session, terminating the app under test.
    ///
    /// This is the standard W3C WebDriver `DELETE /session/{id}` endpoint and
    /// is supported by all WDA builds, unlike the Appium `mobile:` execute routes.
    pub async fn delete_session(&self, session_id: &str) -> Result<(), IdeviceError> {
        self.request_json("DELETE", &format!("/session/{session_id}"), None)
            .await
            .map(|_| ())
    }

    /// Sends a single HTTP request over a direct device connection and parses
    /// the JSON response body.
    ///
    /// This intentionally uses `Connection: close` and per-request sockets to
    /// keep the transport simple and independent per device.
    async fn request_json(
        &self,
        method: &str,
        path: &str,
        payload: Option<&Value>,
    ) -> Result<Value, IdeviceError> {
        let body = match payload {
            Some(payload) => serde_json::to_vec(payload)
                .map_err(|_| IdeviceError::UnexpectedResponse("unexpected response".into()))?,
            None => Vec::new(),
        };

        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nContent-Length: {}\r\n",
            body.len()
        );
        if payload.is_some() {
            request.push_str("Content-Type: application/json\r\n");
        }
        request.push_str("\r\n");

        let mut idevice = self.provider.connect(self.ports.http).await?;
        timeout(self.timeout, async {
            idevice.send_raw(request.as_bytes()).await?;
            if !body.is_empty() {
                idevice.send_raw(&body).await?;
            }
            Self::read_json_response(&mut idevice).await
        })
        .await
        .map_err(|_| timeout_error("wda request"))?
    }

    /// Reads a non-streaming JSON HTTP response.
    ///
    /// The current bootstrap client expects either a `Content-Length` body or
    /// connection-close semantics and does not yet implement chunked transfer
    /// decoding.
    async fn read_json_response(idevice: &mut Idevice) -> Result<Value, IdeviceError> {
        let mut response = Vec::new();
        let mut header_end = None;
        let mut content_length = None;

        loop {
            let chunk = idevice.read_any(8192).await?;
            if chunk.is_empty() {
                break;
            }

            response.extend_from_slice(&chunk);

            if header_end.is_none()
                && let Some(offset) = find_bytes(&response, b"\r\n\r\n")
            {
                let header_len = offset + 4;
                header_end = Some(header_len);
                let header_text = String::from_utf8_lossy(&response[..offset]);
                content_length = parse_content_length(&header_text);
            }

            if let (Some(header_len), Some(content_length)) = (header_end, content_length)
                && response.len() >= header_len + content_length
            {
                break;
            }
        }

        let header_end = header_end.ok_or(IdeviceError::UnexpectedResponse(
            "unexpected response".into(),
        ))?;
        let header_text = String::from_utf8_lossy(&response[..header_end - 4]);
        let mut lines = header_text.lines();
        let status_line = lines.next().ok_or(IdeviceError::UnexpectedResponse(
            "unexpected response".into(),
        ))?;
        let status_code = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<u16>().ok())
            .ok_or(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            ))?;

        let body = &response[header_end..];
        let json: Value = serde_json::from_slice(body)
            .map_err(|_| IdeviceError::UnexpectedResponse("unexpected response".into()))?;

        if !(200..300).contains(&status_code) {
            return Err(IdeviceError::UnknownErrorType(Self::format_error(
                &json,
                status_code,
            )));
        }

        match json.get("status") {
            None | Some(Value::Null) => {}
            Some(Value::Number(number)) if number.as_i64() == Some(0) => {}
            Some(Value::String(value)) if value == "0" => {}
            Some(_) => {
                return Err(IdeviceError::UnknownErrorType(Self::format_error(
                    &json,
                    status_code,
                )));
            }
        }

        Ok(json)
    }

    fn require_session_id<'b>(
        &'b self,
        session_id: Option<&'b str>,
    ) -> Result<&'b str, IdeviceError> {
        session_id
            .or(self.session_id())
            .ok_or_else(|| IdeviceError::UnknownErrorType("session_id is required".into()))
    }

    fn value_field(response: &Value) -> Result<&Value, IdeviceError> {
        response
            .get("value")
            .ok_or(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            ))
    }

    fn extract_session_id(response: &Value) -> Result<String, IdeviceError> {
        response
            .get("sessionId")
            .and_then(Value::as_str)
            .or_else(|| {
                response
                    .get("value")
                    .and_then(Value::as_object)
                    .and_then(|value| value.get("sessionId"))
                    .and_then(Value::as_str)
            })
            .map(ToOwned::to_owned)
            .ok_or(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            ))
    }

    fn extract_element_id(value: &Value) -> Result<String, IdeviceError> {
        let element = value.as_object().ok_or(IdeviceError::UnexpectedResponse(
            "unexpected response".into(),
        ))?;
        element
            .get("ELEMENT")
            .or_else(|| element.get("element-6066-11e4-a52e-4f735466cecf"))
            .or_else(|| element.get("element"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .ok_or(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            ))
    }

    fn format_error(data: &Value, status_code: u16) -> String {
        let message = data
            .get("value")
            .map(|value| match value {
                Value::Object(object) => object
                    .get("message")
                    .or_else(|| object.get("error"))
                    .cloned()
                    .unwrap_or_else(|| Value::Object(object.clone())),
                other => other.clone(),
            })
            .unwrap_or(Value::Null);
        format!("WDA error (status={status_code}): {message}")
    }

    async fn try_keys_endpoint(
        &self,
        session_id: &str,
        normalized: &str,
    ) -> Result<bool, IdeviceError> {
        let key = normalize_wda_key_name(normalized);
        let payload = json!({ "keys": [key] });
        match self
            .request_json(
                "POST",
                &format!("/session/{session_id}/wda/keys"),
                Some(&payload),
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(IdeviceError::UnknownErrorType(message)) if message.contains("404") => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn execute_mobile_method(
        &self,
        session_id: &str,
        method: &str,
        args: Value,
    ) -> Result<Value, IdeviceError> {
        let payload = json!({
            "script": format!("mobile: {method}"),
            "args": [args],
        });

        match self
            .request_json(
                "POST",
                &format!("/session/{session_id}/execute"),
                Some(&payload),
            )
            .await
        {
            Ok(response) => Ok(response),
            Err(IdeviceError::UnknownErrorType(message)) if message.contains("status=404") => {
                self.request_json(
                    "POST",
                    &format!("/session/{session_id}/execute/sync"),
                    Some(&payload),
                )
                .await
            }
            Err(error) => Err(error),
        }
    }

    async fn perform_actions(&self, session_id: &str, actions: Value) -> Result<(), IdeviceError> {
        self.request_json(
            "POST",
            &format!("/session/{session_id}/actions"),
            Some(&json!({ "actions": actions })),
        )
        .await?;
        Ok(())
    }

    async fn perform_tap_actions(
        &self,
        session_id: &str,
        x: f64,
        y: f64,
        tap_count: usize,
    ) -> Result<(), IdeviceError> {
        let mut gesture_actions = vec![pointer_move_action(0, x, y)];
        for index in 0..tap_count {
            gesture_actions.push(pointer_down_action());
            gesture_actions.push(pointer_up_action());
            if index + 1 != tap_count {
                gesture_actions.push(pointer_pause_action(100));
            }
        }

        self.perform_actions(
            session_id,
            json!([{
                "type": "pointer",
                "id": "finger1",
                "parameters": { "pointerType": "touch" },
                "actions": gesture_actions,
            }]),
        )
        .await
    }

    async fn perform_touch_and_hold_actions(
        &self,
        session_id: &str,
        x: f64,
        y: f64,
        duration: f64,
    ) -> Result<(), IdeviceError> {
        let hold_duration_ms = duration_to_millis(duration)?;
        self.perform_actions(
            session_id,
            json!([{
                "type": "pointer",
                "id": "finger1",
                "parameters": { "pointerType": "touch" },
                "actions": [
                    pointer_move_action(0, x, y),
                    pointer_down_action(),
                    pointer_pause_action(hold_duration_ms),
                    pointer_up_action(),
                ],
            }]),
        )
        .await
    }

    async fn resolve_gesture_coordinates(
        &self,
        x: Option<f64>,
        y: Option<f64>,
        element_id: Option<&str>,
        session_id: &str,
    ) -> Result<(f64, f64), IdeviceError> {
        match (x, y) {
            (Some(x), Some(y)) => Ok((x, y)),
            (None, None) => {
                let element_id = element_id.ok_or_else(|| {
                    IdeviceError::UnknownErrorType(
                        "gesture fallback requires coordinates or an element id".into(),
                    )
                })?;
                let rect = self.element_rect(element_id, Some(session_id)).await?;
                let center_x =
                    json_number_field(&rect, "x")? + json_number_field(&rect, "width")? / 2.0;
                let center_y =
                    json_number_field(&rect, "y")? + json_number_field(&rect, "height")? / 2.0;
                Ok((center_x, center_y))
            }
            _ => Err(IdeviceError::UnknownErrorType(
                "gesture fallback requires both x and y coordinates".into(),
            )),
        }
    }

    async fn execute_gesture(
        &self,
        method: &str,
        x: Option<f64>,
        y: Option<f64>,
        element_id: Option<&str>,
        duration: Option<f64>,
        session_id: Option<&str>,
    ) -> Result<(), IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let mut payload = serde_json::Map::new();

        if let Some(x) = x {
            payload.insert("x".into(), Value::from(x));
        }
        if let Some(y) = y {
            payload.insert("y".into(), Value::from(y));
        }
        if let Some(element_id) = element_id {
            payload.insert("elementId".into(), Value::String(element_id.to_owned()));
        }
        if let Some(duration) = duration {
            payload.insert("duration".into(), Value::from(duration));
        }

        self.execute_mobile_method(session_id, method, Value::Object(payload))
            .await?;
        Ok(())
    }

    async fn element_bool_state(
        &self,
        element_id: &str,
        state: &str,
        session_id: Option<&str>,
    ) -> Result<bool, IdeviceError> {
        let session_id = self.require_session_id(session_id)?;
        let response = self
            .request_json(
                "GET",
                &format!("/session/{session_id}/element/{element_id}/{state}"),
                None,
            )
            .await?;
        Self::value_field(&response)?
            .as_bool()
            .ok_or(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            ))
    }
}

fn parse_content_length(headers: &str) -> Option<usize> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("content-length") {
            return None;
        }
        value.trim().parse::<usize>().ok()
    })
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn normalize_wda_button_name(name: &str) -> String {
    match name
        .trim()
        .to_ascii_lowercase()
        .replace(['-', '_'], "")
        .as_str()
    {
        "home" => "home".into(),
        "volumeup" | "volup" | "volumeupbutton" => "volumeUp".into(),
        "volumedown" | "voldown" | "volumedownbutton" => "volumeDown".into(),
        "lock" | "lockscreen" | "sleep" | "power" => "lock".into(),
        _ => name.to_owned(),
    }
}

fn normalize_wda_key_name(name: &str) -> String {
    match name
        .trim()
        .to_ascii_lowercase()
        .replace(['-', '_'], "")
        .as_str()
    {
        "home" => "HOME".into(),
        "volumeup" | "volup" => "VOLUME_UP".into(),
        "volumedown" | "voldown" => "VOLUME_DOWN".into(),
        "lock" | "lockscreen" | "sleep" | "power" => "LOCK".into(),
        _ => name.to_owned(),
    }
}

fn timeout_error(context: &str) -> IdeviceError {
    std::io::Error::new(std::io::ErrorKind::TimedOut, format!("{context} timed out")).into()
}

fn json_number_field(value: &Value, field: &str) -> Result<f64, IdeviceError> {
    value
        .get(field)
        .and_then(Value::as_f64)
        .ok_or(IdeviceError::UnexpectedResponse(
            "unexpected response".into(),
        ))
}

fn pointer_move_action(duration_ms: u64, x: f64, y: f64) -> Value {
    json!({
        "type": "pointerMove",
        "duration": duration_ms,
        "x": x,
        "y": y,
        "origin": "viewport",
    })
}

fn pointer_down_action() -> Value {
    json!({
        "type": "pointerDown",
        "button": 0,
    })
}

fn pointer_up_action() -> Value {
    json!({
        "type": "pointerUp",
        "button": 0,
    })
}

fn pointer_pause_action(duration_ms: u64) -> Value {
    json!({
        "type": "pause",
        "duration": duration_ms,
    })
}

fn duration_to_millis(duration: f64) -> Result<u64, IdeviceError> {
    if !duration.is_finite() || duration < 0.0 {
        return Err(IdeviceError::UnknownErrorType(
            "gesture duration must be a non-negative finite number".into(),
        ));
    }
    Ok((duration * 1000.0).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_WDA_MJPEG_PORT, DEFAULT_WDA_PORT, WDA_READY_POLL_INTERVAL, WdaPorts,
        duration_to_millis, find_bytes, normalize_wda_button_name, normalize_wda_key_name,
        parse_content_length,
    };

    #[test]
    fn default_ports_match_expected_wda_values() {
        let ports = WdaPorts::default();
        assert_eq!(ports.http, DEFAULT_WDA_PORT);
        assert_eq!(ports.mjpeg, DEFAULT_WDA_MJPEG_PORT);
    }

    #[test]
    fn ready_poll_interval_is_conservative() {
        assert_eq!(
            WDA_READY_POLL_INTERVAL,
            std::time::Duration::from_millis(250)
        );
    }

    #[test]
    fn parse_content_length_is_case_insensitive() {
        let headers = "HTTP/1.1 200 OK\r\ncontent-length: 123\r\nConnection: close\r\n";
        assert_eq!(parse_content_length(headers), Some(123));
    }

    #[test]
    fn parse_content_length_ignores_missing_header() {
        let headers = "HTTP/1.1 200 OK\r\nConnection: close\r\n";
        assert_eq!(parse_content_length(headers), None);
    }

    #[test]
    fn find_bytes_locates_header_separator() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}";
        assert_eq!(find_bytes(response, b"\r\n\r\n"), Some(34));
    }

    #[test]
    fn find_bytes_returns_none_when_missing() {
        assert_eq!(find_bytes(b"abcdef", b"xyz"), None);
    }

    #[test]
    fn normalize_button_aliases() {
        assert_eq!(normalize_wda_button_name("home"), "home");
        assert_eq!(normalize_wda_button_name("volume_up"), "volumeUp");
        assert_eq!(normalize_wda_button_name("sleep"), "lock");
    }

    #[test]
    fn normalize_key_aliases() {
        assert_eq!(normalize_wda_key_name("home"), "HOME");
        assert_eq!(normalize_wda_key_name("vol-down"), "VOLUME_DOWN");
        assert_eq!(normalize_wda_key_name("power"), "LOCK");
    }

    #[test]
    fn duration_to_millis_rounds_seconds() {
        assert_eq!(duration_to_millis(0.18).unwrap(), 180);
        assert_eq!(duration_to_millis(1.25).unwrap(), 1250);
    }

    #[test]
    fn duration_to_millis_rejects_negative_values() {
        assert!(duration_to_millis(-0.1).is_err());
    }
}
