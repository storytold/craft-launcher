//! Authored representative main-supported release shapes, not live third-party metadata or payloads.
use super::*;
use crate::source::Answer;

fn representative(app: &CraftApp, tag: &str, at: i64) -> Release {
    let parsed = version::parse(tag).unwrap();
    let name = format!("{}-{parsed}-windows-x64-portable.zip", app.id);
    let prefix = format!("https://github.com/{}/releases/download/{tag}/", app.repo);
    let mut release = artcraft_launcher_core::release::parse_github_release(
        &serde_json::json!({
        "draft":false,"prerelease":true,"tag_name":tag,"assets":[
            {"name":name,"size":123,"browser_download_url":format!("{prefix}{name}")},
            {"name":"SHA256SUMS.txt","size":100,"browser_download_url":format!("{prefix}SHA256SUMS.txt")}
        ]})
        .to_string(),
        app.repo,
    )
    .unwrap();
    release.published_at = Some(at);
    release
}
struct Published(Vec<Release>);
impl ReleaseSource for Published {
    fn latest(&self, _query: &ReleaseQuery, _pre: bool) -> Answer {
        Answer { latest: LatestRelease::None, source: Source::GitHubApi }
    }
    fn versions(&self, _query: &ReleaseQuery) -> Result<Vec<Release>, String> {
        Ok(self.0.clone())
    }
    fn cached(&self, _query: &ReleaseQuery, _pre: bool) -> Option<LatestRelease> {
        None
    }
    fn rate_limited_until(&self) -> Option<i64> {
        None
    }
    fn save(&self) {}
}
#[test]
fn shared_selection_legacy_native_follow_and_pin_keep_main_tag_checksums_and_publication_order() {
    for id in ["solidcraft", "filmcraft"] {
        let app = CraftApp::by_id(id).unwrap();
        for prefix in ["v1.0.0-dev.nightly.1", "v1.0.0-nightly.1"] {
            let mut older = representative(app, prefix, 200);
            if prefix == "v1.0.0-nightly.1" {
                older.assets.truncate(1);
                older.assets[0].sha256 = Some("ab".repeat(32));
            }
            let newer = representative(app, "v2.0.0-nightly.1", 100);
            assert!(artcraft_launcher_core::release::has_published_checksum(&older));
            for releases in [vec![newer.clone(), older.clone()], vec![older.clone(), newer.clone()]] {
                let published = Published(releases);
                let answer = published.chosen(&ReleaseQuery::for_app(app, app.repo), &VersionTrack::Nightly);
                let LatestRelease::Found(chosen) = answer.latest else { panic!("main-supported Nightly must remain selectable for {id}") };
                assert_eq!(chosen, older, "legacy follows publication time, not first-party canonical order");
                let pin = published.chosen(&ReleaseQuery::for_app(app, app.repo), &VersionTrack::Pinned(newer.tag.clone()));
                assert!(matches!(pin.latest, LatestRelease::Found(ref selected) if selected==&newer));
                let mut equal = newer.clone();
                equal.published_at = older.published_at;
                for tied in [vec![older.clone(), equal.clone()], vec![equal, older.clone()]] {
                    let expected = tied.last().unwrap().clone();
                    let selected = Published(tied).chosen(&ReleaseQuery::for_app(app, app.repo), &VersionTrack::Nightly);
                    assert_eq!(selected.latest, LatestRelease::Found(expected), "main keeps the last input on equal publication timestamps");
                }
            }
        }
    }
}
#[test]
fn shared_selection_ordinary_install_plans_accept_legacy_follow_and_pin_but_refuse_separate_channel() {
    let platform = Platform { os: Os::Windows, arch: Arch::X64 };
    let system = install::SystemKinds { available: vec![PackageKind::PortableZip], can_pacman: false };
    for id in ["solidcraft", "filmcraft", "photocraft"] {
        let app = CraftApp::by_id(id).unwrap();
        let release = representative(app, "v1.0.0-dev.nightly.1", 200);
        for track in [VersionTrack::Nightly, VersionTrack::Pinned(release.tag.clone()), VersionTrack::Stable] {
            let mut options = BackendOptions { prereleases: true, kinds: vec![PackageKind::PortableZip], ..Default::default() };
            options.versions.insert(id.into(), track.clone());
            let plan = install_plan(app, Some(release.clone()), Some(platform), &options, (None, None), Asking::Answer(&system));
            if id == "photocraft" {
                assert!(plan.is_err());
            } else {
                let plan = plan.unwrap_or_else(|why| panic!("{id} {track:?}: {why}"));
                assert_eq!(plan.asset.name, release.assets[0].name);
                assert_eq!(plan.kind, PackageKind::PortableZip);
            }
        }
    }
}
#[test]
fn shared_selection_backend_configuration_keeps_legacy_tracks_but_reconciles_photo() {
    let temp = tempfile::tempdir().unwrap();
    let platform = Platform { os: Os::Windows, arch: Arch::X64 };
    for track in [VersionTrack::Nightly, VersionTrack::Pinned("v1.0.0-dev.nightly.1".into())] {
        let mut backend = tests::backend(temp.path(), platform, None);
        backend.pins = None;
        let queued = Arc::new(Mutex::new(Vec::new()));
        let sink = queued.clone();
        backend.spawn = Arc::new(move |work| sink.lock().unwrap().push(work));
        let mut options = BackendOptions { launcher_version: track.clone(), ..Default::default() };
        for id in ["solidcraft", "filmcraft", "photocraft"] {
            options.versions.insert(id.into(), track.clone());
        }
        backend.send(Command::Configure(options.clone()));
        let actual = backend.shared.options.lock().unwrap().clone();
        options.versions.remove("photocraft");
        assert_eq!(actual.versions, options.versions);
        assert_eq!(actual.launcher_version, track);
    }
}
#[test]
fn shared_selection_mac_admission_follows_the_pinned_nightly_signing_team() {
    let app = CraftApp::by_id("photocraft").unwrap();
    let platform = Platform { os: Os::MacOs, arch: Arch::Arm64 };
    assert!(crate::nightly_profile::for_app(app, platform).is_ok(), "known retained copies still have a source-owned validation/management profile");
    let result = crate::nightly_profile::admit_new(app, platform);
    assert_eq!(result, Ok(crate::nightly_profile::Adapter::PhotoCraftMacV1), "the source-owned Nightly team admits new Mac installs");
}

#[cfg(unix)]
#[test]
fn shared_selection_unsafe_root_remains_visible_but_never_adopts_or_mutates_aliased_storage() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let real = root.join("real");
    std::fs::create_dir(&real).unwrap();
    let sentinel = real.join("user-data");
    std::fs::write(&sentinel, b"keep me").unwrap();
    let alias = root.join("aliased-root");
    symlink(&real, &alias).unwrap();
    let paths = Paths::under(&alias).for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
    assert!(crate::nightly_profile::validate_paths(&paths).is_err());
    let platform = Platform { os: Os::Linux, arch: Arch::X64 };
    let stable = tests::backend(&root.join("stable"), platform, None);
    let mut context = stable.nightly_context_with_paths(Ok(paths)).expect("unsafe storage needs a visible refused context, not a vanished page");
    assert!(context.poll().iter().any(|event| matches!(event,Event::Notice{app,result:Err(_)} if app=="photocraft")));
    for command in [
        Command::Install { app: "photocraft".into() },
        Command::Launch { app: "photocraft".into(), args: Vec::new() },
        Command::Reveal { app: "photocraft".into() },
        Command::Uninstall { app: "photocraft".into() },
    ] {
        context.send(command);
    }
    context.send(Command::CheckForUpdates { force: true });
    let repeated = context.poll();
    assert!(!repeated.iter().any(|event| matches!(event, Event::Notice { .. })), "the storage notice is said once: {repeated:?}");
    assert!(repeated.iter().any(|event| matches!(event, Event::NightlyStatus { app, status } if app == "photocraft" && status.message.is_some())));
    assert!(!context.has_active_jobs());
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep me");
    assert_eq!(std::fs::read_dir(&real).unwrap().count(), 1, "no adopted cache, manifest, staging, profile, or installed files");
    assert!(std::fs::symlink_metadata(&alias).unwrap().file_type().is_symlink());
}

#[test]
fn shared_selection_a_feed_repo_rename_keeps_the_apps_separate_channel_policy() {
    let feed = Feed::parse(r#"{"repo_renames":[{"old_name":"storytold/photocraft","new_name":"storytold/photocraft-studio"}]}"#).unwrap();
    let photo = release_query(Some(&feed), CraftApp::by_id("photocraft").unwrap());
    assert_eq!(photo.repo, "storytold/photocraft-studio");
    assert!(photo.separate_nightly, "the policy follows the app, not the renamed repo string");
    assert!(!release_query(Some(&feed), CraftApp::by_id("solidcraft").unwrap()).separate_nightly);
}

#[test]
fn shared_selection_nightly_context_reports_configured_signing_trust() {
    let root = tempfile::tempdir().unwrap();
    let windows = Platform { os: Os::Windows, arch: Arch::X64 };
    let linux = Platform { os: Os::Linux, arch: Arch::X64 };
    for (name, platform) in [("windows", windows), ("linux", linux)] {
        let configured = crate::nightly_profile::new_installs_available(platform);
        let stable = tests::backend(&root.path().join(name).join("stable"), platform, None);
        assert!(!stable.nightly_available(), "the stable context never offers nightly builds");
        let paths = Paths::under(&root.path().join(name).join("home")).for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
        let mut nightly = stable.nightly_with_paths(paths);
        nightly.platform = Some(platform);
        assert_eq!(nightly.nightly_available(), configured, "{name}");
    }
}

/// Unusable storage refuses everything, but whether Pre-Release is offered at all still follows
/// the platform's signing trust, as for a usable context.
#[cfg(unix)]
#[test]
fn shared_selection_refused_context_reports_availability_from_signing_trust_not_storage() {
    let root = tempfile::tempdir().unwrap();
    let real = root.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let alias = root.path().join("aliased-root");
    std::os::unix::fs::symlink(&real, &alias).unwrap();
    for platform in
        [Platform { os: Os::Linux, arch: Arch::X64 }, Platform { os: Os::Windows, arch: Arch::X64 }, Platform { os: Os::MacOs, arch: Arch::Arm64 }]
    {
        let paths = Paths::under(&alias).for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
        let stable = tests::backend(&root.path().join(format!("{platform:?}")), platform, None);
        let context = stable.nightly_context_with_paths(Ok(paths)).unwrap();
        assert_eq!(context.nightly_available(), crate::nightly_profile::new_installs_available(platform), "{platform:?}");
    }
}
