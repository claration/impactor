//! DTX channel names and method selectors for the XCTest / testmanagerd protocol.
//!
//! These constants correspond 1-to-1 with the Objective-C selector strings used by
//! Xcode's IDE interface and the on-device testmanagerd daemon. They are kept in one
//! place so every other module can import them without magic strings.
// Jackson Coxson

// ---------------------------------------------------------------------------
// testmanagerd service names
// ---------------------------------------------------------------------------

/// iOS < 14 — lockdown, no SSL.
pub const TESTMANAGERD_SERVICE: &str = "com.apple.testmanagerd.lockdown";

/// iOS 14–16 — lockdown with SSL.
pub const TESTMANAGERD_SECURE_SERVICE: &str = "com.apple.testmanagerd.lockdown.secure";

/// iOS 17+ — accessed over the RSD tunnel.
pub const TESTMANAGERD_RSD_SERVICE: &str = "com.apple.dt.testmanagerd.remote";

// ---------------------------------------------------------------------------
// DVT (instruments) service names
// ---------------------------------------------------------------------------

/// iOS < 14 — legacy instruments remote server, lockdown, no SSL.
pub const DVT_LEGACY_SERVICE: &str = "com.apple.instruments.remoteserver";

/// iOS 14+ — instruments remote server with DVT secure socket proxy.
pub const DVT_SERVICE: &str = "com.apple.instruments.remoteserver.DVTSecureSocketProxy";

// ---------------------------------------------------------------------------
// DTX channel identifiers
// ---------------------------------------------------------------------------

/// Channel identifier for the XCTest IDE ↔ daemon interface.
/// Used on iOS < 17 (lockdown path).
pub const XCTEST_MANAGER_IDE_INTERFACE: &str = "XCTestManager_IDEInterface";

/// Service identifier for the daemon-facing side of the XCTest proxy channel.
pub const XCTEST_MANAGER_DAEMON_CONNECTION_INTERFACE: &str =
    "XCTestManager_DaemonConnectionInterface";

/// Service identifier for the runner-facing side of the XCTest proxy channel.
pub const XCTEST_DRIVER_INTERFACE: &str = "XCTestDriverInterface";

/// iOS 17+ proxy channel: IDE ↔ DaemonConnectionInterface.
/// Format used by pymobiledevice3's DtxProxyService over RSD.
pub const XCTEST_PROXY_IDE_TO_DAEMON: &str =
    "dtxproxy:XCTestManager_IDEInterface:XCTestManager_DaemonConnectionInterface";

/// iOS 17+ proxy channel: IDE ↔ XCTestDriverInterface (reverse channel from runner).
pub const XCTEST_PROXY_IDE_TO_DRIVER: &str =
    "dtxproxy:XCTestManager_IDEInterface:XCTestDriverInterface";

// ---------------------------------------------------------------------------
// Xcode version reported to testmanagerd
// ---------------------------------------------------------------------------

/// Protocol version number reported to testmanagerd as the IDE's Xcode version.
/// The exact value is not significant; 36 matches a recent Xcode release.
pub const XCODE_VERSION: u64 = 36;

// ---------------------------------------------------------------------------
// Outgoing IDE → daemon selectors
// ---------------------------------------------------------------------------

/// iOS 17+: initiate the control channel, passing IDE capabilities.
pub const IDE_INITIATE_CTRL_SESSION_WITH_CAPABILITIES: &str =
    "_IDE_initiateControlSessionWithCapabilities:";

/// iOS 11–16: initiate the control channel with a protocol version number.
pub const IDE_INITIATE_CTRL_SESSION_WITH_PROTOCOL_VERSION: &str =
    "_IDE_initiateControlSessionWithProtocolVersion:";

/// iOS 17+: initiate the main session with a UUID and IDE capabilities.
pub const IDE_INITIATE_SESSION_WITH_IDENTIFIER_CAPABILITIES: &str =
    "_IDE_initiateSessionWithIdentifier:capabilities:";

/// iOS 11–16: initiate the main session with a UUID, client string, path, and version.
pub const IDE_INITIATE_SESSION_WITH_IDENTIFIER_FOR_CLIENT_AT_PATH_PROTOCOL_VERSION: &str =
    "_IDE_initiateSessionWithIdentifier:forClient:atPath:protocolVersion:";

/// iOS 12+: authorise the test session for a launched process ID.
pub const IDE_AUTHORIZE_TEST_SESSION: &str = "_IDE_authorizeTestSessionWithProcessID:";

/// iOS 10–11: authorise by PID with a protocol version.
pub const IDE_INITIATE_CTRL_SESSION_FOR_PID_PROTOCOL_VERSION: &str =
    "_IDE_initiateControlSessionForTestProcessID:protocolVersion:";

/// iOS < 10: authorise by PID only.
pub const IDE_INITIATE_CTRL_SESSION_FOR_PID: &str = "_IDE_initiateControlSessionForTestProcessID:";

// ---------------------------------------------------------------------------
// Outgoing IDE → driver selectors
// ---------------------------------------------------------------------------

/// Signal the test runner to begin executing its test plan.
pub const IDE_START_EXECUTING_TEST_PLAN: &str = "_IDE_startExecutingTestPlanWithProtocolVersion:";

// ---------------------------------------------------------------------------
// Incoming runner → IDE callbacks (_XCT_*)
// ---------------------------------------------------------------------------

/// Test plan has started executing.
pub const XCT_DID_BEGIN_TEST_PLAN: &str = "_XCT_didBeginExecutingTestPlan";

/// Test plan has finished executing (terminal event).
pub const XCT_DID_FINISH_TEST_PLAN: &str = "_XCT_didFinishExecutingTestPlan";

/// Runner signals readiness and negotiates capabilities (iOS 17+ DDI variant).
pub const XCT_RUNNER_READY_WITH_CAPABILITIES: &str = "_XCT_testRunnerReadyWithCapabilities:";

/// Informational log message from the runner.
pub const XCT_LOG_MESSAGE: &str = "_XCT_logMessage:";

/// Debug log message from the runner.
pub const XCT_LOG_DEBUG_MESSAGE: &str = "_XCT_logDebugMessage:";

/// Protocol version negotiation.
pub const XCT_EXCHANGE_PROTOCOL_VERSION: &str =
    "_XCT_exchangeCurrentProtocolVersion_minimumVersion_";

/// Test bundle is ready (legacy, no capabilities).
pub const XCT_BUNDLE_READY: &str = "_XCT_testBundleReady";

/// Test bundle is ready with a protocol version.
pub const XCT_BUNDLE_READY_WITH_PROTOCOL_VERSION: &str =
    "_XCT_testBundleReadyWithProtocolVersion_minimumVersion_";

/// UI testing initialization began.
pub const XCT_DID_BEGIN_UI_INIT: &str = "_XCT_didBeginInitializingForUITesting";

/// Test runner formed the test plan payload.
pub const XCT_DID_FORM_PLAN: &str = "_XCT_didFormPlanWithData:";

/// Runner requested launch progress for a token.
pub const XCT_GET_PROGRESS_FOR_LAUNCH: &str = "_XCT_getProgressForLaunch:";

/// UI testing initialization failed.
pub const XCT_UI_INIT_DID_FAIL: &str = "_XCT_initializationForUITestingDidFailWithError:";

/// Test runner failed to bootstrap.
pub const XCT_DID_FAIL_BOOTSTRAP: &str = "_XCT_didFailToBootstrapWithError:";

// --- suite lifecycle (legacy string-based, pre-iOS 14) --------------------

/// Test suite started (legacy).
pub const XCT_SUITE_DID_START: &str = "_XCT_testSuite_didStartAt_";

/// Test suite finished (legacy).
pub const XCT_SUITE_DID_FINISH: &str =
    "_XCT_testSuite_didFinishAt_runCount_withFailures_unexpected_testDuration_totalDuration_";

// --- case lifecycle (legacy string-based, pre-iOS 14) ---------------------

/// Test case started (legacy).
pub const XCT_CASE_DID_START: &str = "_XCT_testCaseDidStartForTestClass_method_";

/// Test case finished (legacy).
pub const XCT_CASE_DID_FINISH: &str =
    "_XCT_testCaseDidFinishForTestClass_method_withStatus_duration_";

/// Test case recorded a failure (legacy).
pub const XCT_CASE_DID_FAIL: &str =
    "_XCT_testCaseDidFailForTestClass_method_withMessage_file_line_";

/// Test case stalled on the main thread (legacy).
pub const XCT_CASE_DID_STALL: &str = "_XCT_testCase_method_didStallOnMainThreadInFile_line_";

/// Test case will start an activity (legacy).
pub const XCT_CASE_WILL_START_ACTIVITY: &str = "_XCT_testCase_method_willStartActivity_";

/// Test case finished an activity (legacy).
pub const XCT_CASE_DID_FINISH_ACTIVITY: &str = "_XCT_testCase_method_didFinishActivity_";

// --- suite lifecycle (identifier-based, iOS 14+) --------------------------

/// Test suite started, identified by XCTTestIdentifier.
pub const XCT_SUITE_DID_START_ID: &str = "_XCT_testSuiteWithIdentifier:didStartAt:";

/// Test suite finished, identified by XCTTestIdentifier.
pub const XCT_SUITE_DID_FINISH_ID: &str = "_XCT_testSuiteWithIdentifier:didFinishAt:runCount:skipCount:failureCount:expectedFailureCount:uncaughtExceptionCount:testDuration:totalDuration:";

// --- case lifecycle (identifier-based, iOS 14+) ---------------------------

/// Test case started, identified by XCTTestIdentifier.
pub const XCT_CASE_DID_START_ID: &str =
    "_XCT_testCaseDidStartWithIdentifier:testCaseRunConfiguration:";

/// Test case finished, identified by XCTTestIdentifier.
pub const XCT_CASE_DID_FINISH_ID: &str =
    "_XCT_testCaseWithIdentifier:didFinishWithStatus:duration:";

/// Test case recorded an XCTIssue, identified by XCTTestIdentifier.
pub const XCT_CASE_DID_RECORD_ISSUE: &str = "_XCT_testCaseWithIdentifier:didRecordIssue:";

/// Test case will start an activity, identified by XCTTestIdentifier.
pub const XCT_CASE_WILL_START_ACTIVITY_ID: &str = "_XCT_testCaseWithIdentifier:willStartActivity:";

/// Test case finished an activity, identified by XCTTestIdentifier.
pub const XCT_CASE_DID_FINISH_ACTIVITY_ID: &str = "_XCT_testCaseWithIdentifier:didFinishActivity:";

/// Performance metric measured during a test method.
pub const XCT_METHOD_DID_MEASURE_METRIC: &str =
    "_XCT_testMethod_ofClass_didMeasureMetric_file_line_";
