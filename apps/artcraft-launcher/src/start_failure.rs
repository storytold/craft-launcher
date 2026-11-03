//! A window that can't start, or stops, says why instead of the app quitting silently.
//!
//! Without a usable GPU, graphics driver or desktop (a broken or very old driver, a remote or
//! virtual machine, a program started by a service or a test harness), the window's surface
//! can't be configured and eframe returns an error. ArtCraft Launcher used to log it and exit with no
//! window, which looks like "the app doesn't open".
//! winget-pkgs validation starts installed programs without a desktop, so it rejected the launcher with
//! `Validation-Executable-Error` and it never reached WinGet.
//!
//! Now the reason also goes to an error dialog, as FilmCraft does since 0.14.0. The dialog keeps
//! the process alive until it is dismissed; the exit code afterwards is still the error's.

/// What the dialog and the log say.
pub(crate) fn message(error: &eframe::Error) -> String {
    format!("ArtCraft Launcher could not start its window: {error}\n\nUpdating the graphics driver usually fixes this.")
}

/// Logs the failure and shows it in a dialog, which stays until the user closes it.
pub(crate) fn report(error: &eframe::Error) {
    let message = message(error);
    log::error!("{message}");
    let _ = rfd::MessageDialog::new().set_title("ArtCraft Launcher").set_description(&message).set_level(rfd::MessageLevel::Error).show();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_names_the_cause_and_what_to_do() {
        let text = message(&eframe::Error::AppCreation("no adapter".into()));
        assert!(text.starts_with("ArtCraft Launcher could not start its window"), "{text}");
        assert!(text.contains("no adapter"), "{text}");
        assert!(text.contains("graphics driver"), "{text}");
    }
}
