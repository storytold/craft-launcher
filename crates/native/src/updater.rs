//! Bringing the installed Craft Apps up to date without the launcher's window: the update
//! service's scheduled runs and requests, and the command line (`artcraft-launcher-updater check`,
//! `artcraft-launcher-updater update`).
//!
//! [`Engine::survey`] looks up the latest releases the way the launcher does (ArtCraft's release
//! feed, then GitHub for anything the feed can't vouch for) and sets them against what's
//! installed. [`Engine::apply`] then installs one app's update through the same verified download
//! the launcher uses.
//!
//! - [`Scope::Machine`] is the update service: LocalSystem, nobody at the screen. It only replaces
//!   apps an administrator installed for every user (Windows Installer, registered under HKLM),
//!   only with a newer version, and only with a package that passes [`crate::msi::check`]
//!   (publisher's signature, the app's own package, the promised version).
//! - [`Scope::User`] is someone at a terminal: the launcher's own rules, with the system's prompts
//!   (UAC, polkit) where an install needs them.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use artcraft_launcher_core::catalog::LAUNCHER;
use artcraft_launcher_core::feed::{Feed, FeedAnswer};
use artcraft_launcher_core::platform::{Platform, pick_asset};
use artcraft_launcher_core::protocol::{BackendOptions, CANCELLED, INSTALL_CANCELLED, JobPhase, LatestRelease, PAUSED};
use artcraft_launcher_core::release::{Asset, Release};
use artcraft_launcher_core::{CraftApp, InstallSource, Installed, PackageKind, SUITE, version};

use crate::backend;
use crate::feed_throttle::FeedThrottle;
use crate::http::{Http, Stop};
use crate::install::{self, Job};
use crate::manifest::{Manifest, Record};
use crate::paths::Paths;
use crate::service::{AppState, AppSummary, Outcome};
use crate::source::{GitHubSource, ReleaseSource};

/// The update service's own package, in each ArtCraft Launcher release:
/// `artcraft-launcher-updater-<version>-windows-<arch>.msi`.
pub const UPDATER_ID: &str = "artcraft-launcher-updater";
/// Lookups in flight at once during a survey.
const WORKERS: usize = 4;

/// Whose installs a run may touch, and how.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The update service: apps installed for every user, quietly, after checking each package.
    Machine,
    /// A person at a terminal: whatever the launcher itself would update, the launcher's way.
    User,
}

/// Looks up releases and installs updates for one [`Scope`].
pub struct Engine {
    http: Http,
    paths: Paths,
    source: GitHubSource,
    throttle: Mutex<FeedThrottle>,
    platform: Option<Platform>,
    options: BackendOptions,
    scope: Scope,
}

/// What a look at the releases found.
pub struct Survey {
    /// When (Unix seconds).
    pub at: i64,
    /// Every Craft App, in the suite's order.
    pub apps: Vec<Found>,
    /// The latest ArtCraft Launcher release, when it could be found.
    pub launcher: Option<Release>,
}

impl Survey {
    pub fn find(&self, id: &str) -> Option<&Found> {
        self.apps.iter().find(|f| f.app.id == id)
    }

    /// The apps with an update this run can install.
    pub fn updates(&self) -> impl Iterator<Item = &Found> {
        self.apps.iter().filter(|f| matches!(f.plan, Plan::Update { .. }))
    }
}

/// One app: what's installed, what's out, and what a run would do about it.
pub struct Found {
    pub app: &'static CraftApp,
    pub installed: Option<Installed>,
    pub latest: Option<Release>,
    pub plan: Plan,
}

impl Found {
    /// For the service's status and the command line.
    pub fn summary(&self) -> AppSummary {
        let (state, note) = match &self.plan {
            Plan::NotInstalled => (AppState::NotInstalled, None),
            Plan::UpToDate => (AppState::UpToDate, None),
            Plan::Update { .. } => (AppState::UpdateAvailable, None),
            Plan::Blocked(why) => (AppState::Blocked, Some(why.clone())),
            Plan::Unknown(why) => (AppState::Unknown, Some(why.clone())),
        };
        AppSummary {
            id: self.app.id.to_string(),
            installed: self.installed.as_ref().and_then(|i| i.version.as_ref()).map(ToString::to_string),
            latest: self.latest.as_ref().map(Release::version_label),
            state,
            note,
            last_update: None,
        }
    }
}

/// What a run would do about one app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Plan {
    NotInstalled,
    UpToDate,
    /// Install `asset` (a `kind` package) of `release`.
    Update {
        release: Box<Release>,
        asset: Asset,
        kind: PackageKind,
    },
    /// Installed, but not in a way this run may replace: why.
    Blocked(String),
    /// Couldn't tell: the lookup failed, or a version can't be read.
    Unknown(String),
}

impl Engine {
    /// An engine keeping its downloads and saved answers in `paths`.
    pub fn new(paths: Paths, scope: Scope, options: BackendOptions) -> Result<Self, String> {
        let http = Http::new()?;
        let source = GitHubSource::new(http.clone(), paths.release_cache());
        let throttle = Mutex::new(FeedThrottle::load(&paths.feed_throttle(), crate::now()));
        Ok(Self { http, source, throttle, platform: backend::native_platform(), options, scope, paths })
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// Looks up the latest releases and sets them against what's installed.
    pub fn survey(&self) -> Survey {
        let now = crate::now();
        let mirror = backend::local_mirror();
        let feed = match &mirror {
            Some(base) => backend::local_feed(&self.http, base),
            // The feed serves stable releases only; with pre-releases on, GitHub decides.
            None if self.options.prereleases => None,
            None => match backend::fetch_feed(&self.http, &self.paths, &backend::feed_url(), &self.throttle, backend::FEED_ATTEMPTS) {
                backend::FeedFetch::Answered(feed) => Some(feed),
                // The last good answer stands in, as far as it's still fresh.
                backend::FeedFetch::Failed | backend::FeedFetch::Held => backend::load_feed(&self.paths),
            },
        };
        let queue = Mutex::new(SUITE.iter().chain(std::iter::once(&LAUNCHER)).collect::<Vec<&'static CraftApp>>());
        let answers = Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..WORKERS {
                scope.spawn(|| {
                    loop {
                        let Some(app) = queue.lock().unwrap_or_else(PoisonError::into_inner).pop() else { break };
                        let latest = self.latest(app, feed.as_ref(), mirror.is_some(), now);
                        answers.lock().unwrap_or_else(PoisonError::into_inner).push((app.id, latest));
                    }
                });
            }
        });
        self.source.save();
        let answers = answers.into_inner().unwrap_or_else(PoisonError::into_inner);
        let latest_of =
            |id: &str| answers.iter().find(|(a, _)| *a == id).map(|(_, l)| l.clone()).unwrap_or(LatestRelease::Failed("not looked up".into()));
        let manifest = Manifest::load(&self.paths.manifest());
        let snapshot = crate::detect::Snapshot::take();
        let apps = SUITE
            .iter()
            .map(|app| {
                let installed = self.installed(app, &manifest, &snapshot);
                let latest = latest_of(app.id);
                let plan = decide(app, installed.as_ref(), &latest, self.scope, self.platform, &self.options, manifest.apps.get(app.id));
                let latest = match latest {
                    LatestRelease::Found(r) => Some(r),
                    LatestRelease::None | LatestRelease::Failed(_) => None,
                };
                Found { app, installed, latest, plan }
            })
            .collect();
        let launcher = match latest_of(LAUNCHER.id) {
            LatestRelease::Found(r) => Some(r),
            LatestRelease::None | LatestRelease::Failed(_) => None,
        };
        Survey { at: now, apps, launcher }
    }

    /// The latest release of `app`: the feed's answer while it's fresh, else GitHub's.
    fn latest(&self, app: &CraftApp, feed: Option<&Feed>, mirror: bool, now: i64) -> LatestRelease {
        if mirror {
            return backend::mirror_latest(app, feed, now).0.unwrap_or_else(|| LatestRelease::Failed("the local mirror didn't answer".into()));
        }
        match feed.map(|f| f.answer(app, now)) {
            Some(FeedAnswer::Fresh(Some(r))) => LatestRelease::Found(r),
            Some(FeedAnswer::Fresh(None)) => LatestRelease::None,
            Some(FeedAnswer::AskGitHub) | None => self.source.latest(&backend::feed_repo(feed, app), self.options.prereleases).latest,
        }
    }

    /// The version of `app` installed right now, for the update service (which looks again just
    /// before it installs). `None` for a person's run, which goes by its survey.
    fn installed_now(&self, app: &CraftApp) -> Option<version::Version> {
        match self.scope {
            #[cfg(windows)]
            Scope::Machine => crate::detect::windows::machine_install(app).and_then(|i| i.version),
            _ => {
                let _ = app;
                None
            }
        }
    }

    /// What's installed of `app` that this scope answers for.
    fn installed(&self, app: &CraftApp, manifest: &Manifest, snapshot: &crate::detect::Snapshot) -> Option<Installed> {
        match self.scope {
            #[cfg(windows)]
            Scope::Machine => crate::detect::windows::machine_install(app),
            // The update service is for Windows; elsewhere it has nothing to answer for.
            #[cfg(not(windows))]
            Scope::Machine => None,
            Scope::User => crate::detect::detect(app, manifest, snapshot),
        }
    }

    /// Installs `found`'s update, if it has one. `stop` pauses or cancels the download; `phase`
    /// hears how it goes.
    pub fn apply(&self, found: &Found, stop: &Stop, phase: &mut dyn FnMut(JobPhase)) -> Outcome {
        let app = found.app;
        let (release, asset, kind) = match &found.plan {
            Plan::Update { release, asset, kind } => (release, asset, *kind),
            Plan::UpToDate => {
                return Outcome::UpToDate { version: found.installed.as_ref().and_then(|i| i.version.as_ref()).map(ToString::to_string) };
            }
            Plan::NotInstalled => {
                let whom = if self.scope == Scope::Machine { " for all users" } else { "" };
                return Outcome::NotEligible { reason: format!("{} isn't installed{whom}", app.name) };
            }
            Plan::Blocked(why) => return Outcome::NotEligible { reason: why.clone() },
            Plan::Unknown(why) => return Outcome::Failed { error: why.clone() },
        };
        let Some(inst) = found.installed.as_ref() else { return Outcome::NotEligible { reason: format!("{} isn't installed", app.name) } };
        // Someone may have installed it since the survey (the launcher, with its UAC prompt).
        if let Some(now) = self.installed_now(app)
            && release.version.as_ref().is_some_and(|want| !version::is_update(want, &now))
        {
            return Outcome::UpToDate { version: Some(now.to_string()) };
        }
        // A running app is never replaced: the next run tries again.
        if inst.source.launcher_can_update() && crate::process::is_running(Path::new(&inst.location)) {
            log::info!("{}: running, so the update to {} waits", app.name, release.version_label());
            return Outcome::Running;
        }
        log::info!(
            "{}: updating from {} to {}",
            app.name,
            inst.version.as_ref().map_or_else(|| "?".to_string(), ToString::to_string),
            release.version_label()
        );
        let result = match self.scope {
            Scope::Machine => self.apply_machine(app, inst, release, asset, stop, phase),
            Scope::User => self.apply_user(app, inst, release, asset, kind, stop, phase),
        };
        match result {
            Ok(restart) => {
                log::info!("{}: updated to {}{}", app.name, release.version_label(), if restart { " (restart needed)" } else { "" });
                Outcome::Updated { version: release.version_label(), restart }
            }
            Err(e) if e == PAUSED => Outcome::Paused,
            Err(e) if e == CANCELLED || e == INSTALL_CANCELLED => Outcome::Cancelled,
            Err(e) => {
                log::error!("{}: update to {} failed: {e}", app.name, release.version_label());
                Outcome::Failed { error: e }
            }
        }
    }

    /// An install job of this engine's. It only ever updates what's there, so it never writes over
    /// a folder's contents.
    fn job<'a>(&'a self, stop: &'a Stop, phase: &'a mut dyn FnMut(JobPhase)) -> Job<'a> {
        Job { http: &self.http, paths: &self.paths, stop, phase, install_locations: &self.options.install_locations, replace: false }
    }

    /// The update service's install: download and check, then run the package quietly into the
    /// folder `inst` (what's registered for all users) is in. `Ok(true)` when Windows wants a
    /// restart to finish.
    #[cfg(windows)]
    fn apply_machine(
        &self,
        app: &CraftApp,
        inst: &Installed,
        release: &Release,
        asset: &Asset,
        stop: &Stop,
        phase: &mut dyn FnMut(JobPhase),
    ) -> Result<bool, String> {
        let file = install::download(release, asset, &mut self.job(stop, phase))?;
        let names: Vec<&str> = app.names().collect();
        let expect = crate::msi::Expect { names, version: release.version.as_ref(), signer: Some(crate::msi::SIGNER) };
        // A download that matches its checksum but fails these is kept (the next run would only
        // fetch the same file again); the download folder's cleanup takes it in a week.
        crate::msi::verify(&file, &expect)?;
        log::info!("{}: {} checked (signature, product, version); installing", app.name, asset.name);
        // Past this point the install can't be interrupted halfway.
        phase(JobPhase::Installing);
        let logs = self.paths.logs();
        let _ = std::fs::create_dir_all(&logs);
        // Into the folder the installed copy is in, as the launcher's own updates do: an app put in
        // a chosen folder stays there. It comes from what's registered here, never from a request.
        let folder = install::msi_update_folder(inst);
        let restart = install::windows::run_msi_quiet(&file, folder.as_deref(), &logs.join(format!("msi-{}.log", app.id)), stop, phase)?;
        let _ = std::fs::remove_file(&file);
        // What's registered now is the truth.
        let now = crate::detect::windows::machine_install(app).and_then(|i| i.version);
        let landed = match (now.as_ref(), release.version.as_ref()) {
            (Some(have), Some(want)) => !version::is_update(want, have),
            _ => false,
        };
        if !landed {
            let have = now.map_or_else(|| "unknown".to_string(), |v| v.to_string());
            return Err(format!("the Windows Installer finished, but {} is still at version {have}", app.name));
        }
        Ok(restart)
    }

    #[cfg(not(windows))]
    fn apply_machine(
        &self,
        _app: &CraftApp,
        _inst: &Installed,
        _release: &Release,
        _asset: &Asset,
        _stop: &Stop,
        _phase: &mut dyn FnMut(JobPhase),
    ) -> Result<bool, String> {
        Err("the update service installs Windows Installer packages only".into())
    }

    /// A person's install: the launcher's own, keeping its record of what it installed.
    #[allow(clippy::too_many_arguments)]
    fn apply_user(
        &self,
        app: &CraftApp,
        inst: &Installed,
        release: &Release,
        asset: &Asset,
        kind: PackageKind,
        stop: &Stop,
        phase: &mut dyn FnMut(JobPhase),
    ) -> Result<bool, String> {
        let path = self.paths.manifest();
        let mut manifest = Manifest::load(&path);
        let record = manifest.apps.get(app.id).cloned();
        let mut job = self.job(stop, phase);
        let new_record = install::install(app, release, asset, kind, Some(inst), record.as_ref(), &mut job)?;
        let changed = match new_record {
            Some(r) => {
                manifest.apps.insert(app.id.to_string(), r);
                true
            }
            // Installed the platform's own way: any old record is stale.
            None => manifest.apps.remove(app.id).is_some(),
        };
        if changed && let Err(e) = manifest.save(&path) {
            log::warn!("couldn't save the install record: {e}");
        }
        Ok(false)
    }

    /// The update service's own package in the ArtCraft Launcher release `launcher`, when it's
    /// newer than this build.
    pub fn own_update<'a>(&self, launcher: &'a Release) -> Option<&'a Asset> {
        let (newest, ours) = (launcher.version.as_ref()?, version::parse(env!("CARGO_PKG_VERSION"))?);
        if !version::is_newer(newest, &ours) {
            return None;
        }
        pick_asset(launcher, &[UPDATER_ID], self.platform?, &[PackageKind::Msi]).map(|(asset, _)| asset)
    }

    /// Downloads and checks the update service's newer package, then starts the Windows
    /// Installer on it and returns: the installer stops this service, replaces it and starts the
    /// new one.
    #[cfg(windows)]
    pub fn start_own_update(&self, launcher: &Release, asset: &Asset, stop: &Stop) -> Result<(), String> {
        let file = install::download(launcher, asset, &mut self.job(stop, &mut |_| {}))?;
        let expect =
            crate::msi::Expect { names: vec![crate::service::DISPLAY_NAME], version: launcher.version.as_ref(), signer: Some(crate::msi::SIGNER) };
        crate::msi::verify(&file, &expect)?;
        let logs = self.paths.logs();
        let _ = std::fs::create_dir_all(&logs);
        log::info!("starting the update service's own update to {}", launcher.version_label());
        install::windows::start_msi_quiet(&file, &logs.join(format!("msi-{UPDATER_ID}.log")))
    }

    #[cfg(not(windows))]
    pub fn start_own_update(&self, _launcher: &Release, _asset: &Asset, _stop: &Stop) -> Result<(), String> {
        Err("the update service updates itself on Windows only".into())
    }
}

/// What a run of `scope` would do about `app`, given what's installed and the latest release.
fn decide(
    app: &CraftApp,
    installed: Option<&Installed>,
    latest: &LatestRelease,
    scope: Scope,
    platform: Option<Platform>,
    options: &BackendOptions,
    record: Option<&Record>,
) -> Plan {
    let Some(inst) = installed else { return Plan::NotInstalled };
    match scope {
        Scope::Machine if inst.source != InstallSource::Msi => {
            return Plan::Blocked("not installed for all users with the Windows Installer".into());
        }
        Scope::User => {
            if let Some(why) = install::managed_elsewhere(app, inst, false) {
                return Plan::Blocked(why);
            }
        }
        Scope::Machine => {}
    }
    let release = match latest {
        LatestRelease::Found(r) => r,
        LatestRelease::None => return Plan::Unknown("no release has been published yet".into()),
        LatestRelease::Failed(why) => return Plan::Unknown(why.clone()),
    };
    let (Some(newest), Some(have)) = (release.version.as_ref(), inst.version.as_ref()) else {
        return Plan::Unknown("the installed or the latest version can't be read".into());
    };
    if !version::is_update(newest, have) {
        return Plan::UpToDate;
    }
    let Some(platform) = platform else { return Plan::Blocked("this system isn't supported".into()) };
    let stems: Vec<&str> = app.stems().collect();
    let mut kinds = install::choose_kinds(Some(inst), record, &options.kinds);
    // The update service installs Windows Installer packages only.
    if scope == Scope::Machine {
        kinds.retain(|k| *k == PackageKind::Msi);
    }
    match pick_asset(release, &stems, platform, &kinds) {
        Some((asset, kind)) => Plan::Update { release: Box::new(release.clone()), asset: asset.clone(), kind },
        None => Plan::Blocked(format!("{} {} has no download for {}", app.name, release.version_label(), platform.label())),
    }
}

#[cfg(test)]
mod tests {
    use artcraft_launcher_core::platform::{Arch, Os};
    use artcraft_launcher_core::version::Version;

    use super::*;

    fn photocraft() -> &'static CraftApp {
        CraftApp::by_id("photocraft").unwrap()
    }

    fn installed(source: InstallSource, v: &str) -> Installed {
        Installed { version: version::parse(v), source, location: r"C:\Program Files\PhotoCraft\photocraft.exe".into(), package: None }
    }

    fn release(v: &str) -> Release {
        let names = [format!("photocraft-{v}-windows-x64.msi"), format!("photocraft-{v}-windows-x64-portable.zip"), "SHA256SUMS.txt".into()];
        Release {
            tag: format!("v{v}"),
            version: version::parse(v),
            assets: names
                .iter()
                .map(|n| Asset {
                    name: n.clone(),
                    size: 1,
                    url: format!("https://github.com/storytold/photocraft/releases/download/v{v}/{n}"),
                    sha256: None,
                })
                .collect(),
            ..Release::default()
        }
    }

    const WINDOWS: Option<Platform> = Some(Platform { os: Os::Windows, arch: Arch::X64 });

    fn plan_for(inst: Option<&Installed>, latest: &LatestRelease, scope: Scope) -> Plan {
        decide(photocraft(), inst, latest, scope, WINDOWS, &BackendOptions::default(), None)
    }

    #[test]
    fn nothing_installed_nothing_to_do() {
        let latest = LatestRelease::Found(release("0.8.2"));
        assert_eq!(plan_for(None, &latest, Scope::Machine), Plan::NotInstalled);
        assert_eq!(plan_for(None, &latest, Scope::User), Plan::NotInstalled);
    }

    #[test]
    fn current_and_newer_copies_are_left_alone() {
        let latest = LatestRelease::Found(release("0.8.2"));
        for v in ["0.8.2", "0.9.0"] {
            assert_eq!(plan_for(Some(&installed(InstallSource::Msi, v)), &latest, Scope::Machine), Plan::UpToDate, "{v}");
        }
        // An MSI records a pre-release by its numbers: it is that release, not older.
        let rc = LatestRelease::Found(release("0.8.2-rc.1"));
        assert_eq!(plan_for(Some(&installed(InstallSource::Msi, "0.8.2")), &rc, Scope::Machine), Plan::UpToDate);
    }

    #[test]
    fn the_service_touches_only_installs_for_all_users() {
        let latest = LatestRelease::Found(release("0.8.2"));
        for source in [InstallSource::Launcher, InstallSource::AppImage, InstallSource::Binary] {
            let p = plan_for(Some(&installed(source, "0.8.1")), &latest, Scope::Machine);
            assert!(matches!(p, Plan::Blocked(ref why) if why.contains("all users")), "{source:?}: {p:?}");
        }
    }

    #[test]
    fn lookups_that_failed_are_unknown_not_up_to_date() {
        let inst = installed(InstallSource::Msi, "0.8.1");
        assert_eq!(plan_for(Some(&inst), &LatestRelease::Failed("offline".into()), Scope::Machine), Plan::Unknown("offline".into()));
        assert!(matches!(plan_for(Some(&inst), &LatestRelease::None, Scope::Machine), Plan::Unknown(_)));
        let unreadable = Installed { version: None, ..inst };
        assert!(matches!(plan_for(Some(&unreadable), &LatestRelease::Found(release("0.8.2")), Scope::Machine), Plan::Unknown(_)));
    }

    #[test]
    fn the_user_scope_keeps_away_from_installs_it_doesnt_own() {
        let latest = LatestRelease::Found(release("0.8.2"));
        let p = plan_for(Some(&installed(InstallSource::Binary, "0.8.1")), &latest, Scope::User);
        assert!(matches!(p, Plan::Blocked(ref why) if why.contains("outside the launcher")), "{p:?}");
    }

    #[test]
    fn an_update_needs_a_download_for_this_system() {
        let mut bare = release("0.8.2");
        bare.assets.retain(|a| a.name == "SHA256SUMS.txt");
        let p = plan_for(Some(&installed(InstallSource::Msi, "0.8.1")), &LatestRelease::Found(bare), Scope::Machine);
        assert!(matches!(p, Plan::Blocked(ref why) if why.contains("no download for Windows x64")), "{p:?}");
        let p = decide(
            photocraft(),
            Some(&installed(InstallSource::Msi, "0.8.1")),
            &LatestRelease::Found(release("0.8.2")),
            Scope::Machine,
            None,
            &BackendOptions::default(),
            None,
        );
        assert!(matches!(p, Plan::Blocked(_)), "{p:?}");
    }

    /// The installer the service picks for a Windows install: the release's MSI, never the zip.
    #[cfg(windows)]
    #[test]
    fn the_service_plans_the_releases_msi() {
        let latest = release("0.8.2");
        let p = plan_for(Some(&installed(InstallSource::Msi, "0.8.1")), &LatestRelease::Found(latest.clone()), Scope::Machine);
        let Plan::Update { release: r, asset, kind } = p else { panic!("expected an update, got {p:?}") };
        assert_eq!((r.tag.as_str(), asset.name.as_str(), kind), ("v0.8.2", "photocraft-0.8.2-windows-x64.msi", PackageKind::Msi));
    }

    #[test]
    fn summaries_say_where_each_app_stands() {
        let found = Found {
            app: photocraft(),
            installed: Some(installed(InstallSource::Msi, "0.8.1")),
            latest: Some(release("0.8.2")),
            plan: Plan::Blocked("not installed for all users".into()),
        };
        let s = found.summary();
        assert_eq!((s.id.as_str(), s.installed.as_deref(), s.latest.as_deref()), ("photocraft", Some("0.8.1"), Some("0.8.2")));
        assert_eq!((s.state, s.note.as_deref()), (AppState::Blocked, Some("not installed for all users")));
        let none = Found { app: photocraft(), installed: None, latest: None, plan: Plan::NotInstalled }.summary();
        assert_eq!((none.state, none.installed, none.latest), (AppState::NotInstalled, None, None));
    }

    #[test]
    fn applying_without_an_update_says_why_and_touches_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let engine = Engine::new(Paths::under(dir.path()), Scope::Machine, BackendOptions::default()).unwrap();
        let stop = Stop::default();
        let mut phases = Vec::new();
        let mut apply = |installed: Option<Installed>, plan: Plan| {
            engine.apply(&Found { app: photocraft(), installed, latest: None, plan }, &stop, &mut |p| phases.push(p))
        };
        assert_eq!(apply(Some(installed(InstallSource::Msi, "0.8.2")), Plan::UpToDate), Outcome::UpToDate { version: Some("0.8.2".into()) });
        assert_eq!(apply(None, Plan::NotInstalled), Outcome::NotEligible { reason: "PhotoCraft isn't installed for all users".into() });
        assert_eq!(apply(None, Plan::Blocked("no".into())), Outcome::NotEligible { reason: "no".into() });
        assert_eq!(apply(None, Plan::Unknown("offline".into())), Outcome::Failed { error: "offline".into() });
        assert!(phases.is_empty());
        assert!(!dir.path().join("cache").join("downloads").exists(), "nothing was downloaded");
    }

    #[test]
    fn the_service_updates_itself_only_to_a_newer_release_with_its_package() {
        let dir = tempfile::tempdir().unwrap();
        let mut engine = Engine::new(Paths::under(dir.path()), Scope::Machine, BackendOptions::default()).unwrap();
        engine.platform = WINDOWS;
        let ours = version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        let launcher = |v: &Version, with_updater: bool| {
            let mut names = vec![format!("artcraft-launcher-{v}-windows-x64.msi")];
            if with_updater {
                names.push(format!("{UPDATER_ID}-{v}-windows-x64.msi"));
            }
            Release {
                tag: format!("v{v}"),
                version: Some(v.clone()),
                assets: names.into_iter().map(|name| Asset { name, size: 1, url: String::new(), sha256: None }).collect(),
                ..Release::default()
            }
        };
        let newer = Version::new(ours.major, ours.minor, ours.patch + 1);
        let r = launcher(&newer, true);
        assert_eq!(engine.own_update(&r).map(|a| a.name.clone()), Some(format!("{UPDATER_ID}-{newer}-windows-x64.msi")));
        // Not the launcher's own MSI, and nothing when the release doesn't ship the service.
        assert_eq!(engine.own_update(&launcher(&newer, false)), None);
        // Never the same or an older version.
        assert_eq!(engine.own_update(&launcher(&ours, true)), None);
    }
}
