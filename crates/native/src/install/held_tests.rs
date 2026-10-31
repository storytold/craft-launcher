use super::*;
use sha2::{Digest, Sha256};

fn package(dir: &Path, bytes: &[u8]) -> (std::path::PathBuf, String) {
    let file = dir.join("app.msi");
    std::fs::write(&file, bytes).unwrap();
    (file, Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect())
}

#[test]
fn a_package_with_its_checksum_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let (file, sha256) = package(dir.path(), b"package");
    assert!(hold(&file, &sha256.to_uppercase()).is_ok());
}

#[test]
fn a_package_changed_since_its_download_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (file, sha256) = package(dir.path(), b"package");
    std::fs::write(&file, b"swapped").unwrap();
    assert!(hold(&file, &sha256).unwrap_err().contains("changed after it was downloaded"));
}

/// While held, the package can't be changed, moved or deleted, but can still be read.
#[cfg(windows)]
#[test]
fn a_held_package_can_only_be_read() {
    let dir = tempfile::tempdir().unwrap();
    let (file, sha256) = package(dir.path(), b"package");
    let held = hold(&file, &sha256).unwrap();
    assert!(std::fs::OpenOptions::new().write(true).open(&file).is_err(), "written");
    assert!(std::fs::rename(&file, dir.path().join("moved.msi")).is_err(), "moved");
    assert!(std::fs::remove_file(&file).is_err(), "deleted");
    assert_eq!(std::fs::read(&file).unwrap(), b"package");
    drop(held);
    assert!(std::fs::remove_file(&file).is_ok());
}

/// Renaming a folder on the way to a held package is refused, so its resolved path keeps naming it.
#[cfg(windows)]
#[test]
fn the_folders_above_a_held_package_cannot_be_renamed() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("downloads");
    std::fs::create_dir(&parent).unwrap();
    let (file, sha256) = package(&parent, b"package");
    let held = hold(&file, &sha256).unwrap();
    assert!(std::fs::rename(&parent, dir.path().join("moved")).is_err(), "parent renamed");
    drop(held);
    assert!(std::fs::rename(&parent, dir.path().join("moved")).is_ok());
}

#[test]
fn only_a_development_build_skips_the_publisher_check_for_an_unsigned_launcher() {
    assert!(!skips_unsigned_self(false), "a release build must refuse");
    assert!(skips_unsigned_self(true));
}

#[test]
fn only_an_official_apps_all_users_install_needs_the_publisher() {
    let official = CraftApp::by_id("photocraft").unwrap();
    let community = artcraft_launcher_core::catalog::SUITE.iter().find(|a| a.kind == AppKind::Community).unwrap();
    assert!(needs_publisher(official, true));
    assert!(!needs_publisher(official, false));
    assert!(!needs_publisher(community, true));
}
