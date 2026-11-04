//! macOS only: closing the window to the menu bar hides the app, as ⌘H does, rather than ordering
//! the window out.
//!
//! macOS brings a hidden app back by itself: clicking its Dock icon, opening it again from Finder
//! or Spotlight, or picking it with ⌘Tab all unhide it, window and all. A window that was only
//! ordered out stays gone, as the Dock asks the app to "reopen" instead, and neither winit nor
//! eframe passes that on (issue #456). Handling that event here would take an Apple Event handler,
//! which needs `unsafe`.

use objc2::MainThreadMarker;
use objc2_app_kit::NSApplication;

/// Hides the app and its window. Must run on the main thread, as the UI does; elsewhere it does
/// nothing.
pub fn hide() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).hide(None);
    }
}

/// Brings the app back from [`hide`] and makes it the active app. Main thread only, as [`hide`].
pub fn unhide() {
    if let Some(mtm) = MainThreadMarker::new() {
        NSApplication::sharedApplication(mtm).unhide(None);
    }
}
