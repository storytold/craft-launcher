//! Source-owned nonexecuted mechanics fixtures and injected returned-I/O errors, not ENOSPC or power loss.
#![allow(clippy::unwrap_used)] // Test fixture setup/assertions only; production policy is unchanged.
use super::*;
use artcraft_launcher_core::PackageKind;
use artcraft_launcher_core::nightly::InstallChannel;
use std::cell::{Cell, RefCell};

#[derive(Clone, Copy, Debug)]
enum Fault {
    Partial,
    Sync,
}
thread_local! {
    static FAILURE: RefCell<Option<(&'static str, Fault)>> = const { RefCell::new(None) };
    static NODE_LIMIT: Cell<Option<usize>> = const { Cell::new(None) };
    static LOG_LIMIT: Cell<Option<u64>> = const { Cell::new(None) };
    static REFUSE_REPAIR: Cell<bool> = const { Cell::new(false) };
    static AFTER_CLEAN: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    static APPEND_ACTION: RefCell<Option<Box<dyn FnOnce()>>> = const { RefCell::new(None) };
    static DIRECTORY_SYNC_TARGET: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
    static REFUSE_DIRECTORY_SYNC: Cell<bool> = const { Cell::new(false) };
    static DIRECTORY_SYNC_CALLS: Cell<usize> = const { Cell::new(0) };
}
pub(super) fn refuse_repair() -> bool {
    REFUSE_REPAIR.replace(false)
}
pub(super) fn node_limit() -> Option<usize> {
    NODE_LIMIT.get()
}
pub(super) fn journal_limit() -> Option<u64> {
    LOG_LIMIT.get()
}
pub(super) fn after_clean() {
    AFTER_CLEAN.with(|slot| {
        if let Some(action) = slot.borrow_mut().take() {
            action();
        }
    });
}
fn tag(entry: &Entry) -> &'static str {
    match entry {
        Entry::Header(_) => "Header",
        Entry::Created(_) => "Created",
        Entry::Plan(_) => "Plan",
        Entry::Metadata(_) => "Metadata",
        Entry::Clean { .. } => "Clean",
    }
}
pub(super) fn injected_append(file: &mut File, entry: &Entry, bytes: &[u8]) -> Option<Result<(), String>> {
    let fault = FAILURE.with(|slot| {
        let matches = slot.borrow().as_ref().is_some_and(|(kind, _)| *kind == tag(entry));
        if matches { slot.borrow_mut().take().map(|(_, fault)| fault) } else { None }
    })?;
    APPEND_ACTION.with(|slot| {
        if let Some(action) = slot.borrow_mut().take() {
            action();
        }
    });
    let count = match fault {
        Fault::Partial => bytes.len() / 2,
        Fault::Sync => bytes.len(),
    };
    Some(
        file.write_all(&bytes[..count])
            .map_err(|e| e.to_string())
            .and_then(|()| Err(format!("injected returned {fault:?} journal error after writing {count} bytes"))),
    )
}
fn isolated(name: &str) -> bool {
    const KEY: &str = "ARTCRAFT_JOURNAL_REVIEW_ISOLATED";
    if std::env::var(KEY).as_deref() == Ok(name) {
        return false;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("nightly_transaction::review_tests::{name}"), "--nocapture"])
        .env(KEY, name)
        .status()
        .unwrap();
    assert!(status.success(), "isolated journal probe {name}: {status}");
    true
}
fn paths(root: &Path) -> Paths {
    // This root belongs to the fixture. Production must still refuse aliased ancestors.
    Paths::under(&root.canonicalize().unwrap()).for_channel(InstallChannel::Nightly)
}
fn adapter() -> Adapter {
    #[cfg(windows)]
    {
        Adapter::PhotoCraftWindowsV1
    }
    #[cfg(not(windows))]
    {
        Adapter::PhotoCraftLinuxV1
    }
}
fn fixture(root: &Path) -> (Paths, Record) {
    let paths = paths(root);
    let adapter = adapter();
    let program = paths.apps().join(adapter.app());
    std::fs::create_dir_all(&program).unwrap();
    let mut files = vec![adapter.executable().to_path_buf(), PathBuf::from("NOTICE")];
    if !cfg!(windows) {
        files.push(PathBuf::from("photocraft-nightly.png"));
    }
    for file in &files {
        std::fs::write(program.join(file), b"nonexecuted synthetic owned mechanics bytes").unwrap();
    }
    std::fs::write(program.join("foreign.txt"), b"foreign bytes").unwrap();
    let profile = adapter.profile_root(&paths).unwrap();
    std::fs::create_dir_all(&profile).unwrap();
    std::fs::write(profile.join("preferences"), b"profile bytes").unwrap();
    let record = Record {
        path: program.join(adapter.executable()),
        files: files.into_iter().map(|file| program.join(file)).collect(),
        version: "synthetic mechanics fixture; not a qualified app version".into(),
        kind: adapter.kind(),
        installed_at: 1,
        needs_root: false,
    };
    let mut manifest = Manifest::default();
    manifest.apps.insert(adapter.app().into(), record.clone());
    manifest.save(&paths.manifest()).unwrap();
    (paths, record)
}
fn kept(paths: &Paths, record: &Record) {
    assert_eq!(std::fs::read(&record.path).unwrap(), b"nonexecuted synthetic owned mechanics bytes");
    assert_eq!(Manifest::load(&paths.manifest()).apps.get(adapter().app()), Some(record));
    assert_eq!(std::fs::read(paths.apps().join(adapter().app()).join("foreign.txt")).unwrap(), b"foreign bytes");
    assert_eq!(std::fs::read(adapter().profile_root(paths).unwrap().join("preferences")).unwrap(), b"profile bytes");
}
fn retry(lease: &Lease, record: &Record) {
    remove(lease, record, &mut || lease.publish(Some(record), None).map(|_| ()), &mut |_| Ok(())).unwrap();
    assert!(!record.path.exists());
    assert!(!lease.paths.data.join(lease.journal_name()).exists());
    assert!(!lease.paths.apps().join(lease.stage_name()).exists());
    assert_eq!(std::fs::read(lease.paths.apps().join(adapter().app()).join("foreign.txt")).unwrap(), b"foreign bytes");
    assert_eq!(std::fs::read(adapter().profile_root(&lease.paths).unwrap().join("preferences")).unwrap(), b"profile bytes");
}

#[test]
fn review_returned_metadata_error_preserves_sealed_plan_and_retry() {
    const TEST: &str = "review_returned_metadata_error_preserves_sealed_plan_and_retry";
    if isolated(TEST) {
        return;
    }
    let mut outcomes = Vec::new();
    for fault in [Fault::Partial, Fault::Sync] {
        let temp = tempfile::tempdir().unwrap();
        let (paths, record) = fixture(temp.path());
        let lease = Lease::acquire(&paths, adapter()).unwrap();
        let mut preparation = lease.prepare().unwrap();
        preparation.seal(Some(&record), None).unwrap();
        let sealed = std::fs::read(paths.data.join(lease.journal_name())).unwrap();
        let mut held = None;
        preparation.apply(&mut held, &mut |_| Ok(())).unwrap();
        assert!(!record.path.exists());
        FAILURE.with(|slot| *slot.borrow_mut() = Some(("Metadata", fault)));
        let error = lease.publish(Some(&record), None).unwrap_err();
        assert!(error.contains("injected returned"));
        assert!(std::fs::read(paths.data.join(lease.journal_name())).unwrap().starts_with(&sealed), "older sealed plan prefix changed");
        let result = preparation.finish(Err(error), held, false);
        assert!(result.is_err());
        drop(preparation);
        drop(lease);
        kept(&paths, &record);
        let recovered = Lease::acquire(&paths, adapter());
        outcomes.push(format!("{fault:?}: {:?}", recovered.as_ref().err()));
        if let Ok(lease) = recovered {
            retry(&lease, &record);
        } else {
            continue;
        }
    }
    assert!(outcomes.iter().all(|outcome| outcome.ends_with("None")), "returned metadata append must reconcile and allow retry: {outcomes:?}");
}

#[test]
fn review_returned_plan_error_cannot_publish_clean_with_the_wrong_phase() {
    const TEST: &str = "review_returned_plan_error_cannot_publish_clean_with_the_wrong_phase";
    if isolated(TEST) {
        return;
    }
    const ROOT: &str = "ARTCRAFT_JOURNAL_PLAN_ERROR_ROOT";
    const FAULT: &str = "ARTCRAFT_JOURNAL_PLAN_ERROR_KIND";
    const MISSING: &str = "ARTCRAFT_JOURNAL_REVIEW_MISSING_CLEAN_HOOK";
    if let Some(root) = std::env::var_os(ROOT) {
        let paths = paths(Path::new(&root));
        let lease = Lease::acquire(&paths, adapter()).unwrap();
        let record = lease.manifest().unwrap().apps.get(adapter().app()).cloned().unwrap();
        let mut preparation = lease.prepare().unwrap();
        FAILURE
            .with(|slot| *slot.borrow_mut() = Some(("Plan", if std::env::var(FAULT).unwrap() == "partial" { Fault::Partial } else { Fault::Sync })));
        let error = preparation.seal(Some(&record), None).unwrap_err();
        assert!(error.contains("injected returned"));
        if std::env::var_os(MISSING).is_none() {
            AFTER_CLEAN.with(|slot| *slot.borrow_mut() = Some(Box::new(|| std::process::exit(47))));
        }
        preparation.abort(error);
        std::process::exit(48); // Missing AFTER_CLEAN is not the intended interruption witness.
    }
    let mut outcomes = Vec::new();
    for (fault, missing_hook) in [("partial", false), ("sync", false), ("partial", true), ("sync", true)] {
        let temp = tempfile::tempdir().unwrap();
        let (paths, record) = fixture(temp.path());
        let mut child = std::process::Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", &format!("nightly_transaction::review_tests::{TEST}"), "--nocapture"]).env(ROOT, temp.path()).env(FAULT, fault);
        if missing_hook {
            child.env(MISSING, "1");
        } else {
            child.env_remove(MISSING);
        }
        let status = child.status().unwrap();
        eprintln!("returned {fault} Plan error: missing_hook={missing_hook}, actual child status={status}");
        assert_eq!(
            status.code(),
            Some(if missing_hook { 48 } else { 47 }),
            "missing hook must be distinguishable from actual interruption after Clean"
        );
        kept(&paths, &record);
        let journal = paths.data.join(format!(".nightly-{}-journal-v1.jsonl", adapter().app()));
        if missing_hook {
            assert!(!journal.exists(), "missing-hook negative control completed cleanup, not interrupted recovery");
        } else {
            let bytes = std::fs::read(&journal).unwrap();
            let entries: Vec<Entry> =
                bytes.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()).map(|line| serde_json::from_slice(line).unwrap()).collect();
            assert!(
                matches!(entries.last(), Some(Entry::Clean { committed: None })),
                "actual interruption must leave a terminal Clean(None) before reacquisition"
            );
            assert!(!entries.iter().any(|entry| matches!(entry, Entry::Plan(_))), "failed Plan append must preserve its prior complete prefix");
            eprintln!("returned {fault} Plan error: terminal Clean(None) observed before lease reacquisition");
        }
        let recovered = Lease::acquire(&paths, adapter());
        outcomes.push(format!("{fault}: {:?}", recovered.as_ref().err()));
        if let Ok(lease) = recovered {
            retry(&lease, &record);
        }
    }
    assert!(
        outcomes.iter().all(|outcome| outcome.ends_with("None")),
        "Plan append failure must not leave a partial entry or Plan/Clean(None): {outcomes:?}"
    );
}

#[test]
fn review_live_created_node_admission_and_registration_failures_do_not_orphan_the_slot() {
    const TEST: &str = "review_live_created_node_admission_and_registration_failures_do_not_orphan_the_slot";
    if isolated(TEST) {
        return;
    }
    let mut outcomes = Vec::new();
    for kind in ["directory-capacity", "file-capacity", "returned-created-write"] {
        let temp = tempfile::tempdir().unwrap();
        let (paths, record) = fixture(temp.path());
        let lease = Lease::acquire(&paths, adapter()).unwrap();
        let mut preparation = lease.prepare().unwrap();
        preparation.directories(Path::new("unpacked")).unwrap();
        if kind == "returned-created-write" {
            FAILURE.with(|slot| *slot.borrow_mut() = Some(("Created", Fault::Partial)));
        } else {
            NODE_LIMIT.set(Some(1));
        }
        let error = if kind == "directory-capacity" {
            preparation.directories(Path::new("unpacked/overflow")).err().unwrap()
        } else {
            preparation.create(Path::new("unpacked/overflow")).err().unwrap()
        };
        NODE_LIMIT.set(None);
        let diagnostic = preparation.abort(error);
        drop(preparation);
        drop(lease);
        kept(&paths, &record);
        let recovered = Lease::acquire(&paths, adapter());
        outcomes.push(format!("{kind}: {:?}; {diagnostic}", recovered.as_ref().err()));
        if let Ok(lease) = recovered {
            retry(&lease, &record);
        }
    }
    assert!(
        outcomes.iter().all(|outcome| outcome.contains(": None;")),
        "known live-created objects must not permanently orphan staging: {outcomes:?}"
    );
}

#[test]
fn review_registration_failure_preserves_unknown_arrivals_without_adopting_them() {
    const TEST: &str = "review_registration_failure_preserves_unknown_arrivals_without_adopting_them";
    if isolated(TEST) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, record) = fixture(temp.path());
    let lease = Lease::acquire(&paths, adapter()).unwrap();
    let mut preparation = lease.prepare().unwrap();
    preparation.directories(Path::new("unpacked")).unwrap();
    let stage = preparation.path();
    let foreign = stage.join("unpacked/foreign");
    let arrival = foreign.clone();
    APPEND_ACTION.with(|slot| *slot.borrow_mut() = Some(Box::new(move || std::fs::write(arrival, b"unregistered foreign arrival").unwrap())));
    FAILURE.with(|slot| *slot.borrow_mut() = Some(("Created", Fault::Partial)));
    let error = preparation.create(Path::new("unpacked/known-new")).unwrap_err();
    let diagnostic = preparation.abort(error);
    drop(preparation);
    drop(lease);
    assert_eq!(std::fs::read(&foreign).unwrap(), b"unregistered foreign arrival");
    kept(&paths, &record);
    assert!(!stage.join("unpacked/known-new").exists(), "remove only the returned-failure call's own captured object");
    assert!(Lease::acquire(&paths, adapter()).is_err(), "unknown arrival must remain preserved, not count as clean");
    assert!(diagnostic.contains("preserved"));
}

#[test]
fn review_plan_and_metadata_admission_reserve_terminal_clean_inside_the_same_bound() {
    const TEST: &str = "review_plan_and_metadata_admission_reserve_terminal_clean_inside_the_same_bound";
    if isolated(TEST) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, record) = fixture(temp.path());
    let lease = Lease::acquire(&paths, adapter()).unwrap();
    let mut preparation = lease.prepare().unwrap();
    preparation.seal(Some(&record), None).unwrap();
    let plan = preparation.plan.clone().unwrap();
    let plan_entry = Entry::Plan(Box::new(plan));
    let mut plan_bytes = serde_json::to_vec(&plan_entry).unwrap();
    plan_bytes.push(b'\n');
    let journal = std::fs::read(paths.data.join(lease.journal_name())).unwrap();
    let before_plan = journal[..journal.len() - plan_bytes.len()].to_vec();
    let scratch = lease.data.file(Path::new("probe-scratch"), true).unwrap();
    let scratch_id = lease.data.file_id(Path::new("probe-scratch"), &scratch).unwrap();
    drop(scratch);
    let metadata =
        Entry::Metadata(Node { path: format!(".nightly-{}-manifest-v1.pending", adapter().app()).into(), id: scratch_id, directory: false });
    let clean = Entry::Clean { committed: Some(false) };
    let clean_len = serde_json::to_vec(&clean).unwrap().len() as u64 + 1;
    let mut outcomes = Vec::new();
    for (kind, entry, prefix) in [("Plan", &plan_entry, &before_plan), ("Metadata", &metadata, &journal)] {
        let entry_len = serde_json::to_vec(entry).unwrap().len() as u64 + 1;
        for real_bound in [false, true] {
            let exact = if real_bound { LIMIT } else { prefix.len() as u64 + entry_len + clean_len };
            // Empty complete lines are legal in the strict journal grammar. This
            // makes a valid complete prefix at the actual 16 MiB bound, not sparse garbage.
            let mut padded = prefix.to_vec();
            padded.resize((exact - entry_len - clean_len) as usize, b'\n');
            let mut file = File::create(temp.path().join(format!("{kind}-{real_bound}-insufficient"))).unwrap();
            file.write_all(&padded).unwrap();
            file.write_all(b"\n").unwrap();
            LOG_LIMIT.set(if real_bound { None } else { Some(exact) });
            let admitted = append(&mut file, entry);
            let terminal = admitted.as_ref().ok().map(|_| append(&mut file, &clean).is_ok());
            outcomes.push(format!("{kind} real_bound={real_bound}: accepted={}, terminal={terminal:?}", admitted.is_ok()));
            assert!(file.metadata().unwrap().len() <= exact);
            let mut exact_file = File::create(temp.path().join(format!("{kind}-{real_bound}-exact"))).unwrap();
            exact_file.write_all(&padded).unwrap();
            append(&mut exact_file, entry).unwrap();
            append(&mut exact_file, &clean).unwrap();
            assert_eq!(exact_file.metadata().unwrap().len(), exact);
            LOG_LIMIT.set(None);
        }
    }
    assert!(outcomes.iter().all(|outcome| outcome.contains("accepted=false")), "nonterminal admission must preserve Clean room: {outcomes:?}");
}

#[test]
fn review_unknown_truncated_restart_journal_remains_unmodified_and_unadopted() {
    const TEST: &str = "review_unknown_truncated_restart_journal_remains_unmodified_and_unadopted";
    if isolated(TEST) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, record) = fixture(temp.path());
    let lease = Lease::acquire(&paths, adapter()).unwrap();
    let mut preparation = lease.prepare().unwrap();
    preparation.file.as_mut().unwrap().write_all(b"{unknown truncated suffix").unwrap();
    let path = paths.data.join(lease.journal_name());
    let bytes = std::fs::read(&path).unwrap();
    drop(preparation);
    drop(lease);
    assert!(Lease::acquire(&paths, adapter()).is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    kept(&paths, &record);
}

#[test]
fn review_unreconciled_live_append_cannot_retire_or_rephase_its_journal() {
    const TEST: &str = "review_unreconciled_live_append_cannot_retire_or_rephase_its_journal";
    if isolated(TEST) {
        return;
    }
    for fault in [Fault::Partial, Fault::Sync] {
        let temp = tempfile::tempdir().unwrap();
        let (paths, record) = fixture(temp.path());
        let lease = Lease::acquire(&paths, adapter()).unwrap();
        let mut preparation = lease.prepare().unwrap();
        FAILURE.with(|slot| *slot.borrow_mut() = Some(("Plan", fault)));
        REFUSE_REPAIR.set(true);
        let error = preparation.seal(Some(&record), None).unwrap_err();
        assert!(error.contains("could not be restored"));
        let journal = paths.data.join(lease.journal_name());
        let retained = std::fs::read(&journal).unwrap();
        let diagnostic = preparation.abort(error);
        assert!(diagnostic.contains("could not be reconciled"));
        assert_eq!(std::fs::read(&journal).unwrap(), retained);
        drop(preparation);
        drop(lease);
        kept(&paths, &record);
        let recovered = Lease::acquire(&paths, adapter());
        match fault {
            Fault::Partial => {
                assert!(recovered.is_err());
                assert_eq!(std::fs::read(journal).unwrap(), retained);
            }
            Fault::Sync => {
                let lease = recovered.unwrap();
                kept(&paths, &record);
                retry(&lease, &record);
            }
        }
    }
}

#[cfg(unix)]
#[test]
fn review_test_owned_alias_root_is_resolved_but_production_alias_is_refused() {
    const TEST: &str = "review_test_owned_alias_root_is_resolved_but_production_alias_is_refused";
    if isolated(TEST) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("real-test-owned-root");
    std::fs::create_dir(&root).unwrap();
    let alias = temp.path().join("test-owned-root-alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let raw = Paths::under(&alias).for_channel(InstallChannel::Nightly);
    assert!(Lease::acquire(&raw, adapter()).is_err(), "production must continue rejecting ancestor aliases");
    let (resolved, record) = fixture(&alias);
    let lease = Lease::acquire(&resolved, adapter()).unwrap();
    kept(&resolved, &record);
    retry(&lease, &record);
}

// Returned error at the existing shared API boundary. On Windows this models its
// caller contract, not a new native durability syscall or physical power loss.
pub(super) fn directory_sync(path: &Path) -> Result<(), String> {
    if !DIRECTORY_SYNC_TARGET.with(|target| target.borrow().as_deref() == Some(path)) {
        return Ok(());
    }
    DIRECTORY_SYNC_CALLS.set(DIRECTORY_SYNC_CALLS.get() + 1);
    if REFUSE_DIRECTORY_SYNC.get() { Err("injected returned manifest-directory sync error".into()) } else { Ok(()) }
}

type BackupSnapshot = Vec<(PathBuf, Identity, Vec<u8>)>;
fn barrier_preparation<'a>(lease: &'a Lease, record: &Record) -> (Preparation<'a>, Option<Dir>, String, Vec<u8>, BackupSnapshot) {
    let mut preparation = lease.prepare().unwrap();
    preparation.seal(Some(record), None).unwrap();
    let mut held = None;
    preparation.apply(&mut held, &mut |_| Ok(())).unwrap();
    assert!(!record.path.exists(), "the actual executable has been staged into the backup");
    let backups = preparation
        .plan
        .as_ref()
        .unwrap()
        .old
        .iter()
        .map(|node| {
            let path = preparation.path().join("backup").join(&node.path);
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(bytes, b"nonexecuted synthetic owned mechanics bytes");
            (path, node.id.clone(), bytes)
        })
        .collect();
    DIRECTORY_SYNC_TARGET.with(|target| *target.borrow_mut() = Some(lease.paths.data.clone()));
    DIRECTORY_SYNC_CALLS.set(0);
    REFUSE_DIRECTORY_SYNC.set(true);
    let error = lease.publish(Some(record), None).unwrap_err();
    assert!(error.contains("injected returned manifest-directory sync error"));
    assert_eq!(DIRECTORY_SYNC_CALLS.get(), 1, "the actual publish rename reached its directory barrier and returned an error");
    assert!(!lease.manifest().unwrap().apps.contains_key(adapter().app()), "the renamed manifest is already visibly next despite the sync error");
    let journal = std::fs::read(lease.paths.data.join(lease.journal_name())).unwrap();
    let entries: Vec<Entry> =
        journal.split(|byte| *byte == b'\n').filter(|line| !line.is_empty()).map(|line| serde_json::from_slice(line).unwrap()).collect();
    assert!(entries.iter().any(|entry| matches!(entry, Entry::Plan(_))));
    assert!(!entries.iter().any(|entry| matches!(entry, Entry::Clean { .. })));
    (preparation, held, error, journal, backups)
}
fn barrier_retained(paths: &Paths, record: &Record, journal: &[u8], backups: &BackupSnapshot) {
    let journal_path = paths.data.join(format!(".nightly-{}-journal-v1.jsonl", adapter().app()));
    let found = std::fs::read(&journal_path).ok();
    let all_bytes_retained = backups.iter().all(|(path, _, bytes)| std::fs::read(path).as_ref().ok() == Some(bytes));
    eprintln!(
        "failed manifest barrier: exact sealed journal retained={}, all actual backup bytes retained={all_bytes_retained}",
        found.as_deref() == Some(journal)
    );
    assert!(found.as_deref() == Some(journal), "no Clean append or journal retirement before a successful manifest-directory barrier");
    assert!(all_bytes_retained, "the actual rollback files must remain until a successful directory barrier");
    for (path, id, _) in backups {
        let parent = Dir::open(path.parent().unwrap()).unwrap();
        assert_eq!(parent.info(Path::new(path.file_name().unwrap())).unwrap(), Some((id.clone(), false)), "preserve the captured backup identity");
    }
    assert!(!record.path.exists());
    assert!(!Manifest::load(&paths.manifest()).apps.contains_key(adapter().app()));
    assert_eq!(std::fs::read(paths.apps().join(adapter().app()).join("foreign.txt")).unwrap(), b"foreign bytes");
    assert_eq!(std::fs::read(adapter().profile_root(paths).unwrap().join("preferences")).unwrap(), b"profile bytes");
}
fn barrier_removed(paths: &Paths, record: &Record) {
    assert!(!record.path.exists());
    assert!(!Manifest::load(&paths.manifest()).apps.contains_key(adapter().app()));
    assert!(!paths.data.join(format!(".nightly-{}-journal-v1.jsonl", adapter().app())).exists());
    assert!(!paths.apps().join(format!(".nightly-{}-preparation-v1", adapter().app())).exists());
    assert_eq!(std::fs::read(paths.apps().join(adapter().app()).join("foreign.txt")).unwrap(), b"foreign bytes");
    assert_eq!(std::fs::read(adapter().profile_root(paths).unwrap().join("preferences")).unwrap(), b"profile bytes");
}

#[test]
fn review_manifest_directory_barrier_preserves_live_backups_until_successful_retry() {
    const TEST: &str = "review_manifest_directory_barrier_preserves_live_backups_until_successful_retry";
    if isolated(TEST) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, record) = fixture(temp.path());
    let lease = Lease::acquire(&paths, adapter()).unwrap();
    let (mut preparation, held, error, journal, backups) = barrier_preparation(&lease, &record);
    let failure = preparation.finish(Err(error.clone()), held, false).unwrap_err();
    assert!(failure.contains("injected returned manifest-directory sync error"));
    barrier_retained(&paths, &record, &journal, &backups);
    assert!(preparation.finished.is_none(), "a failed barrier must not advance live finalization state");
    REFUSE_DIRECTORY_SYNC.set(false);
    let result = preparation.finish(Err(error), None, false);
    assert!(result.unwrap_err().contains("The new Nightly state is committed"), "successful recovery retains the original returned publish error");
    assert_eq!(preparation.finished, Some(true));
    assert!(DIRECTORY_SYNC_CALLS.get() > 2, "retry actually revisits the existing directory-sync API");
    barrier_removed(&paths, &record);
    eprintln!("live barrier retry succeeded; sealed journal/backups retired only after successful sync");
}

#[test]
fn review_manifest_directory_barrier_preserves_fresh_lease_backups_until_successful_retry() {
    const TEST: &str = "review_manifest_directory_barrier_preserves_fresh_lease_backups_until_successful_retry";
    if isolated(TEST) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let (paths, record) = fixture(temp.path());
    let lease = Lease::acquire(&paths, adapter()).unwrap();
    let (preparation, held, _, journal, backups) = barrier_preparation(&lease, &record);
    drop(held);
    drop(preparation);
    drop(lease);
    let refused = Lease::acquire(&paths, adapter());
    assert!(refused.as_ref().err().is_some_and(|error| error.contains("injected returned manifest-directory sync error")));
    drop(refused);
    barrier_retained(&paths, &record, &journal, &backups);
    REFUSE_DIRECTORY_SYNC.set(false);
    let recovered = Lease::acquire(&paths, adapter()).unwrap();
    assert!(DIRECTORY_SYNC_CALLS.get() > 2, "fresh lease retry actually revisits the existing directory-sync API");
    barrier_removed(&paths, &record);
    drop(recovered);
    eprintln!("fresh-lease barrier retry succeeded; sealed journal/backups retired only after successful sync");
}

#[test]
fn review_manifest_directory_barrier_preserves_ordinary_success_and_rollback() {
    const TEST: &str = "review_manifest_directory_barrier_preserves_ordinary_success_and_rollback";
    if isolated(TEST) {
        return;
    }
    for committed in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let (paths, record) = fixture(temp.path());
        let lease = Lease::acquire(&paths, adapter()).unwrap();
        let mut preparation = lease.prepare().unwrap();
        preparation.seal(Some(&record), None).unwrap();
        let mut held = None;
        preparation.apply(&mut held, &mut |_| Ok(())).unwrap();
        let outcome = if committed {
            lease.publish(Some(&record), None).unwrap();
            Ok(())
        } else {
            Err("injected ordinary failure before manifest publication".into())
        };
        let result = preparation.finish(outcome, held, committed);
        assert_eq!(result.is_ok(), committed);
        if committed {
            barrier_removed(&paths, &record);
        } else {
            kept(&paths, &record);
            assert!(!paths.data.join(lease.journal_name()).exists());
            assert!(!paths.apps().join(lease.stage_name()).exists());
        }
    }
}

#[cfg(target_os = "linux")]
thread_local! {
    static DIAGNOSTIC_LOCK_ATTEMPT: Cell<usize> = const { Cell::new(0) };
    static DIAGNOSTIC_SPAWNER: RefCell<Option<std::thread::JoinHandle<std::process::ExitStatus>>> = const { RefCell::new(None) };
}
#[cfg(target_os = "linux")]
pub(super) fn diagnose_lock_attempt(path: &Path, file: &File, acquired: bool) {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    let Some(root) = std::env::var_os("ARTCRAFT_NIGHTLY_LOCK_DIAG") else { return };
    if path.file_name().unwrap() != ".nightly-photocraft-mutation-v1.lock" {
        return;
    }
    let root = PathBuf::from(root);
    let attempt = DIAGNOSTIC_LOCK_ATTEMPT.get() + 1;
    DIAGNOSTIC_LOCK_ATTEMPT.set(attempt);
    let metadata = file.metadata().unwrap();
    let observation = serde_json::json!({"attempt":attempt,"path":path,"acquired":acquired,"fd":file.as_raw_fd(),"device":metadata.dev(),"inode":metadata.ino(),"pid":std::process::id(),"fdinfo":std::fs::read_to_string(format!("/proc/self/fdinfo/{}",file.as_raw_fd())).unwrap()});
    std::fs::write(root.join(format!("attempt-{attempt}.json")), serde_json::to_vec_pretty(&observation).unwrap()).unwrap();
    let wait = |name: &str| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !root.join(name).exists() {
            assert!(std::time::Instant::now() < deadline, "diagnostic handshake timed out at {name}");
            std::thread::yield_now();
        }
    };
    if attempt == 2 && acquired {
        DIAGNOSTIC_SPAWNER.with(|slot| {
            *slot.borrow_mut() = Some(std::thread::spawn(|| {
                std::process::Command::new(std::env::current_exe().unwrap())
                    .arg("--list")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .unwrap()
            }))
        });
        wait("fork-held");
    } else if attempt == 3 {
        std::fs::write(root.join("release"), []).unwrap();
        wait("fork-released");
        DIAGNOSTIC_SPAWNER.with(|slot| assert!(slot.borrow_mut().take().unwrap().join().unwrap().success()));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn review_lock_guard_retirement_releases_mutation_and_manifest_with_retained_description() {
    const TEST: &str = "review_lock_guard_retirement_releases_mutation_and_manifest_with_retained_description";
    if isolated(TEST) {
        return;
    }
    let mut observed = Vec::new();
    for mutation in [true, false] {
        let temp = tempfile::tempdir().unwrap();
        let (paths, _) = fixture(temp.path());
        let lease = Lease::acquire(&paths, adapter()).unwrap();
        if mutation {
            let retained = lease._lock.0.try_clone().unwrap();
            assert!(Lease::acquire(&paths, adapter()).is_err(), "the actual live owner must still exclude a second mutation");
            drop(lease);
            let next = Lease::acquire(&paths, adapter());
            observed.push(("mutation", next.is_ok()));
            if let Ok(next) = next {
                drop(retained);
                assert!(Lease::acquire(&paths, adapter()).is_err(), "closing an old description must not unlock the new owner");
                drop(next);
            }
        } else {
            let guard = lock(&lease.data, ".nightly-manifest-v1.lock").unwrap();
            let retained = guard.0.try_clone().unwrap();
            assert!(lease.manifest().is_err(), "the live manifest owner must still exclude a second guard");
            drop(guard);
            observed.push(("manifest", lease.manifest().is_ok()));
            drop(retained);
        }
    }
    eprintln!("actual guard retirement with old open descriptions retained: {observed:?}");
    assert!(
        observed.iter().all(|(_, released)| *released),
        "an operation's retired lock must not depend on another description's later close: {observed:?}"
    );
}

#[test]
fn an_empty_journal_left_before_its_header_does_not_lock_out_later_leases() {
    let temp = tempfile::tempdir().unwrap();
    let (paths, record) = fixture(temp.path());
    let journal = paths.data.join(".nightly-photocraft-journal-v1.jsonl");
    let stage = paths.apps().join(".nightly-photocraft-preparation-v1");
    // Crash after creating the journal, before the stage.
    std::fs::write(&journal, b"").unwrap();
    drop(Lease::acquire(&paths, adapter()).unwrap());
    assert!(!journal.exists());
    // Crash after creating the empty stage, before the header.
    std::fs::write(&journal, b"").unwrap();
    std::fs::create_dir(&stage).unwrap();
    drop(Lease::acquire(&paths, adapter()).unwrap());
    assert!(!journal.exists() && !stage.exists());
    // Anything inside an unsealed stage is never adopted or deleted.
    std::fs::write(&journal, b"").unwrap();
    std::fs::create_dir(&stage).unwrap();
    std::fs::write(stage.join("unknown"), b"preserved").unwrap();
    assert!(Lease::acquire(&paths, adapter()).is_err());
    assert!(journal.exists());
    assert_eq!(std::fs::read(stage.join("unknown")).unwrap(), b"preserved");
    // A journal with bytes that are not a header is still refused, not cleared.
    std::fs::remove_file(stage.join("unknown")).unwrap();
    std::fs::write(&journal, b"{").unwrap();
    assert!(Lease::acquire(&paths, adapter()).is_err());
    assert_eq!(std::fs::read(&journal).unwrap(), b"{");
    kept(&paths, &record);
}

#[test]
fn record_paths_are_made_relative_to_the_records_own_proven_root() {
    // Windows records the installer's `\\?\C:\…` spelling while Paths keeps `C:\…`; this models
    // that with a case difference the injected comparator treats as the same folder.
    let owned = Path::new("/data/apps/photocraft");
    let record = Record {
        path: "/DATA/APPS/photocraft/photocraft".into(),
        files: vec!["/DATA/APPS/photocraft/photocraft".into(), "/DATA/APPS/photocraft/docs/NOTICE".into()],
        version: "synthetic".into(),
        kind: PackageKind::TarGz,
        installed_at: 1,
        needs_root: false,
    };
    let same = |left: &Path, right: &Path| Ok(left.to_string_lossy().to_lowercase() == right.to_string_lossy().to_lowercase());
    assert!(record.files[0].strip_prefix(owned).is_err(), "the fixture reproduces the reported spelling mismatch");
    let root = record_root_with(&record, owned, Path::new("photocraft"), same).unwrap();
    let relative: Vec<_> = record.files.iter().map(|file| file.strip_prefix(root).unwrap().to_path_buf()).collect();
    assert_eq!(relative, [PathBuf::from("photocraft"), PathBuf::from("docs/NOTICE")]);
    let elsewhere = Record { path: "/data/apps/other/photocraft".into(), ..record.clone() };
    assert!(record_root_with(&elsewhere, owned, Path::new("photocraft"), same).is_err(), "a record outside the owned folder is still refused");
}
