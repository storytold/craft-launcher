//! ArtCraft Launcher updating itself.
//!
//! Delegated to [`craft_update::self_update`].

use std::ffi::OsString;

use artcraft_launcher_core::platform::Platform;
use artcraft_launcher_core::protocol::JobPhase;
use artcraft_launcher_core::release::Release;
use artcraft_launcher_core::version::Version;

use crate::http::{Http, Stop};
use crate::paths::Paths;

pub use craft_update::self_update::{FINISH_FLAG, Install, PRETEND_VAR, is_self_updating};

/// The version this launcher counts as (see [`PRETEND_VAR`]).
pub fn current_version() -> Option<Version> {
    craft_update::self_update::current_version(&craft_update::suite::LAUNCHER, env!("CARGO_PKG_VERSION"))
}

/// How this copy is installed, if it's one the launcher knows how to replace.
pub fn current() -> Option<Install> {
    craft_update::self_update::current(&craft_update::suite::LAUNCHER)
}

/// Whether `release` is the version this launcher just updated itself to.
pub fn just_updated_to(paths: &Paths, release: &Release) -> bool {
    craft_update::self_update::just_updated_to(paths, release)
}

/// The error of the last update, if it failed, once.
pub fn take_failure(paths: &Paths) -> Option<String> {
    craft_update::self_update::take_failure(paths)
}

/// Downloads `release` for `platform`, puts it in place and starts the helper that relaunches
/// it once this process has exited. On success the caller must quit promptly. `last` is called
/// once the download is ready and returns when the launcher may replace itself and restart.
#[allow(clippy::too_many_arguments)]
pub fn update(
    install: &Install,
    release: &Release,
    platform: Platform,
    http: &Http,
    paths: &Paths,
    stop: &Stop,
    phase: &mut dyn FnMut(JobPhase),
    last: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    craft_update::self_update::update(
        &craft_update::suite::LAUNCHER,
        install,
        release,
        platform,
        http,
        paths,
        stop,
        phase,
        last,
        Some(env!("CARGO_PKG_VERSION")),
    )
}

/// Finishes an update when this program was started as the update helper.
pub fn finish(args: &[OsString]) -> Option<Result<(), String>> {
    craft_update::self_update::finish(&craft_update::suite::LAUNCHER, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_development_build_does_not_update_itself() {
        assert_eq!(current(), None);
    }
}
