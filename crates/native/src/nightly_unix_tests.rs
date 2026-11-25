//! Synthetic fixtures generated here, not copied from an app or an external archive.
use super::*;
use artcraft_launcher_core::nightly::InstallChannel;
use artcraft_launcher_core::{Arch, CraftApp, Platform};

fn adapter(app: &str, os: Os, arch: Arch) -> Adapter {
    crate::nightly_profile::for_app(CraftApp::by_id(app).unwrap(), Platform { os, arch }).unwrap()
}

fn cadcraft_linux() -> Adapter {
    adapter("cadcraft", Os::Linux, Arch::X64)
}
fn same_directory(path: &Path, identity: &std::fs::Metadata) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink() && meta.dev() == identity.dev() && meta.ino() == identity.ino())
}

// Unlike the old no-op commit fixtures, these callbacks now exercise a real persisted
// previous/next record. Failing callbacks still return BEFORE any manifest replacement.
#[cfg(target_os = "linux")]
fn install(package: Package<'_>, commit: &mut dyn FnMut(&Record) -> Result<(), String>) -> Result<Record, String> {
    let paths = package.paths.clone();
    super::install(package, &mut |record| {
        commit(record)?;
        let mut manifest = crate::nightly_transaction::stored_manifest(&paths)?;
        manifest.apps.insert("photocraft".into(), record.clone());
        manifest.save(&paths.manifest())
    })
}
#[cfg(target_os = "linux")]
fn remove(adapter: Adapter, record: &Record, paths: &Paths) -> Result<(), String> {
    adapter.validate_record(paths, record)?;
    let lease = crate::nightly_transaction::Lease::acquire(paths, adapter)?;
    crate::nightly_transaction::remove(&lease, record, &mut || lease.publish(Some(record), None).map(|_| ()), &mut |_| Ok(()))
}

const VERSION: &str = "1.0.0-nightly.20261121.123456.1.gabcdefabcdef";

pub(crate) fn paths(root: &Path) -> Paths {
    Paths::under(&root.canonicalize().unwrap()).for_channel(InstallChannel::Nightly)
}
#[cfg(target_os = "linux")]
pub(crate) fn archive(path: &Path, entries: &[(&str, &[u8], u32)]) {
    let output = std::fs::File::create(path).unwrap();
    let encoder = flate2::write::GzEncoder::new(output, flate2::Compression::default());
    let mut tar = tar::Builder::new(encoder);
    for (name, bytes, mode) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(*mode);
        header.set_cksum();
        tar.append_data(&mut header, name, *bytes).unwrap();
    }
    // Synthetic resource, not a qualified or user-approved visual badge.
    if entries.iter().any(|(name, _, _)| *name == "photocraft") {
        let bytes = b"synthetic badge fixture";
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o600);
        header.set_cksum();
        tar.append_data(&mut header, "photocraft-nightly.png", &bytes[..]).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
}
#[cfg(any(target_os = "linux", test))]
pub(crate) fn program() -> Vec<u8> {
    // ELF64 little-endian x86_64 executable header; never executed by a test.
    let mut bytes = vec![0; 64];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    bytes[16..20].copy_from_slice(&[2, 0, 62, 0]);
    bytes
}
#[cfg(target_os = "linux")]
pub(crate) fn package<'a>(file: &'a Path, paths: &'a Paths, previous: Option<&'a Record>) -> Package<'a> {
    // Low-level callback tests explicitly persist the supplied previous record, just as a
    // production worker’s strict snapshot does. Never overwrite existing or malformed metadata.
    if let Some(record) = previous
        && !paths.manifest().exists()
    {
        let mut manifest = crate::manifest::Manifest::default();
        manifest.apps.insert("photocraft".into(), record.clone());
        manifest.save(&paths.manifest()).unwrap();
    }
    Package {
        adapter: Adapter::PhotoCraftLinuxV1,
        file,
        version: VERSION,
        previous,
        paths,
        archive_sha256: crate::http::sha256_file(file).unwrap(),
        lease: None,
    }
}

#[cfg(target_os = "linux")]
#[test]
fn root_replacement_before_first_copy_is_refused() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let file = temp.path().join("nightly.tar.gz");
    archive(&file, &[("photocraft", &program(), 0o755)]);
    let root = paths.apps().join("photocraft");
    let moved = temp.path().join("reserved-root-moved-away");
    let mut committed = false;
    let mut foreign_identity = None;
    let result = install_with_checkpoints(
        package(&file, &paths, None),
        &mut |_| {
            committed = true;
            Ok(())
        },
        &mut |point| {
            if point == Checkpoint::RootReserved {
                std::fs::rename(&root, &moved).unwrap();
                std::fs::create_dir(&root).unwrap();
                foreign_identity = Some(std::fs::metadata(&root).unwrap());
            }
            Ok(())
        },
    );
    let foreign_entries: Vec<_> = std::fs::read_dir(&root).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert!(
        !committed && result.is_err(),
        "replacement root was adopted: committed={committed}, result={result:?}, foreign entries={foreign_entries:?}"
    );
    assert!(same_directory(&root, &foreign_identity.unwrap()));
    assert!(foreign_entries.is_empty());
    assert_eq!(std::fs::read_dir(moved).unwrap().count(), 0);
    let error = result.unwrap_err();
    let recovery = Path::new(error.split("Recovery files: ").nth(1).unwrap());
    assert_eq!(std::fs::read(recovery.join("unpacked/photocraft")).unwrap(), program());
}

#[cfg(target_os = "linux")]
#[test]
fn replacement_after_backup_keeps_old_bytes_and_never_restores_into_foreign_root() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let file = temp.path().join("nightly.tar.gz");
    let mut old_program = program();
    old_program.extend_from_slice(b"old program");
    archive(&file, &[("photocraft", &old_program, 0o755), ("NOTICE", b"old notice", 0o644)]);
    let previous = install(package(&file, &paths, None), &mut |_| Ok(())).unwrap();
    let root = paths.apps().join("photocraft");
    let moved = temp.path().join("backed-up-root-moved-away");
    std::fs::write(root.join("user.txt"), b"existing foreign data").unwrap();
    archive(&file, &[("photocraft", &program(), 0o755), ("NOTICE", b"new notice", 0o644)]);
    let mut committed = false;
    let mut foreign_identity = None;
    let error = install_with_checkpoints(
        package(&file, &paths, Some(&previous)),
        &mut |_| {
            committed = true;
            Ok(())
        },
        &mut |point| {
            if point == Checkpoint::BackupsSaved {
                std::fs::rename(&root, &moved).unwrap();
                std::fs::create_dir(&root).unwrap();
                foreign_identity = Some(std::fs::metadata(&root).unwrap());
                std::fs::write(root.join("sentinel"), b"arrived foreign data").unwrap();
            }
            Ok(())
        },
    )
    .unwrap_err();
    assert!(!committed);
    assert!(same_directory(&root, &foreign_identity.unwrap()));
    assert_eq!(std::fs::read(root.join("sentinel")).unwrap(), b"arrived foreign data");
    assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1);
    assert_eq!(std::fs::read(moved.join("user.txt")).unwrap(), b"existing foreign data");
    let recovery = Path::new(error.split("Recovery files: ").nth(1).unwrap());
    assert_eq!(std::fs::read(moved.join("photocraft")).unwrap(), old_program);
    assert_eq!(std::fs::read(moved.join("NOTICE")).unwrap(), b"old notice");
    // Restore is fd-relative into the held original root, never the arriving directory.
    assert_eq!(std::fs::read(recovery.join("unpacked/NOTICE")).unwrap(), b"new notice");
}

#[cfg(target_os = "linux")]
#[test]
fn root_replacement_at_final_checkpoint_never_calls_commit() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let file = temp.path().join("nightly.tar.gz");
    archive(&file, &[("photocraft", &program(), 0o755)]);
    let root = paths.apps().join("photocraft");
    let moved = temp.path().join("copied-root-moved-away");
    let mut committed = false;
    let result = install_with_checkpoints(
        package(&file, &paths, None),
        &mut |_| {
            committed = true;
            Ok(())
        },
        &mut |point| {
            if point == Checkpoint::BeforeCommit {
                std::fs::rename(&root, &moved).unwrap();
                std::fs::create_dir(&root).unwrap();
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(!committed);
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 0);
    let error = result.unwrap_err();
    let recovery = Path::new(error.split("Recovery files: ").nth(1).unwrap());
    assert_eq!(std::fs::read(recovery.join("unpacked/photocraft")).unwrap(), program());
}

#[test]
fn parent_equal_to_root_still_requires_the_reserved_identity() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path().canonicalize().unwrap();
    let root = base.join("program");
    std::fs::create_dir(&root).unwrap();
    let held = crate::nightly_fs::Dir::open(&root).unwrap();
    let moved = base.join("moved");
    std::fs::rename(&root, &moved).unwrap();
    std::fs::create_dir(&root).unwrap();
    assert_ne!(held.id().unwrap(), crate::nightly_fs::Dir::open(&root).unwrap().id().unwrap());
    // The empty relative parent is still the held ORIGINAL directory, not its replacement.
    held.file(Path::new("owned"), true).unwrap();
    assert!(moved.join("owned").is_file());
    assert_eq!(std::fs::read_dir(root).unwrap().count(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn failed_fresh_commit_is_retryable_and_never_removes_an_unowned_directory() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let file = temp.path().join("nightly.tar.gz");
    archive(&file, &[("photocraft", &program(), 0o755)]);
    let root = paths.apps().join("photocraft");
    assert!(install(package(&file, &paths, None), &mut |_| Err("manifest failed".into())).is_err());
    assert!(!root.exists(), "rollback must release the newly-created empty folder for a retry");
    let record = install(package(&file, &paths, None), &mut |_| Ok(())).unwrap();
    assert!(record.path.is_file());
    remove(Adapter::PhotoCraftLinuxV1, &record, &paths).unwrap();
    std::fs::create_dir_all(&root).unwrap();
    assert!(install(package(&file, &paths, None), &mut |_| Err("must not commit".into())).is_err());
    assert!(root.is_dir(), "even an empty pre-existing directory is unowned");
}

#[cfg(target_os = "linux")]
#[test]
fn update_failure_restores_owned_bytes_modes_and_keeps_foreign_files_and_profile() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let file = temp.path().join("nightly.tar.gz");
    archive(&file, &[("photocraft", &program(), 0o755), ("NOTICE", b"old notice", 0o644)]);
    let record = install(package(&file, &paths, None), &mut |_| Ok(())).unwrap();
    std::fs::set_permissions(&record.path, std::fs::Permissions::from_mode(0o751)).unwrap();
    let old_program = std::fs::read(&record.path).unwrap();
    let root = paths.apps().join("photocraft");
    let foreign = root.join("user.txt");
    std::fs::write(&foreign, b"foreign").unwrap();
    let profile = Adapter::PhotoCraftLinuxV1.profile_root(&paths).unwrap();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("preferences"), b"user preferences").unwrap();
    let mut changed = program();
    changed.extend_from_slice(b"different replacement executable");
    archive(&file, &[("photocraft", &changed, 0o755), ("NOTICE", b"new notice", 0o644)]);
    assert!(install(package(&file, &paths, Some(&record)), &mut |_| Err("commit failed".into())).is_err());
    assert_eq!(std::fs::read(root.join("NOTICE")).unwrap(), b"old notice");
    assert_eq!(std::fs::read(&record.path).unwrap(), old_program);
    assert_eq!(std::fs::metadata(&record.path).unwrap().permissions().mode() & 0o777, 0o751);
    assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign");
    remove(Adapter::PhotoCraftLinuxV1, &record, &paths).unwrap();
    assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign");
    assert_eq!(std::fs::read(profile.join("preferences")).unwrap(), b"user preferences");
}

#[cfg(target_os = "linux")]
#[test]
fn failed_commit_preserves_arriving_files_in_a_fresh_owned_directory() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let file = temp.path().join("nightly.tar.gz");
    archive(&file, &[("photocraft", &program(), 0o755)]);
    let arriving = paths.apps().join("photocraft/arriving.txt");
    assert!(
        install(package(&file, &paths, None), &mut |_| {
            std::fs::write(&arriving, b"arrived").unwrap();
            Err("commit failed".into())
        })
        .is_err()
    );
    assert_eq!(std::fs::read(arriving).unwrap(), b"arrived");
}

#[cfg(target_os = "linux")]
#[test]
fn failed_fresh_commit_never_removes_an_arriving_empty_program_directory() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let file = temp.path().join("nightly.tar.gz");
    archive(&file, &[("photocraft", &program(), 0o755)]);
    let program_root = paths.apps().join("photocraft");
    let moved = temp.path().join("moved-program");
    let mut arriving_identity = None;
    let error = install(package(&file, &paths, None), &mut |_| {
        std::fs::rename(&program_root, &moved).unwrap();
        std::fs::create_dir(&program_root).unwrap();
        arriving_identity = Some(std::fs::metadata(&program_root).unwrap());
        Err("commit failed".into())
    })
    .unwrap_err();
    assert!(error.contains("changed identity"), "{error}");
    assert!(same_directory(&program_root, &arriving_identity.unwrap()));
    assert_eq!(std::fs::read_dir(program_root).unwrap().count(), 0);
    assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 0);
    let recovery = Path::new(error.split("Recovery files: ").nth(1).unwrap());
    assert_eq!(std::fs::read(recovery.join("unpacked/photocraft")).unwrap(), program());
}

#[cfg(target_os = "linux")]
#[test]
fn rollback_does_not_truncate_a_file_that_arrived_in_place_of_a_new_program() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let file = temp.path().join("nightly.tar.gz");
    archive(&file, &[("photocraft", &program(), 0o755)]);
    let record = install(package(&file, &paths, None), &mut |_| Ok(())).unwrap();
    let elsewhere = temp.path().join("new-program");
    let error = install(package(&file, &paths, Some(&record)), &mut |new| {
        std::fs::rename(&new.path, &elsewhere).unwrap();
        std::fs::write(&new.path, b"arriving foreign data").unwrap();
        Err("commit failed".into())
    })
    .unwrap_err();
    assert!(error.contains("Recovery files:"), "{error}");
    assert_eq!(std::fs::read(&record.path).unwrap(), b"arriving foreign data");
    let recovery = error.split("Recovery files: ").nth(1).unwrap();
    assert_eq!(std::fs::read(Path::new(recovery).join("backup/photocraft")).unwrap(), program());
}

#[cfg(target_os = "linux")]
#[test]
fn unrecorded_collisions_hardlinks_and_changed_parent_aliases_refuse_before_replacement() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let file = temp.path().join("nightly.tar.gz");
    archive(&file, &[("photocraft", &program(), 0o755)]);
    let record = install(package(&file, &paths, None), &mut |_| Ok(())).unwrap();
    let foreign = paths.apps().join("photocraft/NOTICE");
    std::fs::write(&foreign, b"foreign notice").unwrap();
    archive(&file, &[("photocraft", &program(), 0o755), ("NOTICE", b"payload notice", 0o644)]);
    assert!(install(package(&file, &paths, Some(&record)), &mut |_| Ok(())).is_err());
    assert_eq!(std::fs::read(&foreign).unwrap(), b"foreign notice");
    let link = temp.path().join("stable-hardlink");
    std::fs::hard_link(&record.path, &link).unwrap();
    assert!(Adapter::PhotoCraftLinuxV1.validate_record(&paths, &record).is_err());
    std::fs::remove_file(&link).unwrap();
    let moved = temp.path().join("moved-program");
    std::fs::rename(paths.apps().join("photocraft"), &moved).unwrap();
    std::os::unix::fs::symlink(&moved, paths.apps().join("photocraft")).unwrap();
    assert!(Adapter::PhotoCraftLinuxV1.validate_record(&paths, &record).is_err());
    assert_eq!(std::fs::read(moved.join("photocraft")).unwrap(), program());
}

// Setup failures (such as platform admission) must fail the test, never count as a rejection.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn extract_rejected(file: &Path, destination: &Path) -> Result<Inventory, String> {
    let paths = paths(destination.parent().unwrap());
    let lease = crate::nightly_transaction::Lease::acquire(&paths, Adapter::PhotoCraftLinuxV1).expect("Nightly lease setup");
    let mut preparation = lease.prepare().expect("Nightly preparation setup");
    preparation.directories(Path::new("unpacked")).expect("Nightly staging setup");
    let result = extract_tar(file, &mut preparation);
    preparation.abort("archive fixture cleanup".into());
    result
}

#[cfg(any(target_os = "linux", target_os = "macos"))] // Needs an admitted Nightly profile.
#[test]
fn archive_rejects_traversal_links_special_files_aliases_duplicates_and_size_bombs() {
    let temp = tempfile::tempdir().unwrap();
    let file = temp.path().join("bad.tar.gz");
    let out = temp.path().join("out");
    std::fs::create_dir(&out).unwrap();
    for kind in [tar::EntryType::Symlink, tar::EntryType::Link, tar::EntryType::Fifo, tar::EntryType::Char] {
        let encoder = flate2::write::GzEncoder::new(std::fs::File::create(&file).unwrap(), flate2::Compression::default());
        let mut tar = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o644);
        header.set_entry_type(kind);
        header.set_cksum();
        tar.append_data(&mut header, "photocraft", &b""[..]).unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        assert!(extract_rejected(&file, &out).is_err());
    }
    for name in ["../escape", "/absolute", "a/../escape", "a\\escape"] {
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o644);
        header.as_mut_bytes()[..100].fill(0);
        header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
        header.set_cksum();
        let mut gzip = flate2::write::GzEncoder::new(std::fs::File::create(&file).unwrap(), flate2::Compression::default());
        gzip.write_all(header.as_bytes()).unwrap();
        gzip.write_all(&[0u8; 1024]).unwrap();
        gzip.finish().unwrap();
        assert!(extract_rejected(&file, &out).is_err(), "{name}");
    }
    // An extension header claiming a giant GNU long-name body is refused before reading it.
    let mut header = tar::Header::new_gnu();
    header.set_size(MAX_BYTES + 1);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::GNULongName);
    header.set_cksum();
    let mut gzip = flate2::write::GzEncoder::new(std::fs::File::create(&file).unwrap(), flate2::Compression::default());
    gzip.write_all(header.as_bytes()).unwrap();
    gzip.finish().unwrap();
    assert!(extract_rejected(&file, &out).is_err());
    for unsafe_path in ["../escape", "/absolute", "a/../escape", "a/./escape", "a//escape", "a\\escape", "C:escape", "name.", "name "] {
        assert!(safe_relative(Path::new(unsafe_path)).is_err(), "{unsafe_path}");
    }
    let mut inventory = Inventory::new();
    inventory.add("photocraft".into(), 1, false).unwrap();
    assert!(inventory.add("PhotoCraft".into(), 1, false).is_err());
    assert!(Inventory::new().add("bomb".into(), MAX_BYTES + 1, false).is_err());
    assert!(
        layout(
            Adapter::PhotoCraftLinuxV1,
            &["photocraft".into(), "photocraft-nightly.png".into()],
            &["photocraft".into(), "photocraft-nightly.png".into(), "photocraft-cli".into()]
        )
        .is_err()
    );
    assert!(
        layout(
            Adapter::PhotoCraftLinuxV1,
            &["photocraft".into(), "photocraft-nightly.png".into()],
            &["photocraft".into(), "photocraft-nightly.png".into(), "portable.txt".into()]
        )
        .is_err()
    );
}

#[test]
fn app_specific_stable_roots_refuse_ancestors_descendants_and_aliases_but_allow_launcher_siblings() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let shared = root.join(".local/share");
    let stable = shared.join("photocraft");
    std::fs::create_dir_all(&stable).unwrap();
    std::fs::write(stable.join("sentinel"), b"stable").unwrap();
    for candidate in [&stable, &stable.join("nested"), &shared] {
        assert!(disjoint(std::slice::from_ref(candidate), std::slice::from_ref(&stable)).is_err());
    }
    assert!(disjoint(&[shared.join("artcraft-launcher/pre-release/v1")], std::slice::from_ref(&stable)).is_ok());
    let alias = root.join("alias");
    std::os::unix::fs::symlink(&stable, &alias).unwrap();
    assert!(disjoint(&[alias.join("nightly")], std::slice::from_ref(&stable)).is_err());
    assert_eq!(std::fs::read(stable.join("sentinel")).unwrap(), b"stable");
}

#[cfg(any(target_os = "linux", target_os = "macos"))] // Needs an admitted Nightly profile.
#[test]
fn preexisting_empty_nested_program_directories_are_not_implicitly_adopted() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let lease = crate::nightly_transaction::Lease::acquire(&paths, Adapter::PhotoCraftLinuxV1).unwrap();
    let mut preparation = lease.prepare().unwrap();
    preparation.directories(Path::new("unpacked")).unwrap();
    let foreign = preparation.path().join("unpacked/foreign");
    std::fs::create_dir(&foreign).unwrap();
    let identity = std::fs::metadata(&foreign).unwrap();
    assert!(preparation.directories(Path::new("unpacked/foreign")).is_err());
    assert!(same_directory(&foreign, &identity));
    let first = preparation.directories(Path::new("unpacked/owned")).unwrap().id().unwrap();
    let again = preparation.directories(Path::new("unpacked/owned")).unwrap().id().unwrap();
    assert_eq!(first, again);
    assert!(preparation.abort("test abort".into()).contains("preserved"));
    assert!(same_directory(&foreign, &identity));
}

#[cfg(target_os = "linux")]
#[test]
fn a_custom_stable_install_record_is_a_read_only_fence_before_nightly_extraction() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let stable_manifest = Paths::under(temp.path()).manifest();
    let mut manifest = crate::manifest::Manifest::default();
    manifest.apps.insert(
        "photocraft".into(),
        Record {
            version: "1.0.0".into(),
            kind: artcraft_launcher_core::PackageKind::AppImage,
            path: paths.data.join("custom-stable.AppImage"),
            installed_at: 1,
            files: vec![],
            needs_root: false,
        },
    );
    manifest.save(&stable_manifest).unwrap();
    let bytes = std::fs::read(&stable_manifest).unwrap();
    assert!(validate_namespaces(Adapter::PhotoCraftLinuxV1, &paths).is_err());
    assert_eq!(std::fs::read(stable_manifest).unwrap(), bytes);
    assert!(!paths.apps().exists());
}

#[test]
fn mac_full_nightly_identity_has_no_numeric_or_leading_v_fallback() {
    assert!(exact_full_version(Some(VERSION), VERSION));
    assert!(exact_full_version(Some(VERSION), &format!("v{VERSION}")));
    for found in [None, Some("1.0.0"), Some("1.0.0-beta.1"), Some("1.0.0-nightly.other")] {
        assert!(!exact_full_version(found, VERSION));
    }
    assert!(!exact_full_version(Some(&format!("v{VERSION}")), VERSION));
    assert!(!exact_full_version(Some(VERSION), "1.0.0"));
}

#[cfg(target_os = "linux")]
#[test]
fn archive_mutation_after_extraction_refuses_before_old_program_or_records_move() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let file = temp.path().join("new-signed-successor.tar.gz");
    archive(&file, &[("photocraft", &program(), 0o755)]);
    let old = install(package(&file, &paths, None), &mut |_| Ok(())).unwrap();
    let before = std::fs::read(&old.path).unwrap();
    let profile = Adapter::PhotoCraftLinuxV1.profile_root(&paths).unwrap();
    std::fs::create_dir_all(&profile).unwrap();
    let personal = profile.join("private.txt");
    std::fs::write(&personal, b"do not change").unwrap();
    let mut manifest = crate::manifest::Manifest::default();
    manifest.apps.insert("photocraft".into(), old.clone());
    manifest.save(&paths.manifest()).unwrap();
    let manifest_before = std::fs::read(paths.manifest()).unwrap();
    let mut replacement = program();
    replacement.extend_from_slice(b"new executable");
    archive(&file, &[("photocraft", &replacement, 0o755)]);
    let mut committed = false;
    let result = install_with_checkpoints(
        package(&file, &paths, Some(&old)),
        &mut |_| {
            committed = true;
            Ok(())
        },
        &mut |point| {
            if point == Checkpoint::ArchiveExtracted {
                std::fs::write(&file, b"changed after extraction").unwrap();
            }
            Ok(())
        },
    );
    assert!(result.unwrap_err().contains("authenticated Nightly archive changed"));
    assert!(!committed);
    assert_eq!(std::fs::read(old.path).unwrap(), before);
    assert_eq!(std::fs::read(personal).unwrap(), b"do not change");
    assert_eq!(std::fs::read(paths.manifest()).unwrap(), manifest_before);
}

#[test]
fn mac_layout_accepts_a_stapled_notarization_ticket_but_no_other_bundle_additions() {
    let contents = Path::new("PhotoCraft Nightly.app/Contents");
    let mut files: Vec<PathBuf> =
        ["MacOS/PhotoCraft", "Info.plist", "Resources/photocraft-nightly.icns", "_CodeSignature/CodeResources", "CodeResources"]
            .iter()
            .map(|file| contents.join(file))
            .collect();
    assert!(layout(Adapter::PhotoCraftMacV1, &files, &files).is_ok());
    for extra in ["CodeResources.bak", "Frameworks/Helper.framework", "Resources/CodeResources/x", "MacOS/CodeResources"] {
        files.push(contents.join(extra));
        assert!(layout(Adapter::PhotoCraftMacV1, &files, &files).is_err(), "{extra}");
        files.pop();
    }
}

#[cfg(target_os = "macos")]
#[test]
fn mac_nightly_requires_the_pinned_team_its_own_identifier_and_notarization() {
    let bundle = Path::new("/private/owned/PhotoCraft Nightly.app");
    let accepted = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict>\
        <key>assessment:authority</key><dict><key>assessment:authority:source</key><string>Notarized Developer ID</string></dict>\
        <key>assessment:verdict</key><true/></dict></plist>";
    let mut calls = Vec::new();
    verify_mac_signature(Adapter::PhotoCraftMacV1, bundle, |program, args, _| {
        calls.push(args.iter().map(|arg| arg.to_string()).collect::<Vec<_>>());
        Ok(crate::process::RunOutput::success(match args {
            ["--status"] => "assessments enabled",
            _ if program == "/usr/sbin/spctl" => accepted,
            _ => "",
        }))
    })
    .unwrap();
    let identity = calls.iter().flatten().find(|arg| arg.contains("subject.OU")).unwrap();
    assert!(identity.contains(crate::nightly_trust::MAC_TEAM.unwrap()));
    assert!(identity.contains("identifier \"ai.storyteller.photocraft.nightly\""));
    assert!(!identity.contains("identifier \"ai.storyteller.photocraft\""), "the stable identifier cannot stand in for the Nightly");
    assert!(calls.iter().any(|args| args.iter().any(|arg| arg == "--check-notarization")));
    assert!(calls.iter().any(|args| args.iter().any(|arg| arg == "--assess")));
    let refused = verify_mac_signature(Adapter::PhotoCraftMacV1, bundle, |_, args, _| {
        Ok(if args.contains(&"--check-notarization") {
            crate::process::RunOutput::exit(3, "", "not notarized")
        } else {
            crate::process::RunOutput::success("")
        })
    });
    assert!(refused.unwrap_err().contains("not notarized"));
}

/// cadcraft's Linux flavor is its launcher script, the program beside it, its badge and notices:
/// every required file is required, nothing else may join them, and PhotoCraft's badge is not its own.
#[test]
fn cadcraft_linux_layout_requires_its_script_and_binary_and_refuses_other_content() {
    let cadcraft = cadcraft_linux();
    let files = |extra: &[&str]| {
        let mut files = vec![PathBuf::from("cadcraft"), PathBuf::from("cadcraft-bin")];
        files.extend(cadcraft.badge().map(Path::to_path_buf));
        files.push(PathBuf::from("LICENSE"));
        files.extend(extra.iter().map(PathBuf::from));
        files
    };
    assert_eq!(validate_layout(cadcraft, &files(&[])), Ok(()));
    let missing = |drop: &str| {
        let kept: Vec<PathBuf> = files(&[]).into_iter().filter(|file| file != Path::new(drop)).collect();
        validate_layout(cadcraft, &kept)
    };
    assert!(missing("cadcraft-bin").is_err(), "missing the program the launcher script starts");
    assert!(missing("cadcraft").is_err(), "missing the entrypoint the Launcher opens");
    if let Some(badge) = cadcraft.badge() {
        assert!(files(&[]).iter().any(|file| file == badge));
        assert!(missing(badge.to_str().unwrap()).is_err(), "missing the declared badge");
    }
    for unwanted in ["helper", "sub/cadcraft", "photocraft-nightly.png", "portable.txt"] {
        assert!(!files(&[]).iter().any(|file| file == Path::new(unwanted)), "{unwanted} is not this row's own content");
        assert!(validate_layout(cadcraft, &files(&[unwanted])).is_err(), "{unwanted}");
    }
    assert!(
        layout(cadcraft, &files(&[]), &[PathBuf::from("bin"), PathBuf::from("bin/cadcraft")]).is_err(),
        "a nested directory entry is not this flat flavor"
    );
    // PhotoCraft keeps its own badge, and the Windows row is still not a Unix flavor.
    assert!(validate_layout(Adapter::PhotoCraftLinuxV1, &[PathBuf::from("photocraft")]).is_err());
    assert!(layout(Adapter::PhotoCraftLinuxV1, &["photocraft".into(), "photocraft-nightly.png".into()], &[]).is_ok());
    assert!(validate_layout(Adapter::PhotoCraftWindowsV1, &[PathBuf::from("photocraft.exe")]).is_err());
}

/// Only the entrypoint and the row's binary may be executable in another app's program folder.
#[test]
fn cadcraft_linux_content_refuses_a_second_executable() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let cadcraft = cadcraft_linux();
    for (name, mode) in [("cadcraft", 0o755), ("cadcraft-bin", 0o755), ("cadcraft-nightly.png", 0o644), ("LICENSE", 0o644)] {
        std::fs::write(root.join(name), b"fixture").unwrap();
        std::fs::set_permissions(root.join(name), std::fs::Permissions::from_mode(mode)).unwrap();
    }
    let files: Vec<PathBuf> = ["cadcraft", "cadcraft-bin", "cadcraft-nightly.png", "LICENSE"].iter().map(PathBuf::from).collect();
    assert_eq!(verify_content(cadcraft, &root, &files), Ok(()));
    std::fs::set_permissions(root.join("LICENSE"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(verify_content(cadcraft, &root, &files).is_err());
}

/// A row with its own binary ships a launcher script: it starts the x86_64 ELF beside it and is
/// itself an ELF or a `#!` program. Anything else refuses, and PhotoCraft still needs its ELF.
#[test]
fn cadcraft_linux_structure_accepts_a_shebang_script_beside_its_elf_and_refuses_anything_else() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let cadcraft = cadcraft_linux();
    let script = root.join("cadcraft");
    let binary = root.join("cadcraft-bin");
    let elf = |path: &Path| {
        std::fs::write(path, program()).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    elf(&binary);
    std::fs::write(&script, b"exec ./cadcraft-bin \"$@\"\n").unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(verify_linux_structure(cadcraft, &root, &script).is_err(), "a launcher script without a shebang");
    std::fs::write(&script, b"#!/bin/sh\nexec \"$(dirname \"$0\")/cadcraft-bin\" \"$@\"\n").unwrap();
    assert_eq!(verify_linux_structure(cadcraft, &root, &script), Ok(()));
    elf(&script);
    assert_eq!(verify_linux_structure(cadcraft, &root, &script), Ok(()), "an ELF entrypoint is also a launcher");
    std::fs::write(&binary, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(verify_linux_structure(cadcraft, &root, &script).is_err(), "the binary must be an x86_64 ELF");
    elf(&binary);
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(verify_linux_structure(cadcraft, &root, &script).is_err(), "a non-executable binary");
    std::fs::remove_file(&binary).unwrap();
    assert!(verify_linux_structure(cadcraft, &root, &script).is_err(), "a missing binary");
}

/// The os() dispatch keeps PhotoCraft's Linux entrypoint an ELF and routes cadcraft's script row
/// to the same Linux check on this platform.
#[cfg(target_os = "linux")]
#[test]
fn linux_dispatch_keeps_photocrafts_elf_entrypoint_and_accepts_cadcrafts_script_row() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let entrypoint = root.join("photocraft");
    std::fs::write(&entrypoint, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&entrypoint, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(verify_structure(Adapter::PhotoCraftLinuxV1, &root, VERSION).is_err(), "PhotoCraft has no script in between");
    std::fs::write(&entrypoint, program()).unwrap();
    assert_eq!(verify_structure(Adapter::PhotoCraftLinuxV1, &root, VERSION), Ok(()));
    let cadcraft = cadcraft_linux();
    std::fs::write(root.join("cadcraft"), b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(root.join("cadcraft"), std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(root.join("cadcraft-bin"), program()).unwrap();
    std::fs::set_permissions(root.join("cadcraft-bin"), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(verify_structure(cadcraft, &root, VERSION), Ok(()));
}

/// The Linux fence names the row's own app: the XDG roots and system install paths for it, so a
/// Nightly folder inside the stable app's config refuses while a launcher-private one passes.
#[test]
fn linux_stable_fence_uses_the_rows_app_and_refuses_overlapping_nightly_data() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().canonicalize().unwrap();
    let cadcraft = cadcraft_linux();
    let config = home.join("config");
    std::fs::create_dir_all(config.join("cadcraft")).unwrap();
    let protected = linux_stable_paths(cadcraft, &home, [Some(config.clone().into_os_string()), None, None, None]).unwrap();
    for expected in [config.join("cadcraft"), home.join(".local/share/cadcraft"), home.join(".cache/cadcraft"), home.join(".local/state/cadcraft")] {
        assert!(protected.contains(&expected), "{expected:?}");
    }
    for system in ["/usr/bin/cadcraft", "/usr/local/bin/cadcraft", "/opt/cadcraft"] {
        assert!(protected.contains(&PathBuf::from(system)), "{system}");
    }
    assert!(!protected.iter().any(|path| path.to_string_lossy().contains("photocraft")));
    let inside = config.join("cadcraft/nightly");
    std::fs::create_dir_all(&inside).unwrap();
    assert!(disjoint(std::slice::from_ref(&inside), &protected).is_err());
    let elsewhere = home.join(".local/share/artcraft-launcher/pre-release/v1/data");
    std::fs::create_dir_all(&elsewhere).unwrap();
    assert!(disjoint(&[elsewhere], &protected).is_ok());
}

/// The macOS fence keeps every path main already protected for PhotoCraft, and another app's
/// names come from its own row (`CADCraft`, `Cadcraft`, `ai.storyteller.cadcraft`), never borrowed.
#[test]
fn mac_stable_fence_keeps_photo_crafts_old_list_and_uses_each_apps_own_names() {
    let home = Path::new("/Users/someone");
    let photo = mac_stable_paths(Adapter::PhotoCraftMacV1, home);
    for old in [
        "Library/Application Support/Photocraft",
        "Library/Application Support/PhotoCraft",
        "Library/Caches/Photocraft",
        "Library/Caches/ai.storyteller.photocraft",
        "Library/Caches/PhotoCraft",
        "Library/Preferences/Photocraft",
        "Library/Preferences/ai.storyteller.photocraft.plist",
        "Library/Saved Application State/ai.storyteller.photocraft.savedState",
        "Library/WebKit/ai.storyteller.photocraft",
        "Library/HTTPStorages/ai.storyteller.photocraft",
        "Applications/PhotoCraft.app",
    ] {
        assert!(photo.contains(&home.join(old)), "{old}");
    }
    assert!(photo.contains(&PathBuf::from("/Applications/PhotoCraft.app")));
    let cadcraft = mac_stable_paths_for("cadcraft", "CADCraft", "ai.storyteller.cadcraft", home);
    for expected in [
        "Library/Application Support/CADCraft",
        "Library/Application Support/Cadcraft",
        "Library/Caches/Cadcraft",
        "Library/Caches/ai.storyteller.cadcraft",
        "Library/Preferences/Cadcraft",
        "Library/Preferences/ai.storyteller.cadcraft.plist",
        "Library/Saved Application State/ai.storyteller.cadcraft.savedState",
        "Library/WebKit/ai.storyteller.cadcraft",
        "Library/HTTPStorages/ai.storyteller.cadcraft",
        "Applications/CADCraft.app",
    ] {
        assert!(cadcraft.contains(&home.join(expected)), "{expected}");
    }
    assert!(cadcraft.contains(&PathBuf::from("/Applications/CADCraft.app")));
    assert!(!cadcraft.iter().any(|path| path.to_string_lossy().contains("hotocraft")));
    // Its own row admits it on macOS, under its own bundle and display name, and the plist key
    // stays derived from that name rather than guessed per app.
    let cad = crate::nightly_profile::for_app(CraftApp::by_id("cadcraft").unwrap(), Platform { os: Os::MacOs, arch: Arch::Arm64 }).unwrap();
    assert_eq!(cad.bundle(), Some(("CADCraft Nightly.app", "ai.storyteller.cadcraft.nightly")));
    assert_eq!(mac_version_key("PhotoCraft"), "PhotoCraftVersion");
    assert_eq!(mac_version_key("CADCraft"), "CADCraftVersion");
}

#[test]
fn xdg_roots_use_the_home_default_for_unset_empty_and_relative_values() {
    let home = Path::new("/home/someone");
    for value in [None, Some(""), Some("relative/state")] {
        assert_eq!(xdg_root(value.map(Into::into), home, ".local/state"), home.join(".local/state"), "{value:?}");
    }
    assert_eq!(xdg_root(Some("/srv/state".into()), home, ".local/state"), PathBuf::from("/srv/state"));
}
