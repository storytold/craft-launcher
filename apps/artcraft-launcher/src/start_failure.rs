//! A window that can't start, or stops, says why instead of the app quitting silently.
//!
//! Without a usable GPU, graphics driver or desktop (a broken or very old driver, a remote or
//! virtual machine, a program started by a service or a test harness), the window's surface
//! can't be configured and eframe returns an error. ArtCraft Launcher used to log it and exit with
//! no window, which looks like "the app doesn't open".
//! winget-pkgs validation starts installed programs without a desktop, so it rejected the launcher
//! with `Validation-Executable-Error` and it never reached WinGet.
//!
//! Now the reason also goes to an error dialog, as FilmCraft does since 0.14.0. The dialog keeps
//! the process alive until it is dismissed; the exit code afterwards is still the error's.

use artcraft_launcher_ui_egui::i18n;

/// The dialog text in English, which is also its catalog key. Every catalog marked
/// `complete_menus` has a row for it (`every_complete_catalog_translates_the_dialog`).
const TEMPLATE: &str = "ArtCraft Launcher's window could not start or stopped with an error: {e}\n\nIf this keeps happening, updating the graphics driver often fixes it.";

/// What the dialog and the log say, in `lang`. Any error `run_native` returns lands here, also one
/// after the window was up, so graphics is only the likely cause, not the certain one.
pub(crate) fn message(lang: i18n::Lang, error: &eframe::Error) -> String {
    i18n::fmt(i18n::tr(lang, TEMPLATE), &[("e", &error.to_string())])
}

/// Logs the failure in English and shows it in a dialog in the system language, which stays until
/// the user closes it. Not `i18n::current()`: that is set while frames are drawn, and the window
/// may never have drawn one.
pub(crate) fn report(error: &eframe::Error) {
    log::error!("{}", message(i18n::Lang::EN, error));
    let text = message(i18n::system_lang(), error);
    let _ = rfd::MessageDialog::new().set_title("ArtCraft Launcher").set_description(&text).set_level(rfd::MessageLevel::Error).show();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure() -> eframe::Error {
        eframe::Error::AppCreation("no adapter".into())
    }

    #[test]
    fn the_message_names_the_cause_and_what_to_do() {
        let text = message(i18n::Lang::EN, &failure());
        assert!(text.starts_with("ArtCraft Launcher's window could not start or stopped"), "{text}");
        assert!(text.contains("no adapter"), "{text}");
        assert!(text.contains("graphics driver"), "{text}");
    }

    /// The i18n coverage test only finds `tl!`/`t` literals in the UI crate, so this string is
    /// checked here: every complete catalog translates it, keeps the product name in English and
    /// fills in the error.
    #[test]
    fn every_complete_catalog_translates_the_dialog() {
        for lang in i18n::Lang::all().filter(|l| l.complete_menus()) {
            assert!(i18n::has(lang, TEMPLATE), "{} has no row for the start-failure dialog", lang.code());
            let text = message(lang, &failure());
            assert_ne!(text, message(i18n::Lang::EN, &failure()), "{} isn't translated", lang.code());
            assert!(text.contains("ArtCraft Launcher"), "{}: {text}", lang.code());
            assert!(text.contains("no adapter") && !text.contains("{e}"), "{}: {text}", lang.code());
        }
    }
}
