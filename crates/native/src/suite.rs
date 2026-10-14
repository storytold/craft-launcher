//! Independent, relocatable offline snapshots. Never uses the host install workflow.
use std::collections::{BTreeMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use artcraft_launcher_core::protocol::{JobPhase, LatestRelease};
use artcraft_launcher_core::{CraftApp, InstallSource, Installed, Os, PackageKind, Platform, Release};
use serde::{Deserialize, Serialize};

use crate::http::{Http, Stop, Transfer};
use crate::source::ReleaseSource;

const MARKER: &str = "craft-suite.json";
const MAX_MANIFEST: u64 = 256 * 1024;
const MAX_ARCHIVE: u64 = 4 * 1024 * 1024 * 1024;
const MAX_EXPANDED: u64 = 8 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 50_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub version: String,
    pub path: String,
    pub release_url: String,
    pub asset: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Suite {
    pub format: u32,
    pub platform: Platform,
    pub launcher_version: String,
    pub apps: BTreeMap<String, Entry>,
}

/// Recognise only the known distribution layouts, never scan arbitrary ancestor directories.
pub fn root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    #[cfg(target_os = "linux")]
    let exe = std::env::var_os("APPIMAGE").map(PathBuf::from).unwrap_or(exe);
    root_for_executable(&exe)
}

pub fn root_for_executable(exe: &Path) -> Option<PathBuf> {
    let parent = exe.parent()?;
    if parent.join(MARKER).exists() {
        return Some(parent.to_path_buf());
    }
    // <suite>/ArtCraft Launcher.app/Contents/MacOS/ArtCraft Launcher
    let bundle = parent.parent()?.parent()?;
    if bundle.extension().is_some_and(|e| e == "app") {
        let root = bundle.parent()?;
        if root.join(MARKER).exists() {
            return Some(root.to_path_buf());
        }
    }
    None
}

fn relative(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() || path.contains(['\\', ':', '\0']) || path.starts_with('/') {
        return Err("the suite contains an unsafe path".into());
    }
    if path.split('/').any(|p| p.is_empty() || p == "." || p == "..") {
        return Err("the suite contains an unsafe path".into());
    }
    Ok(PathBuf::from(path))
}

fn inside(root: &Path, path: &str) -> Result<PathBuf, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let path = root.join(relative(path)?).canonicalize().map_err(|e| format!("a suite file is missing ({e})"))?;
    if !path.starts_with(&root) {
        return Err("a suite path points outside its folder".into());
    }
    Ok(path)
}

pub fn load(root: &Path, platform: Platform) -> Result<Suite, String> {
    let file = std::fs::File::open(root.join(MARKER)).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err("the suite manifest is too large".into());
    }
    let suite: Suite = serde_json::from_slice(&bytes).map_err(|e| format!("the suite manifest is unreadable ({e})"))?;
    if suite.format != 1 || suite.platform != platform || suite.apps.len() > artcraft_launcher_core::SUITE.len() {
        return Err("this suite format or platform is not supported".into());
    }
    for (id, entry) in &suite.apps {
        let app = CraftApp::by_id(id).ok_or("the suite contains an unknown app")?;
        if !entry.path.starts_with(&format!("apps/{id}/")) || !entry.release_url.starts_with(&format!("{}/tag/", app.releases_url())) {
            return Err("the suite contains an invalid app record".into());
        }
        relative(&entry.path)?;
        if artcraft_launcher_core::version::parse(&entry.version).is_none()
            || entry.sha256.len() != 64
            || !entry.sha256.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("the suite contains an invalid version or checksum".into());
        }
    }
    Ok(suite)
}

pub fn installed(root: &Path, suite: &Suite, id: &str) -> Option<Installed> {
    let entry = suite.apps.get(id)?;
    let path = inside(root, &entry.path).ok()?;
    Some(Installed {
        version: artcraft_launcher_core::version::parse(&entry.version),
        source: InstallSource::OfflineSuite,
        location: path.display().to_string(),
        package: None,
    })
}

pub fn launch(root: &Path, platform: Platform, app: &CraftApp) -> Result<(), String> {
    let suite = load(root, platform)?;
    let entry = suite.apps.get(app.id).ok_or("this app is not included in the suite")?;
    let mut executable = inside(root, &entry.path)?;
    #[cfg(target_os = "macos")]
    {
        executable = bundle_executable(&executable)?;
    }
    #[cfg(not(target_os = "macos"))]
    let _ = &mut executable;
    let home = root.join("ArtCraftSuiteData").join(app.id);
    std::fs::create_dir_all(&home).map_err(|e| format!("the suite needs a writable folder ({e})"))?;
    let mut cmd = crate::process::command(&executable);
    cmd.current_dir(executable.parent().ok_or("the app has no parent folder")?);
    // Craft's explicit config override, plus the platform directories for other app resources.
    for stem in app.stems() {
        cmd.env(format!("{}_CONFIG_DIR", stem.to_ascii_uppercase()), &home);
    }
    cmd.env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("APPDATA", home.join("config"))
        .env("LOCALAPPDATA", home.join("local"))
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .env("XDG_STATE_HOME", home.join("state"));
    #[cfg(target_os = "linux")]
    cmd.env("APPIMAGE_EXTRACT_AND_RUN", "1");
    crate::process::spawn_detached(cmd)
}

struct Planned {
    app: &'static CraftApp,
    release: Release,
    asset: artcraft_launcher_core::Asset,
    sha: String,
}

/// Preflight every selected app before downloading. Staging is owned by this job; persist refuses
/// an existing destination. A cancellation or failure drops only that private staging directory.
pub fn export(
    destination: &Path,
    ids: &[String],
    platform: Platform,
    source: &dyn ReleaseSource,
    http: &Http,
    stop: &Stop,
    progress: &mut dyn FnMut(&str, JobPhase),
) -> Result<PathBuf, String> {
    if crate::process::in_sandbox() {
        return Err("export a suite from a native launcher outside Flatpak".into());
    }
    if !destination.is_absolute() || destination.exists() {
        return Err("choose a new absolute suite folder; existing folders are never replaced".into());
    }
    let parent = destination.parent().filter(|p| p.is_dir()).ok_or("the destination's parent folder must already exist")?;
    if ids.is_empty() || ids.len() > artcraft_launcher_core::SUITE.len() {
        return Err("select at least one Craft App".into());
    }
    if Platform::current() != Some(platform) || platform.os == Os::FreeBsd {
        return Err("create the suite on its target operating system".into());
    }
    let kind = match platform.os {
        Os::Windows => PackageKind::PortableZip,
        Os::Linux => PackageKind::AppImage,
        Os::MacOs => PackageKind::Dmg,
        Os::FreeBsd => return Err("unsupported suite platform".into()),
    };
    let mut seen = HashSet::new();
    let mut plan = Vec::new();
    for id in ids {
        stop.check()?;
        let app = CraftApp::by_id(id).ok_or("unknown Craft App")?;
        if !seen.insert(id) {
            return Err("an app was selected twice".into());
        }
        progress(app.name, JobPhase::Waiting);
        let release = match source.latest(app.repo, false).latest {
            LatestRelease::Found(r) => r,
            LatestRelease::Failed(e) => return Err(format!("{}: {e}", app.name)),
            LatestRelease::None => return Err(format!("{} has no published release", app.name)),
        };
        let stems: Vec<_> = app.stems().collect();
        let (asset, _) = artcraft_launcher_core::platform::pick_asset(&release, &stems, platform, &[kind])
            .ok_or_else(|| format!("{} has no portable release for {}. Deselect it to continue.", app.name, platform.label()))?;
        // Deliberately ignore an API digest: the published SHA256SUMS.txt is required here.
        let sums = release.sums_asset().ok_or_else(|| format!("{} publishes no SHA256SUMS.txt", app.name))?;
        if !sums.url.starts_with(&artcraft_launcher_core::release::download_prefix(app.repo))
            || !asset.url.starts_with(&artcraft_launcher_core::release::download_prefix(app.repo))
            || relative(&asset.name)?.components().count() != 1
            || asset.size > MAX_ARCHIVE
        {
            return Err("the release contains an unsafe download".into());
        }
        let text = http.get_text(&sums.url)?.ok_or("the release checksum list is missing")?;
        let sha = artcraft_launcher_core::release::parse_sha256sums(&text)
            .into_iter()
            .find(|(_, name)| name == &asset.name)
            .map(|(sha, _)| sha)
            .ok_or("the portable download has no published checksum")?;
        plan.push(Planned { app, asset: asset.clone(), release, sha });
    }
    let stage = tempfile::Builder::new().prefix(".artcraft-suite-").tempdir_in(parent).map_err(|e| format!("couldn't stage the suite ({e})"))?;
    let mut suite = Suite { format: 1, platform, launcher_version: env!("CARGO_PKG_VERSION").into(), apps: BTreeMap::new() };
    copy_launcher(stage.path(), stop)?;
    for planned in plan {
        stop.check()?;
        let archive = stage.path().join("download.tmp");
        http.download_capped(
            &planned.asset.url,
            &archive,
            planned.asset.size,
            &planned.sha,
            stop,
            &mut |p| {
                let phase = match p {
                    Transfer::Bytes { done, total } => JobPhase::Downloading { done, total },
                    Transfer::Reconnecting { done, total } => JobPhase::Reconnecting { done, total },
                };
                progress(planned.app.name, phase);
            },
            MAX_ARCHIVE,
        )?;
        progress(planned.app.name, JobPhase::Verifying);
        stop.check()?;
        progress(planned.app.name, JobPhase::Installing);
        let app_dir = stage.path().join("apps").join(planned.app.id);
        std::fs::create_dir_all(&app_dir).map_err(|e| e.to_string())?;
        let executable = unpack(planned.app, &archive, &app_dir, platform.os, stop)?;
        let path = executable.strip_prefix(stage.path()).map_err(|e| e.to_string())?.to_string_lossy().replace('\\', "/");
        suite.apps.insert(
            planned.app.id.into(),
            Entry {
                version: planned.release.version_label(),
                path,
                release_url: planned.release.html_url,
                asset: planned.asset.name,
                sha256: planned.sha,
            },
        );
        std::fs::remove_file(&archive).map_err(|e| e.to_string())?;
    }
    stop.check()?;
    let bytes = serde_json::to_vec_pretty(&suite).map_err(|e| e.to_string())?;
    std::fs::write(stage.path().join(MARKER), bytes).map_err(|e| e.to_string())?;
    std::fs::write(stage.path().join("README.txt"), README).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(stage.path().join("ArtCraftSuiteData")).map_err(|e| e.to_string())?;
    let moved = stage.keep();
    // Rename on the same filesystem. Never merge into a pre-existing folder.
    let result = finalize(&moved, destination);
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&moved);
    }
    result.map(|_| destination.to_path_buf())
}

fn finalize(stage: &Path, destination: &Path) -> Result<(), String> {
    // A new empty reservation prevents rename replacing someone else's directory on Unix.
    std::fs::create_dir(destination).map_err(|e| format!("couldn't reserve the new suite folder ({e})"))?;
    // Move entries into the owned reservation; the marker is committed last, so incomplete
    // exports can never be mistaken for runnable suites.
    let result = (|| {
        for entry in std::fs::read_dir(stage).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_name() != MARKER {
                std::fs::rename(entry.path(), destination.join(entry.file_name())).map_err(|e| e.to_string())?;
            }
        }
        std::fs::rename(stage.join(MARKER), destination.join(MARKER)).map_err(|e| e.to_string())?;
        std::fs::remove_dir(stage).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(destination);
    }
    result
}

fn copy_launcher(root: &Path, stop: &Stop) -> Result<(), String> {
    stop.check()?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    {
        let bundle = exe
            .parent()
            .and_then(Path::parent)
            .and_then(Path::parent)
            .filter(|p| p.extension().is_some_and(|e| e == "app"))
            .ok_or("export from the packaged ArtCraft Launcher.app")?;
        copy_bundle(bundle, &root.join("ArtCraft Launcher.app"))?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        #[cfg(target_os = "linux")]
        let exe = std::env::var_os("APPIMAGE").map(PathBuf::from).unwrap_or(exe);
        let name = if cfg!(windows) {
            "artcraft-launcher.exe"
        } else if exe.extension().is_some_and(|e| e == "AppImage") {
            "artcraft-launcher.AppImage"
        } else {
            "artcraft-launcher"
        };
        std::fs::copy(&exe, root.join(name)).map_err(|e| format!("couldn't copy the running launcher ({e})"))?;
        std::fs::write(root.join("portable.txt"), b"").map_err(|e| e.to_string())?;
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::PermissionsExt;
            let script = format!("#!/bin/sh\ncd -- \"$(dirname -- \"$0\")\" || exit 1\nexport APPIMAGE_EXTRACT_AND_RUN=1\nexec './{name}' \"$@\"\n");
            std::fs::write(root.join("Start ArtCraft Launcher.sh"), script).map_err(|e| e.to_string())?;
            std::fs::set_permissions(root.join("Start ArtCraft Launcher.sh"), std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn unpack(app: &CraftApp, archive: &Path, directory: &Path, os: Os, stop: &Stop) -> Result<PathBuf, String> {
    match os {
        Os::Windows => {
            extract_zip(archive, directory, stop)?;
            // Releases sometimes wrap the executable in one versioned directory.
            let mut candidates = Vec::new();
            for dir in std::iter::once(directory.to_path_buf()).chain(
                std::fs::read_dir(directory).map_err(|e| e.to_string())?.filter_map(Result::ok).filter(|e| e.path().is_dir()).map(|e| e.path()),
            ) {
                for stem in app.stems() {
                    let exe = dir.join(format!("{stem}.exe"));
                    if exe.is_file() {
                        candidates.push(exe);
                    }
                }
            }
            if candidates.len() != 1 {
                return Err(format!("{}'s portable archive has no unique executable", app.name));
            }
            let exe = candidates.pop().ok_or("missing executable")?;
            std::fs::write(exe.with_file_name("portable.txt"), b"").map_err(|e| e.to_string())?;
            Ok(exe)
        }
        Os::Linux => {
            let dest = directory.join(format!("{}.AppImage", app.id));
            std::fs::copy(archive, &dest).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
            }
            Ok(dest)
        }
        #[cfg(target_os = "macos")]
        Os::MacOs => unpack_dmg(app, archive, directory, stop),
        _ => Err("unsupported portable archive on this system".into()),
    }
}

/// Extract into a new, private directory. Reject links, Windows aliases, duplicates and bombs.
fn extract_zip(archive: &Path, directory: &Path, stop: &Stop) -> Result<(), String> {
    let file = std::fs::File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("unreadable portable ZIP ({e})"))?;
    if zip.len() > MAX_ENTRIES {
        return Err("the portable ZIP contains too many files".into());
    }
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for i in 0..zip.len() {
        stop.check()?;
        let mut entry = zip.by_index(i).map_err(|e| e.to_string())?;
        let decoded_name = entry.name().map_err(|e| e.to_string())?;
        let name = decoded_name.trim_end_matches('/').to_string();
        let name = name.as_str();
        let path = relative(name)?;
        for segment in name.split('/') {
            let base = segment.split('.').next().unwrap_or("").to_ascii_uppercase();
            if segment.ends_with(['.', ' '])
                || segment.contains(['<', '>', '"', '|', '?', '*'])
                || matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                || (base.len() == 4 && (base.starts_with("COM") || base.starts_with("LPT")) && base.ends_with(|c: char| c.is_ascii_digit()))
            {
                return Err("the portable ZIP contains a Windows path alias".into());
            }
        }
        if !seen.insert(name.to_lowercase()) {
            return Err("the portable ZIP contains duplicate paths".into());
        }
        let file_type = entry.unix_mode().unwrap_or(0) & 0o170000;
        if !matches!(file_type, 0 | 0o100000 | 0o040000) {
            return Err("links and special files are not allowed in portable ZIPs".into());
        }
        if entry.size() > MAX_EXPANDED.saturating_sub(total) {
            return Err("the portable ZIP is too large when unpacked".into());
        }
        let dest = directory.join(path);
        if entry.is_dir() {
            std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
            continue;
        }
        std::fs::create_dir_all(dest.parent().ok_or("missing parent")?).map_err(|e| e.to_string())?;
        let mut out = std::fs::OpenOptions::new().write(true).create_new(true).open(&dest).map_err(|e| e.to_string())?;
        let expected = entry.size();
        let mut written = 0u64;
        let mut buffer = [0u8; 65536];
        loop {
            stop.check()?;
            let n = entry.read(&mut buffer).map_err(|e| format!("corrupt portable ZIP ({e})"))?;
            if n == 0 {
                break;
            }
            written = written.checked_add(n as u64).ok_or("portable ZIP size overflow")?;
            if written > expected {
                return Err("the portable ZIP contains an incorrect file size".into());
            }
            out.write_all(buffer.get(..n).ok_or("invalid read size")?).map_err(|e| e.to_string())?;
        }
        if written != expected {
            return Err("the portable ZIP contains a truncated file".into());
        }
        total = total.checked_add(written).ok_or("portable ZIP size overflow")?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn bundle_executable(bundle: &Path) -> Result<PathBuf, String> {
    let info = plist::Value::from_file(bundle.join("Contents/Info.plist")).map_err(|e| e.to_string())?;
    let name = info.as_dictionary().and_then(|d| d.get("CFBundleExecutable")).and_then(plist::Value::as_string).ok_or("missing bundle executable")?;
    let name = relative(name)?;
    if name.components().count() != 1 {
        return Err("invalid bundle executable".into());
    }
    let executable = bundle.join("Contents/MacOS").join(name).canonicalize().map_err(|e| e.to_string())?;
    if !executable.starts_with(bundle.canonicalize().map_err(|e| e.to_string())?) {
        return Err("the bundle executable escapes its bundle".into());
    }
    Ok(executable)
}

#[cfg(target_os = "macos")]
fn copy_bundle(source: &Path, destination: &Path) -> Result<(), String> {
    crate::process::run(
        "/usr/bin/ditto",
        &["--norsrc", "--noextattr", "--noacl", source.to_str().ok_or("invalid bundle path")?, destination.to_str().ok_or("invalid bundle path")?],
        std::time::Duration::from_secs(600),
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn unpack_dmg(app: &CraftApp, archive: &Path, directory: &Path, stop: &Stop) -> Result<PathBuf, String> {
    let mount = tempfile::Builder::new().prefix("artcraft-suite-mount-").tempdir().map_err(|e| e.to_string())?;
    let mount_s = mount.path().to_str().ok_or("invalid mount path")?;
    crate::process::run(
        "/usr/bin/hdiutil",
        &["attach", "-nobrowse", "-readonly", "-noautoopen", "-mountpoint", mount_s, archive.to_str().ok_or("invalid disk image path")?],
        std::time::Duration::from_secs(300),
    )?;
    let result = (|| {
        stop.check()?;
        let source = mount.path().join(format!("{}.app", app.name));
        crate::detect::macos::read_bundle(app, &source).ok_or("the disk image contains no matching Craft App")?;
        if !crate::install::macos::signed_by(&source, crate::install::macos::TEAM_ID) {
            return Err("the app is not signed by its Craft developer".into());
        }
        let dest = directory.join(format!("{}.app", app.name));
        copy_bundle(&source, &dest)?;
        bundle_executable(&dest)?;
        Ok(dest)
    })();
    let detached = crate::process::run("/usr/bin/hdiutil", &["detach", mount_s], std::time::Duration::from_secs(60));
    if detached.is_err() {
        let _ = mount.keep();
        return Err("couldn't detach the suite disk image; eject it in Finder".into());
    }
    result
}

const README: &str = "ArtCraft Launcher - Offline Craft App Suite\n\nStart the included ArtCraft Launcher. On Linux use Start ArtCraft Launcher.sh.\nThis is a fixed snapshot for the OS and architecture recorded in craft-suite.json.\nMove or copy the entire folder, including ArtCraftSuiteData and ArtCraftLauncherData.\nApp settings are directed to ArtCraftSuiteData; launcher settings stay beside it.\nThe folder must be writable. No installers, host shortcuts or automatic updates are run.\nThe launcher is copied from the running build; apps are official stable releases verified\nagainst their published SHA256SUMS.txt. The manifest records versions and provenance.\nExport a new suite to upgrade. Existing suites are never overwritten.\nNo personal settings, projects, tokens or caches from the source machine are copied.\nOptional online app features still need a connection. Linux needs the usual graphics/audio\nruntime; AppImages extract and run without FUSE. macOS may show its normal security prompt.\nThe suite is portable storage, not an OS sandbox; an app can still access user-selected files.\n";

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;

    fn archive(root: &Path, names: &[&str]) -> PathBuf {
        let path = root.join("test.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for name in names {
            zip.start_file(*name, SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated)).unwrap();
            zip.write_all(b"test executable").unwrap();
        }
        zip.finish().unwrap();
        path
    }

    #[test]
    fn zip_unpacks_wrapped_releases_and_adds_portable_marker() {
        let root = tempfile::tempdir().unwrap();
        let file = archive(root.path(), &["photocraft-1.0/photocraft.exe", "photocraft-1.0/LICENSE.txt"]);
        let out = root.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let exe = unpack(CraftApp::by_id("photocraft").unwrap(), &file, &out, Os::Windows, &Stop::default()).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"test executable");
        assert!(exe.with_file_name("portable.txt").is_file());
        assert!(out.join("photocraft-1.0/LICENSE.txt").is_file());
    }

    #[test]
    fn zip_refuses_traversal_aliases_duplicates_and_ambiguous_executables() {
        for names in [
            vec!["../escape"],
            vec!["C:/escape"],
            vec!["x\\escape"],
            vec!["CON.txt"],
            vec!["x./bad"],
            vec!["x", "X"],
            vec!["photocraft.exe", "nested/photocraft.exe"],
        ] {
            let root = tempfile::tempdir().unwrap();
            let file = archive(root.path(), &names);
            let out = root.path().join("out");
            std::fs::create_dir(&out).unwrap();
            assert!(unpack(CraftApp::by_id("photocraft").unwrap(), &file, &out, Os::Windows, &Stop::default()).is_err(), "{names:?}");
            assert!(!root.path().join("escape").exists());
        }
    }

    #[test]
    fn zip_refuses_symlinks_and_honours_cancellation() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("link.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&file).unwrap());
        zip.add_symlink("photocraft.exe", "../outside", SimpleFileOptions::default()).unwrap();
        zip.finish().unwrap();
        assert!(extract_zip(&file, root.path(), &Stop::default()).is_err());
        let stop = Stop::default();
        stop.cancel();
        assert_eq!(extract_zip(&file, root.path(), &stop).unwrap_err(), artcraft_launcher_core::protocol::CANCELLED);
    }

    #[test]
    fn manifest_relocates_for_all_platform_layouts_and_refuses_other_platforms() {
        for (os, path, launcher) in [
            (Os::Windows, "apps/photocraft/photocraft.exe", "artcraft-launcher.exe"),
            (Os::Linux, "apps/photocraft/photocraft.AppImage", "artcraft-launcher.AppImage"),
            (Os::MacOs, "apps/photocraft/PhotoCraft.app", "ArtCraft Launcher.app/Contents/MacOS/ArtCraft Launcher"),
        ] {
            let root = tempfile::tempdir().unwrap();
            let source = root.path().join("source");
            std::fs::create_dir_all(source.join("apps/photocraft")).unwrap();
            std::fs::write(source.join(path), b"binary").unwrap();
            let platform = Platform { os, arch: artcraft_launcher_core::Arch::X64 };
            let mut suite = Suite { format: 1, platform, launcher_version: "0.4.1".into(), apps: BTreeMap::new() };
            suite.apps.insert(
                "photocraft".into(),
                Entry {
                    version: "0.9.0".into(),
                    path: path.into(),
                    release_url: "https://github.com/storytold/photocraft/releases/tag/v0.9.0".into(),
                    asset: "a.zip".into(),
                    sha256: "a".repeat(64),
                },
            );
            std::fs::write(source.join(MARKER), serde_json::to_vec(&suite).unwrap()).unwrap();
            let moved = root.path().join("different folder");
            std::fs::rename(&source, &moved).unwrap();
            let loaded = load(&moved, platform).unwrap();
            let found = installed(&moved, &loaded, "photocraft").unwrap();
            assert_eq!(found.source, InstallSource::OfflineSuite);
            assert!(Path::new(&found.location).starts_with(moved.canonicalize().unwrap()));
            assert_eq!(root_for_executable(&moved.join(launcher)), Some(moved.clone()));
            assert!(load(&moved, Platform { os, arch: artcraft_launcher_core::Arch::Arm64 }).is_err());
            suite.apps.get_mut("photocraft").unwrap().path = "apps/photocraft/../../outside".into();
            std::fs::write(moved.join(MARKER), serde_json::to_vec(&suite).unwrap()).unwrap();
            assert!(load(&moved, platform).is_err());
            // A corrupt manifest still selects suite mode, never the host's installs.
            std::fs::write(moved.join(MARKER), "broken").unwrap();
            assert_eq!(root_for_executable(&moved.join(launcher)), Some(moved.clone()));
            assert!(load(&moved, platform).is_err());
        }
    }

    #[test]
    fn finalization_never_overwrites_and_marker_is_committed_with_complete_contents() {
        let root = tempfile::tempdir().unwrap();
        let stage = root.path().join("stage");
        let dest = root.path().join("suite");
        std::fs::create_dir(&stage).unwrap();
        std::fs::write(stage.join("launcher"), b"binary").unwrap();
        std::fs::write(stage.join(MARKER), b"manifest").unwrap();
        std::fs::create_dir(&dest).unwrap();
        std::fs::write(dest.join("user document"), b"keep").unwrap();
        assert!(finalize(&stage, &dest).is_err());
        assert_eq!(std::fs::read(dest.join("user document")).unwrap(), b"keep");
        let new = root.path().join("new suite");
        finalize(&stage, &new).unwrap();
        assert!(new.join(MARKER).is_file() && new.join("launcher").is_file());
        assert!(!stage.exists());
        // Missing marker simulates a commit failure: the owned destination is cleaned up.
        let broken = root.path().join("broken");
        std::fs::create_dir(&broken).unwrap();
        std::fs::write(broken.join("file"), b"partial").unwrap();
        let dest = root.path().join("failed");
        assert!(finalize(&broken, &dest).is_err());
        assert!(!dest.exists());
    }
}
