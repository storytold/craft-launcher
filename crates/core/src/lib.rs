//! ArtCraft Launcher core: everything that is pure data and logic.
//!
//! No I/O lives here, so the desktop shell, the web build and the tests share it unchanged:
//! the Craft App Suite catalog, version handling, GitHub release parsing, choosing the right
//! download for a platform, and turning "what's installed" plus "what's released" into a status.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod catalog;
pub mod details;
pub mod distro;
pub mod feed;
pub mod platform;
pub mod protocol;
pub mod release;
pub mod status;
pub mod time;
pub mod version;

pub use catalog::{CraftApp, SUITE};
pub use platform::{Arch, Os, PackageKind, Platform};
pub use release::{Asset, Release};
pub use status::{AppStatus, InstallSource, Installed, Latest};
