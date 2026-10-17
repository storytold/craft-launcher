//! macOS only: whether the launcher has a Dock icon.
//!
//! The launcher is a normal Dock app (`LSUIElement` is never set). With "hide the Dock icon while
//! the window is closed" on, it becomes a menu-bar-only (accessory) app while its window is put
//! away in the menu bar, and a Dock app again when the window comes back. Other platforms have no
//! Dock, so this module only exists on macOS.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};

/// Shows (`true`) or hides (`false`) the launcher's Dock icon, which also adds it to or takes it
/// out of the app switcher (Cmd-Tab). Must run on the main thread, as the UI does; elsewhere it
/// does nothing.
pub fn show_icon(on: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let policy = if on { NSApplicationActivationPolicy::Regular } else { NSApplicationActivationPolicy::Accessory };
    let app = NSApplication::sharedApplication(mtm);
    if app.activationPolicy() != policy && !app.setActivationPolicy(policy) {
        log::warn!("couldn't {} the Dock icon", if on { "show" } else { "hide" });
    }
}
