//! Nonexecuted synthetic mechanics fixtures, not signed PhotoCraft or platform acceptance.
#![allow(clippy::unwrap_used)] // Fixture setup/assertions fail immediately; production lint is unchanged.
use crate::manifest::{Manifest, Record};
use crate::nightly_fs::Dir;
use crate::nightly_profile::Adapter;
use crate::nightly_transaction::{self, Checkpoint, Lease};
use crate::paths::Paths;
use artcraft_launcher_core::PackageKind;
use artcraft_launcher_core::nightly::InstallChannel;
use std::path::Path;

const APP: &str = "photocraft";
const ADAPTER: Adapter = Adapter::PhotoCraftWindowsV1;

fn fixture() -> (tempfile::TempDir, Paths, Record) {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::under(temp.path()).for_channel(InstallChannel::Nightly);
    let root = paths.apps().join(APP);
    std::fs::create_dir_all(&root).unwrap();
    // Required source-owned filenames, but intentionally not executable/PE or signed bytes.
    std::fs::write(root.join("photocraft.exe"), b"synthetic nonexecutable mechanics fixture").unwrap();
    std::fs::write(root.join("NOTICE"), b"synthetic owned notice").unwrap();
    std::fs::write(root.join("foreign.txt"), b"unrecorded program-root data").unwrap();
    let profile = ADAPTER.profile_root(&paths).unwrap();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("preferences"), b"persistent profile data").unwrap();
    let record = Record {
        path: root.join("photocraft.exe"),
        files: vec![root.join("photocraft.exe"), root.join("NOTICE")],
        version: "synthetic mechanics-only fixture; not a qualified app version".into(),
        kind: PackageKind::PortableZip,
        installed_at: 1,
        needs_root: false,
    };
    let mut manifest = Manifest::default();
    manifest.apps.insert(APP.into(), record.clone());
    manifest.save(&paths.manifest()).unwrap();
    (temp, paths, record)
}
fn retained(paths: &Paths) {
    assert_eq!(std::fs::read(paths.apps().join(APP).join("foreign.txt")).unwrap(), b"unrecorded program-root data");
    assert_eq!(std::fs::read(ADAPTER.profile_root(paths).unwrap().join("preferences")).unwrap(), b"persistent profile data");
}
fn removed(paths: &Paths) {
    assert!(!Manifest::load(&paths.manifest()).apps.contains_key(APP));
    assert!(!paths.apps().join(APP).join("photocraft.exe").exists());
    assert!(!paths.apps().join(APP).join("NOTICE").exists());
    assert!(!paths.apps().join(".nightly-photocraft-preparation-v1").exists());
    assert!(!paths.data.join(".nightly-photocraft-journal-v1.jsonl").exists());
    retained(paths);
}
fn commit_removal(lease: &Lease, record: &Record) -> Result<(), String> {
    nightly_transaction::remove(lease, record, &mut || lease.publish(Some(record), None).map(|_| ()), &mut |_| Ok(()))
}

#[test]
fn held_directory_ancestors_refuse_rename_and_delete_until_released() {
    let temp = tempfile::tempdir().unwrap();
    let parent = temp.path().join("held parent");
    let child = parent.join("held child");
    std::fs::create_dir_all(&child).unwrap();
    let held = Dir::open(&child).unwrap();
    let moved = temp.path().join("moved parent");
    assert!(std::fs::rename(&parent, &moved).is_err(), "a held ancestor must deny rename");
    assert!(std::fs::remove_dir(&child).is_err(), "a held child must deny deletion");
    assert!(child.is_dir());
    drop(held);
    std::fs::rename(&parent, &moved).unwrap();
    std::fs::remove_dir(moved.join("held child")).unwrap();
}

#[test]
fn exclusive_move_preserves_both_objects_when_destination_exists() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::write(temp.path().join("source"), b"source identity").unwrap();
    std::fs::write(temp.path().join("destination"), b"existing destination").unwrap();
    let root = Dir::open(temp.path()).unwrap();
    let source_id = root.info(Path::new("source")).unwrap().unwrap().0;
    let destination_id = root.info(Path::new("destination")).unwrap().unwrap().0;
    assert!(root.move_to(Path::new("source"), &root, Path::new("destination"), &source_id).is_err());
    assert_eq!(root.info(Path::new("source")).unwrap().unwrap().0, source_id);
    assert_eq!(root.info(Path::new("destination")).unwrap().unwrap().0, destination_id);
    assert_eq!(std::fs::read(temp.path().join("source")).unwrap(), b"source identity");
    assert_eq!(std::fs::read(temp.path().join("destination")).unwrap(), b"existing destination");
    std::fs::rename(temp.path().join("destination"), temp.path().join("quarantined destination")).unwrap();
    root.move_to(Path::new("source"), &root, Path::new("destination"), &source_id).unwrap();
    assert_eq!(root.info(Path::new("destination")).unwrap().unwrap().0, source_id);
}

#[test]
fn observed_junction_is_refused_without_writing_through_it() {
    use std::os::windows::fs::MetadataExt;
    let temp = tempfile::tempdir().unwrap();
    let target = temp.path().join("junction target");
    let junction = temp.path().join("untrusted junction");
    std::fs::create_dir(&target).unwrap();
    std::fs::write(target.join("foreign.txt"), b"untouched junction target").unwrap();
    // Same unprivileged NTFS junction fixture mechanism as portable.rs's ownership regression.
    let status = std::process::Command::new("cmd.exe").args(["/D", "/C", "mklink", "/J"]).arg(&junction).arg(&target).status().unwrap();
    assert!(status.success(), "junction creation must actually run; inability is failure, not skipped coverage");
    assert_ne!(std::fs::symlink_metadata(&junction).unwrap().file_attributes() & 0x400, 0);
    let root = Dir::open(temp.path()).unwrap();
    assert!(root.info(Path::new("untrusted junction")).is_err());
    assert!(root.child(Path::new("untrusted junction")).is_err());
    assert!(Dir::open(&junction).is_err());
    assert!(Dir::ensure(&junction.join("must not be created")).is_err());
    assert!(!target.join("must not be created").exists());
    assert_eq!(std::fs::read(target.join("foreign.txt")).unwrap(), b"untouched junction target");
    std::fs::remove_dir(&junction).unwrap();
}

#[test]
fn common_removal_partial_error_restores_owned_files_then_retries_and_cleans() {
    let (_temp, paths, record) = fixture();
    let root = Dir::open(&paths.apps().join(APP)).unwrap();
    let before = root.info(Path::new("photocraft.exe")).unwrap().unwrap().0;
    let lease = Lease::acquire(&paths, ADAPTER).unwrap();
    let mut moves = 0;
    let result = nightly_transaction::remove(&lease, &record, &mut || panic!("partial mutation must not commit"), &mut |point| {
        if point == Checkpoint::BeforeRemoval {
            moves += 1;
            if moves == 2 {
                return Err("controlled refusal after first staged owned file".into());
            }
        }
        Ok(())
    });
    assert!(result.is_err());
    assert_eq!(moves, 2);
    assert_eq!(root.info(Path::new("photocraft.exe")).unwrap().unwrap().0, before);
    drop(root); // Release the observation-only no-delete handle before the real removal retry.
    assert_eq!(std::fs::read(&record.path).unwrap(), b"synthetic nonexecutable mechanics fixture");
    assert_eq!(std::fs::read(paths.apps().join(APP).join("NOTICE")).unwrap(), b"synthetic owned notice");
    assert_eq!(Manifest::load(&paths.manifest()).apps.get(APP), Some(&record));
    retained(&paths);
    commit_removal(&lease, &record).unwrap();
    removed(&paths);
}

#[test]
fn common_removal_recovers_native_process_exit_before_and_after_commit() {
    const TEST: &str = "nightly_windows_mechanics_tests::common_removal_recovers_native_process_exit_before_and_after_commit";
    const ROOT: &str = "ARTCRAFT_WINDOWS_MECHANICS_ROOT";
    const PHASE: &str = "ARTCRAFT_WINDOWS_MECHANICS_PHASE";
    if let Some(root) = std::env::var_os(ROOT) {
        let paths = Paths::under(Path::new(&root)).for_channel(InstallChannel::Nightly);
        let lease = Lease::acquire(&paths, ADAPTER).unwrap();
        let record = lease.manifest().unwrap().apps.get(APP).cloned().unwrap();
        let committed = std::env::var(PHASE).unwrap() == "committed";
        let mut moves = 0;
        let _ = nightly_transaction::remove(&lease, &record, &mut || lease.publish(Some(&record), None).map(|_| ()), &mut |point| {
            if point == Checkpoint::BeforeRemoval {
                moves += 1;
            }
            if (!committed && point == Checkpoint::BeforeRemoval && moves == 2) || (committed && point == Checkpoint::AfterCommit) {
                std::process::exit(47);
            }
            Ok(())
        });
        panic!("native removal interruption boundary not reached");
    }
    for phase in ["staged", "committed"] {
        let (temp, paths, record) = fixture();
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(ROOT, temp.path())
            .env(PHASE, phase)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(47), "real process exit must occur without unwinding");
        let lease = Lease::acquire(&paths, ADAPTER).unwrap();
        if phase == "staged" {
            assert_eq!(lease.manifest().unwrap().apps.get(APP), Some(&record));
            assert_eq!(std::fs::read(&record.path).unwrap(), b"synthetic nonexecutable mechanics fixture");
            assert_eq!(std::fs::read(paths.apps().join(APP).join("NOTICE")).unwrap(), b"synthetic owned notice");
            retained(&paths);
            commit_removal(&lease, &record).unwrap();
        }
        removed(&paths);
        let mut subsequent = lease.prepare().unwrap();
        assert_eq!(subsequent.abort("subsequent preparation probe".into()), "subsequent preparation probe");
        removed(&paths);
    }
}

#[test]
fn removal_accepts_the_record_the_portable_installer_produces_with_ordinary_paths() {
    let temp = tempfile::tempdir().unwrap();
    // Ordinary drive-letter Paths, as the launcher builds them; the installer canonicalizes.
    let paths = Paths::under(temp.path()).for_channel(InstallChannel::Nightly);
    assert!(!paths.apps().to_string_lossy().starts_with(r"\\?\"));
    let app = artcraft_launcher_core::CraftApp::by_id(APP).unwrap();
    let file = crate::portable::tests::archive(
        temp.path(),
        "synthetic-nightly.zip",
        &[("photocraft.exe", b"synthetic nonexecutable mechanics fixture"), ("NOTICE", b"synthetic owned notice")],
    );
    let record = crate::portable::install_checked(
        crate::portable::Package {
            app,
            file: &file,
            version: "1.0.0-nightly.20261123.1.1.gabcdefabcdef",
            previous: None,
            paths: &paths,
            directory: None,
            replace: false,
        },
        &|files, entries| ADAPTER.validate_layout(files, entries),
        &mut |record| {
            let mut manifest = Manifest::default();
            manifest.apps.insert(APP.into(), record.clone());
            manifest.save(&paths.manifest())
        },
    )
    .unwrap();
    assert!(record.path.to_string_lossy().starts_with(r"\\?\"), "the installer must produce its verbatim spelling: {}", record.path.display());
    let lease = Lease::acquire(&paths, ADAPTER).unwrap();
    commit_removal(&lease, &record).unwrap();
    assert!(!Manifest::load(&paths.manifest()).apps.contains_key(APP));
    assert!(!paths.apps().join(APP).join("photocraft.exe").exists());
    assert!(!paths.apps().join(APP).join("NOTICE").exists());
}
