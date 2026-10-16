//! ArtCraft Launcher's interface: the Craft App Suite as a grid of cards or a table, with
//! install, update, open and remove, shared by the desktop and web builds.
//!
//! The UI is thin: it draws [`model::AppEntry`]s and sends
//! [`Command`](artcraft_launcher_core::protocol::Command)s to a
//! [`Backend`](artcraft_launcher_core::protocol::Backend). The shell supplies a [`Host`] for the
//! things only it can do (notifications, the tray, quitting).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

/// Translate an English UI string into the current language (see [`i18n`]).
#[macro_export]
macro_rules! tl {
    ($s:literal) => {
        $crate::i18n::t($s)
    };
}

mod app;
#[cfg(not(target_arch = "wasm32"))]
mod cjk_font;
mod community;
mod details;
mod dialogs;
pub mod i18n;
pub mod icons;
pub mod model;
mod motion;
mod notes;
pub mod settings;
pub mod theme;
mod views;
mod widgets;

pub use app::{Host, LauncherUi, TitleBar};
pub use i18n::{
    Lang, LangInfo, current, detect_system_lang_in_background, fmt, has, lang_from_tag, message, msg, set_current, set_system_locales, system_lang,
    t, t_id, tn, tr, tr_ctx, tr_id, trn,
};
pub use settings::Settings;
