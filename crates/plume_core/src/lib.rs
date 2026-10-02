pub mod auth;
pub mod developer;
mod utils;

pub use apple_codesign::{AppleCodesignError, SettingsScope, SigningSettings, UnifiedSigner};

pub use omnisette::AnisetteConfiguration;

pub use utils::{CertificateIdentity, MachO, MachOExt, MobileProvision};

use thiserror::Error as ThisError;
#[derive(Debug, ThisError)]
pub enum Error {
    #[error("Executable not found")]
    BundleExecutableMissing,
    #[error("Entitlements not found")]
    ProvisioningEntitlementsUnknown,
    #[error("Missing certificate PEM data")]
    CertificatePemMissing,
    #[error("Certificate error: {0}")]
    Certificate(String),
    #[error("Developer API error {result_code} (HTTP {http_code:?}): {message} [URL: {url}]")]
    DeveloperApi {
        url: String,
        result_code: i64,
        http_code: Option<u16>,
        message: String,
    },
    #[error("Request to developer session failed")]
    DeveloperSessionRequestFailed,
    #[error("Authentication SRP error {0}: {1}")]
    AuthSrpWithMessage(i64, String),
    #[error("Authentication extra step required: {0}")]
    ExtraStep(String),
    #[error("Bad 2FA code")]
    Bad2faCode,
    #[error("Failed to parse")]
    Parse, // TODO: better parsing errors
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Plist error: {0}")]
    Plist(#[from] plist::Error),
    #[error("Codesign error: {0}")]
    Codesign(#[from] apple_codesign::AppleCodesignError),
    #[error("CodeSignBuilder error: {0}")]
    CodeSignBuilder(#[from] apple_codesign::UniversalMachOError),
    #[error("Certificate PEM error: {0}")]
    Pem(#[from] pem::PemError),
    #[error("X509 certificate error: {0}")]
    X509(#[from] x509_certificate::X509CertificateError),
    #[error("Reqwest error: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("Anisette error: {0}")]
    Anisette(#[from] omnisette::AnisetteError),
    #[error("Serde JSON error: {0}")]
    SerdeJson(#[from] serde_json::Error),
    #[error("RSA error: {0}")]
    Rsa(#[from] rsa::Error),
    #[error("PKCS1 RSA error: {0}")]
    PKCS1(#[from] rsa::pkcs1::Error),
    #[error("PKCS8 RSA error: {0}")]
    PKCS8(#[from] rsa::pkcs8::Error),
    #[error("RCGen error: {0}")]
    RcGen(#[from] rcgen::Error),
    #[error("AES-GCM error: {0}")]
    AesGcm(#[from] aes_gcm::Error),
    #[error("AES-GCM slice error: {0}")]
    Slice(#[from] std::array::TryFromSliceError),
    #[error("Invalid key length for AES-GCM: {0}")]
    SHA2(#[from] sha2::digest::InvalidLength),
}

/// Reads the Windows per-user proxy from the registry and returns a usable
/// proxy URL. Handles the "http=127.0.0.1:PORT[;https=...]" value format that
/// VPN clients like Psiphon write, which stock system-proxy handling fails to
/// parse (it silently ignores the proxy in that case).
pub fn system_proxy_url() -> Option<String> {
    #[cfg(windows)]
    {
        let out = std::process::Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Internet Settings",
                "/v",
                "ProxyServer",
            ])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let line = text.lines().find(|l| l.contains("ProxyServer"))?;
        let raw = line.split_whitespace().last()?.trim().to_string();
        let hostport = raw
            .split(';')
            .find_map(|part| {
                part.strip_prefix("http=")
                    .or_else(|| part.strip_prefix("https="))
            })
            .unwrap_or(raw.as_str())
            .to_string();
        if hostport.is_empty() || !hostport.contains(':') {
            return None;
        }
        return Some(if hostport.starts_with("http://") {
            hostport
        } else {
            format!("http://{hostport}")
        });
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Builds the standard HTTP client used for Apple requests. When `proxy` is
/// `None` a direct connection is forced (system/env proxies ignored); when
/// `Some`, that proxy is used explicitly.
pub fn client_with_proxy(proxy: Option<&str>) -> Result<reqwest::Client, Error> {
    const APPLE_ROOT: &[u8] = include_bytes!("./apple_root.der");
    let builder = reqwest::ClientBuilder::new()
        .add_root_certificate(reqwest::Certificate::from_der(APPLE_ROOT)?)
        // uncomment when debugging w/ charles proxy
        // .danger_accept_invalid_certs(true)
        .http1_title_case_headers()
        .connection_verbose(true);
    let builder = match proxy {
        Some(url) => builder.proxy(reqwest::Proxy::all(url)?),
        None => builder.no_proxy(),
    };
    Ok(builder.build()?)
}

pub fn client() -> Result<reqwest::Client, Error> {
    match system_proxy_url() {
        Some(url) => client_with_proxy(Some(&url)),
        None => client_with_proxy(None),
    }
}
