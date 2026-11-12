//! ArtCraft Launcher updating itself.
//!
//! Delegated to [`craft_update::self_update`].

use std::ffi::OsString;

use artcraft_launcher_core::PackageKind;
use artcraft_launcher_core::platform::Platform;
use artcraft_launcher_core::protocol::JobPhase;
use artcraft_launcher_core::release::Release;
use artcraft_launcher_core::version::Version;

use crate::http::{Http, Stop};
use crate::paths::Paths;

pub use craft_update::self_update::{FINISH_FLAG, Install, PRETEND_VAR};

/// The version this launcher counts as (see [`PRETEND_VAR`]).
pub fn current_version() -> Option<Version> {
    craft_update::self_update::current_version(&craft_update::suite::LAUNCHER, env!("CARGO_PKG_VERSION"))
}

/// How this copy is installed, if it's one the launcher knows how to replace.
pub fn current() -> Option<Install> {
    craft_update::self_update::current(&craft_update::suite::LAUNCHER)
}

/// Whether this copy can update itself: [`current`] finds it and, on Windows, it is signed.
pub fn is_self_updating() -> bool {
    craft_update::self_update::is_self_updating(&craft_update::suite::LAUNCHER)
}

/// Whether `release`, read from `repo`, has a file the update can install this copy from: a file
/// for `platform` (the one passed to [`update`]) in the format this copy was installed from,
/// picked as [`update`] picks it. A release with no files fails, since [`update`] picks its file
/// from this release. A copy that [`current`] doesn't find passes; [`is_self_updating`] answers
/// for it.
pub fn release_installs(release: &Release, repo: &str, platform: Option<Platform>) -> bool {
    installs_from(current().as_ref(), platform, release, repo)
}

/// [`release_installs`] for a known installed copy and platform.
fn installs_from(install: Option<&Install>, platform: Option<Platform>, release: &Release, repo: &str) -> bool {
    if release.assets.is_empty() {
        return false;
    }
    let (Some(install), Some(platform)) = (install, platform) else { return true };
    let app = &craft_update::suite::LAUNCHER;
    let update_release = to_update_release(release, repo);
    craft_update::pick_asset(&update_release, &[app.id], to_update_platform(platform), &[to_update_kind(kind(install))]).is_some()
}

fn to_update_paths(paths: &Paths) -> craft_update::Paths {
    craft_update::Paths { data: paths.data.clone(), cache: paths.cache.clone() }
}

/// craft-update's copy of `release`, read from `repo` (`""` for the launcher's own). craft-update
/// downloads only assets whose links belong to that repository.
fn to_update_release(release: &Release, repo: &str) -> craft_update::Release {
    craft_update::Release {
        repo: repo.to_string(),
        tag: release.tag.clone(),
        version: release.version.clone(),
        name: release.name.clone(),
        published_at: release.published_at,
        html_url: release.html_url.clone(),
        notes: release.notes.clone(),
        prerelease: release.prerelease,
        assets: release
            .assets
            .iter()
            .map(|a| craft_update::Asset { name: a.name.clone(), size: a.size, url: a.url.clone(), sha256: a.sha256.clone() })
            .collect(),
    }
}

fn to_update_platform(platform: Platform) -> craft_update::Platform {
    craft_update::Platform {
        os: match platform.os {
            artcraft_launcher_core::Os::Windows => craft_update::Os::Windows,
            artcraft_launcher_core::Os::MacOs => craft_update::Os::MacOs,
            artcraft_launcher_core::Os::Linux => craft_update::Os::Linux,
            artcraft_launcher_core::Os::FreeBsd => craft_update::Os::FreeBsd,
        },
        arch: match platform.arch {
            artcraft_launcher_core::Arch::X64 => craft_update::Arch::X64,
            artcraft_launcher_core::Arch::Arm64 => craft_update::Arch::Arm64,
            artcraft_launcher_core::Arch::X86 => craft_update::Arch::X86,
            artcraft_launcher_core::Arch::Riscv64 => craft_update::Arch::Riscv64,
        },
    }
}

fn to_update_kind(kind: PackageKind) -> craft_update::PackageKind {
    match kind {
        PackageKind::Msi => craft_update::PackageKind::Msi,
        PackageKind::PortableZip => craft_update::PackageKind::PortableZip,
        PackageKind::Dmg => craft_update::PackageKind::Dmg,
        PackageKind::AppImage => craft_update::PackageKind::AppImage,
        PackageKind::Deb => craft_update::PackageKind::Deb,
        PackageKind::Rpm => craft_update::PackageKind::Rpm,
        PackageKind::ArchPkg => craft_update::PackageKind::ArchPkg,
        PackageKind::Flatpak => craft_update::PackageKind::Flatpak,
        PackageKind::TarGz => craft_update::PackageKind::TarGz,
    }
}

/// Whether `release` is the version this launcher just updated itself to.
pub fn just_updated_to(paths: &Paths, release: &Release) -> bool {
    let update_paths = to_update_paths(paths);
    let update_release = to_update_release(release, "");
    craft_update::self_update::just_updated_to(&update_paths, &update_release)
}

/// The error of the last update, if it failed, once.
pub fn take_failure(paths: &Paths) -> Option<String> {
    let update_paths = to_update_paths(paths);
    craft_update::self_update::take_failure(&update_paths)
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
    let update_release = to_update_release(release, repo);
    let update_platform = to_update_platform(platform);
    let update_paths = to_update_paths(paths);
    let update_http = craft_update::Http::new("ArtCraft-Launcher/SelfUpdate")?;
    let mut update_phase = |p: craft_update::JobPhase| {
        phase(match p {
            craft_update::JobPhase::Downloading { done, total } => JobPhase::Downloading { done, total },
            craft_update::JobPhase::Reconnecting { done, total } => JobPhase::Reconnecting { done, total },
            craft_update::JobPhase::Waiting => JobPhase::Waiting,
            craft_update::JobPhase::Verifying => JobPhase::Verifying,
            craft_update::JobPhase::Installing => JobPhase::Installing,
            craft_update::JobPhase::Removing => JobPhase::Removing,
        });
    };
    craft_update::self_update::update(
        &craft_update::suite::LAUNCHER,
        install,
        &update_release,
        update_platform,
        &update_http,
        &update_paths,
        stop,
        &mut update_phase,
        last,
        Some(env!("CARGO_PKG_VERSION")),
    )
}

/// The package format updating `install` downloads: the one it was installed from. Mirrors the
/// private `Install::kind` in `craft_update::self_update`, which picks the update's asset by it.
pub fn kind(install: &Install) -> PackageKind {
    match install {
        Install::MacApp(_) => PackageKind::Dmg,
        Install::Msi(_) => PackageKind::Msi,
        Install::Portable(_) => PackageKind::PortableZip,
        Install::AppImage(_) => PackageKind::AppImage,
        Install::Package(kind, _) => match kind {
            craft_update::PackageKind::Msi => PackageKind::Msi,
            craft_update::PackageKind::PortableZip => PackageKind::PortableZip,
            craft_update::PackageKind::Dmg => PackageKind::Dmg,
            craft_update::PackageKind::AppImage => PackageKind::AppImage,
            craft_update::PackageKind::Deb => PackageKind::Deb,
            craft_update::PackageKind::Rpm => PackageKind::Rpm,
            craft_update::PackageKind::ArchPkg => PackageKind::ArchPkg,
            craft_update::PackageKind::Flatpak => PackageKind::Flatpak,
            craft_update::PackageKind::TarGz => PackageKind::TarGz,
        },
        Install::Flatpak => PackageKind::Flatpak,
        Install::Standalone(_) if cfg!(windows) => PackageKind::PortableZip,
        Install::Standalone(_) => PackageKind::TarGz,
    }
}

/// Finishes an update when this program was started as the update helper.
pub fn finish(args: &[OsString]) -> Option<Result<(), String>> {
    craft_update::self_update::finish(&craft_update::suite::LAUNCHER, args)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// craft-update downloads only links in the repository the release was read from: a release
    /// read from the repository the feed renamed the launcher's to passes when it carries that
    /// repository, and is refused as the launcher's own.
    #[test]
    fn the_update_checks_links_against_the_repo_the_release_came_from() {
        let app = &craft_update::suite::LAUNCHER;
        let sums = |name: &str| [("a".repeat(64), name.to_string())];
        let renamed = artcraft_launcher_core::release::release_from_sums(
            "storytold/artcraft-launcher-next",
            "v0.9.0",
            &sums("artcraft-launcher-0.9.0-linux-x86_64.AppImage"),
        );
        let carried = to_update_release(&renamed, "storytold/artcraft-launcher-next");
        assert!(craft_update::install::check_download_link(app, &carried, &carried.assets[0]).is_ok());
        let own = to_update_release(&renamed, "");
        assert!(craft_update::install::check_download_link(app, &own, &own.assets[0]).is_err());

        let launcher = artcraft_launcher_core::release::release_from_sums(app.repo, "v0.9.0", &sums("artcraft-launcher-0.9.0-linux-x86_64.AppImage"));
        let launcher = to_update_release(&launcher, app.repo);
        assert!(craft_update::install::check_download_link(app, &launcher, &launcher.assets[0]).is_ok());
    }

    /// A release whose files were all dropped (from another tag, or links outside the repository)
    /// has nothing for the update to download, so the launcher offers the download instead.
    #[test]
    fn a_release_without_files_does_not_install() {
        let app = &craft_update::suite::LAUNCHER;
        let sums = [("a".repeat(64), "artcraft-launcher-0.9.0-linux-x86_64.AppImage".to_string())];
        let mut release = artcraft_launcher_core::release::release_from_sums(app.repo, "v0.9.0", &sums);
        assert!(release_installs(&release, app.repo, Platform::current()));
        release.assets.clear();
        assert!(!release_installs(&release, app.repo, Platform::current()));
    }

    /// The update downloads the file for this platform in the format the copy was installed from,
    /// so a release without that file offers the download instead: an AppImage-only release for
    /// an MSI copy, an MSI for another architecture, or an MSI for a portable copy.
    #[test]
    fn a_release_without_the_installed_format_for_this_platform_does_not_install() {
        use artcraft_launcher_core::platform::{Arch, Os};
        use std::path::PathBuf;
        let app = &craft_update::suite::LAUNCHER;
        let release = |names: &[&str]| {
            let sums: Vec<(String, String)> = names.iter().map(|n| ("a".repeat(64), (*n).to_string())).collect();
            artcraft_launcher_core::release::release_from_sums(app.repo, "v0.9.0", &sums)
        };
        let windows_x64 = Some(Platform { os: Os::Windows, arch: Arch::X64 });
        let msi = Install::Msi(PathBuf::new());
        let portable = Install::Portable(PathBuf::new());

        let with_msi = release(&["artcraft-launcher-0.9.0-windows-x64.msi"]);
        assert!(installs_from(Some(&msi), windows_x64, &with_msi, app.repo));
        let appimage_only = release(&["artcraft-launcher-0.9.0-linux-x86_64.AppImage"]);
        assert!(!installs_from(Some(&msi), windows_x64, &appimage_only, app.repo));
        let other_arch = release(&["artcraft-launcher-0.9.0-windows-arm64.msi"]);
        assert!(!installs_from(Some(&msi), windows_x64, &other_arch, app.repo));
        assert!(installs_from(Some(&msi), Some(Platform { os: Os::Windows, arch: Arch::Arm64 }), &other_arch, app.repo));
        assert!(!installs_from(Some(&portable), windows_x64, &with_msi, app.repo));

        let linux_x64 = Some(Platform { os: Os::Linux, arch: Arch::X64 });
        let deb = Install::Package(craft_update::PackageKind::Deb, PathBuf::new());
        assert!(installs_from(Some(&Install::AppImage(PathBuf::new())), linux_x64, &appimage_only, app.repo));
        assert!(!installs_from(Some(&deb), linux_x64, &appimage_only, app.repo));
    }

    #[test]
    fn a_development_build_does_not_update_itself() {
        assert_eq!(current(), None);
    }

    #[test]
    fn an_update_downloads_the_format_the_copy_was_installed_from() {
        use std::path::PathBuf;
        assert_eq!(kind(&Install::Msi(PathBuf::new())), PackageKind::Msi);
        assert_eq!(kind(&Install::Portable(PathBuf::new())), PackageKind::PortableZip);
        assert_eq!(kind(&Install::MacApp(PathBuf::new())), PackageKind::Dmg);
        assert_eq!(kind(&Install::AppImage(PathBuf::new())), PackageKind::AppImage);
        assert_eq!(kind(&Install::Package(craft_update::PackageKind::Rpm, PathBuf::new())), PackageKind::Rpm);
        assert_eq!(kind(&Install::Flatpak), PackageKind::Flatpak);
        let standalone = if cfg!(windows) { PackageKind::PortableZip } else { PackageKind::TarGz };
        assert_eq!(kind(&Install::Standalone(PathBuf::new())), standalone);
    }
}
