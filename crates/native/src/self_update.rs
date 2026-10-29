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

fn to_update_paths(paths: &Paths) -> craft_update::Paths {
    craft_update::Paths { data: paths.data.clone(), cache: paths.cache.clone() }
}

/// `release` as read from `repo` (`""` for the launcher's own; the download links must be that
/// repository's, craft-launcher#764). The caller's repo wins over the one the release carries.
fn with_repo(release: &Release, repo: &str) -> Release {
    Release { repo: repo.to_string(), ..release.clone() }
}

/// Whether `release` is the version this launcher just updated itself to.
pub fn just_updated_to(paths: &Paths, release: &Release) -> bool {
    craft_update::self_update::just_updated_to(&to_update_paths(paths), release)
}

/// The error of the last update, if it failed, once.
pub fn take_failure(paths: &Paths) -> Option<String> {
    craft_update::self_update::take_failure(&to_update_paths(paths))
}

/// Downloads `release`, read from `repo` (the launcher's, or the one the feed renamed it to), for
/// `platform`, puts it in place and starts the helper that relaunches it once this process has
/// exited. On success the caller must quit promptly. `last` is called once the download is ready
/// and returns when the launcher may replace itself and restart.
#[allow(clippy::too_many_arguments)]
pub fn update(
    install: &Install,
    release: &Release,
    repo: &str,
    platform: Platform,
    _http: &Http,
    paths: &Paths,
    stop: &Stop,
    phase: &mut dyn FnMut(JobPhase),
    last: &dyn Fn() -> Result<(), String>,
) -> Result<(), String> {
    let release = with_repo(release, repo);
    // The download goes to github.com's release CDN, never the API, so no token is needed.
    let update_http = craft_update::Http::new(crate::http::USER_AGENT)?;
    craft_update::self_update::update(
        &craft_update::suite::LAUNCHER,
        install,
        &release,
        platform,
        &update_http,
        &to_update_paths(paths),
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

    /// #764: craft-update downloads only links in the repository the release was read from. A
    /// release the feed renamed the launcher's repository to passes once it carries that
    /// repository, and is refused as the launcher's own. A `+` in the tag passes written either
    /// way: `%2B`, as `release_from_sums` writes it, or plain, as an older launcher saved it.
    #[test]
    fn the_update_checks_links_against_the_repo_the_release_came_from() {
        let app = &craft_update::suite::LAUNCHER;
        let sums = |name: &str| [("a".repeat(64), name.to_string())];
        let renamed = artcraft_launcher_core::release::release_from_sums(
            "storytold/artcraft-launcher-next",
            "v0.9.0",
            &sums("artcraft-launcher-0.9.0-linux-x86_64.AppImage"),
        );
        let carried = with_repo(&renamed, "storytold/artcraft-launcher-next");
        assert!(craft_update::install::check_download_link(app, &carried, &carried.assets[0]).is_ok());
        let own = with_repo(&renamed, "");
        assert!(craft_update::install::check_download_link(app, &own, &own.assets[0]).is_err());

        let plus =
            artcraft_launcher_core::release::release_from_sums(app.repo, "v0.9.0+b.1", &sums("artcraft-launcher-0.9.0+b.1-linux-x86_64.AppImage"));
        assert!(plus.assets[0].url.contains("/v0.9.0%2Bb.1/"), "{}", plus.assets[0].url);
        let mut plus = with_repo(&plus, app.repo);
        assert!(craft_update::install::check_download_link(app, &plus, &plus.assets[0]).is_ok());
        plus.assets[0].url = plus.assets[0].url.replace("%2B", "+");
        assert!(craft_update::install::check_download_link(app, &plus, &plus.assets[0]).is_ok());
    }

    #[test]
    fn a_development_build_does_not_update_itself() {
        assert_eq!(current(), None);
    }
}
