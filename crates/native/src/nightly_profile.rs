//! The one source-owned nightly profile contract. Descriptor declarations are data, not argv.
//!
//! Callers must first verify the descriptor against its exact published release. This table
//! does not replace archive/flavor validation or platform qualification.

use std::path::{Component, Path, PathBuf};

use artcraft_launcher_core::nightly::Artifact;
use artcraft_launcher_core::{Arch, CraftApp, Os, PackageKind, Platform, Release};

use crate::paths::Paths;

/// One source-owned Nightly row: an app on one platform. A row names its payload exactly; it
/// is not a claim of package or runtime qualification.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Capability {
    platform: Platform,
    os: &'static str,
    arch: &'static str,
    format: &'static str,
    suffix: &'static str,
    app: &'static str,
    /// The display name its stable copy and data use (`CADCraft`).
    display: &'static str,
    repository: &'static str,
    package: &'static str,
    profile: &'static str,
    profile_version: u32,
    /// What the Launcher opens, relative to the program folder.
    entrypoint: &'static str,
    /// The program a Linux launcher script starts, beside it (`<app>-bin`), when there is one.
    binary: Option<&'static str>,
    /// The Nightly badge the payload must carry, when the row declares one.
    badge: Option<&'static str>,
    /// macOS: the `.app` folder at the payload's root and its Nightly bundle identifier.
    bundle: Option<(&'static str, &'static str)>,
}

/// An app's Nightly on one platform: a reference to its source-owned row.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct Adapter(&'static Capability);

/// `<Display><Os>V1`: the adapter's whole persisted identity. `nightly_lookup::key` puts this into
/// the Nightly scheduling key it saves on disk, and PhotoCraft's keys have read `PhotoCraftWindowsV1`,
/// `PhotoCraftLinuxV1` and `PhotoCraftMacV1` since before this table existed. Written by hand, not
/// derived: a derived Debug would print the row's fields instead, so every existing user's saved
/// Nightly scheduling state would be silently dropped by an upgrade.
impl std::fmt::Debug for Adapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let os = match self.0.platform.os {
            Os::Windows => "Windows",
            Os::Linux => "Linux",
            Os::MacOs => "Mac",
            Os::FreeBsd => "FreeBSD",
        };
        write!(formatter, "{}{os}V1", self.display())
    }
}

// These are staged source adapters, not claims of package/runtime qualification. The rows are
// copied by hand from storytold/craft-common-actions `nightly-apps.json` (craft-nightly-registry/v1)
// at commit 2b4eb3d9 (registry-targets, reviewed by launcher-nightly-platforms), checked in
// unchanged as `crates/native/tests/fixtures/nightly-apps.json`, so review reads the exact table
// that was copied. Nightly admission inputs are source-owned and reviewed (see `nightly_trust.rs`
// and docs/nightly-signing.md), never fetched or generated at build time: a registry change alone
// never admits anything, and it shows up here as a failing fixture test, not as a silent download.
//
// One row per app and platform. What a row carries beyond the registry is the platform's convention,
// not per-app data, so `row!` builds it from the app id and display name with `concat!` (still
// compile-time constants, still visible in this file): the asset suffix, the Nightly package
// identity, the entry point, the extra `<app>-bin` program a Linux launcher script starts, and the
// badge and `.app` bundle a macOS payload must carry. PhotoCraft starts its Linux GUI directly, so
// its Linux row is `linux-direct`. No Windows or macOS row exists for an app the registry does not
// name on that platform.
macro_rules! row {
    // A Linux Nightly: the `<app>` launcher script with `<app>-bin` beside it.
    (linux $app:literal, $display:literal, $repository:literal) => {
        Capability {
            platform: Platform { os: Os::Linux, arch: Arch::X64 },
            os: "linux",
            arch: "x86_64",
            format: "tar_gz",
            suffix: "linux-x86_64-nightly.tar.gz",
            app: $app,
            display: $display,
            repository: $repository,
            package: concat!("ai.storyteller.", $app, ".nightly"),
            profile: "nightly",
            profile_version: 1,
            entrypoint: $app,
            binary: Some(concat!($app, "-bin")),
            badge: Some(concat!($app, "-nightly.png")),
            bundle: None,
        }
    };
    // A Linux Nightly whose entry point is the GUI program itself, with no launcher script.
    (linux $app:literal, $display:literal, $repository:literal, none) => {
        Capability {
            platform: Platform { os: Os::Linux, arch: Arch::X64 },
            os: "linux",
            arch: "x86_64",
            format: "tar_gz",
            suffix: "linux-x86_64-nightly.tar.gz",
            app: $app,
            display: $display,
            repository: $repository,
            package: concat!("ai.storyteller.", $app, ".nightly"),
            profile: "nightly",
            profile_version: 1,
            entrypoint: $app,
            binary: None,
            badge: Some(concat!($app, "-nightly.png")),
            bundle: None,
        }
    };
    // A Windows Nightly: the flat portable ZIP with `<app>.exe` at its root.
    (windows $app:literal, $display:literal, $repository:literal) => {
        Capability {
            platform: Platform { os: Os::Windows, arch: Arch::X64 },
            os: "windows",
            arch: "x86_64",
            format: "portable_zip",
            suffix: "windows-x86_64-nightly.zip",
            app: $app,
            display: $display,
            repository: $repository,
            package: concat!("ai.storyteller.", $app, ".nightly"),
            profile: "nightly",
            profile_version: 1,
            entrypoint: concat!($app, ".exe"),
            binary: None,
            badge: None,
            bundle: None,
        }
    };
    // A macOS Nightly: `<Display> Nightly.app` with its own Nightly identifier and badge.
    (mac $app:literal, $display:literal, $repository:literal) => {
        Capability {
            platform: Platform { os: Os::MacOs, arch: Arch::Arm64 },
            os: "macos",
            arch: "aarch64",
            format: "portable_zip",
            suffix: "macos-aarch64-nightly.zip",
            app: $app,
            display: $display,
            repository: $repository,
            package: concat!("ai.storyteller.", $app, ".nightly"),
            profile: "nightly",
            profile_version: 1,
            entrypoint: concat!($display, " Nightly.app/Contents/MacOS/", $display),
            binary: None,
            badge: Some(concat!($display, " Nightly.app/Contents/Resources/", $app, "-nightly.icns")),
            bundle: Some((concat!($display, " Nightly.app"), concat!("ai.storyteller.", $app, ".nightly"))),
        }
    };
}

const CAPABILITIES: &[Capability] = &[
    row!(windows "photocraft", "PhotoCraft", "storytold/photocraft"),
    row!(linux "photocraft", "PhotoCraft", "storytold/photocraft", none),
    row!(mac "photocraft", "PhotoCraft", "storytold/photocraft"),
    row!(windows "anicraft", "AniCraft", "storytold/anicraft"),
    row!(linux "anicraft", "AniCraft", "storytold/anicraft"),
    row!(mac "anicraft", "AniCraft", "storytold/anicraft"),
    row!(windows "cadcraft", "CADCraft", "storytold/cadcraft"),
    row!(linux "cadcraft", "CADCraft", "storytold/cadcraft"),
    row!(mac "cadcraft", "CADCraft", "storytold/cadcraft"),
    row!(windows "deckcraft", "DeckCraft", "storytold/deckcraft"),
    row!(linux "deckcraft", "DeckCraft", "storytold/deckcraft"),
    row!(mac "deckcraft", "DeckCraft", "storytold/deckcraft"),
    row!(windows "designcraft", "DesignCraft", "storytold/designcraft"),
    row!(linux "designcraft", "DesignCraft", "storytold/designcraft"),
    row!(mac "designcraft", "DesignCraft", "storytold/designcraft"),
    row!(windows "effectcraft", "EffectCraft", "storytold/effectcraft"),
    row!(linux "effectcraft", "EffectCraft", "storytold/effectcraft"),
    row!(mac "effectcraft", "EffectCraft", "storytold/effectcraft"),
    row!(windows "figcraft", "FigCraft", "storytold/figcraft"),
    row!(linux "figcraft", "FigCraft", "storytold/figcraft"),
    row!(mac "figcraft", "FigCraft", "storytold/figcraft"),
    row!(windows "filmcraft", "FilmCraft", "storytold/filmcraft"),
    row!(linux "filmcraft", "FilmCraft", "storytold/filmcraft"),
    row!(mac "filmcraft", "FilmCraft", "storytold/filmcraft"),
    row!(windows "gridcraft", "GridCraft", "storytold/gridcraft"),
    row!(linux "gridcraft", "GridCraft", "storytold/gridcraft"),
    row!(mac "gridcraft", "GridCraft", "storytold/gridcraft"),
    row!(windows "lightcraft", "LightCraft", "storytold/lightcraft"),
    row!(linux "lightcraft", "LightCraft", "storytold/lightcraft"),
    row!(mac "lightcraft", "LightCraft", "storytold/lightcraft"),
    row!(windows "materialcraft", "MaterialCraft", "storytold/materialcraft"),
    row!(linux "materialcraft", "MaterialCraft", "storytold/materialcraft"),
    row!(mac "materialcraft", "MaterialCraft", "storytold/materialcraft"),
    row!(windows "soundcraft", "SoundCraft", "storytold/soundcraft"),
    row!(linux "soundcraft", "SoundCraft", "storytold/soundcraft"),
    row!(mac "soundcraft", "SoundCraft", "storytold/soundcraft"),
    row!(windows "texturecraft", "TextureCraft", "storytold/texturecraft"),
    row!(linux "texturecraft", "TextureCraft", "storytold/texturecraft"),
    row!(mac "texturecraft", "TextureCraft", "storytold/texturecraft"),
    row!(windows "vectorcraft", "VectorCraft", "storytold/vectorcraft"),
    row!(linux "vectorcraft", "VectorCraft", "storytold/vectorcraft"),
    row!(mac "vectorcraft", "VectorCraft", "storytold/vectorcraft"),
    row!(windows "wordcraft", "WordCraft", "storytold/wordcraft"),
    row!(linux "wordcraft", "WordCraft", "storytold/wordcraft"),
    row!(mac "wordcraft", "WordCraft", "storytold/wordcraft"),
];

/// PhotoCraft's three rows by name, for the tests that pin the identities the Launcher persisted
/// before this table existed. Test-only: production code reaches every row through `for_app`.
#[cfg(test)]
#[allow(non_upper_case_globals)]
impl Adapter {
    pub(crate) const PhotoCraftWindowsV1: Self = Self(&CAPABILITIES[0]);
    pub(crate) const PhotoCraftLinuxV1: Self = Self(&CAPABILITIES[1]);
    pub(crate) const PhotoCraftMacV1: Self = Self(&CAPABILITIES[2]);
}

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
            && artifact.asset.name == format!("{}-{version}-{}", row.app, row.suffix)
    });
    row.map(Adapter).ok_or_else(|| "This app, target or profile has no separate nightly adapter.".into()).and_then(configured_for_new_install)
}

/// Admission for new discovery/install, separate from retained-copy management.
pub(crate) fn admit_new(app: &CraftApp, platform: Platform) -> Result<Adapter, String> {
    for_app(app, platform).and_then(configured_for_new_install)
}

fn configured_for_new_install(adapter: Adapter) -> Result<Adapter, String> {
    match adapter.os() {
        Os::MacOs => mac_team(adapter.app()).map(|_| adapter),
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

/// The Developer ID team `app`'s macOS Nightly must be signed by, for admission and every
/// approval: the source-owned Nightly pin, while it still names the team the stable app's Mac
/// install verifies.
pub(crate) fn mac_team(app: &str) -> Result<&'static str, String> {
    let stable = CraftApp::by_id(app).and_then(|app| app.mac_team);
    pinned_mac_team(crate::nightly_trust::MAC_TEAM, stable)
}

/// Ordinary mac_team metadata alone is not Nightly configuration: the source-owned Nightly pin
/// must exist and still name the team the stable app's Mac install verifies.
fn pinned_mac_team(pinned: Option<&'static str>, stable: Option<&str>) -> Result<&'static str, String> {
    let team = pinned.ok_or("Nightly builds on macOS are not configured; existing copies and data were left unchanged.")?;
    if stable != Some(team) {
        return Err("The macOS Nightly signing team differs from the stable app's; existing copies and data were left unchanged.".into());
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
        .map(Adapter)
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

    pub(crate) fn os(self) -> Os {
        self.0.platform.os
    }

    pub(crate) fn kind(self) -> PackageKind {
        match self.0.format {
            "tar_gz" => PackageKind::TarGz,
            _ => PackageKind::PortableZip,
        }
    }

    /// What the Launcher opens, relative to the program folder.
    pub(crate) fn executable(self) -> &'static Path {
        Path::new(self.0.entrypoint)
    }

    /// The program a Linux launcher script starts, beside it.
    #[cfg(any(unix, test))]
    pub(crate) fn binary(self) -> Option<&'static Path> {
        self.0.binary.map(Path::new)
    }

    /// The Nightly badge the payload must carry, when the row declares one.
    #[cfg(any(unix, test))]
    pub(crate) fn badge(self) -> Option<&'static Path> {
        self.0.badge.map(Path::new)
    }

    /// macOS: the `.app` folder at the payload's root and its Nightly bundle identifier.
    #[cfg(any(unix, test))]
    pub(crate) fn bundle(self) -> Option<(&'static str, &'static str)> {
        self.0.bundle
    }

    /// The display name the stable app's copy and data use.
    pub(crate) fn display(self) -> &'static str {
        self.0.display
    }

    /// The row already owns the filename, but bind it again at the install boundary. The
    /// stable asset parser intentionally does not recognize these dedicated nightly flavors.
    pub(crate) fn bind_release(self, release: &Release, asset: &artcraft_launcher_core::Asset) -> Result<(), String> {
        let row = self.0;
        let version = release.version.as_ref().ok_or("The nightly has no exact version.")?;
        if version.pre.as_str().split('.').next() != Some("nightly")
            || asset.name != format!("{}-{version}-{}", row.app, row.suffix)
            || artcraft_launcher_core::version::parse(&release.tag).as_ref() != Some(version)
            || asset.url != format!("https://github.com/{}/releases/download/{}/{}", row.repository, release.tag, asset.name)
        {
            return Err("The nightly payload does not match its source-owned target and exact release.".into());
        }
        Ok(())
    }

    pub(crate) fn app(self) -> &'static str {
        self.0.app
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
        crate::nightly_unix::validate_namespaces(self, paths)?;
        // This app's own stable data and installation, never another app's.
        #[cfg(windows)]
        crate::nightly_windows_fence::validate_windows_namespaces(self, paths)?;
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
        let executable = self.executable();
        if !files.iter().any(|file| file == executable) {
            return Err("The nightly ZIP needs its GUI executable at the archive root.".into());
        }
        for entry in entries {
            let text = entry.to_str().ok_or("The nightly ZIP has a non-textual filename.")?;
            let notice = ["README.md", "LICENSE", "LICENSE-MIT", "LICENSE-APACHE", "NOTICE", "ATTRIBUTION.md"].contains(&text)
                || (text.starts_with("OFL-") && text.ends_with(".txt") && text.len() > "OFL-.txt".len());
            if entry.components().count() != 1 || (entry != executable && !notice) {
                return Err("The nightly ZIP may contain only its root GUI and declared notice files, never portable.txt or another program.".into());
            }
        }
        Ok(())
    }

    /// Literal options are source-owned. Even option-looking filenames follow the delimiter.
    pub(crate) fn launch_arguments(self, paths: &Paths, files: &[String]) -> Result<Vec<String>, String> {
        let root = self.profile_root(paths)?;
        let root = root.to_str().ok_or("The nightly profile path cannot be passed losslessly to the app.")?;
        // Every row is the version-1 Nightly profile contract (`nightly`, 1).
        let selector = "nightly-v1";
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
        assert_eq!(
            new_installs_available(Platform { os: Os::MacOs, arch: Arch::Arm64 }),
            cfg!(target_os = "macos") && mac_team("photocraft").is_ok()
        );
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
        assert_eq!(mac_team("photocraft"), Ok(stable.unwrap()), "the shipped pin is the stable PhotoCraft team");
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

    /// The exact table `CAPABILITIES` was copied from: `storytold/craft-common-actions`
    /// `nightly-apps.json` at commit `2b4eb3d9`, byte for byte. A fixture
    /// for review, not a build input: nothing at runtime reads it, so a registry change fails this
    /// test instead of admitting anything by itself.
    const REGISTRY: &str = include_str!("../tests/fixtures/nightly-apps.json");

    /// One registry target, flattened to the values a `Capability` row copies.
    #[derive(Debug)]
    struct RegistryTarget {
        app: String,
        display: String,
        repository: String,
        os: String,
        arch: String,
        format: String,
        suffix: String,
        package: String,
        profile: String,
        profile_version: u64,
        associations: String,
        entrypoint: String,
        binary: Option<String>,
    }

    fn registry_targets() -> Vec<RegistryTarget> {
        let registry: serde_json::Value = serde_json::from_str(REGISTRY).unwrap();
        assert_eq!(registry["schema"], "craft-nightly-registry/v1");
        let mut targets = Vec::new();
        for (id, app) in registry["apps"].as_object().unwrap() {
            for target in app["targets"].as_array().unwrap() {
                let os = target["target"]["os"].as_str().unwrap().to_string();
                let arch = target["target"]["arch"].as_str().unwrap().to_string();
                let pattern = target["asset_pattern"].as_str().unwrap();
                let prefix = format!("{id}-{{version}}-");
                let suffix = pattern.strip_prefix(prefix.as_str()).expect("an asset pattern names its app and version").to_string();
                // The row's extra program is the registry's `binary` only where it isn't the
                // entrypoint: PhotoCraft's rows name its one GUI program in both fields. A missing
                // or non-string `binary` is a malformed registry row, not the same as "no program".
                let listed = &app["entrypoints"][format!("{os}-{arch}")];
                let entrypoint = listed["entrypoint"].as_str().expect("a registry entrypoint names its entrypoint").to_string();
                let binary = listed["binary"].as_str().expect("a registry entrypoint names its binary");
                let binary = (binary != entrypoint.as_str()).then(|| binary.to_string());
                let isolation = &target["isolation"];
                targets.push(RegistryTarget {
                    app: id.clone(),
                    display: app["display_name"].as_str().unwrap().to_string(),
                    repository: app["repository"].as_str().unwrap().to_string(),
                    os,
                    arch,
                    format: target["target"]["format"].as_str().unwrap().to_string(),
                    suffix,
                    package: isolation["package_identity"].as_str().unwrap().to_string(),
                    profile: isolation["profile_contract"].as_str().unwrap().to_string(),
                    profile_version: isolation["profile_version"].as_u64().unwrap(),
                    associations: isolation["associations"].as_str().unwrap().to_string(),
                    entrypoint,
                    binary,
                });
            }
        }
        targets
    }

    /// Every registry target has exactly one row with equal values, and no row lacks a target: a
    /// reviewed registry change and a reviewed table change have to land together, and neither alone
    /// can widen what the Launcher will install.
    #[test]
    fn capability_table_is_exactly_the_checked_in_registry() {
        let targets = registry_targets();
        assert_eq!(targets.len(), CAPABILITIES.len(), "one row per registry target, no row without one");
        let mut matched = vec![false; targets.len()];
        for row in CAPABILITIES {
            let platform_os = match row.platform.os {
                Os::Windows => "windows",
                Os::FreeBsd => "freebsd",
                Os::Linux => "linux",
                Os::MacOs => "macos",
            };
            let platform_arch = match row.platform.arch {
                Arch::X64 => "x86_64",
                Arch::Arm64 => "aarch64",
                Arch::X86 => "x86",
                Arch::Riscv64 => "riscv64",
            };
            assert_eq!((row.os, row.arch), (platform_os, platform_arch), "{} declares its own target", row.app);
            assert_eq!((row.profile, row.profile_version), ("nightly", 1), "{} uses the version-1 Nightly profile contract", row.app);
            let expected_badge: Option<String> = match row.os {
                "windows" => None,
                "linux" => Some(format!("{}-nightly.png", row.app)),
                _ => Some(format!("{} Nightly.app/Contents/Resources/{}-nightly.icns", row.display, row.app)),
            };
            assert_eq!(row.badge.map(str::to_string), expected_badge, "{} on {} carries its reviewed badge", row.app, row.os);
            let expected_bundle =
                (row.os == "macos").then(|| (format!("{} Nightly.app", row.display), format!("ai.storyteller.{}.nightly", row.app)));
            assert_eq!(
                row.bundle.map(|(folder, id)| (folder.to_string(), id.to_string())),
                expected_bundle,
                "{} on {} keeps to its own macOS bundle",
                row.app,
                row.os
            );
            let found = targets
                .iter()
                .enumerate()
                .filter(|(_, target)| {
                    target.app == row.app
                        && target.display == row.display
                        && target.repository == row.repository
                        && target.os == row.os
                        && target.arch == row.arch
                        && target.format == row.format
                        && target.suffix == row.suffix
                        && target.package == row.package
                        && target.profile == row.profile
                        && target.profile_version == u64::from(row.profile_version)
                        // The table holds no other association policy, and admission requires "none".
                        && target.associations == "none"
                        && target.entrypoint == row.entrypoint
                        && target.binary.as_deref() == row.binary
                })
                .collect::<Vec<_>>();
            assert_eq!(found.len(), 1, "{} on {} {} has exactly one reviewed row", row.app, row.os, row.arch);
            matched[found[0].0] = true;
        }
        assert!(matched.iter().all(|matched| *matched), "every registry target is in the table");
    }

    /// The persisted Nightly scheduling key contains `{adapter:?}`: PhotoCraft's three names may
    /// never change, and no two rows may share one.
    #[test]
    fn adapter_debug_names_stay_the_persisted_lookup_identities() {
        assert_eq!(format!("{:?}", Adapter::PhotoCraftWindowsV1), "PhotoCraftWindowsV1");
        assert_eq!(format!("{:?}", Adapter::PhotoCraftLinuxV1), "PhotoCraftLinuxV1");
        assert_eq!(format!("{:?}", Adapter::PhotoCraftMacV1), "PhotoCraftMacV1");
        let names = CAPABILITIES.iter().map(|row| format!("{:?}", Adapter(row))).collect::<std::collections::BTreeSet<_>>();
        assert_eq!(names.len(), CAPABILITIES.len(), "every row has its own lookup identity");
        for row in CAPABILITIES {
            let name = format!("{:?}", Adapter(row));
            assert!(name.starts_with(row.display) && name.ends_with("V1"), "{name}");
            assert!(!name.contains(['{', ':', ' ', ',']), "a derived Debug would print the row's fields: {name}");
        }
    }

    fn cadcraft_fixture() -> (&'static CraftApp, Release, Artifact, Platform) {
        let app = CraftApp::by_id("cadcraft").unwrap();
        let release = Release {
            tag: "v1.0.0-nightly.20261120.123456.1.gabcdefabcdef".into(),
            version: artcraft_launcher_core::version::parse("v1.0.0-nightly.20261120.123456.1.gabcdefabcdef"),
            prerelease: true,
            ..Default::default()
        };
        let artifact = Artifact {
            signature: None,
            target: Target { os: "linux".into(), arch: "x86_64".into(), format: "tar_gz".into() },
            asset: Download {
                name: format!("cadcraft-{}-linux-x86_64-nightly.tar.gz", release.version.as_ref().unwrap()),
                url: "unused: caller verifies the published asset before table lookup".into(),
                size: 1,
                sha256: "a".repeat(64),
            },
            isolation: Isolation {
                package_identity: "ai.storyteller.cadcraft.nightly".into(),
                profile_contract: "nightly".into(),
                profile_version: 1,
                associations: "none".into(),
            },
        };
        (app, release, artifact, Platform { os: Os::Linux, arch: Arch::X64 })
    }

    /// A row admits one app's own payload: no other app's asset name, package identity, profile or
    /// format stands in, and the same payload under another app is refused.
    #[test]
    fn a_generic_app_is_admitted_only_with_its_own_payload_and_profile() {
        let (app, release, good, platform) = cadcraft_fixture();
        let row = adapter(app, &release, &good, platform).unwrap();
        assert_eq!(Some(row), for_app(app, platform).ok());
        assert_eq!(row.app(), "cadcraft");
        assert_eq!(row.display(), "CADCraft");
        assert_eq!(row.os(), Os::Linux);
        assert_eq!(row.kind(), PackageKind::TarGz);
        assert_eq!(row.executable(), Path::new("cadcraft"));
        assert_eq!(row.binary(), Some(Path::new("cadcraft-bin")));
        assert_eq!(row.badge(), Some(Path::new("cadcraft-nightly.png")));
        assert_eq!(row.bundle(), None);
        let version = release.version.as_ref().unwrap();
        let mut changes = Vec::new();
        for name in [
            format!("anicraft-{version}-linux-x86_64-nightly.tar.gz"),
            format!("photocraft-{version}-linux-x86_64-nightly.tar.gz"),
            format!("cadcraft-{version}-linux-x86_64.zip"),
        ] {
            let mut other = good.clone();
            other.asset.name = name;
            changes.push(other);
        }
        let mut other = good.clone();
        other.isolation.package_identity = "ai.storyteller.anicraft.nightly".into();
        changes.push(other);
        let mut other = good.clone();
        other.isolation.profile_contract = "stable".into();
        changes.push(other);
        let mut other = good.clone();
        other.isolation.profile_version = 2;
        changes.push(other);
        let mut other = good.clone();
        other.isolation.associations = "stable".into();
        changes.push(other);
        let mut other = good.clone();
        other.target.format = "AppImage".into();
        changes.push(other);
        let mut other = good.clone();
        other.target.os = "macos".into();
        changes.push(other);
        for artifact in changes {
            assert!(adapter(app, &release, &artifact, platform).is_err(), "{:?}", artifact.asset.name);
        }
        let photocraft = CraftApp::by_id("photocraft").unwrap();
        assert!(adapter(photocraft, &release, &good, platform).is_err(), "PhotoCraft cannot install CADCraft's payload");
        let community = CraftApp { kind: artcraft_launcher_core::catalog::AppKind::Community, ..*app };
        assert!(adapter(&community, &release, &good, platform).is_err(), "a community build is not the official app");
    }

    /// Every row launches with its own profile root, the version-1 selector and a literal file tail.
    #[test]
    fn a_generic_app_launches_with_its_own_profile_root_and_literal_arguments() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::under(root.path()).for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
        let adapter = for_app(CraftApp::by_id("cadcraft").unwrap(), Platform { os: Os::Linux, arch: Arch::X64 }).unwrap();
        let files = vec!["drawing.dxf".into(), "--profile-root".into(), "--".into()];
        let args = adapter.launch_arguments(&paths, &files).unwrap();
        assert_eq!(
            &args[..5],
            &["--launch-profile", "nightly-v1", "--profile-root", paths.data.join("profiles").join("cadcraft").to_str().unwrap(), "--"]
        );
        assert_eq!(&args[5..], files.as_slice());
        for row in CAPABILITIES {
            let args = Adapter(row).launch_arguments(&paths, &[]).unwrap();
            assert_eq!(&args[1], "nightly-v1", "{}", row.app);
            assert_eq!(&args[3], paths.data.join("profiles").join(row.app).to_str().unwrap(), "{}", row.app);
        }
        assert!(!root.path().join("data").exists(), "building an argv performs no filesystem access");
    }
}
