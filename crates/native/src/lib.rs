//! ArtCraft Launcher's desktop backend.
//!
//! [`NativeBackend`] runs every slow thing (HTTP, scanning disks, installers) on worker threads
//! and reports back through [`Event`](artcraft_launcher_core::protocol::Event)s, so the UI never
//! waits on it. Release information comes from a [`source::ReleaseSource`]: GitHub today, the
//! ArtCraft catalog API later.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod backend;
pub mod detect;
pub mod discord;
pub mod feed_throttle;
pub mod http;
pub mod install;
pub mod login;
#[cfg(any(windows, test))]
mod msi_context;
pub mod paths;
pub mod screenshots;
pub mod self_update;
pub mod single_instance;

pub use backend::NativeBackend;
// Shared with every Craft app's installer through craft-update: the launcher's record of what it
// installed, starting programs, the system proxy settings downloads go through, GitHub release
// lookups, Linux menu entries and package-manager listings, and the one-installer-at-a-time queue.
pub use craft_update::{desktop_entry, manifest, now, packages, process, proxy, queue, source};
// Portable zip installs on Windows.
#[cfg(windows)]
pub use craft_update::portable;
