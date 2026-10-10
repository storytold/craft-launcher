//! ArtCraft Launcher's interface: the Craft App Suite as a grid of cards or a table, with
//! install, update, open and remove, shared by the desktop and web builds.
//!
//! The UI is thin: it draws [`model::AppEntry`]s and sends
//! [`Command`](artcraft_launcher_core::protocol::Command)s to a
//! [`Backend`](artcraft_launcher_core::protocol::Backend). The shell supplies a [`Host`] for the
//! things only it can do (notifications, the tray, quitting).

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod app;
mod dialogs;
pub mod icons;
pub mod model;
mod notes;
pub mod settings;
pub mod theme;
mod views;
mod widgets;

pub use app::{Host, LauncherUi, TitleBar};
pub use settings::Settings;
