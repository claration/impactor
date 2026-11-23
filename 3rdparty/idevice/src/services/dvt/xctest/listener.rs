//! XCUITest lifecycle callback trait.
//!
//! Implement [`XCUITestListener`] and pass it to [`super::XCUITestService::run`] to
//! receive per-test-case and per-suite events as they arrive from the runner.
//! All methods have default no-op implementations; override only what you need.
// Jackson Coxson

use crate::IdeviceError;

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// Result record for a single finished test case.
#[derive(Debug, Clone)]
pub struct XCTestCaseResult {
    /// Test class name (e.g. `"UITests"`).
    pub test_class: String,
    /// Test method name (e.g. `"testLogin"`).
    pub method: String,
    /// Outcome string: `"passed"`, `"failed"`, or `"skipped"`.
    pub status: String,
    /// Wall-clock duration of the test case in seconds.
    pub duration: f64,
}

// ---------------------------------------------------------------------------
// Listener trait
// ---------------------------------------------------------------------------

/// Callback interface for XCUITest lifecycle events.
///
/// All methods receive `&mut self` so implementors can accumulate state (e.g.
/// counters, log buffers).  Every method returns `Result<(), IdeviceError>` so
/// that the orchestrator can propagate fatal listener errors back to the caller.
///
/// The default implementation is a no-op for every method.
#[allow(async_fn_in_trait)]
pub trait XCUITestListener: Send {
    // --- test plan ----------------------------------------------------------

    /// Invoked when the runner begins executing the test plan.
    async fn did_begin_executing_test_plan(&mut self) -> Result<(), IdeviceError> {
        Ok(())
    }

    /// Invoked when the runner has finished executing the entire test plan.
    async fn did_finish_executing_test_plan(&mut self) -> Result<(), IdeviceError> {
        Ok(())
    }

    // --- bundle ready -------------------------------------------------------

    /// Invoked when the test bundle signals readiness (legacy protocol, no capabilities).
    async fn test_bundle_ready(&mut self) -> Result<(), IdeviceError> {
        Ok(())
    }

    /// Invoked when the test bundle reports its protocol version.
    async fn test_bundle_ready_with_protocol_version(
        &mut self,
        _protocol_version: u64,
        _minimum_version: u64,
    ) -> Result<(), IdeviceError> {
        Ok(())
    }

    /// Invoked when the runner announces readiness together with its capability set.
    async fn test_runner_ready_with_capabilities(&mut self) -> Result<(), IdeviceError> {
        Ok(())
    }

    // --- suite lifecycle ----------------------------------------------------

    /// Invoked when a test suite starts.
    async fn test_suite_did_start(
        &mut self,
        _suite: &str,
        _started_at: &str,
    ) -> Result<(), IdeviceError> {
        Ok(())
    }

    /// Invoked when a test suite finishes.
    #[allow(clippy::too_many_arguments)]
    async fn test_suite_did_finish(
        &mut self,
        _suite: &str,
        _finished_at: &str,
        _run_count: u64,
        _failures: u64,
        _unexpected: u64,
        _test_duration: f64,
        _total_duration: f64,
        _skipped: u64,
        _expected_failures: u64,
        _uncaught_exceptions: u64,
    ) -> Result<(), IdeviceError> {
        Ok(())
    }

    // --- case lifecycle -----------------------------------------------------

    /// Invoked when a single test case starts.
    async fn test_case_did_start(
        &mut self,
        _test_class: &str,
        _method: &str,
    ) -> Result<(), IdeviceError> {
        Ok(())
    }

    /// Invoked when a single test case finishes.
    async fn test_case_did_finish(
        &mut self,
        _result: XCTestCaseResult,
    ) -> Result<(), IdeviceError> {
        Ok(())
    }

    /// Invoked when a test case records a failure.
    async fn test_case_did_fail(
        &mut self,
        _test_class: &str,
        _method: &str,
        _message: &str,
        _file: &str,
        _line: u64,
    ) -> Result<(), IdeviceError> {
        Ok(())
    }

    /// Invoked when a test case stalls on the main thread.
    async fn test_case_did_stall(
        &mut self,
        _test_class: &str,
        _method: &str,
        _file: &str,
        _line: u64,
    ) -> Result<(), IdeviceError> {
        Ok(())
    }

    // --- activities ---------------------------------------------------------

    /// Invoked when a test case is about to start an activity step.
    async fn test_case_will_start_activity(
        &mut self,
        _test_class: &str,
        _method: &str,
        _activity_title: &str,
    ) -> Result<(), IdeviceError> {
        Ok(())
    }

    /// Invoked when a test case finishes an activity step.
    async fn test_case_did_finish_activity(
        &mut self,
        _test_class: &str,
        _method: &str,
        _activity_title: &str,
    ) -> Result<(), IdeviceError> {
        Ok(())
    }

    // --- metrics ------------------------------------------------------------

    /// Invoked when a test method measures a performance metric.
    async fn test_method_did_measure_metric(
        &mut self,
        _test_class: &str,
        _method: &str,
        _metric: &str,
        _file: &str,
        _line: u64,
    ) -> Result<(), IdeviceError> {
        Ok(())
    }

    // --- logging ------------------------------------------------------------

    /// Invoked for informational log messages from the runner.
    async fn log_message(&mut self, _message: &str) -> Result<(), IdeviceError> {
        Ok(())
    }

    /// Invoked for debug log messages from the runner.
    async fn log_debug_message(&mut self, _message: &str) -> Result<(), IdeviceError> {
        Ok(())
    }

    // --- protocol negotiation -----------------------------------------------

    /// Invoked when the runner negotiates protocol versions.
    async fn exchange_protocol_version(
        &mut self,
        _current: u64,
        _minimum: u64,
    ) -> Result<(), IdeviceError> {
        Ok(())
    }

    // --- iOS 14+ UI testing init --------------------------------------------

    /// Invoked when initialization for UI testing begins.
    async fn did_begin_initializing_for_ui_testing(&mut self) -> Result<(), IdeviceError> {
        Ok(())
    }

    /// Invoked when the runner forms a test plan payload.
    async fn did_form_plan(&mut self, _data: &str) -> Result<(), IdeviceError> {
        Ok(())
    }

    /// Invoked when the runner asks for launch progress.
    async fn get_progress_for_launch(&mut self, _token: &str) -> Result<(), IdeviceError> {
        Ok(())
    }

    /// Invoked when UI testing initialization fails.
    async fn initialization_for_ui_testing_did_fail(
        &mut self,
        _description: &str,
    ) -> Result<(), IdeviceError> {
        Ok(())
    }

    /// Invoked when the test runner fails to bootstrap.
    async fn did_fail_to_bootstrap(&mut self, description: &str) -> Result<(), IdeviceError> {
        Err(IdeviceError::UnexpectedResponse(format!(
            "test runner failed to bootstrap: {description}"
        )))
    }
}
