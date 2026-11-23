//! XCTest service client for iOS instruments protocol.
//!
//! This module provides orchestration for running XCTest bundles (including
//! WebDriverAgent) on iOS devices through the instruments and testmanagerd
//! protocols. It handles session setup, test runner launch, and lifecycle
//! event dispatch.
//!
//! Supports iOS 11+ via lockdown and iOS 17+ via RSD tunnel.
//!
//! # Example
//! ```rust,no_run
//! # #[cfg(feature = "xctest")]
//! # {
//! use idevice::services::dvt::xctest::{TestConfig, XCUITestService};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), idevice::IdeviceError> {
//!     // provider setup omitted
//!     Ok(())
//! }
//! # }
//! ```

pub mod dtx_services;
pub mod listener;
pub mod types;

use std::sync::Arc;

use plist::{Dictionary, Value};
#[cfg(feature = "wda")]
use serde_json::Value as JsonValue;
use tracing::{debug, warn};

#[cfg(feature = "wda")]
use crate::services::wda::{WdaClient, WdaPorts};
#[cfg(feature = "wda")]
use crate::services::wda_bridge::WdaBridge;
use crate::{
    IdeviceError, IdeviceService, ReadWrite,
    dvt::message::{AuxValue, Message},
    provider::{IdeviceProvider, RsdProvider},
    services::{
        core_device_proxy::CoreDeviceProxy,
        dvt::{
            process_control::ProcessControlClient,
            remote_server::{IncomingHandlerOutcome, OwnedChannel, RemoteServerClient},
        },
        installation_proxy::InstallationProxyClient,
        lockdown::LockdownClient,
        rsd::RsdHandshake,
    },
};
use dtx_services::{
    DVT_LEGACY_SERVICE, DVT_SERVICE, IDE_AUTHORIZE_TEST_SESSION, IDE_INITIATE_CTRL_SESSION_FOR_PID,
    IDE_INITIATE_CTRL_SESSION_FOR_PID_PROTOCOL_VERSION,
    IDE_INITIATE_CTRL_SESSION_WITH_CAPABILITIES, IDE_INITIATE_CTRL_SESSION_WITH_PROTOCOL_VERSION,
    IDE_INITIATE_SESSION_WITH_IDENTIFIER_CAPABILITIES,
    IDE_INITIATE_SESSION_WITH_IDENTIFIER_FOR_CLIENT_AT_PATH_PROTOCOL_VERSION,
    IDE_START_EXECUTING_TEST_PLAN, TESTMANAGERD_RSD_SERVICE, TESTMANAGERD_SECURE_SERVICE,
    TESTMANAGERD_SERVICE, XCODE_VERSION, XCT_BUNDLE_READY, XCT_BUNDLE_READY_WITH_PROTOCOL_VERSION,
    XCT_CASE_DID_FAIL, XCT_CASE_DID_FINISH, XCT_CASE_DID_FINISH_ACTIVITY,
    XCT_CASE_DID_FINISH_ACTIVITY_ID, XCT_CASE_DID_FINISH_ID, XCT_CASE_DID_RECORD_ISSUE,
    XCT_CASE_DID_STALL, XCT_CASE_DID_START, XCT_CASE_DID_START_ID, XCT_CASE_WILL_START_ACTIVITY,
    XCT_CASE_WILL_START_ACTIVITY_ID, XCT_DID_BEGIN_TEST_PLAN, XCT_DID_BEGIN_UI_INIT,
    XCT_DID_FAIL_BOOTSTRAP, XCT_DID_FINISH_TEST_PLAN, XCT_DID_FORM_PLAN,
    XCT_EXCHANGE_PROTOCOL_VERSION, XCT_GET_PROGRESS_FOR_LAUNCH, XCT_LOG_DEBUG_MESSAGE,
    XCT_LOG_MESSAGE, XCT_METHOD_DID_MEASURE_METRIC, XCT_RUNNER_READY_WITH_CAPABILITIES,
    XCT_SUITE_DID_FINISH, XCT_SUITE_DID_FINISH_ID, XCT_SUITE_DID_START, XCT_SUITE_DID_START_ID,
    XCT_UI_INIT_DID_FAIL, XCTEST_DRIVER_INTERFACE, XCTEST_MANAGER_DAEMON_CONNECTION_INTERFACE,
    XCTEST_MANAGER_IDE_INTERFACE, XCTEST_PROXY_IDE_TO_DRIVER,
};
use listener::{XCTestCaseResult, XCUITestListener};
use types::{
    XCActivityRecord, XCTCapabilities, XCTIssue, XCTTestIdentifier, XCTestConfiguration,
    archive_nsuuid_to_bytes, archive_xct_capabilities_to_bytes,
};

#[cfg(feature = "wda")]
use tokio::task::JoinHandle;

// ---------------------------------------------------------------------------
// TestConfig
// ---------------------------------------------------------------------------

/// Launch configuration for the XCTest runner and optional target application.
///
/// Built from `InstallationProxyClient` and used to generate both the
/// on-device `XCTestConfiguration` file and the process-launch environment.
///
/// # Example
/// ```rust,no_run
/// # #[cfg(feature = "xctest")]
/// # async fn example() -> Result<(), idevice::IdeviceError> {
/// // let cfg = TestConfig::from_installation_proxy(&mut proxy, "com.example.App.xctrunner", None).await?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug, Clone)]
pub struct TestConfig {
    // --- Runner app info (from installation_proxy) -------------------------
    /// Bundle identifier of the runner app (e.g. `"com.example.App.xctrunner"`).
    pub runner_bundle_id: String,
    /// On-device path of the runner app bundle (`"Path"` key).
    pub runner_app_path: String,
    /// On-device container path of the runner app (`"Container"` key).
    pub runner_app_container: String,
    /// Executable name inside the runner bundle (`"CFBundleExecutable"` key).
    /// Must end with `"-Runner"`.
    pub runner_bundle_executable: String,

    // --- Target app (optional) --------------------------------------------
    /// Bundle identifier of the app under test, if any.
    pub target_bundle_id: Option<String>,
    /// On-device path of the target app bundle, if any.
    pub target_app_path: Option<String>,
    /// Extra environment variables forwarded to the target app.
    pub target_app_env: Option<Dictionary>,
    /// Extra launch arguments forwarded to the target app.
    pub target_app_args: Option<Vec<String>>,

    // --- Test filters ------------------------------------------------------
    /// If set, only these test identifiers are run.
    pub tests_to_run: Option<Vec<String>>,
    /// If set, these test identifiers are skipped.
    pub tests_to_skip: Option<Vec<String>>,

    // --- Runner overrides -------------------------------------------------
    /// Additional environment variables merged into the runner launch env.
    pub runner_env: Option<Dictionary>,
    /// Additional arguments appended to the runner launch args.
    pub runner_args: Option<Vec<String>>,
}

impl TestConfig {
    /// Constructs a `TestConfig` by querying `InstallationProxyClient` for
    /// the runner (and optionally target) application information.
    ///
    /// # Arguments
    /// * `install_proxy` - Connected `InstallationProxyClient`
    /// * `runner_bundle_id` - Bundle identifier of the `.xctrunner` app
    /// * `target_bundle_id` - Optional bundle identifier of the app under test
    ///
    /// # Errors
    /// * `IdeviceError::AppNotInstalled` if runner or target is not found
    /// * `IdeviceError::UnexpectedResponse("unexpected response".into())` if `CFBundleExecutable` does not
    ///   end with `"-Runner"` or required keys are missing
    pub async fn from_installation_proxy(
        install_proxy: &mut InstallationProxyClient,
        runner_bundle_id: &str,
        target_bundle_id: Option<&str>,
    ) -> Result<Self, IdeviceError> {
        let app_string = |dict: &Dictionary, key: &str| -> Result<String, IdeviceError> {
            dict.get(key)
                .and_then(|value| value.as_string())
                .map(ToOwned::to_owned)
                .ok_or_else(|| {
                    warn!("Missing or non-string key '{}' in app info dict", key);
                    IdeviceError::UnexpectedResponse("unexpected response".into())
                })
        };

        // Build the bundle ID list to look up in one request
        let mut ids = vec![runner_bundle_id.to_owned()];
        if let Some(t) = target_bundle_id {
            ids.push(t.to_owned());
        }

        let apps = install_proxy.get_apps(None, Some(ids)).await?;

        // --- Runner ---
        let runner_info = apps.get(runner_bundle_id).ok_or_else(|| {
            warn!("Runner app not installed: {}", runner_bundle_id);
            IdeviceError::AppNotInstalled
        })?;

        let runner_dict = runner_info.as_dictionary().ok_or_else(|| {
            warn!("Runner info is not a dictionary");
            IdeviceError::UnexpectedResponse("unexpected response".into())
        })?;

        let runner_app_path = app_string(runner_dict, "Path")?;
        let runner_app_container = app_string(runner_dict, "Container")?;
        let runner_bundle_executable = app_string(runner_dict, "CFBundleExecutable")?;

        if !runner_bundle_executable.ends_with("-Runner") {
            warn!(
                "CFBundleExecutable '{}' does not end with '-Runner'; this is not a valid xctest runner bundle",
                runner_bundle_executable
            );
            return Err(IdeviceError::UnexpectedResponse(
                "unexpected response".into(),
            ));
        }

        // --- Target (optional) ---
        let (target_bundle_id_out, target_app_path) = if let Some(t) = target_bundle_id {
            let target_info = apps.get(t).ok_or_else(|| {
                warn!("Target app not installed: {}", t);
                IdeviceError::AppNotInstalled
            })?;
            let target_dict = target_info.as_dictionary().ok_or_else(|| {
                warn!("Target info is not a dictionary");
                IdeviceError::UnexpectedResponse("unexpected response".into())
            })?;
            let path = app_string(target_dict, "Path")?;
            (Some(t.to_owned()), Some(path))
        } else {
            (None, None)
        };

        Ok(Self {
            runner_bundle_id: runner_bundle_id.to_owned(),
            runner_app_path,
            runner_app_container,
            runner_bundle_executable,
            target_bundle_id: target_bundle_id_out,
            target_app_path,
            target_app_env: None,
            target_app_args: None,
            tests_to_run: None,
            tests_to_skip: None,
            runner_env: None,
            runner_args: None,
        })
    }

    /// Returns the config name — the executable name with `"-Runner"` stripped.
    ///
    /// For example, `"WebDriverAgentRunner-Runner"` → `"WebDriverAgentRunner"`.
    pub fn config_name(&self) -> &str {
        self.runner_bundle_executable
            .strip_suffix("-Runner")
            .unwrap_or(&self.runner_bundle_executable)
    }

    /// Builds an [`XCTestConfiguration`] for this test run.
    ///
    /// # Arguments
    /// * `session_id` - Unique UUID for this test session
    /// * `ios_major_version` - iOS major version number (e.g. `17`)
    ///
    /// # Errors
    /// Propagates serialisation errors from nested types.
    pub fn build_xctest_configuration(
        &self,
        session_id: uuid::Uuid,
        ios_major_version: u8,
    ) -> Result<XCTestConfiguration, IdeviceError> {
        let config_name = self.config_name();

        let test_bundle_url = format!(
            "file://{}/PlugIns/{}.xctest",
            self.runner_app_path, config_name
        );

        let automation_framework_path = if ios_major_version >= 17 {
            "/System/Developer/Library/PrivateFrameworks/XCTAutomationSupport.framework".to_owned()
        } else {
            "/Developer/Library/PrivateFrameworks/XCTAutomationSupport.framework".to_owned()
        };

        // productModuleName: config_name when a target app is set, else default WDA name
        let product_module_name = if self.target_bundle_id.is_some() {
            config_name.to_owned()
        } else {
            "WebDriverAgentRunner".to_owned()
        };

        // When a target app is specified, targetApplicationEnvironment must be at
        // least an empty dict (not null) — mirrors Python's `self.target_app_env or {}`
        let target_application_environment = if self.target_bundle_id.is_some() {
            Some(self.target_app_env.clone().unwrap_or_default())
        } else {
            None
        };

        Ok(XCTestConfiguration {
            test_bundle_url,
            session_identifier: session_id,
            product_module_name,
            automation_framework_path,
            target_application_bundle_id: self.target_bundle_id.clone(),
            target_application_path: self.target_app_path.clone(),
            target_application_environment,
            target_application_arguments: self.target_app_args.clone().unwrap_or_default(),
            tests_to_run: self.tests_to_run.clone(),
            tests_to_skip: self.tests_to_skip.clone(),
            ide_capabilities: XCTCapabilities::ide_defaults(),
        })
    }
}

// ---------------------------------------------------------------------------
// build_launch_env
// ---------------------------------------------------------------------------

/// Builds the process-launch arguments, environment, and options for the
/// XCTest runner process.
///
/// # Arguments
/// * `ios_major_version` - iOS major version number
/// * `session_id` - Test session UUID
/// * `runner_app_path` - On-device path of the runner app bundle
/// * `runner_app_container` - On-device container path of the runner app
/// * `target_name` - Config name (executable without `"-Runner"` suffix)
/// * `xctest_config_path` - Device path to the `.xctestconfiguration` file
///   (e.g. `"/tmp/{UUID}.xctestconfiguration"`)
/// * `extra_env` - Additional env vars merged on top of the base set
/// * `extra_args` - Additional args appended after the base set
///
/// # Returns
/// `(launch_args, launch_env, launch_options)` as `(Vec<String>, Dictionary, Dictionary)`
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_launch_env(
    ios_major_version: u8,
    session_id: &uuid::Uuid,
    runner_app_path: &str,
    runner_app_container: &str,
    target_name: &str,
    xctest_config_path: &str,
    extra_env: Option<&Dictionary>,
    extra_args: Option<&[String]>,
) -> (Vec<String>, Dictionary, Dictionary) {
    let session_upper = session_id.to_string().to_uppercase();

    // Base environment
    let mut env = crate::plist!(dict {
        "CA_ASSERT_MAIN_THREAD_TRANSACTIONS": "0",
        "CA_DEBUG_TRANSACTIONS": "0",
        "DYLD_FRAMEWORK_PATH": format!("{}/Frameworks:", runner_app_path),
        "DYLD_LIBRARY_PATH": format!("{}/Frameworks", runner_app_path),
        "MTC_CRASH_ON_REPORT": "1",
        "NSUnbufferedIO": "YES",
        "SQLITE_ENABLE_THREAD_ASSERTIONS": "1",
        "WDA_PRODUCT_BUNDLE_IDENTIFIER": "",
        "XCTestBundlePath": format!("{}/PlugIns/{}.xctest", runner_app_path, target_name),
        "XCTestConfigurationFilePath": format!("{}{}", runner_app_container, xctest_config_path),
        "XCODE_DBG_XPC_EXCLUSIONS": "com.apple.dt.xctestSymbolicator",
        "XCTestSessionIdentifier": session_upper.clone(),
    });

    // iOS >= 11
    if ios_major_version >= 11 {
        let ios11_env = crate::plist!(dict {
            "DYLD_INSERT_LIBRARIES": "/Developer/usr/lib/libMainThreadChecker.dylib",
            "OS_ACTIVITY_DT_MODE": "YES",
        });
        for (key, value) in ios11_env {
            env.insert(key, value);
        }
    }

    // iOS >= 17 — extend DYLD paths and clear config path (sent via capabilities)
    if ios_major_version >= 17 {
        let existing_fw = env
            .get("DYLD_FRAMEWORK_PATH")
            .and_then(|v| v.as_string())
            .unwrap_or("")
            .to_owned();
        let existing_lib = env
            .get("DYLD_LIBRARY_PATH")
            .and_then(|v| v.as_string())
            .unwrap_or("")
            .to_owned();
        // Prepend '$' so dyld expands the existing path value at launch time,
        // matching Python: f"${app_env['DYLD_FRAMEWORK_PATH']}/System/..."
        let ios17_env = crate::plist!(dict {
            "DYLD_FRAMEWORK_PATH": format!(
                "${}/System/Developer/Library/Frameworks:",
                existing_fw
            ),
            "DYLD_LIBRARY_PATH": format!("${}:/System/Developer/usr/lib", existing_lib),
            // Config path is sent as return value of _XCT_testRunnerReadyWithCapabilities_
            "XCTestConfigurationFilePath": "",
            "XCTestManagerVariant": "DDI",
        });
        for (key, value) in ios17_env {
            env.insert(key, value);
        }
    }

    // Merge caller-provided overrides
    if let Some(extra) = extra_env {
        for (k, v) in extra.iter() {
            env.insert(k.clone(), v.clone());
        }
    }

    // Launch arguments
    let mut args = vec![
        "-NSTreatUnknownArgumentsAsOpen".to_owned(),
        "NO".to_owned(),
        "-ApplePersistenceIgnoreState".to_owned(),
        "YES".to_owned(),
    ];
    if let Some(extra) = extra_args {
        args.extend_from_slice(extra);
    }

    // Launch options
    let opts = if ios_major_version >= 12 {
        crate::plist!(dict {
            "StartSuspendedKey": false,
            "ActivateSuspended": true,
        })
    } else {
        crate::plist!(dict {
            "StartSuspendedKey": false,
        })
    };

    (args, env, opts)
}

// ---------------------------------------------------------------------------
// testmanagerd connections
// ---------------------------------------------------------------------------

/// Active DTX connections for running XCTest.
///
/// Holds three `RemoteServerClient` instances:
/// - `ctrl`  — testmanagerd control channel connection
/// - `main`  — testmanagerd main channel connection
/// - `dvt`   — DVT instruments connection (for `ProcessControl`)
pub(super) struct TestManagerConnections {
    pub ctrl: RemoteServerClient<Box<dyn ReadWrite>>,
    pub main: RemoteServerClient<Box<dyn ReadWrite>>,
    pub dvt: RemoteServerClient<Box<dyn ReadWrite>>,
    /// Keeps the software tunnel/adapter handles alive for the duration of the session.
    #[allow(dead_code)]
    rsd_handles: Vec<crate::tcp::handle::AdapterHandle>,
}

/// Connects to a lockdown-based DTX service, trying each name in order.
///
/// Returns the first successful `RemoteServerClient`.
async fn connect_dtx_service(
    provider: &dyn IdeviceProvider,
    service_names: &[&str],
    read_greeting: bool,
) -> Result<RemoteServerClient<Box<dyn ReadWrite>>, IdeviceError> {
    let mut lockdown = LockdownClient::connect(provider).await?;
    lockdown
        .start_session(&provider.get_pairing_file().await?)
        .await?;

    let mut last_err: Option<IdeviceError> = None;
    for &name in service_names {
        match lockdown.start_service(name).await {
            Ok((port, ssl)) => {
                let mut idevice = provider.connect(port).await?;
                if ssl {
                    idevice
                        .start_session(&provider.get_pairing_file().await?, false)
                        .await?;
                }
                let socket = idevice
                    .get_socket()
                    .ok_or(IdeviceError::NoEstablishedConnection)?;
                let label = format!("lockdown:{name}");
                let client = RemoteServerClient::with_label(socket, label);
                if read_greeting {
                    // testmanagerd sends a capabilities hello on connect.
                    let _ = client
                        .wait_for_capabilities(std::time::Duration::from_secs(10))
                        .await;
                }
                return Ok(client);
            }
            Err(e) => {
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or(IdeviceError::ServiceNotFound))
}

const RSD_GREETING_TIMEOUT_SECS: u64 = 30;

/// DTX capabilities dict announced to the daemon on each connection.
///
/// Mirrors `DTXConnection.DEFAULT_CAPABILITIES` from the Instruments protocol.
fn dtx_capabilities_dict(include_process_control_callback: bool) -> plist::Dictionary {
    let mut caps = crate::plist!(dict {
        "com.apple.private.DTXBlockCompression": 0i64,
        "com.apple.private.DTXConnection": 1i64,
    });
    if include_process_control_callback {
        caps.insert(
            "com.apple.instruments.client.processcontrol.capability.terminationCallback".into(),
            plist::Value::Integer(1i64.into()),
        );
    }
    caps
}

/// Opens a single RSD service port and performs the DTX capability handshake,
/// retrying up to `MAX_ATTEMPTS` times.
async fn rsd_connect(
    handle: &mut crate::tcp::handle::AdapterHandle,
    handshake: &RsdHandshake,
    service_name: &str,
    label: &str,
    include_process_control_callback: bool,
) -> Result<RemoteServerClient<Box<dyn ReadWrite>>, IdeviceError> {
    const MAX_ATTEMPTS: usize = 5;
    let service = handshake
        .services
        .get(service_name)
        .ok_or_else(|| {
            warn!("RSD service not found: {}", service_name);
            IdeviceError::ServiceNotFound
        })?
        .clone();
    let port = service.port;

    let mut last_err = None;
    for attempt in 1..=MAX_ATTEMPTS {
        debug!(
            "[{}] opening service '{}' on remote port {} (attempt {}/{})",
            label, service_name, port, attempt, MAX_ATTEMPTS
        );
        let stream = handle.connect_to_service_port(port).await?;
        debug!("[{}] service port {} connected", label, port);
        let mut client = RemoteServerClient::with_label(stream, label);
        match client
            .perform_handshake(
                Some(dtx_capabilities_dict(include_process_control_callback)),
                std::time::Duration::from_secs(RSD_GREETING_TIMEOUT_SECS),
            )
            .await
        {
            Ok(remote_capabilities) => {
                debug!(
                    "[{}] RSD DTX capabilities exchange complete: {:?}",
                    label, remote_capabilities
                );
                return Ok(client);
            }
            Err(error) => {
                warn!(
                    "[{}] RSD DTX handshake failed on attempt {}/{}: {}",
                    label, attempt, MAX_ATTEMPTS, error
                );
                last_err = Some(error);
                if attempt < MAX_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_millis(750)).await;
                }
            }
        }
    }

    Err(last_err.unwrap_or(IdeviceError::UnexpectedResponse(
        "unexpected response".into(),
    )))
}

/// Attempts a single CoreDeviceProxy + RSD stack setup, returning all three
/// DTX connections on success.
async fn connect_rsd_stack_once(
    provider: &dyn IdeviceProvider,
) -> Result<TestManagerConnections, IdeviceError> {
    let proxy = CoreDeviceProxy::connect(provider).await?;
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    let adapter = proxy.create_software_tunnel()?;
    let mut handle = adapter.to_async_handle();

    debug!("[rsd] connecting to shared RSD port {}", rsd_port);
    let rsd_stream = handle.connect_to_service_port(rsd_port).await?;
    let handshake = RsdHandshake::new(rsd_stream).await?;
    debug!(
        "[rsd] shared RSD handshake OK — {} services advertised",
        handshake.services.len()
    );

    let dvt = match rsd_connect(
        &mut handle,
        &handshake,
        "com.apple.instruments.dtservicehub",
        "dtservicehub",
        true,
    )
    .await
    {
        Ok(client) => client,
        Err(e) => {
            warn!(
                "RSD dtservicehub connect failed ({}), falling back to lockdown DVT",
                e
            );
            connect_dtx_service(provider, &[DVT_SERVICE, DVT_LEGACY_SERVICE], false).await?
        }
    };
    let ctrl = rsd_connect(
        &mut handle,
        &handshake,
        TESTMANAGERD_RSD_SERVICE,
        "testmanagerd-ctrl",
        false,
    )
    .await?;
    let main = rsd_connect(
        &mut handle,
        &handshake,
        TESTMANAGERD_RSD_SERVICE,
        "testmanagerd-main",
        false,
    )
    .await?;

    Ok(TestManagerConnections {
        ctrl,
        main,
        dvt,
        rsd_handles: vec![handle],
    })
}

/// Establishes the three DTX connections for iOS 17+ via CoreDeviceProxy + RSD.
///
/// Opens a software TCP tunnel through CoreDeviceProxy, does the RSD handshake
/// to discover service ports, then connects to testmanagerd (×2) and
/// `dtservicehub` on their advertised ports.
async fn connect_testmanagerd_rsd(
    provider: &dyn IdeviceProvider,
) -> Result<TestManagerConnections, IdeviceError> {
    const RSD_STACK_ATTEMPTS: usize = 3;

    let mut last_err = None;
    for attempt in 1..=RSD_STACK_ATTEMPTS {
        debug!(
            "[rsd] establishing CoreDeviceProxy/software tunnel stack (attempt {}/{})",
            attempt, RSD_STACK_ATTEMPTS
        );
        match connect_rsd_stack_once(provider).await {
            Ok(connections) => return Ok(connections),
            Err(error) => {
                warn!(
                    "[rsd] CoreDeviceProxy/software tunnel stack attempt {}/{} failed: {}",
                    attempt, RSD_STACK_ATTEMPTS, error
                );
                last_err = Some(error);
                if attempt < RSD_STACK_ATTEMPTS {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    }

    Err(last_err.unwrap_or(IdeviceError::UnexpectedResponse(
        "unexpected response".into(),
    )))
}

/// Establishes the three DTX connections required for an XCTest run.
///
/// For iOS 17+ tries `CoreDeviceProxy` + RSD first. Falls back to lockdown
/// for iOS < 17 or if CoreDeviceProxy is unavailable.
///
/// # Arguments
/// * `provider` - Device connection provider
/// * `ios_major_version` - iOS major version (used to select service names)
pub(super) async fn connect_testmanagerd(
    provider: &dyn IdeviceProvider,
    ios_major_version: u8,
) -> Result<TestManagerConnections, IdeviceError> {
    // iOS 17+ must use RSD tunnel path
    if ios_major_version >= 17 {
        return connect_testmanagerd_rsd(provider).await;
    }

    // iOS < 17 (or fallback): lockdown path
    let tm_service = if ios_major_version >= 14 {
        TESTMANAGERD_SECURE_SERVICE
    } else {
        TESTMANAGERD_SERVICE
    };

    let ctrl = connect_dtx_service(provider, &[tm_service], true).await?;
    let main = connect_dtx_service(provider, &[tm_service], true).await?;
    let dvt = connect_dtx_service(provider, &[DVT_SERVICE, DVT_LEGACY_SERVICE], false).await?;

    Ok(TestManagerConnections {
        ctrl,
        main,
        dvt,
        rsd_handles: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// session init + process launch
// ---------------------------------------------------------------------------

/// Initialises the control session on the ctrl DTX channel.
///
/// Sends the appropriate IDE-initiation method based on `ios_major_version`.
pub(super) async fn init_ctrl_session<R: ReadWrite + 'static>(
    ctrl_channel: &mut OwnedChannel<R>,
    ios_major_version: u8,
) -> Result<(), IdeviceError> {
    if ios_major_version >= 17 {
        let caps_bytes =
            AuxValue::Array(archive_xct_capabilities_to_bytes(&XCTCapabilities::empty())?);
        let reply = ctrl_channel
            .call_method_with_reply(
                Some(IDE_INITIATE_CTRL_SESSION_WITH_CAPABILITIES),
                Some(vec![caps_bytes]),
            )
            .await?;
        debug!("init_ctrl_session (iOS 17+) reply: {:?}", reply.data);
    } else if ios_major_version >= 11 {
        let version_bytes = AuxValue::archived_value(Value::Integer((XCODE_VERSION as i64).into()));
        let reply = ctrl_channel
            .call_method_with_reply(
                Some(IDE_INITIATE_CTRL_SESSION_WITH_PROTOCOL_VERSION),
                Some(vec![version_bytes]),
            )
            .await?;
        debug!("init_ctrl_session (iOS 11-16) reply: {:?}", reply.data);
    }
    // iOS < 11: nothing to do
    Ok(())
}

/// Initialises the main test session on the main DTX channel.
pub(super) async fn init_session<R: ReadWrite + 'static>(
    main_channel: &mut OwnedChannel<R>,
    ios_major_version: u8,
    session_id: &uuid::Uuid,
    xctest_config: &XCTestConfiguration,
) -> Result<(), IdeviceError> {
    let uuid_bytes = AuxValue::Array(archive_nsuuid_to_bytes(session_id)?);

    if ios_major_version >= 17 {
        let caps_bytes = AuxValue::Array(archive_xct_capabilities_to_bytes(
            &XCTCapabilities::ide_defaults(),
        )?);
        let reply = main_channel
            .call_method_with_reply(
                Some(IDE_INITIATE_SESSION_WITH_IDENTIFIER_CAPABILITIES),
                Some(vec![uuid_bytes, caps_bytes]),
            )
            .await?;
        debug!("init_session (iOS 17+) reply: {:?}", reply.data);
    } else if ios_major_version >= 11 {
        let client_bytes = AuxValue::archived_value(Value::String("not-very-important".into()));
        let path_bytes = AuxValue::archived_value(Value::String(
            "/Applications/Xcode.app/Contents/Developer/usr/bin/xcodebuild".into(),
        ));
        let version_bytes = AuxValue::archived_value(Value::Integer((XCODE_VERSION as i64).into()));
        let reply = main_channel
            .call_method_with_reply(
                Some(IDE_INITIATE_SESSION_WITH_IDENTIFIER_FOR_CLIENT_AT_PATH_PROTOCOL_VERSION),
                Some(vec![uuid_bytes, client_bytes, path_bytes, version_bytes]),
            )
            .await?;
        debug!("init_session (iOS 11-16) reply: {:?}", reply.data);
    } else {
        return Ok(());
    }

    let _ = xctest_config; // used by caller for bootstrap reply handling
    Ok(())
}

/// Launches the XCTest runner process via ProcessControl.
///
/// Uses `launchSuspendedProcessWithDevicePath:bundleIdentifier:environment:arguments:options:`
/// with the provided arguments and options dictionaries.
///
/// # Returns
/// PID of the launched process.
pub(super) async fn launch_runner<R: ReadWrite + 'static>(
    process_control: &mut ProcessControlClient<'_, R>,
    bundle_id: &str,
    launch_args: Vec<String>,
    launch_env: Dictionary,
    launch_options: Dictionary,
) -> Result<u64, IdeviceError> {
    let args_array: Vec<Value> = launch_args.into_iter().map(Value::String).collect();

    process_control
        .launch_with_options(bundle_id, launch_env, args_array, launch_options)
        .await
}

// ---------------------------------------------------------------------------
// authorize + driver channel + start plan
// ---------------------------------------------------------------------------

/// Authorises the test session for the launched runner process.
pub(super) async fn authorize_test<R: ReadWrite + 'static>(
    ctrl_channel: &mut OwnedChannel<R>,
    ios_major_version: u8,
    pid: u64,
) -> Result<(), IdeviceError> {
    let pid_bytes = AuxValue::archived_value(Value::Integer((pid as i64).into()));

    if ios_major_version >= 12 {
        let reply = ctrl_channel
            .call_method_with_reply(Some(IDE_AUTHORIZE_TEST_SESSION), Some(vec![pid_bytes]))
            .await?;
        match reply.data {
            Some(Value::Boolean(true)) | None => {
                debug!("authorize_test: OK");
            }
            Some(Value::Boolean(false)) => {
                warn!("authorize_test returned false");
                return Err(IdeviceError::UnexpectedResponse(
                    "unexpected response".into(),
                ));
            }
            other => {
                debug!("authorize_test reply: {:?}", other);
            }
        }
    } else if ios_major_version >= 10 {
        let version_bytes = AuxValue::archived_value(Value::Integer((XCODE_VERSION as i64).into()));
        let reply = ctrl_channel
            .call_method_with_reply(
                Some(IDE_INITIATE_CTRL_SESSION_FOR_PID_PROTOCOL_VERSION),
                Some(vec![pid_bytes, version_bytes]),
            )
            .await?;
        debug!("authorize_test (<12, >=10) reply: {:?}", reply.data);
    } else {
        let reply = ctrl_channel
            .call_method_with_reply(
                Some(IDE_INITIATE_CTRL_SESSION_FOR_PID),
                Some(vec![pid_bytes]),
            )
            .await?;
        debug!("authorize_test (<10) reply: {:?}", reply.data);
    }
    Ok(())
}

struct TestManagerProxy<R: ReadWrite> {
    channel: OwnedChannel<R>,
}

impl<R: ReadWrite + 'static> TestManagerProxy<R> {
    async fn open(
        client: &mut RemoteServerClient<R>,
        ios_major_version: u8,
    ) -> Result<Self, IdeviceError> {
        let channel = if testmanager_uses_proxy(ios_major_version) {
            client
                .open_proxied_service_channel(
                    XCTEST_MANAGER_IDE_INTERFACE,
                    XCTEST_MANAGER_DAEMON_CONNECTION_INTERFACE,
                )
                .await?
        } else {
            client
                .open_service_channel(XCTEST_MANAGER_IDE_INTERFACE)
                .await?
        };

        Ok(Self {
            channel: channel.detach(),
        })
    }

    async fn install_bootstrap_handler(&mut self, xctest_config: XCTestConfiguration) {
        install_early_xctest_handler(&mut self.channel, xctest_config).await;
    }

    async fn init_ctrl_session(&mut self, ios_major_version: u8) -> Result<(), IdeviceError> {
        init_ctrl_session(&mut self.channel, ios_major_version).await
    }

    async fn init_session(
        &mut self,
        ios_major_version: u8,
        session_id: &uuid::Uuid,
        xctest_config: &XCTestConfiguration,
    ) -> Result<(), IdeviceError> {
        init_session(
            &mut self.channel,
            ios_major_version,
            session_id,
            xctest_config,
        )
        .await
    }

    async fn authorize_test(
        &mut self,
        ios_major_version: u8,
        pid: u64,
    ) -> Result<(), IdeviceError> {
        authorize_test(&mut self.channel, ios_major_version, pid).await
    }
}

struct DriverProxy {
    channel: OwnedChannel<Box<dyn ReadWrite>>,
}

impl DriverProxy {
    async fn wait(
        client: &mut RemoteServerClient<Box<dyn ReadWrite>>,
        timeout_secs: f64,
    ) -> Result<Self, IdeviceError> {
        Ok(Self {
            channel: wait_for_driver_channel(client, timeout_secs).await?,
        })
    }

    async fn start_executing_test_plan(&mut self) -> Result<(), IdeviceError> {
        start_executing_test_plan(&mut self.channel).await
    }
}

struct XCTestProcessControlChannel<'a, R: ReadWrite> {
    service: ProcessControlClient<'a, R>,
}

impl<'a, R: ReadWrite + 'static> XCTestProcessControlChannel<'a, R> {
    async fn open(client: &'a mut RemoteServerClient<R>) -> Result<Self, IdeviceError> {
        Ok(Self {
            service: ProcessControlClient::new(client).await?,
        })
    }

    async fn launch_suspended_process(
        &mut self,
        bundle_id: &str,
        launch_args: Vec<String>,
        launch_env: Dictionary,
        launch_options: Dictionary,
    ) -> Result<u64, IdeviceError> {
        launch_runner(
            &mut self.service,
            bundle_id,
            launch_args,
            launch_env,
            launch_options,
        )
        .await
    }
}

/// Waits for the test runner to open the reverse `XCTestDriverInterface` channel.
///
/// After launching, the runner sends `_requestChannelWithCode:identifier:` on root
/// channel 0.  This function reads root-channel messages until that request arrives,
/// replies with an empty acknowledgement, registers the channel, and returns a
/// `Channel` handle to it.
fn testmanager_uses_proxy(ios_major_version: u8) -> bool {
    ios_major_version >= 17
}

async fn wait_for_xctest_service_channel(
    main_client: &mut RemoteServerClient<Box<dyn ReadWrite>>,
    plain_identifiers: &[&str],
    proxy_remote_identifiers: &[&str],
    timeout_secs: f64,
) -> Result<OwnedChannel<Box<dyn ReadWrite>>, IdeviceError> {
    let timeout = Some(std::time::Duration::from_secs_f64(timeout_secs));

    let code = match main_client
        .wait_for_proxied_service_channel_code(proxy_remote_identifiers, true, Some(true), timeout)
        .await
    {
        Ok(code) => code,
        Err(IdeviceError::XcTestTimeout(_)) => match main_client
            .wait_for_service_channel_code(plain_identifiers, Some(true), timeout)
            .await
        {
            Ok(code) => code,
            Err(IdeviceError::XcTestTimeout(_)) => return Err(IdeviceError::TestRunnerTimeout),
            Err(error) => return Err(error),
        },
        Err(error) => return Err(error),
    };

    Ok(main_client.accept_owned_channel(code))
}

async fn register_early_driver_channel_handler(
    main_client: &mut RemoteServerClient<Box<dyn ReadWrite>>,
    xctest_config: &XCTestConfiguration,
) {
    let xctest_config = xctest_config.clone();
    main_client
        .register_incoming_channel_initializer(
            &[XCTEST_DRIVER_INTERFACE, XCTEST_PROXY_IDE_TO_DRIVER],
            move |mut channel, _identifier| {
                let xctest_config = xctest_config.clone();
                Box::pin(async move {
                    install_early_xctest_handler(&mut channel, xctest_config).await;
                    Ok(())
                })
            },
        )
        .await;
}

async fn initialize_testmanager_sessions(
    ctrl_proxy: &mut TestManagerProxy<Box<dyn ReadWrite>>,
    main_proxy: &mut TestManagerProxy<Box<dyn ReadWrite>>,
    xctest_config: &XCTestConfiguration,
) -> Result<(), IdeviceError> {
    ctrl_proxy
        .install_bootstrap_handler(xctest_config.clone())
        .await;
    main_proxy
        .install_bootstrap_handler(xctest_config.clone())
        .await;
    Ok(())
}

async fn initialize_testmanager_daemon_sessions(
    ctrl_proxy: &mut TestManagerProxy<Box<dyn ReadWrite>>,
    main_proxy: &mut TestManagerProxy<Box<dyn ReadWrite>>,
    ios_major_version: u8,
    session_id: &uuid::Uuid,
    xctest_config: &XCTestConfiguration,
) -> Result<(), IdeviceError> {
    ctrl_proxy.init_ctrl_session(ios_major_version).await?;
    main_proxy
        .init_session(ios_major_version, session_id, xctest_config)
        .await?;

    Ok(())
}

async fn launch_and_authorize_test_runner(
    ctrl_proxy: &mut TestManagerProxy<Box<dyn ReadWrite>>,
    process_control: &mut XCTestProcessControlChannel<'_, Box<dyn ReadWrite>>,
    ios_major_version: u8,
    runner_bundle_id: &str,
    launch_args: Vec<String>,
    launch_env: Dictionary,
    launch_options: Dictionary,
) -> Result<u64, IdeviceError> {
    let pid = process_control
        .launch_suspended_process(runner_bundle_id, launch_args, launch_env, launch_options)
        .await?;
    debug!("Launched test runner pid={}", pid);

    if ios_major_version < 17 {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    ctrl_proxy.authorize_test(ios_major_version, pid).await?;
    Ok(pid)
}

async fn start_test_plan_session(
    main_client: &mut RemoteServerClient<Box<dyn ReadWrite>>,
    _main_proxy: &mut TestManagerProxy<Box<dyn ReadWrite>>,
) -> Result<OwnedChannel<Box<dyn ReadWrite>>, IdeviceError> {
    let mut driver_proxy = DriverProxy::wait(main_client, 30.0).await?;
    driver_proxy.start_executing_test_plan().await?;
    driver_proxy.channel.clear_incoming_handler().await;
    Ok(driver_proxy.channel)
}

pub(super) async fn wait_for_driver_channel(
    main_client: &mut RemoteServerClient<Box<dyn ReadWrite>>,
    timeout_secs: f64,
) -> Result<OwnedChannel<Box<dyn ReadWrite>>, IdeviceError> {
    const DRIVER_SERVICE_IDENTIFIERS: &[&str] = &[XCTEST_DRIVER_INTERFACE];
    wait_for_xctest_service_channel(
        main_client,
        DRIVER_SERVICE_IDENTIFIERS,
        DRIVER_SERVICE_IDENTIFIERS,
        timeout_secs,
    )
    .await
}

/// Signals the test runner to begin executing the test plan.
pub(super) async fn start_executing_test_plan<R: ReadWrite + 'static>(
    driver_channel: &mut OwnedChannel<R>,
) -> Result<(), IdeviceError> {
    let version_bytes = AuxValue::archived_value(Value::Integer((XCODE_VERSION as i64).into()));
    let reply = driver_channel
        .call_method_with_reply(
            Some(IDE_START_EXECUTING_TEST_PLAN),
            Some(vec![version_bytes]),
        )
        .await?;
    debug!("start_executing_test_plan reply: {:?}", reply.data);
    Ok(())
}

// ---------------------------------------------------------------------------
// _XCT_* dispatch + run_dispatch_loop + XCUITestService
// ---------------------------------------------------------------------------

// --- Aux-value helpers ------------------------------------------------------

fn decode_aux_archive(aux: &AuxValue) -> Result<Value, IdeviceError> {
    match aux {
        AuxValue::Array(bytes) => ns_keyed_archive::decode::from_bytes(bytes)
            .map_err(|_| IdeviceError::UnexpectedResponse("unexpected response".into())),
        _ => Err(IdeviceError::UnexpectedResponse(
            "unexpected response".into(),
        )),
    }
}

fn aux_as_string(aux: &AuxValue) -> Result<String, IdeviceError> {
    if let AuxValue::String(s) = aux {
        return Ok(s.clone());
    }
    match decode_aux_archive(aux)? {
        Value::String(s) => Ok(s),
        _ => Err(IdeviceError::UnexpectedResponse(
            "unexpected response".into(),
        )),
    }
}

fn aux_as_u64(aux: &AuxValue) -> Result<u64, IdeviceError> {
    match aux {
        AuxValue::U32(v) => return Ok(*v as u64),
        AuxValue::I64(v) => return Ok(*v as u64),
        _ => {}
    }
    match decode_aux_archive(aux)? {
        Value::Integer(i) => i.as_unsigned().ok_or(IdeviceError::UnexpectedResponse(
            "unexpected response".into(),
        )),
        _ => Err(IdeviceError::UnexpectedResponse(
            "unexpected response".into(),
        )),
    }
}

fn aux_as_f64(aux: &AuxValue) -> Result<f64, IdeviceError> {
    match decode_aux_archive(aux) {
        Ok(Value::Real(f)) => return Ok(f),
        Ok(Value::Integer(i)) => return Ok(i.as_unsigned().unwrap_or(0) as f64),
        _ => {}
    }
    match aux {
        AuxValue::U32(v) => Ok(*v as f64),
        AuxValue::I64(v) => Ok(*v as f64),
        AuxValue::Double(v) => Ok(*v),
        _ => Err(IdeviceError::UnexpectedResponse(
            "unexpected response".into(),
        )),
    }
}

// --- Dispatch ---------------------------------------------------------------

/// Dispatches a single incoming `_XCT_*` message to the appropriate listener
/// method.
///
/// Returns `Some(reply_bytes)` if the caller must send a reply (only for
/// `_XCT_testRunnerReadyWithCapabilities_`); `None` otherwise.
pub(super) async fn dispatch_xct_message<L: XCUITestListener>(
    method: &str,
    aux: &[AuxValue],
    xctest_config: &XCTestConfiguration,
    listener: &mut L,
    done_flag: &mut bool,
) -> Result<Option<Vec<u8>>, IdeviceError> {
    match method {
        // --- logging ---
        m if m == XCT_LOG_DEBUG_MESSAGE => {
            if let Some(msg) = aux.first().map(aux_as_string).transpose()? {
                listener.log_debug_message(&msg).await?;
            }
        }
        m if m == XCT_LOG_MESSAGE => {
            if let Some(msg) = aux.first().map(aux_as_string).transpose()? {
                listener.log_message(&msg).await?;
            }
        }

        // --- protocol negotiation ---
        m if m == XCT_EXCHANGE_PROTOCOL_VERSION => {
            let current = aux.first().map(aux_as_u64).transpose()?.unwrap_or(0);
            let minimum = aux.get(1).map(aux_as_u64).transpose()?.unwrap_or(0);
            listener.exchange_protocol_version(current, minimum).await?;
        }

        // --- bundle ready ---
        m if m == XCT_BUNDLE_READY => {
            listener.test_bundle_ready().await?;
        }
        m if m == XCT_BUNDLE_READY_WITH_PROTOCOL_VERSION => {
            let proto = aux.first().map(aux_as_u64).transpose()?.unwrap_or(0);
            let min = aux.get(1).map(aux_as_u64).transpose()?.unwrap_or(0);
            listener
                .test_bundle_ready_with_protocol_version(proto, min)
                .await?;
        }
        m if m == XCT_RUNNER_READY_WITH_CAPABILITIES => {
            if let Some(raw) = aux.first()
                && let Ok(decoded) = decode_aux_archive(raw)
                && let Some(caps) = XCTCapabilities::from_plist(&decoded)
            {
                debug!("testRunnerReadyWithCapabilities: {:?}", caps.capabilities);
            }
            listener.test_runner_ready_with_capabilities().await?;
            let reply = xctest_config.to_archive_bytes()?;
            return Ok(Some(reply));
        }

        // --- test plan lifecycle ---
        m if m == XCT_DID_BEGIN_TEST_PLAN => {
            listener.did_begin_executing_test_plan().await?;
        }
        m if m == XCT_DID_FINISH_TEST_PLAN => {
            *done_flag = true;
            listener.did_finish_executing_test_plan().await?;
        }

        // --- suite lifecycle (legacy string-based) ---
        m if m == XCT_SUITE_DID_START => {
            let suite = aux
                .first()
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let started_at = aux
                .get(1)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            listener.test_suite_did_start(&suite, &started_at).await?;
        }
        m if m == XCT_SUITE_DID_FINISH => {
            let suite = aux
                .first()
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let finished_at = aux
                .get(1)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let run_count = aux.get(2).map(aux_as_u64).transpose()?.unwrap_or(0);
            let failures = aux.get(3).map(aux_as_u64).transpose()?.unwrap_or(0);
            let unexpected = aux.get(4).map(aux_as_u64).transpose()?.unwrap_or(0);
            let test_dur = aux.get(5).map(aux_as_f64).transpose()?.unwrap_or(0.0);
            let total_dur = aux.get(6).map(aux_as_f64).transpose()?.unwrap_or(0.0);
            listener
                .test_suite_did_finish(
                    &suite,
                    &finished_at,
                    run_count,
                    failures,
                    unexpected,
                    test_dur,
                    total_dur,
                    0,
                    0,
                    0,
                )
                .await?;
        }

        // --- suite lifecycle (identifier-based, iOS 14+) ---
        m if m == XCT_SUITE_DID_START_ID => {
            if let Some(raw) = aux.first()
                && let Ok(decoded) = decode_aux_archive(raw)
                && let Some(id) = XCTTestIdentifier::from_plist(&decoded)
            {
                let tc = id.test_class();
                if !tc.is_empty() && tc != "All tests" {
                    let started_at = aux
                        .get(1)
                        .map(aux_as_string)
                        .transpose()?
                        .unwrap_or_default();
                    listener.test_suite_did_start(tc, &started_at).await?;
                }
            }
        }
        m if m == XCT_SUITE_DID_FINISH_ID => {
            if let Some(raw) = aux.first()
                && let Ok(decoded) = decode_aux_archive(raw)
                && let Some(id) = XCTTestIdentifier::from_plist(&decoded)
            {
                let tc = id.test_class().to_owned();
                if !tc.is_empty() && tc != "All tests" {
                    let finished_at = aux
                        .get(1)
                        .map(aux_as_string)
                        .transpose()?
                        .unwrap_or_default();
                    let run_count = aux.get(2).map(aux_as_u64).transpose()?.unwrap_or(0);
                    let skip_count = aux.get(3).map(aux_as_u64).transpose()?.unwrap_or(0);
                    let fail_count = aux.get(4).map(aux_as_u64).transpose()?.unwrap_or(0);
                    let expected_fail = aux.get(5).map(aux_as_u64).transpose()?.unwrap_or(0);
                    let uncaught = aux.get(6).map(aux_as_u64).transpose()?.unwrap_or(0);
                    let test_dur = aux.get(7).map(aux_as_f64).transpose()?.unwrap_or(0.0);
                    let total_dur = aux.get(8).map(aux_as_f64).transpose()?.unwrap_or(0.0);
                    listener
                        .test_suite_did_finish(
                            &tc,
                            &finished_at,
                            run_count,
                            fail_count,
                            uncaught,
                            test_dur,
                            total_dur,
                            skip_count,
                            expected_fail,
                            0,
                        )
                        .await?;
                }
            }
        }

        // --- case lifecycle (legacy) ---
        m if m == XCT_CASE_DID_START => {
            let test_class = aux
                .first()
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let method_name = aux
                .get(1)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            listener
                .test_case_did_start(&test_class, &method_name)
                .await?;
        }
        m if m == XCT_CASE_DID_FINISH => {
            let test_class = aux
                .first()
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let method_name = aux
                .get(1)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let status = aux
                .get(2)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let duration = aux.get(3).map(aux_as_f64).transpose()?.unwrap_or(0.0);
            listener
                .test_case_did_finish(XCTestCaseResult {
                    test_class,
                    method: method_name,
                    status,
                    duration,
                })
                .await?;
        }
        m if m == XCT_CASE_DID_FAIL => {
            let test_class = aux
                .first()
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let method_name = aux
                .get(1)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let message = aux
                .get(2)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let file = aux
                .get(3)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let line = aux.get(4).map(aux_as_u64).transpose()?.unwrap_or(0);
            listener
                .test_case_did_fail(&test_class, &method_name, &message, &file, line)
                .await?;
        }
        m if m == XCT_CASE_DID_STALL => {
            let test_class = aux
                .first()
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let method_name = aux
                .get(1)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let file = aux
                .get(2)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let line = aux.get(3).map(aux_as_u64).transpose()?.unwrap_or(0);
            listener
                .test_case_did_stall(&test_class, &method_name, &file, line)
                .await?;
        }

        // --- case lifecycle (identifier-based, iOS 14+) ---
        m if m == XCT_CASE_DID_START_ID => {
            if let Some(raw) = aux.first()
                && let Ok(decoded) = decode_aux_archive(raw)
                && let Some(id) = XCTTestIdentifier::from_plist(&decoded)
            {
                let method_name = id.test_method().unwrap_or("").to_owned();
                listener
                    .test_case_did_start(id.test_class(), &method_name)
                    .await?;
            }
        }
        m if m == XCT_CASE_DID_FINISH_ID => {
            if let Some(raw) = aux.first()
                && let Ok(decoded) = decode_aux_archive(raw)
                && let Some(id) = XCTTestIdentifier::from_plist(&decoded)
            {
                let test_class = id.test_class().to_owned();
                let method_name = id.test_method().unwrap_or("").to_owned();
                let status = aux
                    .get(1)
                    .map(aux_as_string)
                    .transpose()?
                    .unwrap_or_default();
                let duration = aux.get(2).map(aux_as_f64).transpose()?.unwrap_or(0.0);
                listener
                    .test_case_did_finish(XCTestCaseResult {
                        test_class,
                        method: method_name,
                        status,
                        duration,
                    })
                    .await?;
            }
        }
        m if m == XCT_CASE_DID_RECORD_ISSUE => {
            if let (Some(id_raw), Some(issue_raw)) = (aux.first(), aux.get(1))
                && let (Ok(id_val), Ok(issue_val)) =
                    (decode_aux_archive(id_raw), decode_aux_archive(issue_raw))
                && let (Some(id), Some(issue)) = (
                    XCTTestIdentifier::from_plist(&id_val),
                    XCTIssue::from_plist(&issue_val),
                )
            {
                let test_class = id.test_class().to_owned();
                let method_name = id.test_method().unwrap_or("").to_owned();
                let file = issue
                    .source_code_context
                    .as_ref()
                    .and_then(|c| c.location.as_ref())
                    .and_then(|l| l.file_path())
                    .unwrap_or("")
                    .to_owned();
                let line = issue
                    .source_code_context
                    .as_ref()
                    .and_then(|c| c.location.as_ref())
                    .map(|l| l.line_number)
                    .unwrap_or(0);
                listener
                    .test_case_did_fail(
                        &test_class,
                        &method_name,
                        &issue.compact_description,
                        &file,
                        line,
                    )
                    .await?;
            }
        }

        // --- activities (legacy) ---
        m if m == XCT_CASE_WILL_START_ACTIVITY => {
            let test_class = aux
                .first()
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let method_name = aux
                .get(1)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            // aux[2] is an XCActivityRecord NSKeyedArchive blob, not a plain string
            let title = aux
                .get(2)
                .and_then(|a| decode_aux_archive(a).ok())
                .and_then(|v| XCActivityRecord::from_plist(&v))
                .map(|r| r.title)
                .unwrap_or_default();
            listener
                .test_case_will_start_activity(&test_class, &method_name, &title)
                .await?;
        }
        m if m == XCT_CASE_DID_FINISH_ACTIVITY => {
            let test_class = aux
                .first()
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let method_name = aux
                .get(1)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            // aux[2] is an XCActivityRecord NSKeyedArchive blob, not a plain string
            let title = aux
                .get(2)
                .and_then(|a| decode_aux_archive(a).ok())
                .and_then(|v| XCActivityRecord::from_plist(&v))
                .map(|r| r.title)
                .unwrap_or_default();
            listener
                .test_case_did_finish_activity(&test_class, &method_name, &title)
                .await?;
        }

        // --- activities (identifier-based) ---
        m if m == XCT_CASE_WILL_START_ACTIVITY_ID => {
            if let Some(id_raw) = aux.first()
                && let Ok(id_val) = decode_aux_archive(id_raw)
                && let Some(id) = XCTTestIdentifier::from_plist(&id_val)
            {
                let method_name = id.test_method().unwrap_or("").to_owned();
                // aux[1] is an XCActivityRecord NSKeyedArchive blob
                let title = aux
                    .get(1)
                    .and_then(|a| decode_aux_archive(a).ok())
                    .and_then(|v| XCActivityRecord::from_plist(&v))
                    .map(|r| r.title)
                    .unwrap_or_default();
                listener
                    .test_case_will_start_activity(id.test_class(), &method_name, &title)
                    .await?;
            }
        }
        m if m == XCT_CASE_DID_FINISH_ACTIVITY_ID => {
            if let Some(id_raw) = aux.first()
                && let Ok(id_val) = decode_aux_archive(id_raw)
                && let Some(id) = XCTTestIdentifier::from_plist(&id_val)
            {
                let method_name = id.test_method().unwrap_or("").to_owned();
                // aux[1] is an XCActivityRecord NSKeyedArchive blob
                let title = aux
                    .get(1)
                    .and_then(|a| decode_aux_archive(a).ok())
                    .and_then(|v| XCActivityRecord::from_plist(&v))
                    .map(|r| r.title)
                    .unwrap_or_default();
                listener
                    .test_case_did_finish_activity(id.test_class(), &method_name, &title)
                    .await?;
            }
        }

        // --- metrics ---
        // Python selector: _XCT_testMethod:ofClass:didMeasureMetric:file:line:
        // → aux[0]=method, aux[1]=test_class, aux[2]=metric, aux[3]=file, aux[4]=line
        m if m == XCT_METHOD_DID_MEASURE_METRIC => {
            let method_name = aux
                .first()
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let test_class = aux
                .get(1)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let metric = aux
                .get(2)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let file = aux
                .get(3)
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            let line = aux.get(4).map(aux_as_u64).transpose()?.unwrap_or(0);
            listener
                .test_method_did_measure_metric(&test_class, &method_name, &metric, &file, line)
                .await?;
        }

        // --- iOS 14+ UI testing ---
        m if m == XCT_DID_BEGIN_UI_INIT => {
            listener.did_begin_initializing_for_ui_testing().await?;
        }
        m if m == XCT_DID_FORM_PLAN => {
            let data = aux
                .first()
                .and_then(|value| aux_as_string(value).ok())
                .unwrap_or_default();
            listener.did_form_plan(&data).await?;
        }
        m if m == XCT_GET_PROGRESS_FOR_LAUNCH => {
            let token = aux
                .first()
                .and_then(|value| aux_as_string(value).ok())
                .unwrap_or_default();
            listener.get_progress_for_launch(&token).await?;
        }
        m if m == XCT_UI_INIT_DID_FAIL => {
            let desc = aux
                .first()
                .map(aux_as_string)
                .transpose()?
                .unwrap_or_default();
            listener
                .initialization_for_ui_testing_did_fail(&desc)
                .await?;
        }
        m if m == XCT_DID_FAIL_BOOTSTRAP => {
            // The aux is an NSKeyedArchived NSError. Try plain string first,
            // then decode the archive and pull NSLocalizedDescription out of
            // the error dictionary, falling back to a generic message.
            let desc = aux
                .first()
                .and_then(|v| {
                    // plain string (unlikely but handle it)
                    if let AuxValue::String(s) = v {
                        return Some(s.clone());
                    }
                    // NSKeyedArchive -> plist Value
                    let decoded = decode_aux_archive(v).ok()?;
                    // NSError serialises as a Dictionary. String fields come
                    // through as plain values; other fields (domain, userInfo)
                    // are Uid references into the archive's $objects table
                    // which the decoder doesn't follow.
                    if let Value::Dictionary(d) = &decoded {
                        // Try inline string fields first
                        if let Some(s) = d
                            .get("NSLocalizedDescription")
                            .or_else(|| d.get("NSLocalizedFailureReason"))
                            .and_then(|v| v.as_string())
                        {
                            return Some(s.to_owned());
                        }
                        // Fall back to the numeric code with a hint for
                        // the most common values seen from testmanagerd
                        if let Some(code) = d.get("NSCode").and_then(|v| v.as_signed_integer()) {
                            let hint = match code {
                                103 => " (untrusted developer certificate — go to Settings → General → VPN & Device Management and trust your developer app)",
                                _ => "",
                            };
                            return Some(format!("NSError code {code}{hint}"));
                        }
                    }
                    None
                })
                .unwrap_or_else(|| "unknown error".to_owned());
            listener.did_fail_to_bootstrap(&desc).await?;
        }

        other => {
            warn!("Unknown _XCT_ method: {}", other);
        }
    }

    Ok(None)
}

struct EarlyXCTestBootstrapListener;

impl XCUITestListener for EarlyXCTestBootstrapListener {}

fn should_handle_in_bootstrap(method: &str) -> bool {
    matches!(
        method,
        XCT_EXCHANGE_PROTOCOL_VERSION
            | XCT_RUNNER_READY_WITH_CAPABILITIES
            | XCT_BUNDLE_READY
            | XCT_BUNDLE_READY_WITH_PROTOCOL_VERSION
            | XCT_LOG_MESSAGE
            | XCT_LOG_DEBUG_MESSAGE
    )
}

async fn install_early_xctest_handler<R: ReadWrite + 'static>(
    main_channel: &mut OwnedChannel<R>,
    xctest_config: XCTestConfiguration,
) {
    main_channel
        .set_incoming_handler(move |msg: Message| {
            let xctest_config = xctest_config.clone();
            Box::pin(async move {
                let method = match msg.data.as_ref() {
                    Some(Value::String(method)) => method.as_str(),
                    _ => return Ok(IncomingHandlerOutcome::Unhandled),
                };

                if !should_handle_in_bootstrap(method) {
                    return Ok(IncomingHandlerOutcome::Unhandled);
                }

                let aux = msg.aux.as_ref().map(|a| a.values.as_slice()).unwrap_or(&[]);

                let mut listener = EarlyXCTestBootstrapListener;
                let mut done = false;
                let reply =
                    dispatch_xct_message(method, aux, &xctest_config, &mut listener, &mut done)
                        .await?;

                Ok(match reply {
                    Some(reply_bytes) => IncomingHandlerOutcome::Reply(reply_bytes),
                    None => IncomingHandlerOutcome::HandledNoReply,
                })
            })
        })
        .await;
}

/// Main event loop: reads incoming `_XCT_*` messages and dispatches them until
/// `_XCT_didFinishExecutingTestPlan` or `timeout` elapses.
pub(super) async fn run_dispatch_loop<L: XCUITestListener>(
    driver_channel: &mut OwnedChannel<Box<dyn ReadWrite>>,
    xctest_config: &XCTestConfiguration,
    listener: &mut L,
    timeout: Option<std::time::Duration>,
) -> Result<(), IdeviceError> {
    let deadline = timeout.map(|t| std::time::Instant::now() + t);
    let mut done = false;

    loop {
        let remaining = if let Some(dl) = deadline {
            let r = dl
                .checked_duration_since(std::time::Instant::now())
                .ok_or_else(|| IdeviceError::XcTestTimeout(timeout.unwrap().as_secs_f64()))?;
            Some(r)
        } else {
            None
        };

        let msg = match remaining {
            Some(r) => driver_channel.read_message_timeout(r).await?,
            None => driver_channel.read_message().await?,
        };

        let method = match &msg.data {
            Some(Value::String(s)) => s.clone(),
            None => continue, // heartbeat / empty
            _ => {
                warn!("Non-string message data on XCTest channel");
                continue;
            }
        };

        let aux = msg.aux.as_ref().map(|a| a.values.as_slice()).unwrap_or(&[]);

        let msg_id = msg.message_header.identifier();
        let conversation_index = msg.message_header.conversation_index();
        let reply_opt =
            dispatch_xct_message(&method, aux, xctest_config, listener, &mut done).await?;

        if msg.message_header.expects_reply() {
            match reply_opt {
                Some(reply_bytes) => {
                    driver_channel
                        .send_raw_reply_for(msg_id, conversation_index, &reply_bytes)
                        .await?;
                }
                None => {
                    driver_channel
                        .send_raw_reply_for(msg_id, conversation_index, &[])
                        .await?;
                }
            }
        }

        if done {
            return Ok(());
        }
    }
}

/// Mirrors pymobiledevice3's "test done vs disconnect" race.
///
/// Once the test plan has started, the runner may terminate its own DTX
/// connection before `_XCT_didFinishExecutingTestPlan` is delivered. In that
/// case we surface `TestRunnerDisconnected` rather than hanging until timeout.
async fn run_dispatch_loop_until_done_or_disconnect<L: XCUITestListener>(
    main_client: &mut RemoteServerClient<Box<dyn ReadWrite>>,
    mut driver_channel: OwnedChannel<Box<dyn ReadWrite>>,
    xctest_config: &XCTestConfiguration,
    listener: &mut L,
    timeout: Option<std::time::Duration>,
) -> Result<(), IdeviceError> {
    let disconnected = main_client.disconnect_waiter();
    tokio::pin!(disconnected);

    tokio::select! {
        result = run_dispatch_loop(&mut driver_channel, xctest_config, listener, timeout) => result,
        _ = &mut disconnected => Err(IdeviceError::TestRunnerDisconnected),
    }
}

// ---------------------------------------------------------------------------
// XCUITestService
// ---------------------------------------------------------------------------

/// High-level service that orchestrates an XCTest (or WDA) run end-to-end.
///
/// # Example
/// ```rust,no_run
/// # #[cfg(feature = "xctest")]
/// # async fn example() -> Result<(), idevice::IdeviceError> {
/// // let svc = XCUITestService::new(provider);
/// // svc.run(cfg, &mut listener, None).await?;
/// # Ok(())
/// # }
/// ```
pub struct XCUITestService {
    provider: Arc<dyn IdeviceProvider>,
}

#[cfg(feature = "wda")]
#[derive(Debug)]
pub struct WdaRunHandle {
    task: JoinHandle<Result<(), IdeviceError>>,
    ports: WdaPorts,
    status: JsonValue,
}

#[cfg(feature = "wda")]
#[derive(Debug)]
pub struct WdaBridgedRunHandle {
    runner: WdaRunHandle,
    bridge: WdaBridge,
}

#[cfg(feature = "wda")]
impl WdaRunHandle {
    /// Returns the device-side ports used by the running WDA instance.
    pub fn ports(&self) -> WdaPorts {
        self.ports
    }

    /// Returns the `/status` payload observed when WDA became reachable.
    pub fn status(&self) -> &JsonValue {
        &self.status
    }

    /// Waits for the underlying xctrunner task to complete.
    pub async fn wait(self) -> Result<(), IdeviceError> {
        match self.task.await {
            Ok(result) => result,
            Err(error) => Err(IdeviceError::UnknownErrorType(format!(
                "wda runner task join failed: {error}"
            ))),
        }
    }

    /// Aborts the underlying xctrunner task.
    pub fn abort(&self) {
        self.task.abort();
    }
}

#[cfg(feature = "wda")]
impl WdaBridgedRunHandle {
    /// Returns the localhost bridge for this WDA runner.
    pub fn bridge(&self) -> &WdaBridge {
        &self.bridge
    }

    /// Returns the device-side ports used by the running WDA instance.
    pub fn ports(&self) -> WdaPorts {
        self.runner.ports()
    }

    /// Returns the `/status` payload observed when WDA became reachable.
    pub fn status(&self) -> &JsonValue {
        self.runner.status()
    }

    /// Returns the localhost WDA HTTP URL.
    pub fn wda_url(&self) -> &str {
        self.bridge.wda_url()
    }

    /// Returns the localhost MJPEG URL.
    pub fn mjpeg_url(&self) -> &str {
        self.bridge.mjpeg_url()
    }

    /// Waits for the underlying xctrunner task to complete.
    pub async fn wait(self) -> Result<(), IdeviceError> {
        self.runner.wait().await
    }

    /// Aborts the underlying xctrunner task.
    pub fn abort(&self) {
        self.runner.abort();
    }
}

#[cfg(feature = "wda")]
struct NoopXCTestListener;

#[cfg(feature = "wda")]
impl XCUITestListener for NoopXCTestListener {}

impl std::fmt::Debug for XCUITestService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XCUITestService")
            .field("provider", &"<IdeviceProvider>")
            .finish()
    }
}

impl XCUITestService {
    /// Creates a new `XCUITestService` backed by the given provider.
    pub fn new(provider: Arc<dyn IdeviceProvider>) -> Self {
        Self { provider }
    }

    /// Runs the XCTest bundle described by `cfg` to completion.
    ///
    /// # Arguments
    /// * `cfg`      - Test configuration (runner/target app info, filters, …)
    /// * `listener` - Receives lifecycle events as the test runs
    /// * `timeout`  - Optional wall-clock timeout from when the test plan starts
    ///
    /// # Errors
    /// * `IdeviceError::XcTestTimeout` if `timeout` elapses
    /// * `IdeviceError::TestRunnerTimeout` if the runner does not open
    ///   `XCTestDriverInterface` within 30 seconds
    pub async fn run<L: XCUITestListener>(
        &self,
        cfg: TestConfig,
        listener: &mut L,
        timeout: Option<std::time::Duration>,
    ) -> Result<(), IdeviceError> {
        // 1. Session UUID + config path
        let session_id = uuid::Uuid::new_v4();
        let xctest_path = format!(
            "/tmp/{}.xctestconfiguration",
            session_id.to_string().to_uppercase()
        );

        // 2. iOS major version (needed for service selection and launch env)
        let ios_major_version: u8 = {
            let mut lockdown = LockdownClient::connect(&*self.provider).await?;
            lockdown
                .start_session(&self.provider.get_pairing_file().await?)
                .await?;
            let ver = lockdown.get_value(Some("ProductVersion"), None).await?;
            ver.as_string()
                .and_then(|s| s.split('.').next())
                .and_then(|s| s.parse().ok())
                .unwrap_or(16u8)
        };

        // 3. Build XCTestConfiguration
        let xctest_config = cfg.build_xctest_configuration(session_id, ios_major_version)?;

        // 4. Connect to testmanagerd (ctrl + main) and DVT
        let mut conns = connect_testmanagerd(&*self.provider, ios_major_version).await?;
        let mut ctrl_proxy = TestManagerProxy::open(&mut conns.ctrl, ios_major_version).await?;
        let mut main_proxy = TestManagerProxy::open(&mut conns.main, ios_major_version).await?;
        let mut process_control = XCTestProcessControlChannel::open(&mut conns.dvt).await?;

        let config_name = cfg.config_name().to_owned();
        initialize_testmanager_sessions(&mut ctrl_proxy, &mut main_proxy, &xctest_config).await?;
        register_early_driver_channel_handler(&mut conns.main, &xctest_config).await;
        initialize_testmanager_daemon_sessions(
            &mut ctrl_proxy,
            &mut main_proxy,
            ios_major_version,
            &session_id,
            &xctest_config,
        )
        .await?;

        // Build launch environment from the config
        let (launch_args, launch_env, launch_options) = build_launch_env(
            ios_major_version,
            &session_id,
            &cfg.runner_app_path,
            &cfg.runner_app_container,
            &config_name,
            &xctest_path,
            cfg.runner_env.as_ref(),
            cfg.runner_args.as_deref(),
        );

        let _pid = launch_and_authorize_test_runner(
            &mut ctrl_proxy,
            &mut process_control,
            ios_major_version,
            &cfg.runner_bundle_id,
            launch_args,
            launch_env,
            launch_options,
        )
        .await?;

        // 6-7. Wait for driver channel and start the test plan.
        let driver_channel = start_test_plan_session(&mut conns.main, &mut main_proxy).await?;

        // 8. Dispatch loop, raced against the runner connection dropping.
        run_dispatch_loop_until_done_or_disconnect(
            &mut conns.main,
            driver_channel,
            &xctest_config,
            listener,
            timeout,
        )
        .await?;

        Ok(())
    }

    /// Starts an XCTest runner intended to host WebDriverAgent and waits
    /// until WDA responds on its device-side HTTP port.
    ///
    /// The xctrunner orchestration continues on a background task. This is
    /// designed for automation use cases where callers want a durable WDA
    /// session instead of waiting for the XCTest plan to terminate.
    ///
    /// Readiness detection currently uses a simple polling loop against
    /// `WdaClient::status()`. This is intentionally conservative bootstrap
    /// behavior for now; large-scale orchestration should still stagger or
    /// back off parallel startup attempts at a higher layer.
    #[cfg(feature = "wda")]
    pub async fn run_until_wda_ready(
        &self,
        cfg: TestConfig,
        readiness_timeout: std::time::Duration,
    ) -> Result<WdaRunHandle, IdeviceError> {
        let provider = self.provider.clone();
        let runner_cfg = cfg.clone();
        let task = tokio::spawn(async move {
            let service = XCUITestService::new(provider);
            let mut listener = NoopXCTestListener;
            service.run(runner_cfg, &mut listener, None).await
        });

        let wda = WdaClient::new(&*self.provider);
        let deadline = std::time::Instant::now() + readiness_timeout;
        let poll_interval = std::time::Duration::from_millis(250);

        let status = loop {
            if task.is_finished() {
                let result = match task.await {
                    Ok(result) => result,
                    Err(error) => {
                        return Err(IdeviceError::UnknownErrorType(format!(
                            "wda runner task join failed: {error}"
                        )));
                    }
                };
                result?;
                return Err(IdeviceError::UnexpectedResponse(
                    "unexpected response".into(),
                ));
            }

            match wda.status().await {
                Ok(status) => break status,
                Err(_) if std::time::Instant::now() < deadline => {
                    tokio::time::sleep(poll_interval).await;
                }
                Err(error) => {
                    task.abort();
                    return Err(error);
                }
            }
        };

        Ok(WdaRunHandle {
            task,
            ports: wda.ports(),
            status,
        })
    }

    /// Starts an XCTest-hosted WDA runner, waits until WDA is reachable, and
    /// exposes localhost URLs suitable for GUI/web consumers.
    #[cfg(feature = "wda")]
    pub async fn run_until_wda_ready_with_bridge(
        &self,
        cfg: TestConfig,
        readiness_timeout: std::time::Duration,
    ) -> Result<WdaBridgedRunHandle, IdeviceError> {
        let runner = self.run_until_wda_ready(cfg, readiness_timeout).await?;
        let bridge = WdaBridge::start_with_ports(self.provider.clone(), runner.ports()).await?;
        Ok(WdaBridgedRunHandle { runner, bridge })
    }
}
