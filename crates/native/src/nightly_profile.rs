//! The one source-owned nightly profile contract. Descriptor declarations are data, not argv.
//!
//! Callers must first verify the descriptor against its exact published release. This table
//! does not replace archive/flavor validation or platform qualification.

use std::path::{Component, Path, PathBuf};

use artcraft_launcher_core::nightly::Artifact;
use artcraft_launcher_core::{Arch, CraftApp, Os, PackageKind, Platform, Release};

use crate::paths::Paths;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Adapter {
    PhotoCraftWindowsV1,
    PhotoCraftLinuxV1,
    PhotoCraftMacV1,
}

struct Capability {
    platform: Platform,
    os: &'static str,
    arch: &'static str,
    format: &'static str,
    suffix: &'static str,
    app: &'static str,
    repository: &'static str,
    package: &'static str,
    profile: &'static str,
    profile_version: u32,
    adapter: Adapter,
}

// These are staged source adapters, not claims of package/runtime qualification.
const CAPABILITIES: [Capability; 3] = [
    Capability {
        platform: Platform { os: Os::Windows, arch: Arch::X64 },
        os: "windows",
        arch: "x86_64",
        format: "portable_zip",
        suffix: "windows-x86_64-nightly.zip",
        app: "photocraft",
        repository: "storytold/photocraft",
        package: "ai.storyteller.photocraft.nightly",
        profile: "nightly",
        profile_version: 1,
        adapter: Adapter::PhotoCraftWindowsV1,
    },
    Capability {
        platform: Platform { os: Os::Linux, arch: Arch::X64 },
        os: "linux",
        arch: "x86_64",
        format: "tar_gz",
        suffix: "linux-x86_64-nightly.tar.gz",
        app: "photocraft",
        repository: "storytold/photocraft",
        package: "ai.storyteller.photocraft.nightly",
        profile: "nightly",
        profile_version: 1,
        adapter: Adapter::PhotoCraftLinuxV1,
    },
    Capability {
        platform: Platform { os: Os::MacOs, arch: Arch::Arm64 },
        os: "macos",
        arch: "aarch64",
        format: "portable_zip",
        suffix: "macos-aarch64-nightly.zip",
        app: "photocraft",
        repository: "storytold/photocraft",
        package: "ai.storyteller.photocraft.nightly",
        profile: "nightly",
        profile_version: 1,
        adapter: Adapter::PhotoCraftMacV1,
    },
];

/// A known row is not a substitute for the caller's verified descriptor or archive checks.
pub(crate) fn adapter(app: &CraftApp, release: &Release, artifact: &Artifact, platform: Platform) -> Result<Adapter, String> {
    let version = release.version.as_ref().ok_or("The nightly has no parsed build version.")?;
    let row = CAPABILITIES.iter().find(|row| {
        app.kind == artcraft_launcher_core::catalog::AppKind::Official
            && app.id == row.app
            && app.repo == row.repository
            && platform == row.platform
            && artifact.target.os == row.os
            && artifact.target.arch == row.arch
            && artifact.target.format == row.format
            && artifact.isolation.package_identity == row.package
            && artifact.isolation.profile_contract == row.profile
            && artifact.isolation.profile_version == row.profile_version
            && artifact.isolation.associations == "none"
            && artifact.asset.name == format!("photocraft-{version}-{}", row.suffix)
    });
    row.map(|row| row.adapter)
        .ok_or_else(|| "This app, target or profile has no separate nightly adapter.".into())
        .and_then(configured_for_new_install)
}

/// Admission for new discovery/install, separate from retained-copy management.
pub(crate) fn admit_new(app: &CraftApp, platform: Platform) -> Result<Adapter, String> {
    for_app(app, platform).and_then(configured_for_new_install)
}

fn configured_for_new_install(adapter: Adapter) -> Result<Adapter, String> {
    match adapter {
        Adapter::PhotoCraftMacV1 => mac_team().map(|_| adapter),
        _ => Ok(adapter),
    }
}

/// Whether new Nightly builds can be discovered and installed on `platform` at all: some app
/// has an admitted adapter here and the platform's signing trust is configured. Retained copies
/// are managed either way.
pub(crate) fn new_installs_available(platform: Platform) -> bool {
    crate::nightly_trust::configured_for(platform.os)
        && artcraft_launcher_core::hosted_catalog::apps().iter().any(|app| app.has_separate_nightly_channel() && admit_new(app, platform).is_ok())
}

/// The Developer ID team a macOS Nightly must be signed by, for admission and every approval.
pub(crate) fn mac_team() -> Result<&'static str, String> {
    let stable = CraftApp::by_id("photocraft").and_then(|app| app.mac_team);
    pinned_mac_team(crate::nightly_trust::MAC_TEAM, stable)
}

/// Ordinary mac_team metadata alone is not Nightly configuration: the source-owned Nightly pin
/// must exist and still name the team stable PhotoCraft's Mac install verifies.
fn pinned_mac_team(pinned: Option<&'static str>, stable: Option<&str>) -> Result<&'static str, String> {
    let team = pinned.ok_or("PhotoCraft Nightly on macOS is not configured; existing copies and data were left unchanged.")?;
    if stable != Some(team) {
        return Err("The macOS Nightly signing team differs from stable PhotoCraft's; existing copies and data were left unchanged.".into());
    }
    Ok(team)
}

/// Retained management is tied to the same source-owned row, not a descriptor supplied command.
pub(crate) fn for_app(app: &CraftApp, platform: Platform) -> Result<Adapter, String> {
    CAPABILITIES
        .iter()
        .find(|row| {
            app.kind == artcraft_launcher_core::catalog::AppKind::Official
                && app.id == row.app
                && app.repo == row.repository
                && platform == row.platform
        })
        .map(|row| row.adapter)
        .ok_or_else(|| "No separate nightly profile is known for this app and platform.".into())
}

/// Check only metadata under the launcher's ownership boundary, without making folders.
/// Missing components are fine; aliases and non-directories inside that boundary are not.
fn validate_tree(boundary: &Path, folder: &Path) -> Result<(), String> {
    if !boundary.is_absolute()
        || !folder.starts_with(boundary)
        || folder.components().any(|part| matches!(part, Component::CurDir | Component::ParentDir))
    {
        return Err("The nightly folder is outside its private namespace.".into());
    }
    let ancestors: Vec<_> = folder.ancestors().collect();
    for ancestor in ancestors.into_iter().rev() {
        match std::fs::symlink_metadata(ancestor) {
            Ok(meta) => {
                #[cfg(windows)]
                let alias = {
                    use std::os::windows::fs::MetadataExt;
                    meta.file_attributes() & 0x400 != 0
                };
                #[cfg(not(windows))]
                let alias = meta.file_type().is_symlink();
                if alias || !meta.is_dir() {
                    return Err("The nightly folder contains an alias or a non-directory component.".into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

/// Inspect a pre-existing file without accepting reparse points or shared file identities.
/// Missing files are fine; their private parents are separately validated top-down.
pub(crate) fn validate_file(file: &Path) -> Result<(), String> {
    let meta = match std::fs::symlink_metadata(file) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    #[cfg(windows)]
    let alias = {
        use std::os::windows::fs::MetadataExt;
        meta.file_attributes() & 0x400 != 0
    };
    #[cfg(not(windows))]
    let alias = meta.file_type().is_symlink();
    if alias || !meta.is_file() {
        return Err("The nightly file is an alias or not a regular file.".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return Err("The nightly file has a shared hardlink identity.".into());
        }
    }
    #[cfg(windows)]
    {
        use winsafe::{HFILE, co};
        let (handle, _) = HFILE::CreateFile(
            file.to_str().ok_or("The nightly file path cannot be represented without loss.")?,
            co::GENERIC::READ,
            Some(co::FILE_SHARE::READ | co::FILE_SHARE::WRITE | co::FILE_SHARE::DELETE),
            None,
            co::DISPOSITION::OPEN_EXISTING,
            co::FILE_ATTRIBUTE::NORMAL,
            Some(co::FILE_FLAG::OPEN_REPARSE_POINT),
            None,
            None,
        )
        .map_err(|error| error.to_string())?;
        if handle.GetFileInformationByHandle().map_err(|error| error.to_string())?.nNumberOfLinks != 1 {
            return Err("The nightly file has a shared hardlink identity.".into());
        }
    }
    Ok(())
}

/// The existing seven-day finished/thirty-day partial policy, only within the validated
/// private downloads cache. A suspicious entry leaves the cache unchanged, never a profile.
/// This is two streaming O(number of cache entries) passes, with constant auxiliary space.
pub(crate) fn clean_downloads(paths: &Paths) -> Result<(), String> {
    validate_paths(paths)?;
    let entries = match std::fs::read_dir(paths.downloads()) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    for entry in entries {
        validate_file(&entry.map_err(|error| error.to_string())?.path())?;
    }
    crate::http::clean_downloads(&paths.downloads(), std::time::Duration::from_secs(30 * 86_400), std::time::Duration::from_secs(7 * 86_400));
    Ok(())
}

pub(crate) fn validate_download(paths: &Paths, asset: &artcraft_launcher_core::Asset) -> Result<(), String> {
    validate_paths(paths)?;
    if !artcraft_launcher_core::release::valid_file_name(&asset.name) {
        return Err("The nightly download has an unsafe filename.".into());
    }
    let destination = crate::install::download_path(paths, asset);
    validate_file(&destination)?;
    validate_file(&crate::http::part_path(&destination, asset.size))
}

pub(crate) fn validate_paths(paths: &Paths) -> Result<(), String> {
    for namespace in [&paths.data, &paths.cache] {
        let pre = namespace.parent().ok_or("The nightly folder has no private parent.")?;
        if namespace.file_name() != Some(std::ffi::OsStr::new("v1")) || pre.file_name() != Some(std::ffi::OsStr::new("pre-release")) {
            return Err("The nightly folder has no versioned private namespace.".into());
        }
        validate_tree(pre.parent().ok_or("The nightly namespace has no owner.")?, namespace)?;
    }
    for folder in [paths.apps(), paths.data.join("profiles")] {
        validate_tree(&paths.data, &folder)?;
    }
    for folder in [paths.downloads(), paths.cache.join("staging"), paths.cache.join("screenshots")] {
        validate_tree(&paths.cache, &folder)?;
    }
    for file in [paths.manifest(), paths.release_cache(), paths.feed_throttle()] {
        validate_file(&file)?;
    }
    Ok(())
}

impl Adapter {
    pub(crate) fn admit_new(self) -> Result<Self, String> {
        configured_for_new_install(self)
    }

    pub(crate) fn kind(self) -> PackageKind {
        match self {
            Self::PhotoCraftLinuxV1 => PackageKind::TarGz,
            Self::PhotoCraftWindowsV1 | Self::PhotoCraftMacV1 => PackageKind::PortableZip,
        }
    }

    pub(crate) fn executable(self) -> &'static Path {
        Path::new(match self {
            Self::PhotoCraftWindowsV1 => "photocraft.exe",
            Self::PhotoCraftLinuxV1 => "photocraft",
            Self::PhotoCraftMacV1 => "PhotoCraft Nightly.app/Contents/MacOS/PhotoCraft",
        })
    }

    /// The row already owns the filename, but bind it again at the install boundary. The
    /// stable asset parser intentionally does not recognize these dedicated nightly flavors.
    pub(crate) fn bind_release(self, release: &Release, asset: &artcraft_launcher_core::Asset) -> Result<(), String> {
        let row = CAPABILITIES.iter().find(|row| row.adapter == self).ok_or("Unknown nightly adapter.")?;
        let version = release.version.as_ref().ok_or("The nightly has no exact version.")?;
        if version.pre.as_str().split('.').next() != Some("nightly")
            || asset.name != format!("photocraft-{version}-{}", row.suffix)
            || artcraft_launcher_core::version::parse(&release.tag).as_ref() != Some(version)
            || asset.url != format!("https://github.com/{}/releases/download/{}/{}", row.repository, release.tag, asset.name)
        {
            return Err("The nightly payload does not match its source-owned target and exact release.".into());
        }
        Ok(())
    }

    pub(crate) fn app(self) -> &'static str {
        match self {
            Self::PhotoCraftWindowsV1 | Self::PhotoCraftLinuxV1 | Self::PhotoCraftMacV1 => "photocraft",
        }
    }

    /// A persistent per-app root, never inside the replaceable program/rollback directory.
    pub(crate) fn profile_root(self, paths: &Paths) -> Result<PathBuf, String> {
        if !paths.data.is_absolute() || paths.data.components().any(|part| matches!(part, Component::CurDir | Component::ParentDir)) {
            return Err("The nightly profile needs an absolute private data folder.".into());
        }
        Ok(paths.data.join("profiles").join(self.app()))
    }

    pub(crate) fn validate_profile(self, paths: &Paths) -> Result<(), String> {
        validate_paths(paths)?;
        #[cfg(unix)]
        crate::nightly_unix::validate_namespaces(paths)?;
        validate_tree(&paths.data, &self.profile_root(paths)?)
    }

    pub(crate) fn validate_record(self, paths: &Paths, record: &crate::manifest::Record) -> Result<(), String> {
        validate_paths(paths)?;
        if record.kind != self.kind()
            || record.needs_root
            || !artcraft_launcher_core::version::parse(&record.version)
                .is_some_and(|version| version.pre.as_str().split('.').next() == Some("nightly"))
        {
            return Err("The saved copy is not a private nightly portable installation.".into());
        }
        let program = paths.apps().join(self.app());
        validate_tree(&paths.data, &program)?;
        #[cfg(windows)]
        {
            crate::portable::validate_record_at(CraftApp::by_id(self.app()).ok_or("Unknown nightly app.")?, record, &program)?;
            let root = record.path.parent().ok_or("The nightly program has no owned parent.")?;
            let relative = record
                .files
                .iter()
                .map(|file| file.strip_prefix(root).map(Path::to_path_buf).map_err(|error| error.to_string()))
                .collect::<Result<Vec<_>, _>>()?;
            self.validate_layout(&relative, &relative)?;
            for file in &record.files {
                validate_file(file)?;
            }
            Ok(())
        }
        #[cfg(not(windows))]
        {
            #[cfg(unix)]
            return crate::nightly_unix::validate_record(self, paths, record);
            #[cfg(not(unix))]
            {
                let _ = program;
                Err("This nightly installation is unavailable on this system.".into())
            }
        }
    }

    /// The first flavor is GUI-only and flat. Textual license/notice material is allowed;
    /// portable mode, a CLI, nested program trees and any extra executable are not.
    #[cfg(any(windows, test))]
    pub(crate) fn validate_layout(self, files: &[PathBuf], entries: &[PathBuf]) -> Result<(), String> {
        let executable = format!("{}.exe", self.app());
        if !files.iter().any(|file| file == Path::new(&executable)) {
            return Err("The nightly ZIP needs its GUI executable at the archive root.".into());
        }
        for entry in entries {
            let text = entry.to_str().ok_or("The nightly ZIP has a non-textual filename.")?;
            let notice = ["README.md", "LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "NOTICE", "ATTRIBUTION.md"].contains(&text)
                || (text.starts_with("OFL-") && text.ends_with(".txt") && text.len() > "OFL-.txt".len());
            if entry.components().count() != 1 || (entry != Path::new(&executable) && !notice) {
                return Err("The nightly ZIP may contain only its root GUI and declared notice files, never portable.txt or another program.".into());
            }
        }
        Ok(())
    }

    /// Literal options are source-owned. Even option-looking filenames follow the delimiter.
    pub(crate) fn launch_arguments(self, paths: &Paths, files: &[String]) -> Result<Vec<String>, String> {
        let root = self.profile_root(paths)?;
        let root = root.to_str().ok_or("The nightly profile path cannot be passed losslessly to the app.")?;
        let selector = match self {
            Self::PhotoCraftWindowsV1 | Self::PhotoCraftLinuxV1 | Self::PhotoCraftMacV1 => "nightly-v1",
        };
        let mut args = vec!["--launch-profile".into(), selector.into(), "--profile-root".into(), root.into(), "--".into()];
        args.extend_from_slice(files);
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use artcraft_launcher_core::nightly::{Download, Isolation, Target};

    /// New Nightly installs are offered where the platform's signing trust is configured and an app
    /// has an admitted adapter: today Windows, Linux and macOS each on its own OS, since each trust
    /// input is compiled only there.
    #[test]
    fn new_installs_are_available_only_where_signing_trust_is_configured() {
        // The reviewed Windows publisher, Linux signer and macOS team pins are configured, each on its own OS.
        assert_eq!(new_installs_available(Platform { os: Os::Windows, arch: Arch::X64 }), cfg!(windows));
        assert_eq!(new_installs_available(Platform { os: Os::Linux, arch: Arch::X64 }), cfg!(target_os = "linux"));
        assert_eq!(new_installs_available(Platform { os: Os::MacOs, arch: Arch::Arm64 }), cfg!(target_os = "macos") && mac_team().is_ok());
        // Without an admitted adapter (no Arm64 Windows or Linux Nightly yet) nothing is offered,
        // whatever trust the OS has.
        for platform in [Platform { os: Os::Windows, arch: Arch::Arm64 }, Platform { os: Os::Linux, arch: Arch::Arm64 }] {
            assert!(admit_new(CraftApp::by_id("photocraft").unwrap(), platform).is_err(), "{platform:?}");
            assert!(!new_installs_available(platform), "{platform:?}");
        }
    }

    fn fixture() -> (&'static CraftApp, Release, Artifact, Platform) {
        let app = CraftApp::by_id("photocraft").unwrap();
        let release = Release {
            tag: "v1.0.0-nightly.20261120.123456.1.gabcdefabcdef".into(),
            version: artcraft_launcher_core::version::parse("v1.0.0-nightly.20261120.123456.1.gabcdefabcdef"),
            prerelease: true,
            ..Default::default()
        };
        let artifact = Artifact {
            signature: None,
            target: Target { os: "windows".into(), arch: "x86_64".into(), format: "portable_zip".into() },
            asset: Download {
                name: format!("photocraft-{}-windows-x86_64-nightly.zip", release.version.as_ref().unwrap()),
                url: "unused: caller verifies the published asset before table lookup".into(),
                size: 1,
                sha256: "a".repeat(64),
            },
            isolation: Isolation {
                package_identity: "ai.storyteller.photocraft.nightly".into(),
                profile_contract: "nightly".into(),
                profile_version: 1,
                associations: "none".into(),
            },
        };
        (app, release, artifact, Platform { os: Os::Windows, arch: Arch::X64 })
    }

    #[test]
    fn known_nightly_profile_cannot_borrow_a_different_target_package_or_contract() {
        let (app, release, good, platform) = fixture();
        assert_eq!(adapter(app, &release, &good, platform), Ok(Adapter::PhotoCraftWindowsV1));
        let mut changes = Vec::new();
        let mut other = good.clone();
        other.target.os = "linux".into();
        changes.push(other);
        let mut other = good.clone();
        other.target.arch = "aarch64".into();
        changes.push(other);
        let mut other = good.clone();
        other.target.format = "msi".into();
        changes.push(other);
        let mut other = good.clone();
        other.isolation.package_identity = "ai.storyteller.photocraft".into();
        changes.push(other);
        let mut other = good.clone();
        other.isolation.profile_contract = "--new-instance".into();
        changes.push(other);
        let mut other = good.clone();
        other.isolation.profile_version += 1;
        changes.push(other);
        let mut other = good.clone();
        other.isolation.associations = "stable".into();
        changes.push(other);
        let mut other = good.clone();
        other.asset.name = "photocraft-windows-x86_64.zip".into();
        changes.push(other);
        for artifact in changes {
            assert!(adapter(app, &release, &artifact, platform).is_err());
        }
        assert!(adapter(CraftApp::by_id("filmcraft").unwrap(), &release, &good, platform).is_err());
        assert!(adapter(app, &release, &good, Platform { os: Os::Linux, arch: Arch::X64 }).is_err());
    }

    #[test]
    fn staged_unix_rows_cannot_borrow_each_others_payload_or_community_identity() {
        let (app, release, mut artifact, _) = fixture();
        for (platform, os, arch, format, suffix, expected) in [
            (Platform { os: Os::Linux, arch: Arch::X64 }, "linux", "x86_64", "tar_gz", "linux-x86_64-nightly.tar.gz", Adapter::PhotoCraftLinuxV1),
            (
                Platform { os: Os::MacOs, arch: Arch::Arm64 },
                "macos",
                "aarch64",
                "portable_zip",
                "macos-aarch64-nightly.zip",
                Adapter::PhotoCraftMacV1,
            ),
        ] {
            artifact.target = Target { os: os.into(), arch: arch.into(), format: format.into() };
            artifact.asset.name = format!("photocraft-{}-{suffix}", release.version.as_ref().unwrap());
            assert_eq!(adapter(app, &release, &artifact, platform), Ok(expected));
            let community = CraftApp { kind: artcraft_launcher_core::catalog::AppKind::Community, ..*app };
            assert!(adapter(&community, &release, &artifact, platform).is_err());
            let asset = artcraft_launcher_core::Asset {
                name: artifact.asset.name.clone(),
                url: format!("https://github.com/{}/releases/download/{}/{}", app.repo, release.tag, artifact.asset.name),
                ..Default::default()
            };
            assert!(expected.bind_release(&release, &asset).is_ok());
            let mut changed = asset.clone();
            changed.name = "photocraft-stable.tar.gz".into();
            assert!(expected.bind_release(&release, &changed).is_err());
            artifact.target.format = "dmg".into();
            assert!(adapter(app, &release, &artifact, platform).is_err());
        }
    }

    #[test]
    fn mac_admission_needs_the_nightly_pin_and_the_stable_signing_team() {
        let stable = CraftApp::by_id("photocraft").unwrap().mac_team;
        assert!(stable.is_some());
        assert!(pinned_mac_team(None, stable).is_err(), "catalog signing metadata alone does not configure the Nightly");
        assert!(pinned_mac_team(Some("OTHERTEAM1"), stable).is_err(), "a pin that drifts from stable's team is refused");
        assert!(pinned_mac_team(stable, None).is_err());
        assert_eq!(pinned_mac_team(stable, stable), Ok(stable.unwrap()));
        assert_eq!(mac_team(), Ok(stable.unwrap()), "the shipped pin is the stable PhotoCraft team");
    }

    #[test]
    fn nightly_launch_options_are_typed_and_file_options_cannot_change_the_profile() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::under(root.path()).for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
        let files = vec!["a file.pcf".into(), "--profile-root".into(), "--launch-profile".into(), "--".into(), "色.pcf".into()];
        let args = Adapter::PhotoCraftWindowsV1.launch_arguments(&paths, &files).unwrap();
        assert_eq!(
            &args[..5],
            &["--launch-profile", "nightly-v1", "--profile-root", paths.data.join("profiles").join("photocraft").to_str().unwrap(), "--"]
        );
        assert_eq!(&args[5..], files);
        assert!(!root.path().join("data").exists(), "building an argv performs no filesystem access");
    }

    #[test]
    fn nightly_profile_is_not_beneath_programs_and_has_no_tag_lifetime() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::under(root.path()).for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
        let profile = Adapter::PhotoCraftWindowsV1.profile_root(&paths).unwrap();
        assert!(!profile.starts_with(paths.apps()));
        assert!(profile.starts_with(&paths.data));
        assert!(Adapter::PhotoCraftWindowsV1.profile_root(&Paths::under(std::path::Path::new("relative"))).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn nightly_state_and_actual_resume_files_cannot_borrow_a_stable_hardlink() {
        let root = tempfile::tempdir().unwrap();
        let stable = Paths::under(&root.path().canonicalize().unwrap());
        let paths = stable.clone().for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
        std::fs::create_dir_all(&paths.data).unwrap();
        std::fs::create_dir_all(paths.downloads()).unwrap();
        let sentinel = stable.data.join("stable-sentinel");
        std::fs::write(&sentinel, b"stable bytes").unwrap();
        let modified = std::fs::metadata(&sentinel).unwrap().modified().unwrap();
        std::fs::hard_link(&sentinel, paths.manifest()).unwrap();
        assert!(validate_paths(&paths).is_err());
        std::fs::remove_file(paths.manifest()).unwrap();
        let asset = artcraft_launcher_core::Asset { name: fixture().2.asset.name, size: 123, ..Default::default() };
        let destination = crate::install::download_path(&paths, &asset);
        let partial = crate::http::part_path(&destination, asset.size);
        std::fs::hard_link(&sentinel, &partial).unwrap();
        assert!(validate_download(&paths, &asset).is_err(), "check the exact size-qualified resume file before HTTP can touch it");
        std::fs::remove_file(&partial).unwrap();
        std::fs::hard_link(&sentinel, &destination).unwrap();
        assert!(validate_download(&paths, &asset).is_err());
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"stable bytes");
        assert_eq!(std::fs::metadata(&sentinel).unwrap().modified().unwrap(), modified);
    }

    #[cfg(unix)]
    #[test]
    fn nightly_namespace_rejects_an_aliased_parent_before_descending_into_v1() {
        let root = tempfile::tempdir().unwrap();
        let stable = Paths::under(&root.path().canonicalize().unwrap());
        let outside = root.path().join("outside");
        std::fs::create_dir_all(outside.join("v1")).unwrap();
        std::fs::create_dir_all(&stable.data).unwrap();
        std::os::unix::fs::symlink(&outside, stable.data.join("pre-release")).unwrap();
        let paths = stable.for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
        assert!(validate_paths(&paths).is_err());
        assert!(!outside.join("v1/installed.json").exists());
    }

    #[test]
    fn nightly_cache_aging_keeps_profiles_and_reuses_the_existing_download_age_policy() {
        use std::time::{Duration, SystemTime};
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::under(&root.path().canonicalize().unwrap()).for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
        std::fs::create_dir_all(paths.downloads()).unwrap();
        let profile = Adapter::PhotoCraftWindowsV1.profile_root(&paths).unwrap();
        std::fs::create_dir_all(&profile).unwrap();
        let sentinel = profile.join("preferences.json");
        std::fs::write(&sentinel, b"user preferences").unwrap();
        let old = paths.downloads().join("old.zip");
        let recent_partial = paths.downloads().join("paused.zip.123.part");
        for file in [&old, &recent_partial] {
            std::fs::write(file, b"download").unwrap();
        }
        std::fs::File::options()
            .write(true)
            .open(&old)
            .unwrap()
            .set_times(std::fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_secs(8 * 86_400)))
            .unwrap();
        let aliased = paths.downloads().join("aliased.zip");
        std::fs::hard_link(&sentinel, &aliased).unwrap();
        assert!(clean_downloads(&paths).is_err(), "a shared cache identity refuses housekeeping before any deletion");
        assert!(old.exists(), "validation must finish before aging any other cache entry");
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"user preferences");
        std::fs::remove_file(aliased).unwrap();
        clean_downloads(&paths).unwrap();
        assert!(!old.exists());
        assert!(recent_partial.exists());
        assert_eq!(std::fs::read(sentinel).unwrap(), b"user preferences");
    }

    #[test]
    fn nightly_zip_flavor_accepts_only_the_flat_gui_and_declared_notices() {
        let adapter = Adapter::PhotoCraftWindowsV1;
        let gui = PathBuf::from("photocraft.exe");
        let good = vec![gui.clone(), "LICENSE-MIT".into(), "OFL-Noto.txt".into()];
        assert!(adapter.validate_layout(&good, &good).is_ok());
        for unwanted in ["portable.txt", "photocraft-cli.exe", "helper.dll", "data/preferences.json", "nested"] {
            let mut entries = good.clone();
            entries.push(unwanted.into());
            assert!(adapter.validate_layout(&good, &entries).is_err(), "{unwanted}");
        }
        assert!(adapter.validate_layout(&[], &good).is_err());
    }
}
