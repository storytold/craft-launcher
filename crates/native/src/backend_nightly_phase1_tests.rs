#![cfg_attr(not(any(target_os = "linux", target_os = "macos", target_os = "windows")), allow(dead_code))] // Helpers for tests that run only where Nightly installs are staged.
use super::*;
use crate::nightly_lookup::{self, Outcome};

fn fixture() -> (tempfile::TempDir, Arc<Shared>, Paths, Platform) {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::under(&temp.path().canonicalize().unwrap()).for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
    let mut shared = tests::shared();
    shared.channel = artcraft_launcher_core::nightly::InstallChannel::Nightly;
    shared.options.lock().unwrap().nightly_enabled = true;
    (temp, Arc::new(shared), paths, Platform { os: Os::Linux, arch: artcraft_launcher_core::Arch::X64 })
}
fn app() -> &'static CraftApp {
    CraftApp::by_id("photocraft").unwrap()
}
fn complete(shared: &Shared, paths: &Paths, platform: Platform, run: u64) {
    let key = nightly_lookup::key(app(), platform);
    let now = crate::now();
    shared.nightly_state.lock().unwrap().begin(&[key], now, true, true);
    finish_nightly_result(app(), platform, 0, Outcome::Verified(Box::new(nightly_lookup::tests::verified(run))), shared, paths);
    shared.nightly_state.lock().unwrap().finish();
}
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))] // A Nightly install plan is staged only there.
#[test]
fn selection_ui_and_install_gate_agree_and_failed_refresh_retains_stale_success() {
    let (_temp, shared, paths, platform) = fixture();
    complete(&shared, &paths, platform, 20);
    assert_eq!(installable_event(app(), &shared, Some(platform), None), Some(Event::Installable { app: "photocraft".into(), installable: true }));
    assert!(nightly_install_plan(app(), &shared, &paths, Some(platform)).is_ok());
    let key = nightly_lookup::key(app(), platform);
    let verified_at = shared.nightly_state.lock().unwrap().record(&key).verified_at;
    let previous = shared.latest.lock().unwrap().get("photocraft").cloned().unwrap();
    finish_nightly_result(
        app(),
        platform,
        0,
        Outcome::Indeterminate(crate::http::NightlyHttpError { why: "offline".into(), retry_at: Some(crate::now() + 120) }),
        &shared,
        &paths,
    );
    assert_eq!(shared.latest.lock().unwrap().get("photocraft"), Some(&previous));
    assert!(shared.nightly_verified.lock().unwrap().contains_key("photocraft"));
    assert_eq!(shared.nightly_state.lock().unwrap().record(&key).verified_at, verified_at);
    assert!(nightly_install_plan(app(), &shared, &paths, Some(platform)).is_err());
    assert_eq!(installable_event(app(), &shared, Some(platform), None), Some(Event::Installable { app: "photocraft".into(), installable: false }));
}
#[test]
fn later_off_and_off_then_on_drop_older_generation_without_promoting_or_advancing_success() {
    let (_temp, shared, paths, platform) = fixture();
    complete(&shared, &paths, platform, 20);
    let previous = shared.latest.lock().unwrap().get("photocraft").cloned().unwrap();
    let key = nightly_lookup::key(app(), platform);
    let before = shared.nightly_state.lock().unwrap().record(&key).verified_at;
    shared.options.lock().unwrap().nightly_enabled = false;
    shared.nightly_state.lock().unwrap().invalidate();
    finish_nightly_result(app(), platform, 0, Outcome::Verified(Box::new(nightly_lookup::tests::verified(30))), &shared, &paths);
    shared.options.lock().unwrap().nightly_enabled = true;
    shared.nightly_state.lock().unwrap().invalidate();
    finish_nightly_result(app(), platform, 0, Outcome::Verified(Box::new(nightly_lookup::tests::verified(30))), &shared, &paths);
    assert_eq!(shared.latest.lock().unwrap().get("photocraft"), Some(&previous));
    assert_eq!(shared.nightly_state.lock().unwrap().record(&key).verified_at, before);
    assert!(nightly_install_plan(app(), &shared, &paths, Some(platform)).is_err());
}
#[test]
fn an_older_complete_response_does_not_implicitly_regress_the_verified_selection() {
    let (_temp, shared, paths, platform) = fixture();
    complete(&shared, &paths, platform, 20);
    let key = nightly_lookup::key(app(), platform);
    let before = shared.nightly_state.lock().unwrap().record(&key).verified_at;
    finish_nightly_result(app(), platform, 0, Outcome::Verified(Box::new(nightly_lookup::tests::verified(10))), &shared, &paths);
    assert!(shared.latest.lock().unwrap().get("photocraft").unwrap().tag.contains(".20.1."));
    let record = shared.nightly_state.lock().unwrap().record(&key);
    assert_eq!(record.verified_at, before);
    assert!(!record.complete);
    assert!(record.message.unwrap().contains("regress"));
}

#[test]
fn actual_backend_fresh_off_schedules_nothing_and_later_off_cancels_queued_sweep() {
    let (_temp, shared, paths, platform) = fixture();
    shared.options.lock().unwrap().nightly_enabled = false;
    type Task = Box<dyn FnOnce() + Send>;
    let queued: Arc<Mutex<Vec<Task>>> = Arc::new(Mutex::new(Vec::new()));
    let target = queued.clone();
    let mut backend = NativeBackend {
        shared: shared.clone(),
        paths: paths.clone(),
        http: None,
        source: None,
        platform: Some(platform),
        pins: None,
        spawn: Arc::new(move |task| target.lock().unwrap().push(task)),
    };
    backend.check(false);
    backend.check(true);
    assert!(queued.lock().unwrap().is_empty());
    assert!(!paths.nightly_checks().exists());
    backend.send(Command::Configure(BackendOptions { nightly_enabled: true, ..Default::default() }));
    backend.check(true);
    backend.check(false);
    assert_eq!(queued.lock().unwrap().len(), 1, "actual concurrent asks coalesce before spawning");
    backend.send(Command::Configure(BackendOptions::default()));
    let task = queued.lock().unwrap().remove(0);
    task();
    assert!(queued.lock().unwrap().is_empty());
    assert!(shared.nightly_verified.lock().unwrap().is_empty());
    let record = shared.nightly_state.lock().unwrap().record(&nightly_lookup::key(app(), platform));
    assert!(record.attempted_at.is_some());
    assert!(record.verified_at.is_none());
    assert!(!shared.checking.load(Ordering::SeqCst));
}

#[cfg(unix)]
#[test]
fn failed_attempt_metadata_write_refreshes_stale_ui_state_without_spawning_or_changing_stable_data() {
    let (temp, shared, paths, platform) = fixture();
    complete(&shared, &paths, platform, 20);
    let key = nightly_lookup::key(app(), platform);
    let before = shared.nightly_state.lock().unwrap().record(&key);
    let sentinel = temp.path().join("stable-sentinel");
    std::fs::write(&sentinel, b"stable data").unwrap();
    std::fs::remove_file(paths.nightly_checks()).unwrap();
    std::fs::hard_link(&sentinel, paths.nightly_checks()).unwrap();
    shared.events.lock().unwrap().clear();
    let spawned = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = spawned.clone();
    let backend = NativeBackend {
        shared: shared.clone(),
        paths: paths.clone(),
        http: None,
        source: None,
        platform: Some(platform),
        pins: None,
        spawn: Arc::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
    };
    backend.check_nightly_at(true, before.attempted_at.unwrap() + nightly_lookup::EVERY);
    assert_eq!(spawned.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::read(sentinel).unwrap(), b"stable data");
    assert_eq!(shared.nightly_state.lock().unwrap().record(&key).verified_at, before.verified_at);
    let events = shared.events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(event,Event::NightlyStatus{status,..} if status.stale && status.verified_at==before.verified_at)));
    assert!(events.iter().any(|event| matches!(event, Event::Installable { installable: false, .. })));
    assert!(shared.nightly_verified.lock().unwrap().contains_key("photocraft"));
}

#[test]
fn failed_result_save_retains_maximum_known_retry_and_flushes_it_before_admission() {
    let (temp, shared, paths, platform) = fixture();
    complete(&shared, &paths, platform, 20);
    let key = nightly_lookup::key(app(), platform);
    let before = shared.nightly_state.lock().unwrap().record(&key);
    let deadline = before.attempted_at.unwrap() + 3600;
    let saved = temp.path().join("before-result-checks.json");
    std::fs::rename(paths.nightly_checks(), &saved).unwrap();
    std::fs::create_dir(paths.nightly_checks()).unwrap();
    finish_nightly_result(
        app(),
        platform,
        0,
        Outcome::Indeterminate(crate::http::NightlyHttpError { why: "limited".into(), retry_at: Some(deadline) }),
        &shared,
        &paths,
    );
    {
        let mut state = shared.nightly_state.lock().unwrap();
        state.finish();
        state.outcome(
            &key,
            before.attempted_at.unwrap() + 20,
            &Outcome::Indeterminate(crate::http::NightlyHttpError { why: "subsequent storage failure".into(), retry_at: Some(deadline - 60) }),
        );
        let failed = state.record(&key);
        assert_eq!(failed.retry_at, Some(deadline));
        assert_eq!(failed.verified_at, before.verified_at);
        assert_eq!(failed.tag, before.tag);
        assert!(!failed.complete);
    }
    assert!(shared.nightly_verified.lock().unwrap().contains_key("photocraft"));
    assert!(nightly_install_plan(app(), &shared, &paths, Some(platform)).is_err());
    assert!(shared.events.lock().unwrap().iter().any(|event|matches!(event,Event::NightlyStatus{status,..} if status.stale && status.retry_at==Some(deadline) && status.verified_at==before.verified_at)));
    std::fs::remove_dir(paths.nightly_checks()).unwrap();
    std::fs::rename(saved, paths.nightly_checks()).unwrap();
    assert_eq!(nightly_lookup::State::load(&paths).record(&key).retry_at, None, "uncommitted retry is explicitly not durable across process loss");
    let spawned = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = spawned.clone();
    let backend = NativeBackend {
        shared: shared.clone(),
        paths: paths.clone(),
        http: None,
        source: None,
        platform: Some(platform),
        pins: None,
        spawn: Arc::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
    };
    backend.check_nightly_at(true, before.attempted_at.unwrap() + 60);
    assert_eq!(spawned.load(Ordering::SeqCst), 0, "storage recovery cannot bypass a known server deadline");
    let restored = nightly_lookup::State::load(&paths).record(&key);
    assert_eq!(restored.retry_at, Some(deadline), "recovered storage commits pending backoff even while admission remains denied");
    assert_eq!(restored.verified_at, before.verified_at);
    assert!(!restored.complete);
}

#[cfg(unix)]
#[test]
fn unreadable_existing_metadata_blocks_restart_without_replacement_until_valid_reload() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let (_temp, original, paths, platform) = fixture();
    complete(&original, &paths, platform, 20);
    let key = nightly_lookup::key(app(), platform);
    let before = original.nightly_state.lock().unwrap().record(&key);
    let deadline = before.attempted_at.unwrap() + 3600;
    finish_nightly_result(
        app(),
        platform,
        0,
        Outcome::Indeterminate(crate::http::NightlyHttpError { why: "limited".into(), retry_at: Some(deadline) }),
        &original,
        &paths,
    );
    let bytes = std::fs::read(paths.nightly_checks()).unwrap();
    let inode = std::fs::metadata(paths.nightly_checks()).unwrap().ino();
    std::fs::set_permissions(paths.nightly_checks(), std::fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(
        std::fs::File::open(paths.nightly_checks()).unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied,
        "fixture must run as an ordinary owner, like Launcher"
    );
    let mut restarted = tests::shared();
    restarted.channel = artcraft_launcher_core::nightly::InstallChannel::Nightly;
    restarted.options.lock().unwrap().nightly_enabled = true;
    *restarted.nightly_state.get_mut().unwrap() = nightly_lookup::State::load(&paths);
    let restarted = Arc::new(restarted);
    let spawned = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = spawned.clone();
    let backend = NativeBackend {
        shared: restarted.clone(),
        paths: paths.clone(),
        http: None,
        source: None,
        platform: Some(platform),
        pins: None,
        spawn: Arc::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
        }),
    };
    backend.check_nightly_at(true, before.attempted_at.unwrap() + 60);
    assert_eq!(spawned.load(Ordering::SeqCst), 0);
    assert_eq!(std::fs::metadata(paths.nightly_checks()).unwrap().ino(), inode, "writable parent must not replace unreadable metadata");
    assert_eq!(std::fs::metadata(paths.nightly_checks()).unwrap().permissions().mode() & 0o777, 0);
    assert!(restarted.events.lock().unwrap().iter().any(|event|matches!(event,Event::NightlyStatus{status,..} if status.stale && status.message.as_ref().is_some_and(|why|why.contains("Could not read")))));
    std::fs::set_permissions(paths.nightly_checks(), std::fs::Permissions::from_mode(0o600)).unwrap();
    backend.check_nightly_at(true, before.attempted_at.unwrap() + 60);
    assert_eq!(spawned.load(Ordering::SeqCst), 0, "valid reload recovers and honors the stored retry deadline");
    assert_eq!(restarted.nightly_state.lock().unwrap().record(&key).retry_at, Some(deadline));
    assert_eq!(std::fs::read(paths.nightly_checks()).unwrap(), bytes);
    backend.check_nightly_at(true, deadline);
    assert_eq!(spawned.load(Ordering::SeqCst), 1, "admission resumes only after valid recovery and the absolute deadline");
}
