//! ArtCraft Launcher's desktop backend.
//!
//! [`NativeBackend`] runs every slow thing (HTTP, scanning disks, installers) on worker threads
//! and reports back through [`Event`](artcraft_launcher_core::protocol::Event)s, so the UI never
//! waits on it. Release information comes from a [`source::ReleaseSource`]: GitHub today, the
//! ArtCraft catalog API later.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod backend;
pub mod desktop_entry;
pub mod detect;
pub mod feed_throttle;
pub mod http;
pub mod install;
pub mod login;
pub mod manifest;
pub mod packages;
pub mod paths;
#[cfg(windows)]
pub mod portable;
pub mod process;
pub mod proxy;
pub mod queue;
pub mod screenshots;
pub mod self_update;
pub mod single_instance;
pub mod source;

pub use backend::NativeBackend;

/// Unix seconds now.
pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}
