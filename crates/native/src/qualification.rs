//! Opt-in release qualification, invoked by the packaged launcher on disposable CI runners.
//!
//! This is deliberately part of the ordinary binary: no local mirror, unsigned-app exception,
//! replacement HTTP client, installer stub, or test-feature build can qualify a release.
//! Each operation is a new process; install records are read back from disk between operations.

use std::path::{Path, PathBuf};

use artcraft_launcher_core::protocol::{CANCELLED, InstallLocations, JobPhase, PAUSED};
use artcraft_launcher_core::release::{self, Asset, Release};
use artcraft_launcher_core::{CraftApp, Installed, PackageKind, Platform, platform, version};
use serde::Serialize;

use crate::detect::{self, Snapshot};
use crate::http::{self, Http, Stop};
use crate::install::{self, Job};
use crate::manifest::Manifest;
use crate::paths::Paths;

mod self_update;
pub use self_update::run as update;

const APP: &str = "figcraft";
const OLD: &str = "0.12.1";
const NEW: &str = "0.13.1";
const OLD_SUMS: &str = include_str!("../../../packaging/e2e/fixtures/figcraft-0.12.1.sha256");
const NEW_SUMS: &str = include_str!("../../../packaging/e2e/fixtures/figcraft-0.13.1.sha256");
const MARKER: &str = "artcraft-launcher-qualification-v1";

#[derive(Serialize)]
struct Outcome<'a> {
    schema: u32,
    operation: &'a str,
    launcher_version: &'a str,
    build_sha: Option<&'a str>,
    binary_arch: &'a str,
    platform: String,
    success: bool,
    error: Option<&'a str>,
    installed: Option<Installed>,
}

/// Run one explicitly requested qualification operation. This guard protects against accidental
/// invocation on a workstation; environment variables are not a security boundary. The actual
/// install/verification code retains all its ordinary trust checks.
pub fn run(args: &[String]) -> Result<(), String> {
    let [root, kind, operation] = args else {
        return Err("usage: --qualify-install <fresh-runner-temp-directory> <dmg|portable|appimage|msi|deb> <operation>".into());
    };
    require_runner()?;
    let kind = parse_kind(kind)?;
    let root = checked_root(Path::new(root))?;
    let app = CraftApp::by_id(APP).ok_or("qualification app is not in the catalog")?;
    let paths = Paths::under(&root);
    if operation == "init" {
        initialize(&root, kind, app)?;
    } else if std::fs::read_to_string(root.join("owner.txt")).map_err(|e| e.to_string())? != format!("{MARKER}\n{kind:?}\n") {
        return Err("qualification directory has the wrong owner or package kind".into());
    }
    let result = if operation == "init" { Ok(()) } else { operate(&root, kind, app, operation, &paths) };
    let manifest = Manifest::load(&paths.manifest());
    let outcome = Outcome {
        schema: 1,
        operation,
        launcher_version: env!("CARGO_PKG_VERSION"),
        build_sha: option_env!("ARTCRAFT_LAUNCHER_BUILD_SHA"),
        binary_arch: std::env::consts::ARCH,
        platform: Platform::current().map_or_else(|| "unknown".into(), Platform::label),
        success: result.is_ok(),
        error: result.as_ref().err().map(String::as_str),
        installed: detect::detect(app, &manifest, &Snapshot::take()),
    };
    let bytes = serde_json::to_vec_pretty(&outcome).map_err(|e| e.to_string())?;
    crate::paths::write_atomic(&root.join("result.json"), &bytes)?;
    result
}

fn require_runner() -> Result<(), String> {
    let hosted = std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("github-hosted");
    let disposable_vm = std::env::var("RUNNER_ENVIRONMENT").as_deref() == Ok("self-hosted")
        && std::env::var("ARTCRAFT_QUALIFICATION_DISPOSABLE_VM").as_deref() == Ok("1");
    if std::env::var("GITHUB_ACTIONS").as_deref() != Ok("true") || !(hosted || disposable_vm) {
        return Err("installation qualification requires a disposable Actions runner".into());
    }
    Ok(())
}

/// Where the ordinary GUI records that it constructed and rendered multiple UI frames.
pub fn ui_report(root: &Path) -> Result<PathBuf, String> {
    require_runner()?;
    let root = checked_root(root)?;
    let marker = std::fs::read_to_string(root.join("owner.txt")).map_err(|e| e.to_string())?;
    if ![PackageKind::Dmg, PackageKind::Msi, PackageKind::PortableZip, PackageKind::Deb, PackageKind::AppImage]
        .iter()
        .any(|kind| marker == format!("{MARKER}\n{kind:?}\n"))
    {
        return Err("initialize qualification before launching its UI".into());
    }
    Ok(root.join("ui.json"))
}

fn parse_kind(kind: &str) -> Result<PackageKind, String> {
    match kind {
        "dmg" if cfg!(target_os = "macos") => Ok(PackageKind::Dmg),
        "portable" if cfg!(windows) => Ok(PackageKind::PortableZip),
        "msi" if cfg!(windows) => Ok(PackageKind::Msi),
        "appimage" if cfg!(target_os = "linux") => Ok(PackageKind::AppImage),
        "deb" if cfg!(target_os = "linux") => Ok(PackageKind::Deb),
        _ => Err("unsupported qualification package on this OS".into()),
    }
}

fn checked_root(root: &Path) -> Result<PathBuf, String> {
    let temp = std::env::var_os("RUNNER_TEMP").ok_or("RUNNER_TEMP is missing")?;
    checked_root_under(root, Path::new(&temp))
}

// Keep Windows paths in the normal namespace: MSI and OS process inventory can disagree with
// verbatim \\?\ paths even when they name the same file. Existing symlinks/junctions still resolve.
fn canonical(path: &Path) -> Result<PathBuf, String> {
    #[cfg(windows)]
    let result = dunce::canonicalize(path);
    #[cfg(not(windows))]
    let result = std::fs::canonicalize(path);
    result.map_err(|e| e.to_string())
}

fn checked_root_under(root: &Path, temp: &Path) -> Result<PathBuf, String> {
    let temp = canonical(temp)?;
    let parent = root.parent().ok_or("qualification directory needs a parent")?;
    let parent = canonical(parent)?;
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| n.starts_with("launcher-e2e-"))
        .ok_or("qualification directory needs a launcher-e2e- prefix")?;
    if !parent.starts_with(&temp) || root.is_symlink() {
        return Err("qualification must stay inside RUNNER_TEMP without a root symlink".into());
    }
    Ok(parent.join(name))
}

fn initialize(root: &Path, kind: PackageKind, app: &CraftApp) -> Result<(), String> {
    // Never adopt a pre-existing install, even on a disposable runner.
    if detect::detect(app, &Manifest::default(), &Snapshot::take()).is_some() {
        return Err("qualification app is already installed; refusing to adopt or remove it".into());
    }
    std::fs::create_dir(root).map_err(|e| format!("qualification root must be new: {e}"))?;
    std::fs::create_dir(root.join("applications")).map_err(|e| e.to_string())?;
    std::fs::write(root.join("owner.txt"), format!("{MARKER}\n{kind:?}\n")).map_err(|e| e.to_string())?;
    // Separate from install files: update/uninstall must preserve this user's document.
    std::fs::create_dir(root.join("documents")).map_err(|e| e.to_string())?;
    std::fs::write(root.join("documents/keep.txt"), b"user document\n").map_err(|e| e.to_string())
}

fn fixture(app: &CraftApp, kind: PackageKind, newer: bool) -> Result<(Release, Asset), String> {
    let (v, sums) = if newer { (NEW, NEW_SUMS) } else { (OLD, OLD_SUMS) };
    let release = release::release_from_sums(app.repo, &format!("v{v}"), &release::parse_sha256sums(sums));
    let platform = Platform::current().ok_or("unsupported qualification platform")?;
    let stems: Vec<_> = app.stems().collect();
    let (asset, _) = platform::pick_asset(&release, &stems, platform, &[kind]).ok_or("pinned fixture has no asset for this architecture")?;
    let asset = asset.clone();
    Ok((release, asset))
}

fn installed(app: &CraftApp, paths: &Paths) -> Option<Installed> {
    detect::detect(app, &Manifest::load(&paths.manifest()), &Snapshot::take())
}

fn verify(app: &CraftApp, paths: &Paths, want: Option<&str>) -> Result<(), String> {
    let got = installed(app, paths);
    match (want, got) {
        (None, None) => Ok(()),
        (Some(want), Some(got)) if got.version == version::parse(want) && Path::new(&got.location).exists() => Ok(()),
        (want, got) => Err(format!("detected {got:?}, expected version {want:?}")),
    }
}

fn assert_owned(root: &Path, kind: PackageKind, found: Option<&Installed>, manifest: &Manifest) -> Result<(), String> {
    for record in manifest.apps.values() {
        if !canonical(&record.path)?.starts_with(root) {
            return Err("qualification record escapes its root".into());
        }
    }
    if let Some(found) = found {
        let path = canonical(Path::new(&found.location))?;
        if kind != PackageKind::Deb && !path.starts_with(root) {
            return Err("detected installation escapes qualification root".into());
        }
        if kind == PackageKind::Deb && (found.source != artcraft_launcher_core::InstallSource::Deb || found.package.as_deref() != Some(APP)) {
            return Err("unexpected system package; refusing to operate".into());
        }
    }
    Ok(())
}

fn operate(root: &Path, kind: PackageKind, app: &CraftApp, operation: &str, paths: &Paths) -> Result<(), String> {
    let mut manifest = Manifest::load(&paths.manifest());
    let found = installed(app, paths);
    assert_owned(root, kind, found.as_ref(), &manifest)?;
    match operation {
        "verify-old" => return verify(app, paths, Some(OLD)),
        "verify-new" => return verify(app, paths, Some(NEW)),
        "verify-absent" => return verify(app, paths, None),
        "launch" => {
            let found = found.as_ref().ok_or("install the qualification app before launching it")?;
            install::launch(app, found, paths)?;
            let start = std::time::Instant::now();
            while start.elapsed() < std::time::Duration::from_secs(20) {
                if crate::process::is_running(Path::new(&found.location)) {
                    std::thread::sleep(std::time::Duration::from_secs(3));
                    return if crate::process::is_running(Path::new(&found.location)) {
                        Ok(())
                    } else {
                        Err("launched app exited immediately".into())
                    };
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            return Err("launcher did not start the installed app".into());
        }
        "remove" => {
            if let Some(found) = &found {
                let record = manifest.apps.get(APP);
                let files = record.map(|r| r.files.clone()).unwrap_or_default();
                install::uninstall(app, found, record, paths, &Stop::default(), &mut |_| {})?;
                if Path::new(&found.location).exists() || files.iter().any(|f| f.exists()) {
                    return Err("uninstall left installed files or desktop integration behind".into());
                }
                manifest.apps.remove(APP);
                manifest.save(&paths.manifest())?;
            }
            verify(app, paths, None)?;
            return check_document(root);
        }
        "install-old" | "install-new" | "pause-new" | "cancel-new" | "bad-checksum" | "reject-downgrade" | "reject-running" | "corrupt-cache" => {}
        _ => return Err("unknown qualification operation".into()),
    }
    let newer = !matches!(operation, "install-old" | "reject-downgrade");
    let (release, mut asset) = fixture(app, kind, newer)?;
    let client = Http::new()?;
    let stop = Stop::default();
    let previous = manifest.apps.get(APP).cloned();
    let before = manifest.clone();
    let locations =
        InstallLocations { root: (kind != PackageKind::Deb).then(|| root.join("applications").display().to_string()), ..Default::default() };
    if operation == "corrupt-cache" {
        let dest = install::download_path(paths, &asset);
        std::fs::create_dir_all(paths.downloads()).map_err(|e| e.to_string())?;
        std::fs::write(dest, b"corrupt cached artifact").map_err(|e| e.to_string())?;
        return Ok(());
    }
    if operation == "bad-checksum" {
        // A real small published asset with an intentionally wrong digest exercises the full
        // install entry point but must fail before any OS installer is reached.
        asset.name = "SHA256SUMS.txt".into();
        asset.url = format!("{}v{NEW}/SHA256SUMS.txt", release::download_prefix(app.repo));
        asset.sha256 = Some("00".repeat(32));
        asset.size = 0;
    }
    let mut installer_started = false;
    let mut phase = |phase| {
        if phase == JobPhase::Installing {
            installer_started = true;
        }
        if matches!(phase, JobPhase::Downloading { done, .. } if done > 0) {
            if operation == "pause-new" {
                stop.pause();
            }
            if operation == "cancel-new" {
                stop.cancel();
            }
        }
    };
    let mut job = Job { http: &client, paths, stop: &stop, phase: &mut phase, install_locations: &locations, replace: false, downgrade: None };
    let mut commit = |record: &crate::manifest::Record| {
        let mut updated = manifest.clone();
        updated.apps.insert(APP.into(), record.clone());
        updated.save(&paths.manifest())?;
        manifest = updated;
        Ok(())
    };
    let result = install::install_at(app, &release, &asset, kind, found.as_ref(), previous.as_ref(), &mut job, Some(&mut commit));
    if matches!(operation, "pause-new" | "cancel-new" | "bad-checksum" | "reject-downgrade" | "reject-running") {
        let error = result.err().ok_or("negative scenario unexpectedly installed an app")?;
        let expected = match operation {
            "pause-new" => error == PAUSED && http::partial_bytes(&install::download_path(paths, &asset), asset.size).is_some_and(|n| n > 0),
            "cancel-new" => error == CANCELLED && http::partial_bytes(&install::download_path(paths, &asset), asset.size).is_none(),
            "bad-checksum" => error.contains("checksum"),
            "reject-running" => {
                let found = found.as_ref().ok_or("running app disappeared")?;
                error.contains("running")
                    && install::uninstall(app, found, previous.as_ref(), paths, &Stop::default(), &mut |_| {})
                        .is_err_and(|error| error.contains("running"))
            }
            _ => error.contains("newer"),
        };
        if !expected || installer_started || Manifest::load(&paths.manifest()) != before {
            return Err(format!("negative scenario did not preserve the installation: {error}"));
        }
        return check_document(root);
    }
    if let Some(record) = result? {
        manifest.apps.insert(APP.into(), record);
        manifest.save(&paths.manifest())?;
    }
    verify(app, paths, Some(if newer { NEW } else { OLD }))?;
    check_document(root)
}

fn check_document(root: &Path) -> Result<(), String> {
    if std::fs::read(root.join("documents/keep.txt")).map_err(|e| e.to_string())? != b"user document\n" {
        return Err("lifecycle operation changed the user document".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixtures_have_two_ordered_versions_and_every_native_architecture() {
        use artcraft_launcher_core::{Arch, Os};
        let app = CraftApp::by_id(APP).unwrap();
        assert!(version::is_newer(&version::parse(NEW).unwrap(), &version::parse(OLD).unwrap()));
        for (v, sums) in [(OLD, OLD_SUMS), (NEW, NEW_SUMS)] {
            let r = release::release_from_sums(app.repo, &format!("v{v}"), &release::parse_sha256sums(sums));
            for (os, arch, kinds) in [
                (Os::MacOs, Arch::Arm64, vec![PackageKind::Dmg]),
                (Os::MacOs, Arch::X64, vec![PackageKind::Dmg]),
                (Os::Windows, Arch::X64, vec![PackageKind::Msi, PackageKind::PortableZip]),
                (Os::Windows, Arch::X86, vec![PackageKind::Msi, PackageKind::PortableZip]),
                (Os::Windows, Arch::Arm64, vec![PackageKind::Msi, PackageKind::PortableZip]),
                (Os::Linux, Arch::X64, vec![PackageKind::Deb, PackageKind::AppImage]),
                (Os::Linux, Arch::Arm64, vec![PackageKind::Deb, PackageKind::AppImage]),
            ] {
                for kind in kinds {
                    let (asset, _) = platform::pick_asset(&r, &[APP], Platform { os, arch }, &[kind]).unwrap();
                    assert!(asset.sha256.as_ref().is_some_and(|sha| sha.len() == 64));
                    assert!(release::download_url_under(&asset.url, &release::download_prefix(app.repo)));
                }
            }
        }
    }

    #[test]
    fn qualification_is_not_an_implicit_workstation_install() {
        assert!(run(&[]).is_err());
        assert!(parse_kind("untrusted-package").is_err());
    }

    #[test]
    fn qualification_root_rejects_escape_and_requires_an_explicit_name() {
        let temp = tempfile::tempdir().unwrap();
        let owned = temp.path().join("owned");
        std::fs::create_dir(&owned).unwrap();
        let root = owned.join("launcher-e2e-test");
        assert_eq!(checked_root_under(&root, temp.path()).unwrap(), canonical(&owned).unwrap().join("launcher-e2e-test"));
        assert!(checked_root_under(&owned.join("unmarked"), temp.path()).is_err());
        assert!(checked_root_under(&temp.path().join("launcher-e2e-escape"), &owned).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn qualification_root_rejects_symlinked_root_and_escaping_parent() {
        let temp = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        let root = temp.path().join("launcher-e2e-link");
        std::os::unix::fs::symlink(elsewhere.path(), &root).unwrap();
        assert!(checked_root_under(&root, temp.path()).is_err());
        assert!(checked_root_under(&root.join("launcher-e2e-child"), temp.path()).is_err());
    }
}
