#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(target_os = "macos")]
static APP_WAS_ACTIVE: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "macos")]
pub(crate) fn set_main_window_visible(visible: bool) {
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

    let Some(main_thread) = MainThreadMarker::new() else {
        log::warn!("Unable to update macOS activation policy off the main thread");
        return;
    };

    let app = NSApplication::sharedApplication(main_thread);
    let policy = if visible {
        NSApplicationActivationPolicy::Regular
    } else {
        NSApplicationActivationPolicy::Accessory
    };

    if !app.setActivationPolicy(policy) {
        log::warn!("Failed to switch macOS activation policy");
    }

    if visible && objc2::available!(macos = 14.0) {
        app.activate();
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn reset_activation_state() {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };

    let app = NSApplication::sharedApplication(main_thread);
    APP_WAS_ACTIVE.store(app.isActive(), Ordering::Relaxed);
}

#[cfg(target_os = "macos")]
pub(crate) fn activation_reopen_requested() -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let Some(main_thread) = MainThreadMarker::new() else {
        return false;
    };

    let app = NSApplication::sharedApplication(main_thread);
    let is_active = app.isActive();
    let was_active = APP_WAS_ACTIVE.swap(is_active, Ordering::Relaxed);

    is_active && !was_active
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn set_main_window_visible(_visible: bool) {}

#[cfg(not(target_os = "macos"))]
pub(crate) fn reset_activation_state() {}

#[cfg(not(target_os = "macos"))]
pub(crate) fn activation_reopen_requested() -> bool {
    false
}
