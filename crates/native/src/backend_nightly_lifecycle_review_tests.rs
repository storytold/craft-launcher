//! Pure shared-mechanism/admission fixtures; no synthetic app is executed or signed here.
#![allow(clippy::unwrap_used)] // Test setup and assertions only.
use super::*;
use artcraft_launcher_core::nightly::InstallChannel;
use artcraft_launcher_core::{Arch, InstallSource, Os, PackageKind};
use std::path::Path;
const APP: &str = "photocraft";
fn isolated(name: &str) -> bool {
    const KEY: &str = "ARTCRAFT_LIFECYCLE_REVIEW_ISOLATED";
    if std::env::var(KEY).as_deref() == Ok(name) {
        return false;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("backend::nightly_lifecycle_review_tests::{name}"), "--nocapture"])
        .env(KEY, name)
        .status()
        .unwrap();
    assert!(status.success(), "isolated lifecycle probe {name}: {status}");
    true
}
fn record(paths: &Paths) -> crate::manifest::Record {
    let file = paths.apps().join(APP).join("nonexecuted-fixture.exe");
    crate::manifest::Record {
        path: file.clone(),
        files: vec![file],
        version: "1.0.0-nightly.20261122.123456.1.gabcdefabcdef".into(),
        kind: PackageKind::PortableZip,
        installed_at: 1,
        needs_root: false,
    }
}
fn backend(root: &Path, channel: InstallChannel) -> NativeBackend {
    let mut shared = tests::shared();
    shared.channel = channel;
    let paths = Paths::under(root).for_channel(channel);
    shared.manifest.lock().unwrap().apps.insert(APP.into(), record(&paths));
    NativeBackend {
        shared: Arc::new(shared),
        paths,
        http: None,
        source: None,
        platform: Some(Platform { os: Os::Windows, arch: Arch::X64 }),
        pins: None,
        spawn: Arc::new(|_| panic!("this fixture reserves jobs; it never runs them")),
    }
}
fn detected(record: &crate::manifest::Record) -> Installed {
    Installed {
        version: version::parse(&record.version),
        source: InstallSource::Launcher,
        location: record.path.display().to_string(),
        package: None,
        externally_managed: false,
        origin: None,
        installation: None,
    }
}
fn wait_detection(backend: &NativeBackend) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while backend.shared.detecting.load(Ordering::SeqCst) {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}
#[test]
fn review_lifecycle_same_record_snapshots_reject_active_and_completed_jobs_but_publish_without_mutation() {
    const TEST: &str = "review_lifecycle_same_record_snapshots_reject_active_and_completed_jobs_but_publish_without_mutation";
    if isolated(TEST) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let backend = backend(temp.path(), InstallChannel::Nightly);
    let app = CraftApp::by_id(APP).unwrap();
    let record = backend.shared.manifest.lock().unwrap().apps.get(APP).cloned().unwrap();
    let installed = detected(&record);
    publish_detected(app, Some(installed.clone()), &backend.shared, backend.platform);
    let before = nightly_detection_snapshot(&backend.shared, app);
    let admitted = backend.prepare_job(APP, Work::Uninstall).unwrap();
    let active = nightly_detection_snapshot(&backend.shared, app);
    let accepted_while_active = publish_nightly_detection(app, &before, None, &backend.shared, backend.platform);
    publish_detected(app, Some(installed.clone()), &backend.shared, backend.platform);
    drop(admitted);
    // A rollback may restore exactly the same durable record. Terminal worker publication precedes guard retirement.
    publish_detected(app, Some(installed.clone()), &backend.shared, backend.platform);
    let accepted_old_before = publish_nightly_detection(app, &before, None, &backend.shared, backend.platform);
    publish_detected(app, Some(installed.clone()), &backend.shared, backend.platform);
    let accepted_old_active = publish_nightly_detection(app, &active, None, &backend.shared, backend.platform);
    publish_detected(app, Some(installed.clone()), &backend.shared, backend.platform);
    let ordinary = nightly_detection_snapshot(&backend.shared, app);
    let accepted_ordinary = publish_nightly_detection(app, &ordinary, Some(installed.clone()), &backend.shared, backend.platform);
    assert_eq!(backend.shared.manifest.lock().unwrap().apps.get(APP), Some(&record));
    assert_eq!(
        (accepted_while_active, accepted_old_before, accepted_old_active, accepted_ordinary),
        (false, false, false, true),
        "same-record observations need an active/revision fence, not record equality alone"
    );
    assert_eq!(backend.installed(APP), Some(installed));
}
#[test]
fn review_lifecycle_restart_admission_spans_related_channels_and_excludes_discovery() {
    const TEST: &str = "review_lifecycle_restart_admission_spans_related_channels_and_excludes_discovery";
    if isolated(TEST) {
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let stable = backend(temp.path(), InstallChannel::Stable);
    let paths = Paths::under(temp.path()).for_channel(InstallChannel::Nightly);
    let mut saved = Manifest::default();
    saved.apps.insert(APP.into(), record(&paths));
    saved.save(&paths.manifest()).unwrap();
    let mut nightly = stable.nightly_with_paths(paths.clone());
    wait_detection(&nightly);
    nightly.platform = stable.platform;
    let mut same_channel_view = stable.nightly_with_paths(paths);
    wait_detection(&same_channel_view);
    same_channel_view.platform = stable.platform;
    assert!(!Arc::ptr_eq(&stable.shared, &nightly.shared));
    assert_ne!(stable.paths.manifest(), nightly.paths.manifest());
    let stable_job = stable.prepare_job(APP, Work::Uninstall).unwrap();
    let nightly_job = nightly.prepare_job(APP, Work::Uninstall).unwrap();
    assert!(stable.shared.jobs.lock().unwrap().contains_key(APP));
    assert!(nightly.shared.jobs.lock().unwrap().contains_key(APP));
    let duplicate_same_channel_blocked = same_channel_view.prepare_job(APP, Work::Uninstall).is_none();
    drop(stable_job);
    let blocked_by_admitted_nightly = claim_backend_restart(&stable.shared, &Stop::default(), Duration::ZERO, &mut || {}).is_err();
    stable.shared.checking.store(true, Ordering::SeqCst);
    nightly.shared.checking.store(true, Ordering::SeqCst);
    nightly.shared.detecting.store(true, Ordering::SeqCst);
    let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
    let shared = stable.shared.clone();
    let waiter = std::thread::spawn(move || {
        claim_backend_restart(&shared, &Stop::default(), Duration::from_secs(5), &mut || {
            waiting_tx.send(()).unwrap();
        })
    });
    let actually_waited_for_admitted_work = waiting_rx.recv_timeout(Duration::from_secs(5)).is_ok();
    drop(nightly_job);
    assert!(!stable.has_active_jobs());
    assert!(!nightly.has_active_jobs());
    let discovery_does_not_block = waiter.join().unwrap().is_ok();
    assert!(actually_waited_for_admitted_work, "restart must actually wait for the admitted Nightly operation");
    let new_stable_blocked = stable.prepare_job(APP, Work::Uninstall).is_none();
    let new_nightly_blocked = nightly.prepare_job(APP, Work::Uninstall).is_none();
    let new_other_view_blocked = same_channel_view.prepare_job(APP, Work::Uninstall).is_none();
    assert_eq!(
        (
            duplicate_same_channel_blocked,
            blocked_by_admitted_nightly,
            discovery_does_not_block,
            new_stable_blocked,
            new_nightly_blocked,
            new_other_view_blocked
        ),
        (true, true, true, true, true, true),
        "restart/admission must share channel-qualified active identities across related contexts"
    );
}
