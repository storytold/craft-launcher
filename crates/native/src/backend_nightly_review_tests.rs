//! Controlled synthetic filesystem probes; no fixture is an executable app or signing evidence.
#![allow(clippy::unwrap_used)] // Fixture setup and assertions intentionally fail immediately.
use super::*;
use crate::nightly_profile::Adapter;
use crate::nightly_unix::tests::{archive, package, paths, program};
use crate::nightly_unix::{self, Checkpoint};
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

// File locks belong to open file descriptions: a concurrent fork in an unrelated test
// can briefly inherit one until exec closes CLOEXEC descriptors. Run each probe in its own
// test process, still with its real worker/child-process boundaries and all assertions.
fn isolated_probe(name: &str) -> bool {
    const ISOLATED: &str = "ARTCRAFT_REVIEW_ISOLATED_PROBE";
    if std::env::var(ISOLATED).as_deref() == Ok(name) {
        return false;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("backend::nightly_review_tests::{name}"), "--nocapture"])
        .env(ISOLATED, name)
        .status()
        .unwrap();
    assert!(status.success(), "isolated boundary probe {name} failed: {status}");
    true
}

const APP: &str = "photocraft";
const ADAPTER: Adapter = Adapter::PhotoCraftLinuxV1;

fn baseline(root: &Path) -> (Paths, crate::manifest::Record) {
    let paths = paths(root);
    let file = root.join("payload.tar.gz");
    let mut old = program();
    old.extend_from_slice(b"old owned program bytes");
    archive(&file, &[("photocraft", &old, 0o700)]);
    let record = nightly_unix::install(package(&file, &paths, None), &mut |record| {
        let mut manifest = Manifest::default();
        manifest.apps.insert(APP.into(), record.clone());
        manifest.save(&paths.manifest())
    })
    .unwrap();
    std::fs::set_permissions(&record.path, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(paths.apps().join(APP).join("user.txt"), b"foreign program-root file").unwrap();
    let profile = ADAPTER.profile_root(&paths).unwrap();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("preferences"), b"persistent profile").unwrap();
    (paths, record)
}

/// These probes inject a real write failure with mode 0500. A privileged process (for example
/// with CAP_DAC_OVERRIDE) is not denied, so fail clearly instead of testing nothing.
fn assert_writes_denied(directory: &Path) {
    let probe = directory.join(".write-denial-probe");
    let created = std::fs::File::create(&probe).is_ok();
    if created {
        let _ = std::fs::remove_file(&probe);
    }
    assert!(!created, "mode 0500 did not deny writes to {}; run these probes as an unprivileged user", directory.display());
}

fn fresh_backend(paths: &Paths) -> NativeBackend {
    let mut backend = NativeBackend::with_paths(artcraft_launcher_core::nightly::InstallChannel::Nightly, || {}, paths.clone());
    backend.spawn = Arc::new(|task| task());
    let deadline = Instant::now() + Duration::from_secs(5);
    while backend.shared.detecting.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline, "background recovery/detection did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
    backend
}

fn assert_retained(paths: &Paths) {
    assert_eq!(std::fs::read(paths.apps().join(APP).join("user.txt")).unwrap(), b"foreign program-root file");
    assert_eq!(std::fs::read(ADAPTER.profile_root(paths).unwrap().join("preferences")).unwrap(), b"persistent profile");
}

fn retry_removal(paths: &Paths) {
    let backend = fresh_backend(paths);
    backend.start_job(APP, Work::Uninstall);
    let events = backend.shared.events.lock().unwrap();
    assert!(
        events.iter().any(|event| matches!(event, Event::JobFinished { app, result: Ok(_) } if app == APP)),
        "a fresh backend could not retry the interrupted uninstall: {events:?}"
    );
    assert!(!Manifest::load(&paths.manifest()).apps.contains_key(APP));
    assert_retained(paths);
}

#[test]
fn review_probe_abrupt_replace_can_restart_with_old_record() {
    if isolated_probe("review_probe_abrupt_replace_can_restart_with_old_record") {
        return;
    }
    const CHILD_ROOT: &str = "ARTCRAFT_LAUNCHER_REVIEW_CRASH_TEST_ROOT";
    if let Some(root) = std::env::var_os(CHILD_ROOT) {
        let root = std::path::PathBuf::from(root);
        let paths = paths(&root);
        let previous = Manifest::load(&paths.manifest()).apps.remove(APP).unwrap();
        let file = root.join("payload.tar.gz");
        archive(&file, &[("photocraft", &program(), 0o755)]);
        let mut update = package(&file, &paths, Some(&previous));
        update.version = "1.0.0-nightly.20261122.123456.1.gabcdefabcdef";
        let _ = nightly_unix::install_with_checkpoints(update, &mut |_| Ok(()), &mut |point| {
            if point == Checkpoint::BackupsSaved {
                std::process::exit(47);
            }
            Ok(())
        });
        panic!("child did not reach the interruption boundary");
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, old) = baseline(temp.path());
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "backend::nightly_review_tests::review_probe_abrupt_replace_can_restart_with_old_record", "--nocapture"])
        .env(CHILD_ROOT, temp.path())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(47), "the actual child must stop without unwinding");
    let restarted = fresh_backend(&paths);
    let saved = restarted.shared.manifest.lock().unwrap().apps.get(APP).cloned().unwrap();
    assert_eq!(saved, old);
    let ownership = ADAPTER.validate_record(&paths, &saved);
    assert!(ownership.is_ok(), "actual abrupt exit left the persisted record unrecoverable: {ownership:?}");
    assert!(std::fs::read(&saved.path).unwrap().ends_with(b"old owned program bytes"));
    assert_eq!(std::fs::metadata(&saved.path).unwrap().permissions().mode() & 0o777, 0o700);
    assert_retained(&paths);
}

#[test]
fn review_probe_parent_swap_never_creates_in_arriving_foreign_tree() {
    if isolated_probe("review_probe_parent_swap_never_creates_in_arriving_foreign_tree") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    let file = temp.path().join("payload.tar.gz");
    archive(&file, &[("photocraft", &program(), 0o755)]);
    let root = paths.apps().join(APP);
    let moved = temp.path().join("original-owned-root");
    let foreign = temp.path().join("foreign-directory");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("arrived.txt"), b"foreign sentinel").unwrap();
    let mut swapped = false;
    let result = nightly_unix::install_with_checkpoints(package(&file, &paths, None), &mut |_| Ok(()), &mut |point| {
        if point == Checkpoint::BeforeProgramCreate && !swapped {
            swapped = true;
            std::fs::rename(&root, &moved).unwrap();
            symlink(&foreign, &root).unwrap();
        }
        Ok(())
    });
    assert!(swapped && result.is_err());
    assert_eq!(std::fs::read(foreign.join("arrived.txt")).unwrap(), b"foreign sentinel");
    let arriving: Vec<_> = std::fs::read_dir(&foreign).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert_eq!(arriving.len(), 1, "the actual create_new traversed a replaced parent and created outside the reserved tree: {arriving:?}");
    assert!(root.is_symlink());
}

#[test]
fn review_probe_partial_unlink_can_retry_from_fresh_backend() {
    if isolated_probe("review_probe_partial_unlink_can_retry_from_fresh_backend") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, record) = baseline(temp.path());
    let root = paths.apps().join(APP);
    let mut removals = 0;
    let result = nightly_unix::remove_with_checkpoints(ADAPTER, &record, &paths, &mut |point| {
        if point == Checkpoint::BeforeRemoval {
            removals += 1;
            if removals == 2 {
                std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
                assert_writes_denied(&root);
            }
        }
        Ok(())
    });
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(removals >= 2 && result.is_err(), "expected a real owned-file move denial after the first was staged: {result:?}");
    eprintln!("actual second owned-file move failed after the first was staged: {result:?}");
    retry_removal(&paths);
}

#[test]
fn review_probe_manifest_save_failure_can_retry_from_fresh_backend() {
    if isolated_probe("review_probe_manifest_save_failure_can_retry_from_fresh_backend") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, record) = baseline(temp.path());
    let backend = fresh_backend(&paths);
    let worker = backend.prepare_job(APP, Work::Uninstall).unwrap();
    let exercised = std::rc::Rc::new(std::cell::Cell::new(false));
    let hit = exercised.clone();
    let data = paths.data.clone();
    let executable = record.path.clone();
    REVIEW_BEFORE_MANIFEST_SAVE.with(|hook| {
        *hook.borrow_mut() = Some(Box::new(move || {
            assert!(!executable.exists(), "save fault must occur after real removal staging");
            hit.set(true);
            std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o500)).unwrap();
            assert_writes_denied(&data);
        }))
    });
    worker();
    assert!(exercised.get(), "must reach the actual manifest replacement boundary");
    std::fs::set_permissions(&paths.data, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(backend.shared.events.lock().unwrap().iter().any(|event| matches!(event, Event::JobFinished { app, result: Err(_) } if app == APP)));
    assert_eq!(Manifest::load(&paths.manifest()).apps.get(APP), Some(&record), "failed actual atomic save must retain its prior record");
    retry_removal(&paths);
}

// These synthetic transactions exercise the real filesystem/manifest boundaries, not networking,
// source publisher trust, actual app execution, or native Mac/Windows installation acceptance.
fn synthetic_update(
    backend: &NativeBackend,
    file: &Path,
    checkpoint: &mut dyn FnMut(Checkpoint) -> Result<(), String>,
) -> Result<crate::manifest::Record, String> {
    let lease = crate::nightly_transaction::Lease::acquire(&backend.paths, ADAPTER)?;
    let previous = lease.manifest()?.apps.get(APP).cloned().ok_or("Missing fixture record.")?;
    let mut update = package(file, &backend.paths, Some(&previous));
    update.lease = Some(&lease);
    update.version = "1.0.0-nightly.20261122.123456.1.gabcdefabcdef";
    nightly_unix::install_with_checkpoints(
        update,
        &mut |record| {
            let manifest = lease.publish(Some(&previous), Some(record))?;
            store_nightly_manifest(&backend.shared, manifest);
            Ok(())
        },
        checkpoint,
    )
}
fn run_review_child(test: &str, root: &Path, phase: &str) {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("backend::nightly_review_tests::{test}"), "--nocapture"])
        .env("ARTCRAFT_REVIEW_MATRIX_ROOT", root)
        .env("ARTCRAFT_REVIEW_MATRIX_PHASE", phase)
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(47), "child must exit at the actual boundary, without Rust unwinding");
}
fn pending_preparations(paths: &Paths) -> usize {
    let cache = paths.cache.join("staging");
    let cached = std::fs::read_dir(cache).map_or(0, |entries| entries.count());
    let persistent = std::fs::read_dir(paths.apps())
        .map_or(0, |entries| entries.filter(|entry| entry.as_ref().is_ok_and(|entry| entry.file_name() != APP)).count());
    cached + persistent
}

#[test]
fn review_probe_preplan_interruptions_keep_one_preparation_and_restart() {
    if isolated_probe("review_probe_preplan_interruptions_keep_one_preparation_and_restart") {
        return;
    }
    if let Some(root) = std::env::var_os("ARTCRAFT_REVIEW_MATRIX_ROOT") {
        let root = std::path::PathBuf::from(root);
        let paths = paths(&root);
        let backend = fresh_backend(&paths);
        let file = root.join("payload.tar.gz");
        archive(&file, &[("photocraft", &program(), 0o755)]);
        let _ = synthetic_update(&backend, &file, &mut |point| {
            if point == Checkpoint::ArchiveExtracted {
                std::process::exit(47);
            }
            Ok(())
        });
        panic!("pre-plan boundary not reached");
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, old) = baseline(temp.path());
    for _ in 0..2 {
        run_review_child("review_probe_preplan_interruptions_keep_one_preparation_and_restart", temp.path(), "preplan");
        assert!(pending_preparations(&paths) <= 1, "repeated abrupt preparation must not grow a new unreferenced tree");
        let _restarted = fresh_backend(&paths);
        assert_eq!(pending_preparations(&paths), 0);
        assert_eq!(Manifest::load(&paths.manifest()).apps.get(APP), Some(&old));
        assert!(std::fs::read(&old.path).unwrap().ends_with(b"old owned program bytes"));
        assert_retained(&paths);
    }
    retry_removal(&paths);
}

#[test]
fn review_probe_after_commit_and_cleanup_exit_keep_committed_authority() {
    if isolated_probe("review_probe_after_commit_and_cleanup_exit_keep_committed_authority") {
        return;
    }
    if let Some(root) = std::env::var_os("ARTCRAFT_REVIEW_MATRIX_ROOT") {
        let root = std::path::PathBuf::from(root);
        let paths = paths(&root);
        let backend = fresh_backend(&paths);
        let phase = std::env::var("ARTCRAFT_REVIEW_MATRIX_PHASE").unwrap();
        if phase == "stage-removed" {
            REVIEW_AFTER_STAGE_REMOVED.with(|hook| *hook.borrow_mut() = Some(Box::new(|| std::process::exit(47))));
        }
        let file = root.join("payload.tar.gz");
        let mut new = program();
        new.extend_from_slice(b"committed new program");
        archive(&file, &[("photocraft", &new, 0o755), ("NOTICE", b"new notice", 0o644)]);
        let _ = synthetic_update(&backend, &file, &mut |point| {
            if phase == "committed" && point == Checkpoint::AfterCommit {
                std::process::exit(47);
            }
            Ok(())
        });
        panic!("post-commit boundary not reached");
    }
    for phase in ["committed", "stage-removed"] {
        let temp = tempfile::tempdir().unwrap();
        let (paths, old) = baseline(temp.path());
        run_review_child("review_probe_after_commit_and_cleanup_exit_keep_committed_authority", temp.path(), phase);
        let committed = Manifest::load(&paths.manifest()).apps.get(APP).cloned().unwrap();
        assert_ne!(committed.version, old.version);
        let restarted = fresh_backend(&paths);
        assert_eq!(restarted.shared.manifest.lock().unwrap().apps.get(APP), Some(&committed));
        assert!(std::fs::read(&committed.path).unwrap().ends_with(b"committed new program"));
        assert_eq!(pending_preparations(&paths), 0, "phase {phase}; events: {:?}", restarted.shared.events.lock().unwrap());
        assert!(!paths.data.join(".nightly-photocraft-journal-v1.jsonl").exists());
        assert_retained(&paths);
    }
}

#[test]
fn review_probe_uninstall_exit_after_commit_keeps_uninstalled_state() {
    if isolated_probe("review_probe_uninstall_exit_after_commit_keeps_uninstalled_state") {
        return;
    }
    if let Some(root) = std::env::var_os("ARTCRAFT_REVIEW_MATRIX_ROOT") {
        let root = std::path::PathBuf::from(root);
        let paths = paths(&root);
        let lease = crate::nightly_transaction::Lease::acquire(&paths, ADAPTER).unwrap();
        let record = lease.manifest().unwrap().apps.get(APP).cloned().unwrap();
        let _ = crate::nightly_transaction::remove(&lease, &record, &mut || lease.publish(Some(&record), None).map(|_| ()), &mut |point| {
            if point == Checkpoint::AfterCommit {
                std::process::exit(47);
            }
            Ok(())
        });
        panic!("uninstall post-commit boundary not reached");
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, record) = baseline(temp.path());
    run_review_child("review_probe_uninstall_exit_after_commit_keeps_uninstalled_state", temp.path(), "uninstall");
    let restarted = fresh_backend(&paths);
    assert!(!restarted.shared.manifest.lock().unwrap().apps.contains_key(APP));
    assert!(!record.path.exists());
    assert_eq!(pending_preparations(&paths), 0);
    assert_retained(&paths);
}

#[test]
fn review_probe_preexisting_backend_reloads_new_record_and_preserves_other_apps() {
    if isolated_probe("review_probe_preexisting_backend_reloads_new_record_and_preserves_other_apps") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, old) = baseline(temp.path());
    let first = fresh_backend(&paths);
    let second = fresh_backend(&paths);
    assert_eq!(second.shared.manifest.lock().unwrap().apps.get(APP), Some(&old));
    let other = crate::manifest::Record {
        path: temp.path().join("other-app/program"),
        files: vec![],
        version: "2.0.0".into(),
        kind: artcraft_launcher_core::PackageKind::AppImage,
        installed_at: 1,
        needs_root: false,
    };
    let mut persisted = Manifest::load(&paths.manifest());
    persisted.apps.insert("other-app".into(), other.clone());
    persisted.save(&paths.manifest()).unwrap();
    let file = temp.path().join("payload.tar.gz");
    archive(&file, &[("photocraft", &program(), 0o755), ("NOTICE", b"new owned file", 0o644)]);
    let new = synthetic_update(&first, &file, &mut |_| Ok(())).unwrap();
    assert_ne!(new, old);
    assert_eq!(second.shared.manifest.lock().unwrap().apps.get(APP), Some(&old), "second backend really predates the commit");
    second.start_job(APP, Work::Uninstall);
    assert!(second.shared.events.lock().unwrap().iter().any(|event| matches!(event, Event::JobFinished { app, result: Ok(_) } if app == APP)));
    let saved = Manifest::load(&paths.manifest());
    assert!(!saved.apps.contains_key(APP));
    assert_eq!(saved.apps.get("other-app"), Some(&other));
    assert!(!paths.apps().join(APP).join("NOTICE").exists(), "the stale backend must use the new owned inventory");
    assert_retained(&paths);
}

#[test]
fn review_probe_error_after_actual_manifest_replace_never_rolls_back_or_reports_old_record() {
    if isolated_probe("review_probe_error_after_actual_manifest_replace_never_rolls_back_or_reports_old_record") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, old) = baseline(temp.path());
    let backend = fresh_backend(&paths);
    REVIEW_AFTER_MANIFEST_REPLACE
        .with(|hook| *hook.borrow_mut() = Some(Box::new(|| Err("controlled error after the real manifest replacement".into()))));
    backend.start_job(APP, Work::Uninstall);
    assert!(
        !Manifest::load(&paths.manifest()).apps.contains_key(APP),
        "post-replacement disk state; events: {:?}",
        backend.shared.events.lock().unwrap()
    );
    assert!(!backend.shared.manifest.lock().unwrap().apps.contains_key(APP));
    assert!(!old.path.exists());
    assert!(
        backend
            .shared
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event, Event::JobFinished { app, result: Err(error) } if app == APP && error.contains("committed")))
    );
    let restarted = fresh_backend(&paths);
    assert!(!restarted.shared.manifest.lock().unwrap().apps.contains_key(APP));
    assert_retained(&paths);
}

#[test]
fn review_probe_open_dispatch_admits_without_waiting_for_storage_or_download_queue() {
    if isolated_probe("review_probe_open_dispatch_admits_without_waiting_for_storage_or_download_queue") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, old) = baseline(temp.path());
    let mut backend = fresh_backend(&paths);
    let lease = crate::nightly_transaction::Lease::acquire(&paths, ADAPTER).unwrap();
    let queued = Arc::new(Mutex::new(Vec::new()));
    let tasks = queued.clone();
    backend.spawn = Arc::new(move |task| tasks.lock().unwrap().push(task));
    backend.send(Command::Launch { app: APP.into(), args: vec![] });
    assert_eq!(queued.lock().unwrap().len(), 1, "dispatch must not try the held filesystem lock or run tools");
    assert!(backend.has_active_jobs());
    assert!(backend.prepare_job(APP, Work::Uninstall).is_none(), "Open shares per-app mutation exclusion");
    assert!(!backend.shared.events.lock().unwrap().iter().any(|event| matches!(event, Event::Progress { .. })));
    let worker = queued.lock().unwrap().remove(0);
    worker(); // busy filesystem refusal, never executes the synthetic ELF
    assert!(!backend.has_active_jobs());
    drop(lease);
    assert!(std::fs::read(old.path).unwrap().ends_with(b"old owned program bytes"));
    assert_retained(&paths);
    backend.shared.checking.store(true, Ordering::SeqCst);
    backend.shared.detecting.store(true, Ordering::SeqCst);
    assert!(!backend.has_active_jobs(), "discovery alone is not a quit-blocking job");
}

#[test]
fn review_probe_obsolete_detection_does_not_overwrite_worker_commit() {
    if isolated_probe("review_probe_obsolete_detection_does_not_overwrite_worker_commit") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, record) = baseline(temp.path());
    let backend = fresh_backend(&paths);
    let app = CraftApp::by_id(APP).unwrap();
    let installed = crate::nightly::owned_installation(app, &record, &paths, backend.platform.unwrap()).unwrap();
    publish_detected(app, Some(installed.clone()), &backend.shared, backend.platform);
    let mut stale = nightly_detection_snapshot(&backend.shared, app);
    stale.record = None;
    assert!(!publish_nightly_detection(app, &stale, None, &backend.shared, backend.platform));
    assert_eq!(backend.shared.installed.lock().unwrap().get(APP), Some(&installed));
}

#[test]
fn review_probe_readonly_data_before_preparation_does_not_leave_an_orphan_slot() {
    if isolated_probe("review_probe_readonly_data_before_preparation_does_not_leave_an_orphan_slot") {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, old) = baseline(temp.path());
    let backend = fresh_backend(&paths);
    std::fs::set_permissions(&paths.data, std::fs::Permissions::from_mode(0o500)).unwrap();
    assert_writes_denied(&paths.data);
    backend.start_job(APP, Work::Uninstall);
    std::fs::set_permissions(&paths.data, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(std::fs::read(&old.path).unwrap().ends_with(b"old owned program bytes"));
    assert_eq!(pending_preparations(&paths), 0);
    retry_removal(&paths);
}

#[test]
fn review_probe_unregistered_manifest_scratch_exit_is_preserved_and_retry_is_actionable() {
    const TEST: &str = "review_probe_unregistered_manifest_scratch_exit_is_preserved_and_retry_is_actionable";
    if isolated_probe(TEST) {
        return;
    }
    if let Some(root) = std::env::var_os("ARTCRAFT_REVIEW_MATRIX_ROOT") {
        let paths = paths(&std::path::PathBuf::from(root));
        let backend = fresh_backend(&paths);
        REVIEW_AFTER_MANIFEST_SCRATCH_CREATED.with(|hook| *hook.borrow_mut() = Some(Box::new(|| std::process::exit(47))));
        backend.start_job(APP, Work::Uninstall);
        panic!("scratch creation boundary not reached");
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, old) = baseline(temp.path());
    run_review_child(TEST, temp.path(), "scratch-unregistered");
    let scratch = paths.data.join(".nightly-photocraft-manifest-v1.pending");
    let journal = paths.data.join(".nightly-photocraft-journal-v1.jsonl");
    assert_eq!(std::fs::read(&scratch).unwrap(), b"");
    assert!(!std::fs::read_to_string(&journal).unwrap().contains("Metadata"));
    let scratch_identity = std::fs::metadata(&scratch).unwrap();
    let restarted = fresh_backend(&paths);
    restarted.start_job(APP, Work::Uninstall);
    let events = restarted.shared.events.lock().unwrap();
    eprintln!("actual unregistered scratch recovery and mutation retry: {events:?}");
    assert_eq!(Manifest::load(&paths.manifest()).apps.get(APP), Some(&old));
    assert!(std::fs::read(&old.path).unwrap().ends_with(b"old owned program bytes"));
    assert_eq!(std::fs::metadata(&scratch).unwrap().ino(), scratch_identity.ino());
    assert_eq!(std::fs::read(&scratch).unwrap(), b"");
    assert!(journal.exists(), "unknown metadata must not silently retire its recovery journal; events: {events:?}");
    assert!(
        events.iter().any(|event| match event {
            Event::Notice { result: Err(error), .. } | Event::JobFinished { result: Err(error), .. } => {
                error.contains("unregistered Nightly metadata") && error.contains(scratch.to_str().unwrap())
            }
            _ => false,
        }),
        "retry must explain the preserved scratch path: {events:?}"
    );
    drop(events);
    assert_retained(&paths);
    // Model explicit operator quarantine, not automatic adoption or deletion by basename.
    let quarantine = temp.path().join("operator-preserved-metadata");
    std::fs::rename(&scratch, &quarantine).unwrap();
    retry_removal(&paths);
    assert!(quarantine.exists());
    assert!(!journal.exists());
}

// Transaction-only Mac-shaped fixtures on Linux: no platform approval or Mac execution claim.
fn nested_install(paths: &Paths, docs: bool, stop_at: Option<Checkpoint>, abrupt: bool, foreign: bool) -> Result<crate::manifest::Record, String> {
    nested_install_with_action(paths, docs, stop_at, abrupt, foreign, None)
}
fn nested_install_with_action(
    paths: &Paths,
    docs: bool,
    stop_at: Option<Checkpoint>,
    abrupt: bool,
    foreign: bool,
    mut action: Option<&mut dyn FnMut(&Path)>,
) -> Result<crate::manifest::Record, String> {
    use std::io::Write;
    let lease = crate::nightly_transaction::Lease::acquire(paths, Adapter::PhotoCraftMacV1)?;
    let previous = lease.manifest()?.apps.get(APP).cloned();
    let root = paths.apps().join(APP);
    let executable = "PhotoCraft Nightly.app/Contents/MacOS/PhotoCraft";
    let mut payload = vec![
        (executable, b"synthetic nested program".as_slice()),
        ("PhotoCraft Nightly.app/Contents/Resources/photocraft-nightly.icns", b"synthetic badge".as_slice()),
    ];
    if docs {
        payload.push(("PhotoCraft Nightly.app/Contents/Resources/docs/NOTICE", b"synthetic nested notice"));
    }
    let next = crate::manifest::Record {
        path: root.join(executable),
        files: payload.iter().map(|(path, _)| root.join(path)).collect(),
        version: if previous.is_some() { "1.0.0-nightly.20261122.123456.1.gabcdefabcdef" } else { "1.0.0-nightly.20261121.123456.1.gabcdefabcdef" }
            .into(),
        kind: artcraft_launcher_core::PackageKind::PortableZip,
        installed_at: 1,
        needs_root: false,
    };
    let mut preparation = lease.prepare()?;
    for (path, bytes) in payload {
        let mut file = preparation.create(&Path::new("unpacked").join(path))?;
        file.write_all(bytes).map_err(|e| e.to_string())?;
        file.set_permissions(std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
    }
    preparation.seal(previous.as_ref(), Some(&next))?;
    let mut held = None;
    let mut committed = false;
    let mut checkpoint = |point| {
        if stop_at == Some(point) {
            if let Some(action) = action.as_mut() {
                action(&root);
            }
            if foreign {
                std::fs::write(root.join("PhotoCraft Nightly.app/Contents/Resources/docs/foreign.txt"), b"arriving foreign bytes").unwrap();
            }
            if abrupt {
                std::process::exit(47);
            }
            return Err("controlled nested transaction interruption".into());
        }
        Ok(())
    };
    let outcome = preparation.apply(&mut held, &mut checkpoint).and_then(|()| {
        checkpoint(Checkpoint::BeforeCommit)?;
        lease.publish(previous.as_ref(), Some(&next))?;
        committed = true;
        Ok(())
    });
    preparation.finish(outcome, held, committed)?;
    Ok(next)
}

#[test]
fn review_probe_nested_unmoved_parents_restore_fresh_and_update_after_real_exit() {
    const TEST: &str = "review_probe_nested_unmoved_parents_restore_fresh_and_update_after_real_exit";
    if isolated_probe(TEST) {
        return;
    }
    if let Some(root) = std::env::var_os("ARTCRAFT_REVIEW_MATRIX_ROOT") {
        let paths = paths(&std::path::PathBuf::from(root));
        let fresh = std::env::var("ARTCRAFT_REVIEW_MATRIX_PHASE").unwrap() == "fresh-root";
        let _ = nested_install(&paths, true, Some(if fresh { Checkpoint::RootReserved } else { Checkpoint::BackupsSaved }), true, false);
        panic!("nested exit boundary not reached");
    }
    for fresh in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let old = (!fresh).then(|| nested_install(&paths, false, None, false, false).unwrap());
        run_review_child(TEST, temp.path(), if fresh { "fresh-root" } else { "update-backups" });
        let recovered = crate::nightly_transaction::Lease::acquire(&paths, Adapter::PhotoCraftMacV1);
        assert!(recovered.is_ok(), "known unmoved new parents must not block nested rollback: {:?}", recovered.err());
        drop(recovered);
        if let Some(old) = &old {
            assert_eq!(std::fs::read(&old.path).unwrap(), b"synthetic nested program");
            assert_eq!(std::fs::metadata(&old.path).unwrap().mode() & 0o777, 0o700);
        } else {
            assert!(!paths.apps().join(APP).exists());
        }
        assert!(!paths.apps().join(APP).join("PhotoCraft Nightly.app/Contents/Resources/docs").exists());
        assert!(!paths.data.join(".nightly-photocraft-journal-v1.jsonl").exists());
        nested_install(&paths, true, None, false, false).unwrap();
    }
}

#[test]
fn review_probe_nested_precommit_rollback_prunes_only_known_empty_new_directories() {
    const TEST: &str = "review_probe_nested_precommit_rollback_prunes_only_known_empty_new_directories";
    if isolated_probe(TEST) {
        return;
    }
    for fresh in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let old = (!fresh).then(|| nested_install(&paths, false, None, false, false).unwrap());
        let result = nested_install(&paths, true, Some(Checkpoint::BeforeCommit), false, false);
        assert!(result.is_err());
        assert!(
            !paths.apps().join(APP).join("PhotoCraft Nightly.app/Contents/Resources/docs").exists(),
            "rollback must prune its known new docs directory: {result:?}"
        );
        if let Some(old) = &old {
            assert_eq!(std::fs::read(&old.path).unwrap(), b"synthetic nested program");
        } else {
            assert!(!paths.apps().join(APP).exists(), "fresh rollback must release its own empty nested root");
        }
        assert!(!paths.data.join(".nightly-photocraft-journal-v1.jsonl").exists());
        nested_install(&paths, true, None, false, false).unwrap();
    }
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    assert!(nested_install(&paths, true, Some(Checkpoint::BeforeCommit), false, true).is_err());
    assert_eq!(
        std::fs::read(paths.apps().join(APP).join("PhotoCraft Nightly.app/Contents/Resources/docs/foreign.txt")).unwrap(),
        b"arriving foreign bytes"
    );
}

#[test]
fn review_probe_nested_rollback_refuses_missing_original_or_arriving_parent() {
    const TEST: &str = "review_probe_nested_rollback_refuses_missing_original_or_arriving_parent";
    if isolated_probe(TEST) {
        return;
    }
    for replacement in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let old = nested_install(&paths, false, None, false, false).unwrap();
        let held_parent = temp.path().join("original-parent");
        let mut interleave = |root: &Path| {
            let original = root.join("PhotoCraft Nightly.app/Contents/MacOS");
            std::fs::rename(&original, &held_parent).unwrap();
            if replacement {
                std::fs::create_dir(&original).unwrap();
                std::fs::write(original.join("foreign.txt"), b"foreign parent identity").unwrap();
            }
        };
        let result = nested_install_with_action(&paths, true, Some(Checkpoint::BackupsSaved), false, false, Some(&mut interleave));
        assert!(
            result.as_ref().is_err_and(|error| error.contains("recovery is pending")),
            "original parent absence/replacement must not be skipped: {result:?}"
        );
        assert!(paths.data.join(".nightly-photocraft-journal-v1.jsonl").exists());
        assert_eq!(Manifest::load(&paths.manifest()).apps.get(APP), Some(&old));
        assert_eq!(
            std::fs::read(paths.apps().join(".nightly-photocraft-preparation-v1/backup/PhotoCraft Nightly.app/Contents/MacOS/PhotoCraft")).unwrap(),
            b"synthetic nested program"
        );
        if replacement {
            assert_eq!(
                std::fs::read(paths.apps().join(APP).join("PhotoCraft Nightly.app/Contents/MacOS/foreign.txt")).unwrap(),
                b"foreign parent identity"
            );
        }
    }
}

#[test]
fn review_probe_unjournaled_metadata_orphan_refuses_before_program_mutation() {
    const TEST: &str = "review_probe_unjournaled_metadata_orphan_refuses_before_program_mutation";
    if isolated_probe(TEST) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, old) = baseline(temp.path());
    let scratch = paths.data.join(".nightly-photocraft-manifest-v1.pending");
    std::fs::write(&scratch, b"unknown preserved metadata").unwrap();
    let before = std::fs::metadata(&old.path).unwrap().ino();
    let backend = fresh_backend(&paths);
    backend.start_job(APP, Work::Uninstall);
    let events = backend.shared.events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(event, Event::JobFinished { result: Err(error), .. } if error.contains("unregistered Nightly metadata") && error.contains(scratch.to_str().unwrap()))));
    assert_eq!(std::fs::metadata(&old.path).unwrap().ino(), before);
    assert_eq!(std::fs::read(&scratch).unwrap(), b"unknown preserved metadata");
    assert!(!paths.data.join(".nightly-photocraft-journal-v1.jsonl").exists());
    assert_retained(&paths);
}

#[test]
fn review_lifecycle_real_detector_cannot_hide_same_record_restored_by_failed_removal() {
    const TEST: &str = "review_lifecycle_real_detector_cannot_hide_same_record_restored_by_failed_removal";
    if isolated_probe(TEST) {
        return;
    }
    use std::sync::mpsc;
    let temp = tempfile::tempdir().unwrap();
    let (paths, record) = baseline(temp.path());
    let backend = Arc::new(fresh_backend(&paths));
    let app = CraftApp::by_id(APP).unwrap();
    let installed = crate::nightly::owned_installation(app, &record, &paths, backend.platform.unwrap()).unwrap();
    assert_eq!(backend.installed(APP), Some(installed.clone()), "ordinary no-mutation detection must publish the valid retained copy");
    let (staged_tx, staged_rx) = mpsc::channel();
    let (rollback_tx, rollback_rx) = mpsc::channel();
    let (observed_tx, observed_rx) = mpsc::channel();
    let (publish_tx, publish_rx) = mpsc::channel();
    *backend.shared.nightly_detection_observer.lock().unwrap() = Some(Box::new(move |missing| {
        assert!(missing, "detector must actually observe the staged-away executable");
        observed_tx.send(()).unwrap();
        publish_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }));
    let worker = {
        let backend = backend.clone();
        std::thread::spawn(move || {
            REVIEW_BEFORE_NIGHTLY_PUBLISH.with(|hook| {
                *hook.borrow_mut() = Some(Box::new(move || {
                    staged_tx.send(()).unwrap();
                    rollback_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    Err("controlled returned removal publication failure".into())
                }))
            });
            assert!(backend.start_job(APP, Work::Uninstall));
        })
    };
    staged_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(!record.path.exists());
    backend.detect_nightly_records();
    observed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    rollback_tx.send(()).unwrap();
    worker.join().unwrap();
    assert_eq!(backend.shared.manifest.lock().unwrap().apps.get(APP), Some(&record));
    assert_eq!(backend.installed(APP), Some(installed.clone()));
    assert!(std::fs::read(&record.path).unwrap().ends_with(b"old owned program bytes"));
    assert_retained(&paths);
    publish_tx.send(()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while backend.shared.detecting.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(
        backend.installed(APP),
        Some(installed),
        "paused staged-away detection hid a real restored installation with an unchanged manifest record"
    );
    assert!(backend.shared.events.lock().unwrap().iter().any(|event| matches!(event, Event::JobFinished { app, result: Err(error) } if app == APP && error.contains("controlled returned removal publication failure"))));
    assert!(!backend.has_active_jobs());
}

#[test]
fn review_unreadable_manifest_keeps_the_last_known_copy_instead_of_publishing_none() {
    const TEST: &str = "review_unreadable_manifest_keeps_the_last_known_copy_instead_of_publishing_none";
    if isolated_probe(TEST) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, record) = baseline(temp.path());
    let backend = fresh_backend(&paths);
    let app = CraftApp::by_id(APP).unwrap();
    let installed = crate::nightly::owned_installation(app, &record, &paths, backend.platform.unwrap()).unwrap();
    assert_eq!(backend.installed(APP), Some(installed.clone()));
    std::fs::write(paths.manifest(), b"{ unreadable").unwrap();
    backend.shared.events.lock().unwrap().clear();
    backend.detect_nightly_records();
    let deadline = Instant::now() + Duration::from_secs(5);
    while backend.shared.detecting.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    assert_eq!(backend.shared.manifest.lock().unwrap().apps.get(APP), Some(&record), "a read error is not an empty manifest");
    assert_eq!(backend.installed(APP), Some(installed));
    let events = backend.shared.events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(event, Event::Notice { app, result: Err(_) } if app == APP)), "{events:?}");
    assert!(!events.iter().any(|event| matches!(event, Event::Detected { app, installed: None } if app == APP)), "{events:?}");
}
