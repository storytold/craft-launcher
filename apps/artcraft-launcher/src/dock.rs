//! macOS only: whether the launcher has a Dock icon.
//!
//! The bundle is a normal Dock app (`LSUIElement` is never set). With "Hide the Dock icon while
//! in the menu bar" on, closing the window to the menu bar switches the app to the accessory
//! activation policy, which takes it out of the Dock and the app switcher, and opening the window
//! switches it back to regular, so an open window behaves like any other app's.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

/// Puts the launcher in the Dock (`true`) or leaves only its menu bar icon (`false`). Must run on
/// the main thread, as the UI does; elsewhere it does nothing.
pub fn show_in_dock(on: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let policy = if on { NSApplicationActivationPolicy::Regular } else { NSApplicationActivationPolicy::Accessory };
    if !NSApplication::sharedApplication(mtm).setActivationPolicy(policy) {
        log::warn!("couldn't {} the Dock icon", if on { "show" } else { "hide" });
    }
}
