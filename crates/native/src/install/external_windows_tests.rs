//! Ownership checks must use current registrations even when the caller has no installed copy.

use std::io::Write;
use std::path::{Path, PathBuf};

use artcraft_launcher_core::protocol::{InstallLocations, JobPhase};
use artcraft_launcher_core::release::{Asset, Release};
use artcraft_launcher_core::{CraftApp, InstallSource, Installed, PackageKind};

use super::{Job, check_external_install, download_path, install_at};
use crate::http::{Http, Stop};
use crate::manifest::{Manifest, Record};
use crate::paths::Paths;

/// An external copy in the exact folder a portable install would otherwise replace.
fn external_copy(root: &Path) -> (PathBuf, PathBuf) {
    let folder = root.join("Craft Apps").join("photocraft");
    std::fs::create_dir_all(&folder).unwrap();
    let exe = folder.join("photocraft.exe");
    let personal = folder.join("personal.txt");
    std::fs::write(&exe, b"external program").unwrap();
    std::fs::write(&personal, b"personal files stay here").unwrap();
    (exe, personal)
}

/// Explicit opt-in with a valid executable name and a pin for the external bytes.
fn register(paths: &Paths, exe: &Path) {
    let config = serde_json::json!({
        "apps": {
            "photocraft": {
                "path": exe,
                "sha256": crate::http::sha256_file(exe).unwrap(),
                "version": "0.4.0"
            }
        }
    });
    std::fs::create_dir_all(&paths.data).unwrap();
    std::fs::write(paths.external_apps(), serde_json::to_vec(&config).unwrap()).unwrap();
}

/// A real, checksum-verified cached package needs no HTTP and would replace the external bytes.
fn cached_portable(paths: &Paths) -> (Asset, Release) {
    let name = "photocraft-0.5.0-windows-x64-portable.zip";
    std::fs::create_dir_all(paths.downloads()).unwrap();
    let file = paths.downloads().join(name);
    let mut archive = zip::ZipWriter::new(std::fs::File::create(&file).unwrap());
    archive.start_file("photocraft.exe", zip::write::SimpleFileOptions::default()).unwrap();
    archive.write_all(b"replacement program").unwrap();
    archive.finish().unwrap();
    let asset = Asset {
        name: name.into(),
        size: std::fs::metadata(&file).unwrap().len(),
        url: format!("https://example.invalid/{name}"),
        sha256: Some(crate::http::sha256_file(&file).unwrap()),
    };
    // No claimed PE version: this fixture exercises ownership, without requiring an executable
    // VERSIONINFO resource or ever starting the program.
    let release = Release { tag: "v0.5.0".into(), assets: vec![asset.clone()], ..Release::default() };
    (asset, release)
}

#[test]
/// A caller's absent installed snapshot must not allow an already registered copy to be adopted.
fn an_uncached_external_registration_blocks_install_before_download() {
    let app = CraftApp::by_id("photocraft").unwrap();
    for kind in [PackageKind::Msi, PackageKind::PortableZip] {
        for replace in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let paths = Paths::under(dir.path());
            let (exe, personal) = external_copy(dir.path());
            register(&paths, &exe);
            let locations = InstallLocations { root: Some(dir.path().join("Craft Apps").display().to_string()), ..Default::default() };
            let http = Http::new().unwrap();
            let stop = Stop::default();
            let mut phases = Vec::new();
            let mut phase = |phase| phases.push(phase);
            let mut job = Job { http: &http, paths: &paths, stop: &stop, phase: &mut phase, install_locations: &locations, replace, downgrade: None };
            let mut commits = Vec::new();
            let mut commit = |record: &Record| {
                commits.push(record.clone());
                Ok(())
            };
            let result = install_at(app, &Release::default(), &Asset::default(), kind, None, None, &mut job, Some(&mut commit));
            assert!(result.unwrap_err().contains("installed outside the launcher"), "{kind:?}, replace={replace}");
            assert!(phases.is_empty());
            assert!(commits.is_empty());
            assert!(!paths.downloads().exists());
            assert!(!paths.manifest().exists());
            assert_eq!(std::fs::read(&exe).unwrap(), b"external program");
            assert_eq!(std::fs::read(&personal).unwrap(), b"personal files stay here");
        }
    }
}

#[test]
/// A registration written after download verification still wins before replacement or adoption.
fn a_registration_added_during_download_blocks_portable_replacement() {
    let app = CraftApp::by_id("photocraft").unwrap();
    for replace in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let (exe, personal) = external_copy(dir.path());
        let (asset, release) = cached_portable(&paths);
        let locations = InstallLocations { root: Some(dir.path().join("Craft Apps").display().to_string()), ..Default::default() };
        let http = Http::new().unwrap();
        let stop = Stop::default();
        let mut phases = Vec::new();
        let mut phase = |phase| {
            if phase == JobPhase::Verifying {
                register(&paths, &exe);
            }
            phases.push(phase);
        };
        let mut job = Job { http: &http, paths: &paths, stop: &stop, phase: &mut phase, install_locations: &locations, replace, downgrade: None };
        let mut commits = Vec::new();
        let mut commit = |record: &Record| {
            commits.push(record.clone());
            Ok(())
        };
        assert!(!paths.external_apps().exists());
        let result = install_at(app, &release, &asset, PackageKind::PortableZip, None, None, &mut job, Some(&mut commit));
        assert!(result.unwrap_err().contains("installed outside the launcher"), "replace={replace}");
        assert!(phases.contains(&JobPhase::Verifying));
        assert!(!phases.contains(&JobPhase::Installing));
        assert!(commits.is_empty());
        assert!(download_path(&paths, &asset).is_file(), "a refused install keeps its verified package");
        assert!(!paths.manifest().exists());
        assert_eq!(std::fs::read(&exe).unwrap(), b"external program");
        assert_eq!(std::fs::read(&personal).unwrap(), b"personal files stay here");
    }
}

#[test]
/// Previously managed ownership can disappear during download before an external registration wins.
fn a_managed_copy_removed_during_download_does_not_hide_a_new_registration() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::under(dir.path());
    let app = CraftApp::by_id("photocraft").unwrap();
    let (exe, personal) = external_copy(dir.path());
    let exe = std::fs::canonicalize(exe).unwrap();
    let previous = Record {
        version: "0.4.0".into(),
        kind: PackageKind::PortableZip,
        path: exe.clone(),
        installed_at: 1,
        files: vec![exe.clone()],
        needs_root: false,
    };
    let mut manifest = Manifest::default();
    manifest.apps.insert(app.id.into(), previous.clone());
    manifest.save(&paths.manifest()).unwrap();
    let installed = Installed { version: None, source: InstallSource::Launcher, location: exe.display().to_string(), package: None };
    let (asset, release) = cached_portable(&paths);
    let http = Http::new().unwrap();
    let stop = Stop::default();
    let locations = InstallLocations::default();
    let mut phases = Vec::new();
    let mut phase = |phase| {
        if phase == JobPhase::Verifying {
            std::fs::remove_file(paths.manifest()).unwrap();
            register(&paths, &exe);
        }
        phases.push(phase);
    };
    let mut job = Job { http: &http, paths: &paths, stop: &stop, phase: &mut phase, install_locations: &locations, replace: true, downgrade: None };
    let mut commits = Vec::new();
    let mut commit = |record: &Record| {
        commits.push(record.clone());
        Ok(())
    };
    let result = install_at(app, &release, &asset, PackageKind::PortableZip, Some(&installed), Some(&previous), &mut job, Some(&mut commit));
    assert!(result.unwrap_err().contains("installed outside the launcher"));
    assert!(phases.contains(&JobPhase::Verifying));
    assert!(!phases.contains(&JobPhase::Installing));
    assert!(commits.is_empty());
    assert!(!paths.manifest().exists());
    assert!(download_path(&paths, &asset).is_file());
    assert_eq!(std::fs::read(&exe).unwrap(), b"external program");
    assert_eq!(std::fs::read(&personal).unwrap(), b"personal files stay here");
}

#[test]
/// Current launcher records and known MSI installs retain precedence over external registrations.
fn managed_installations_keep_precedence_over_external_registrations() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::under(dir.path());
    let app = CraftApp::by_id("photocraft").unwrap();
    let (exe, _) = external_copy(dir.path());
    register(&paths, &exe);
    let managed = dir.path().join("managed").join("photocraft.exe");
    std::fs::create_dir_all(managed.parent().unwrap()).unwrap();
    std::fs::write(&managed, b"launcher program").unwrap();
    for source in [InstallSource::Launcher, InstallSource::Msi] {
        let installed = Installed { version: None, source, location: managed.display().to_string(), package: None };
        assert_eq!(check_external_install(app, Some(&installed), &paths), Ok(()));
    }
    // An earlier managed result is not current ownership: a fresh check must consult the disk.
    assert!(check_external_install(app, None, &paths).unwrap_err().contains("installed outside the launcher"));
    let mut manifest = Manifest::default();
    manifest.apps.insert(
        app.id.into(),
        Record { version: "0.5.0".into(), kind: PackageKind::PortableZip, path: managed, installed_at: 1, files: vec![], needs_root: false },
    );
    manifest.save(&paths.manifest()).unwrap();
    assert_eq!(check_external_install(app, None, &paths), Ok(()));
}
