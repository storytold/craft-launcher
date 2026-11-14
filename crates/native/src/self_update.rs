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

/// craft-update's copy of `release`, read from `repo` (`""` for the launcher's own; the download
/// links must be that repository's, craft-launcher#764).
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
    match update_kind(install) {
        craft_update::PackageKind::Msi => PackageKind::Msi,
        craft_update::PackageKind::PortableZip => PackageKind::PortableZip,
        craft_update::PackageKind::Dmg => PackageKind::Dmg,
        craft_update::PackageKind::AppImage => PackageKind::AppImage,
        craft_update::PackageKind::Deb => PackageKind::Deb,
        craft_update::PackageKind::Rpm => PackageKind::Rpm,
        craft_update::PackageKind::ArchPkg => PackageKind::ArchPkg,
        craft_update::PackageKind::Flatpak => PackageKind::Flatpak,
        craft_update::PackageKind::TarGz => PackageKind::TarGz,
    }
}

/// [`kind`] as `craft_update` names it.
fn update_kind(install: &Install) -> craft_update::PackageKind {
    match install {
        Install::MacApp(_) => craft_update::PackageKind::Dmg,
        Install::Msi(_) => craft_update::PackageKind::Msi,
        Install::Portable(_) => craft_update::PackageKind::PortableZip,
        Install::AppImage(_) => craft_update::PackageKind::AppImage,
        Install::Package(kind, _) => *kind,
        Install::Flatpak => craft_update::PackageKind::Flatpak,
        Install::Standalone(_) if cfg!(windows) => craft_update::PackageKind::PortableZip,
        Install::Standalone(_) => craft_update::PackageKind::TarGz,
    }
}

/// Whether `release` has the file that updating `install` on `platform` downloads ([`kind`]),
/// picked as [`update`] picks it.
pub fn installs_from(install: &Install, release: &Release, platform: Platform) -> bool {
    let app = &craft_update::suite::LAUNCHER;
    craft_update::pick_asset(&to_update_release(release, ""), &[app.id], to_update_platform(platform), &[update_kind(install)]).is_some()
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
    /// repository, and is refused as the launcher's own. A plain `+`, as the launcher's own
    /// `release_from_sums` writes it, passes too.
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

        let plus =
            artcraft_launcher_core::release::release_from_sums(app.repo, "v0.9.0+b.1", &sums("artcraft-launcher-0.9.0+b.1-linux-x86_64.AppImage"));
        assert!(plus.assets[0].url.contains("/v0.9.0+b.1/"), "{}", plus.assets[0].url);
        let plus = to_update_release(&plus, app.repo);
        assert!(craft_update::install::check_download_link(app, &plus, &plus.assets[0]).is_ok());
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

    #[test]
    fn an_update_is_installable_only_when_the_release_has_its_file() {
        use std::path::PathBuf;

        use artcraft_launcher_core::release::Asset;
        use artcraft_launcher_core::{Arch, Os};
        // ArtCraft Launcher v0.19.0's files, without the signatures: macOS has only the disk
        // image, and Windows has no x86 build.
        let names = [
            "artcraft-launcher-0.19.0-freebsd-aarch64.tar.gz",
            "artcraft-launcher-0.19.0-freebsd-x86_64.tar.gz",
            "artcraft-launcher-0.19.0-linux-aarch64.AppImage",
            "artcraft-launcher-0.19.0-linux-aarch64.AppImage.zsync",
            "artcraft-launcher-0.19.0-linux-aarch64.deb",
            "artcraft-launcher-0.19.0-linux-aarch64.flatpak",
            "artcraft-launcher-0.19.0-linux-aarch64.oci-manifest.json",
            "artcraft-launcher-0.19.0-linux-aarch64.pkg.tar.zst",
            "artcraft-launcher-0.19.0-linux-aarch64.rpm",
            "artcraft-launcher-0.19.0-linux-aarch64.tar.gz",
            "artcraft-launcher-0.19.0-linux-riscv64.deb",
            "artcraft-launcher-0.19.0-linux-riscv64.rpm",
            "artcraft-launcher-0.19.0-linux-riscv64.tar.gz",
            "artcraft-launcher-0.19.0-linux-x86_64.AppImage",
            "artcraft-launcher-0.19.0-linux-x86_64.AppImage.zsync",
            "artcraft-launcher-0.19.0-linux-x86_64.deb",
            "artcraft-launcher-0.19.0-linux-x86_64.flatpak",
            "artcraft-launcher-0.19.0-linux-x86_64.oci-manifest.json",
            "artcraft-launcher-0.19.0-linux-x86_64.pkg.tar.zst",
            "artcraft-launcher-0.19.0-linux-x86_64.rpm",
            "artcraft-launcher-0.19.0-linux-x86_64.tar.gz",
            "artcraft-launcher-0.19.0-macos-universal.dmg",
            "artcraft-launcher-0.19.0-windows-arm64-portable.zip",
            "artcraft-launcher-0.19.0-windows-arm64.msi",
            "artcraft-launcher-0.19.0-windows-x64-portable.zip",
            "artcraft-launcher-0.19.0-windows-x64.msi",
            "artcraft-launcher-web-0.19.0.zip",
            "SHA256SUMS.txt",
        ];
        let release = Release {
            assets: names.iter().map(|n| Asset { name: (*n).into(), size: 1, url: format!("https://example.invalid/{n}"), sha256: None }).collect(),
            ..Release::default()
        };
        let mac = Platform { os: Os::MacOs, arch: Arch::Arm64 };
        assert!(installs_from(&Install::MacApp(PathBuf::new()), &release, mac));
        if !cfg!(windows) {
            assert!(!installs_from(&Install::Standalone(PathBuf::new()), &release, mac));
            let linux = Platform { os: Os::Linux, arch: Arch::X64 };
            assert!(installs_from(&Install::Standalone(PathBuf::new()), &release, linux));
            assert!(installs_from(&Install::AppImage(PathBuf::new()), &release, linux));
            assert!(installs_from(&Install::Package(craft_update::PackageKind::Deb, PathBuf::new()), &release, linux));
            let riscv = Platform { os: Os::Linux, arch: Arch::Riscv64 };
            assert!(!installs_from(&Install::AppImage(PathBuf::new()), &release, riscv));
        }
        let windows = |arch| Platform { os: Os::Windows, arch };
        assert!(installs_from(&Install::Msi(PathBuf::new()), &release, windows(Arch::X64)));
        assert!(installs_from(&Install::Portable(PathBuf::new()), &release, windows(Arch::X64)));
        assert!(!installs_from(&Install::Msi(PathBuf::new()), &release, windows(Arch::X86)));
        assert!(!installs_from(&Install::Portable(PathBuf::new()), &release, windows(Arch::X86)));
        assert!(!installs_from(&Install::MacApp(PathBuf::new()), &Release::default(), mac));
        // The file is looked for by the update's own rules, which don't take this spelling.
        let name = "artcraft-launcher_0.18.0_x86_64.msi";
        let other = Release {
            assets: vec![Asset { name: name.into(), size: 1, url: format!("https://example.invalid/{name}"), sha256: None }],
            ..Release::default()
        };
        assert!(!installs_from(&Install::Msi(PathBuf::new()), &other, windows(Arch::X64)));
    }

    #[test]
    fn installs_from_looks_for_the_format_the_update_downloads() {
        use std::path::PathBuf;

        use artcraft_launcher_core::{Arch, Os};
        // `update` names the format it looked for when the release has no file for it, so an
        // empty release shows which format craft-update picks for each kind of copy.
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths { data: dir.path().join("data"), cache: dir.path().join("cache") };
        let http = Http::new().unwrap();
        let platform = Platform { os: Os::Linux, arch: Arch::X64 };
        let mut installs = vec![
            Install::MacApp(PathBuf::new()),
            Install::Msi(PathBuf::new()),
            Install::Portable(PathBuf::new()),
            Install::AppImage(PathBuf::new()),
            Install::Flatpak,
            Install::Standalone(PathBuf::new()),
        ];
        for kind in
            [craft_update::PackageKind::Deb, craft_update::PackageKind::Rpm, craft_update::PackageKind::ArchPkg, craft_update::PackageKind::TarGz]
        {
            installs.push(Install::Package(kind, PathBuf::new()));
        }
        for install in installs {
            let error = update(&install, &Release::default(), "", platform, &http, &paths, &Stop::default(), &mut |_| {}, &|| Ok(())).unwrap_err();
            let looked_for = format!(" has no {} for ", update_kind(&install).label());
            assert!(error.contains(&looked_for), "{install:?}: {error}");
        }
    }
}
