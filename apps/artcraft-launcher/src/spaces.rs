//! macOS only: which desktop (Space) the launcher's window appears on when it's shown.
//!
//! By default macOS switches to the desktop the window was last on, which is jarring when the
//! launcher is opened from the menu bar on another desktop. With "move to active space" set, the
//! window comes to the desktop you're on instead (and leaves the one it was on). Other platforms
//! show a hidden window on the current desktop anyway, so this module only exists on macOS.

use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSWindowCollectionBehavior};

pub(super) fn activation_policy(
    window_visible: bool,
    hide_when_hidden: bool,
    tray_visible: bool,
    keep_in_tray: bool,
) -> NSApplicationActivationPolicy {
    if !window_visible && hide_when_hidden && tray_visible && keep_in_tray {
        NSApplicationActivationPolicy::Accessory
    } else {
        NSApplicationActivationPolicy::Regular
    }
}

/// Applies the Dock visibility preference. Must run on the main thread.
pub fn set_dock_visibility(window_visible: bool, hide_when_hidden: bool, tray_visible: bool, keep_in_tray: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(activation_policy(window_visible, hide_when_hidden, tray_visible, keep_in_tray));
}

/// Makes the launcher's windows follow the current desktop when shown (`true`), or stay on their
/// own desktop and switch to it (`false`, macOS's default). Must run on the main thread, as the
/// UI does; elsewhere it does nothing.
pub fn follow_current_desktop(on: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    for window in app.windows().iter() {
        let mut behavior = window.collectionBehavior();
        behavior.set(NSWindowCollectionBehavior::MoveToActiveSpace, on);
        window.setCollectionBehavior(behavior);
    }
}
