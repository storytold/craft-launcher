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
    {
        let mut state = shared.nightly_state.lock().unwrap();
        state.begin(std::slice::from_ref(&key), &Default::default(), now, true, true);
        state.attempt(&key, now);
    }
    finish_nightly_result(app(), platform, 0, Outcome::Verified(Box::new(nightly_lookup::tests::verified(run))), Origin::GitHub, shared, paths);
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
        Outcome::Indeterminate(crate::http::NightlyHttpError { over_budget: false, why: "offline".into(), retry_at: Some(crate::now() + 120) }),
        Origin::GitHub,
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
    finish_nightly_result(app(), platform, 0, Outcome::Verified(Box::new(nightly_lookup::tests::verified(30))), Origin::GitHub, &shared, &paths);
    shared.options.lock().unwrap().nightly_enabled = true;
    shared.nightly_state.lock().unwrap().invalidate();
    finish_nightly_result(app(), platform, 0, Outcome::Verified(Box::new(nightly_lookup::tests::verified(30))), Origin::GitHub, &shared, &paths);
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
    finish_nightly_result(app(), platform, 0, Outcome::Verified(Box::new(nightly_lookup::tests::verified(10))), Origin::GitHub, &shared, &paths);
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
    // The signed catalog sweep schedules the whole due roster, but withdrawing consent
    // before the worker starts still prevents every lookup and verified promotion.
    let state = shared.nightly_state.lock().unwrap();
    let attempted: Vec<_> = artcraft_launcher_core::hosted_catalog::apps()
        .iter()
        .filter(|app| app.has_separate_nightly_channel() && app.runs_on(platform.os))
        .filter(|app| state.record(&nightly_lookup::key(app, platform)).attempted_at.is_some())
        .collect();
    assert!(attempted.is_empty(), "unstarted apps do not consume their next check turn");
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
        Outcome::Indeterminate(crate::http::NightlyHttpError { over_budget: false, why: "limited".into(), retry_at: Some(deadline) }),
        Origin::GitHub,
        &shared,
        &paths,
    );
    {
        let mut state = shared.nightly_state.lock().unwrap();
        state.finish();
        state.outcome(
            &key,
            before.attempted_at.unwrap() + 20,
            &Outcome::Indeterminate(crate::http::NightlyHttpError {
                over_budget: false,
                why: "subsequent storage failure".into(),
                retry_at: Some(deadline - 60),
            }),
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
        Outcome::Indeterminate(crate::http::NightlyHttpError { over_budget: false, why: "limited".into(), retry_at: Some(deadline) }),
        Origin::GitHub,
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

#[test]
fn catalog_verified_nightly_publishes_the_same_install_gate_with_feed_origin() {
    let (_temp, shared, paths, platform) = fixture();
    finish_nightly_result(app(), platform, 0, Outcome::Verified(Box::new(nightly_lookup::tests::verified(20))), Origin::ArtCraftApi, &shared, &paths);
    let events = shared.events.lock().unwrap();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::Latest {app, latest: LatestRelease::Found(_), from: Origin::ArtCraftApi} if app == "photocraft"))
    );
    assert!(events.iter().any(|event| matches!(event, Event::Installable { app, installable: true } if app == "photocraft")));
}

#[test]
fn nightly_catalog_fetches_one_signed_snapshot_and_reverifies_held_cache() {
    use artcraft_launcher_core::hosted_catalog::{BUILD_CATALOG_SIG, BUILD_CATALOG_TOML, CATALOG_URL, SIGNATURE_URL};
    let (_temp, _shared, paths, _) = fixture();
    let http = Http::new().unwrap();
    let throttle = Mutex::new(FeedThrottle::new());
    let hits = std::cell::Cell::new(0);
    let get = |url: &str, _| {
        hits.set(hits.get() + 1);
        match url {
            CATALOG_URL => Ok(Some(BUILD_CATALOG_TOML.to_vec())),
            SIGNATURE_URL => Ok(Some(BUILD_CATALOG_SIG.to_vec())),
            _ => panic!("unexpected URL"),
        }
    };
    let feed = nightly_catalog_with_fetch(&http, &paths, &throttle, &|| true, get).unwrap();
    assert!(feed.channel_aware);
    assert_eq!(hits.get(), 2, "document and detached signature exactly once");
    assert_eq!(nightly_catalog_with_fetch(&http, &paths, &throttle, &|| true, get), Some(feed));
    assert_eq!(hits.get(), 2, "held request reuses the pinned-key-verified cache");
    crate::paths::write_atomic(&paths.feed_cache(), b"not a signed catalog").unwrap();
    assert!(nightly_catalog_with_fetch(&http, &paths, &throttle, &|| true, get).is_none());
    assert_eq!(hits.get(), 2);
    assert!(nightly_catalog_with_fetch(&http, &paths, &throttle, &|| false, get).is_none());
}

#[test]
fn nightly_catalog_fetch_stops_before_signature_if_consent_is_withdrawn() {
    use artcraft_launcher_core::hosted_catalog::{BUILD_CATALOG_TOML, CATALOG_URL};
    let (_temp, _shared, paths, _) = fixture();
    let http = Http::new().unwrap();
    let throttle = Mutex::new(FeedThrottle::new());
    let enabled = std::cell::Cell::new(true);
    assert!(
        nightly_catalog_with_fetch(&http, &paths, &throttle, &|| enabled.get(), |url, _| {
            assert_eq!(url, CATALOG_URL);
            enabled.set(false);
            Ok(Some(BUILD_CATALOG_TOML.to_vec()))
        })
        .is_none()
    );
    assert!(!paths.feed_cache().exists());
}

#[test]
fn real_sweep_keeps_installed_priority_and_uses_catalog_after_github_budget_refusal() {
    let (_temp, shared, paths, platform) = fixture();
    let now = crate::now();
    let (mut feed, _, body) = nightly_lookup::tests::catalog_fixture("v1.0.0-nightly.20261120.20.1.gabcdefabcdef", true);
    feed.last_fetched = Some(now);
    let entry = feed.apps.get_mut(app().id).unwrap();
    entry.fetched_at = Some(now);
    entry.nightly.as_mut().unwrap().reconciled_at = Some(now);
    let release = nightly_lookup::catalog_candidate(&feed, app(), now, None).unwrap();
    let verified = crate::nightly::verified_descriptor(app(), release, &body, platform).unwrap().unwrap();
    shared.nightly_verified.lock().unwrap().insert(app().id.into(), verified);
    *shared.nightly_catalog_fixture.lock().unwrap() = Some(feed);
    let cad = CraftApp::by_id("cadcraft").unwrap();
    shared.manifest.lock().unwrap().apps.insert(
        cad.id.into(),
        crate::manifest::Record {
            version: "1.0.0".into(),
            kind: PackageKind::TarGz,
            path: paths.apps().join("cadcraft/cadcraft"),
            installed_at: now,
            files: vec![],
            needs_root: false,
        },
    );
    let budget = crate::github_budget::GitHubBudget::in_memory(crate::github_budget::DEVELOPER_BUILDS);
    while budget.take(Priority::User, now).is_ok() {}
    let http = Http::new().unwrap().without_token().with_budget(budget);
    let backend = NativeBackend {
        shared: shared.clone(),
        paths: paths.clone(),
        http: Some(http),
        source: None,
        platform: Some(platform),
        pins: None,
        spawn: Arc::new(|task| task()),
    };
    backend.check(false);
    let state = shared.nightly_state.lock().unwrap();
    assert_eq!(state.record(&nightly_lookup::key(cad, platform)).attempted_at, None, "installed fallback refused before a request stays due");
    assert!(state.record(&nightly_lookup::key(app(), platform)).complete, "cached verified candidate still completes after quota refusal");
    assert!(state.record(&nightly_lookup::key(app(), platform)).attempted_at.is_some());
    let events = shared.events.lock().unwrap();
    assert!(events.iter().any(|e| matches!(e,Event::Latest {app,from:Origin::ArtCraftApi,latest:LatestRelease::Found(_)} if app=="photocraft")));
    assert!(events.iter().any(|e| matches!(e, Event::CheckFinished { source: Source::Feed, .. })));
}

#[test]
fn unavailable_https_keeps_unstarted_apps_due() {
    let (_temp, shared, paths, platform) = fixture();
    let backend = NativeBackend {
        shared: shared.clone(),
        paths,
        http: None,
        source: None,
        platform: Some(platform),
        pins: None,
        spawn: Arc::new(|task| task()),
    };
    backend.check(false);
    let state = shared.nightly_state.lock().unwrap();
    assert!(backend.nightly_apps().iter().all(|app| state.record(&nightly_lookup::key(app, platform)).attempted_at.is_none()));
    assert!(
        shared
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| matches!(event,Event::Notice {result:Err(why),..} if why.contains("Secure Nightly connections")))
    );
}
