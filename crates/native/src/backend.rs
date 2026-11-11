//! [`NativeBackend`]: the desktop implementation of the UI's [`Backend`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use artcraft_launcher_core::catalog::{LAUNCHER, LAUNCHER_ID};
use artcraft_launcher_core::feed::{Feed, FeedAnswer};
use artcraft_launcher_core::platform::{Arch, Os, Platform, pick_asset};
use artcraft_launcher_core::protocol::{
    Backend, BackendOptions, CANCELLED, Capabilities, Command, Event, INSTALL_CANCELLED, JobPhase, LatestRelease, Origin, PAUSED, PausedDownload,
    Source, VersionTrack,
};
use artcraft_launcher_core::release::{Asset, Release};
use artcraft_launcher_core::{CraftApp, Installed, PackageKind, SUITE, version};

use crate::feed_throttle::{self, FeedThrottle, Outcome};
use crate::http::{self, Http, Stop};
use crate::install::{self, Job};
use crate::manifest::Manifest;
use crate::paths::Paths;
use crate::process;
use crate::source::{GitHubSource, ReleaseSource};

/// The longest the launcher's own update waits for other installs to end before it gives up
/// (it can be cancelled sooner).
const RESTART_WAIT: Duration = Duration::from_secs(10 * 60);
/// Automatic checks closer together than this reuse the last answers. The UI asks every five
/// minutes; this stops a burst of requests (refresh, focus, timers) from repeating work.
const FRESH_FOR: i64 = 4 * 60;
/// Once every app has loaded, GitHub is asked about an app the feed can't vouch for at most this
/// often (pressing Refresh asks right away).
const GITHUB_FALLBACK_EVERY: i64 = 60 * 60;
/// Until every app has loaded once, GitHub may be asked a little more eagerly.
const EARLY_GITHUB_EVERY: i64 = 5 * 60;
/// Tries at the feed per check (after the very first poll). The pause between tries is the
/// [`FeedThrottle`]'s: seconds during the first load, so a check retries; a minute or more after
/// that, so a check then tries once and the retries come from the next checks.
const FEED_ATTEMPTS: usize = 3;
/// The longest a check waits on the [`FeedThrottle`] before carrying on without the feed.
const MAX_THROTTLE_WAIT: i64 = 10;
/// After everything has loaded, the feed counts as seriously failed (and GitHub takes over)
/// only after this many checks in a row without an answer…
const SERIOUS_FEED_FAILURES: u32 = 3;
/// …and this long without a good answer.
const SERIOUS_FEED_OUTAGE: i64 = 60 * 60;
/// Requests in flight at once during a check.
const WORKERS: usize = 4;

type Waker = Arc<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct DownloadSize {
    revision: u64,
    plan: Option<(PackageKind, u64)>,
}

impl DownloadSize {
    fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_add(1);
        self.plan = None;
    }
}

struct Shared {
    events: Mutex<Vec<Event>>,
    /// Held while an app's latest release or installed copy changes and its [`Event::Installable`]
    /// is worked out and queued, so no answer from older state lands after a newer one. Always
    /// taken first, before any other lock here.
    reporting: Mutex<()>,
    /// Held while a report of what an app's install would download is worked out and sent
    /// ([`report_partial`]), so one worked out from older settings or state can't land after a newer
    /// one. Taken before `reporting`.
    planning: Mutex<()>,
    /// What this system could install, and when that was asked ([`Shared::system_kinds`]).
    system: Mutex<Option<(Instant, install::SystemKinds)>>,
    waker: Waker,
    latest: Mutex<HashMap<String, Release>>,
    download_sizes: Mutex<HashMap<String, DownloadSize>>,
    installed: Mutex<HashMap<String, Installed>>,
    jobs: Mutex<HashMap<String, Arc<Stop>>>,
    /// The launcher is replacing itself and will restart: no new job starts.
    restarting: AtomicBool,
    manifest: Mutex<Manifest>,
    options: Mutex<BackendOptions>,
    checking: AtomicBool,
    /// A detection pass is running; another request waits for the next one.
    detecting: AtomicBool,
    last_check: Mutex<Option<i64>>,
    /// The newer ArtCraft Launcher release, once a check has found one.
    launcher_release: Mutex<Option<Release>>,
    /// How this launcher is installed, when it can replace itself. Worked out on the check thread
    /// just before a newer launcher is announced: finding out can ask the host's Flatpak, which
    /// mustn't hold up the first frame.
    self_install: OnceLock<Option<crate::self_update::Install>>,
    /// When GitHub was last asked about each app (Unix seconds), to keep the fallback rare.
    github_asked: Mutex<HashMap<String, i64>>,
    /// Apps with an answer (a release or "none yet") from either service.
    answered: Mutex<std::collections::HashSet<String>>,
    /// Every app has had an answer at least once: from then on GitHub is asked sparingly.
    loaded: AtomicBool,
    /// Checks in a row in which the feed didn't answer.
    feed_failures: Mutex<u32>,
    /// When the feed last answered (Unix seconds).
    feed_ok_at: Mutex<Option<i64>>,
    /// How often ArtCraft's API may be called.
    feed_throttle: Mutex<FeedThrottle>,
    /// Apps whose details pictures are on their way (a second request meanwhile does nothing).
    pictures_busy: Mutex<std::collections::HashSet<&'static str>>,
}

#[derive(Clone, Copy)]
enum BusyFlag {
    Checking,
    Detecting,
}

struct BusyGuard {
    shared: Arc<Shared>,
    flag: BusyFlag,
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        let flag = match self.flag {
            BusyFlag::Checking => &self.shared.checking,
            BusyFlag::Detecting => &self.shared.detecting,
        };
        flag.store(false, Ordering::SeqCst);
    }
}

struct JobGuard {
    shared: Arc<Shared>,
    app_id: String,
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.app_id);
    }
}

/// How long the installability answer trusts what this system could install ([`Shared::system_kinds`]).
const SYSTEM_KINDS_FOR: Duration = Duration::from_secs(60);

impl Shared {
    fn store_latest(&self, app: &str, release: &Release) {
        let mut latest = self.latest.lock().unwrap_or_else(PoisonError::into_inner);
        if latest.get(app) != Some(release) {
            latest.insert(app.to_string(), release.clone());
            self.invalidate_download_size(app);
        }
    }

    fn store_detected(&self, app: &str, found: Option<&Installed>) {
        let mut installed = self.installed.lock().unwrap_or_else(PoisonError::into_inner);
        if installed.get(app) != found {
            match found {
                Some(i) => installed.insert(app.to_string(), i.clone()),
                None => installed.remove(app),
            };
            self.invalidate_download_size(app);
        }
    }

    fn invalidate_download_size(&self, app: &str) {
        self.download_sizes.lock().unwrap_or_else(PoisonError::into_inner).entry(app.to_string()).or_default().invalidate();
    }

    fn download_revision(&self, app: &str) -> u64 {
        self.download_sizes.lock().unwrap_or_else(PoisonError::into_inner).entry(app.to_string()).or_default().revision
    }

    fn report_download(&self, app: &str, revision: u64, plan: Option<(PackageKind, u64)>, download: Option<PausedDownload>, ready: bool) -> bool {
        {
            let mut sizes = self.download_sizes.lock().unwrap_or_else(PoisonError::into_inner);
            let Some(cached) = sizes.get_mut(app).filter(|cached| cached.revision == revision) else { return false };
            cached.plan = plan;
            let mut events = self.events.lock().unwrap_or_else(PoisonError::into_inner);
            events.push(Event::Paused { app: app.to_string(), download });
            events.push(Event::ReadyToInstall { app: app.to_string(), ready });
        }
        (self.waker)();
        true
    }

    /// What this system can install, asked again at most once a [`SYSTEM_KINDS_FOR`]: inside the
    /// Flatpak sandbox asking runs a program on the host (up to 3 s), and the installability answer
    /// is worked out for each installed app on every check. An install still asks afresh. Always
    /// asked before `reporting` is taken. [`report_partial`] asks it holding `planning`, which only
    /// its own worker threads take.
    fn system_kinds(&self) -> install::SystemKinds {
        if let Some((at, kinds)) = self.system.lock().unwrap_or_else(PoisonError::into_inner).as_ref()
            && at.elapsed() < SYSTEM_KINDS_FOR
        {
            return kinds.clone();
        }
        let kinds = install::SystemKinds::now();
        *self.system.lock().unwrap_or_else(PoisonError::into_inner) = Some((Instant::now(), kinds.clone()));
        kinds
    }

    /// What this system could install when last asked, however long ago, for an answer worked out
    /// holding `reporting`: no probe runs there. A copy only enters `installed` through
    /// [`publish_detected`], which asks [`Self::system_kinds`] first, so with a copy installed
    /// there is always a view; without one (never in practice) nothing installs.
    fn last_system_kinds(&self) -> install::SystemKinds {
        self.system
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map_or_else(|| install::SystemKinds { available: Vec::new(), can_pacman: false }, |(_, kinds)| kinds.clone())
    }

    /// Whether some copy of `app` is installed (as last detected).
    fn has_copy(&self, app: &CraftApp) -> bool {
        self.installed.lock().unwrap_or_else(PoisonError::into_inner).contains_key(app.id)
    }

    fn emit(&self, event: Event) {
        self.emit_all([event]);
    }

    /// Queues `events` together: a poll sees all of them or none.
    fn emit_all(&self, events: impl IntoIterator<Item = Event>) {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).extend(events);
        (self.waker)();
    }
}

pub struct NativeBackend {
    shared: Arc<Shared>,
    paths: Paths,
    http: Option<Http>,
    source: Option<Arc<dyn ReleaseSource>>,
    platform: Option<Platform>,
    /// The version-choice file the apps read (see [`crate::pins`]); `None` when there is no
    /// per-user data folder.
    pins: Option<std::path::PathBuf>,
}

impl NativeBackend {
    /// `waker` is called (from any thread) whenever events are waiting; it should repaint the UI.
    pub fn new(waker: impl Fn() + Send + Sync + 'static) -> Self {
        let waker: Waker = Arc::new(waker);
        let paths = Paths::new().unwrap_or_else(|e| {
            log::error!("{e}; keeping files in the temporary folder");
            Paths::under(&std::env::temp_dir().join("artcraft-launcher"))
        });
        let http = match Http::new() {
            Ok(h) => Some(h),
            Err(e) => {
                log::error!("HTTPS unavailable: {e}");
                None
            }
        };
        let source: Option<Arc<dyn ReleaseSource>> =
            http.clone().map(|h| Arc::new(GitHubSource::new(h, paths.release_cache())) as Arc<dyn ReleaseSource>);
        let shared = Arc::new(Shared {
            events: Mutex::new(Vec::new()),
            reporting: Mutex::new(()),
            system: Mutex::new(None),
            planning: Mutex::new(()),
            waker,
            latest: Mutex::new(HashMap::new()),
            download_sizes: Mutex::new(HashMap::new()),
            installed: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            restarting: AtomicBool::new(false),
            manifest: Mutex::new(Manifest::load(&paths.manifest())),
            options: Mutex::new(BackendOptions::default()),
            checking: AtomicBool::new(false),
            detecting: AtomicBool::new(false),
            launcher_release: Mutex::new(None),
            self_install: OnceLock::new(),
            github_asked: Mutex::new(HashMap::new()),
            answered: Mutex::new(std::collections::HashSet::new()),
            loaded: AtomicBool::new(false),
            feed_failures: Mutex::new(0),
            feed_ok_at: Mutex::new(None),
            feed_throttle: Mutex::new(FeedThrottle::load(&paths.feed_throttle(), crate::now())),
            pictures_busy: Mutex::new(std::collections::HashSet::new()),
            last_check: Mutex::new(None),
        });
        let pins = craft_update::pins::Pins::default_path();
        if let Some(path) = &pins {
            crate::pins::prepare_folder(path);
        }
        let backend = Self { shared, paths, http, source, platform: native_platform(), pins };
        backend.replay_cache();
        backend.detect_all();
        backend
    }

    /// Shows the saved answers from the last run straight away, before any network request: the
    /// release feed's where they're still fresh, GitHub's otherwise.
    fn replay_cache(&self) {
        let options = self.options();
        let pre = options.prereleases;
        let feed = if pre { None } else { load_feed(&self.paths) };
        let now = crate::now();
        for app in SUITE.iter().filter(|a| self.platform.is_none_or(|p| a.runs_on(p.os))) {
            let answer = match feed.as_ref().map(|f| f.answer(app, now)) {
                Some(FeedAnswer::Fresh(Some(r))) => Some((LatestRelease::Found(r), Origin::ArtCraftApi)),
                Some(FeedAnswer::Fresh(None)) => Some((LatestRelease::None, Origin::ArtCraftApi)),
                _ => self.source.as_ref().and_then(|s| s.cached(&feed_repo(feed.as_ref(), app), pre)).map(|l| (l, Origin::GitHub)),
            };
            if let Some((latest, from)) = answer {
                publish_latest(app, latest, from, &self.shared, self.platform);
            }
        }
        if let Some(f) = feed {
            self.shared.emit(Event::Notices { announcements: f.announcements, alerts: f.alerts });
        }
    }

    /// Works out again, off the UI thread, what each app's install would download, and tells the
    /// UI ([`report_partial`]): inside the Flatpak sandbox that asks the host (up to 3 s an app).
    fn replan(&self) {
        let shared = self.shared.clone();
        let paths = self.paths.clone();
        let platform = self.platform;
        std::thread::spawn(move || {
            for app in SUITE.iter().filter(|a| platform.is_none_or(|p| a.runs_on(p.os))) {
                report_partial(app, &shared, &paths, platform);
            }
        });
    }

    fn options(&self) -> BackendOptions {
        self.shared.options.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Scans for installed Craft Apps and paused downloads on a background thread.
    fn detect_all(&self) {
        if self.shared.detecting.swap(true, Ordering::SeqCst) {
            return;
        }
        let shared = self.shared.clone();
        let busy = BusyGuard { shared: shared.clone(), flag: BusyFlag::Detecting };
        let paths = self.paths.clone();
        let platform = self.platform;
        std::thread::spawn(move || {
            let busy = busy;
            // Leftovers from interrupted installs or older versions of the launcher. A paused
            // download stays a month; a finished one kept after a failed install, a week.
            http::clean_downloads(&paths.downloads(), Duration::from_secs(30 * 86_400), Duration::from_secs(7 * 86_400));
            let snapshot = crate::detect::Snapshot::take();
            let manifest = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).clone();
            for app in SUITE.iter().filter(|a| platform.is_none_or(|p| a.runs_on(p.os))) {
                let found = crate::detect::detect(app, &manifest, &snapshot);
                publish_detected(app, found, &shared, platform);
            }
            // Downloads paused (or cut off by quitting) in an earlier run.
            for app in SUITE.iter().filter(|a| platform.is_none_or(|p| a.runs_on(p.os))) {
                report_partial(app, &shared, &paths, platform);
            }
            drop(busy);
        });
    }

    /// Looks up the latest releases: ArtCraft's release feed first (one request for every app),
    /// GitHub for anything the feed can't vouch for (apps it doesn't list, entries more than a
    /// day old, or no answer at all), at most once an hour per app unless `force`.
    fn check(&self, force: bool) {
        let (Some(source), Some(http)) = (self.source.clone(), self.http.clone()) else {
            for app in SUITE.iter().filter(|a| self.platform.is_none_or(|p| a.runs_on(p.os))) {
                self.shared.emit(Event::Latest {
                    app: app.id.to_string(),
                    latest: LatestRelease::Failed("secure connections aren't available".into()),
                    from: Origin::GitHub,
                });
            }
            return;
        };
        let now = crate::now();
        let recent = self.shared.last_check.lock().unwrap_or_else(PoisonError::into_inner).is_some_and(|t| now - t < FRESH_FOR);
        if (recent && !force) || self.shared.checking.swap(true, Ordering::SeqCst) {
            return;
        }
        let shared = self.shared.clone();
        let busy = BusyGuard { shared: shared.clone(), flag: BusyFlag::Checking };
        let paths = self.paths.clone();
        let platform = self.platform;
        let options = self.options();
        let pre = options.prereleases;
        let mirror = local_mirror();
        std::thread::spawn(move || {
            let busy = busy;
            shared.emit(Event::CheckStarted);
            let (feed, fallback) = check_plan(&http, mirror.as_ref(), pre, &paths, &shared);
            if let Some(f) = &feed {
                shared.emit(Event::Notices { announcements: f.announcements.clone(), alerts: f.alerts.clone() });
            }
            let now = crate::now();
            let queue = Mutex::new(
                SUITE.iter().filter(|a| platform.is_none_or(|p| a.runs_on(p.os))).chain(std::iter::once(&LAUNCHER)).collect::<Vec<&CraftApp>>(),
            );
            let sources = sweep(&queue, &mirror, feed.as_ref(), fallback, &source, &shared, &paths, (platform, &options), force, now);
            source.save();
            {
                let answered = shared.answered.lock().unwrap_or_else(PoisonError::into_inner);
                if SUITE.iter().filter(|a| platform.is_none_or(|p| a.runs_on(p.os))).all(|a| answered.contains(a.id)) {
                    shared.loaded.store(true, Ordering::SeqCst);
                }
            }
            // A newer release changes which file an install would use.
            for app in SUITE.iter().filter(|a| platform.is_none_or(|p| a.runs_on(p.os))) {
                report_partial(app, &shared, &paths, platform);
            }
            let source_used = source_used_of(&sources);
            let at = crate::now();
            *shared.last_check.lock().unwrap_or_else(PoisonError::into_inner) = Some(at);
            let rate_limited_until = limit_notice(source_used, source.rate_limited_until());
            drop(busy);
            shared.emit(Event::CheckFinished { at, source: source_used, rate_limited_until });
        });
    }

    /// Starts the job, or returns `false` without starting one: the app is unknown, doesn't run
    /// on this platform, is already running a job, or the launcher is restarting into an update.
    fn start_job(&self, app_id: &str, work: Work) -> bool {
        let uninstall = matches!(work, Work::Uninstall);
        let Some(app) = CraftApp::by_id(app_id) else { return false };
        if self.platform.is_some_and(|p| !app.runs_on(p.os)) {
            return false;
        }
        log::info!("{}: starting {} job", app.name, if uninstall { "uninstall" } else { "install/update" });
        let stop = Arc::new(Stop::default());
        let restarting = {
            let mut jobs = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            if jobs.contains_key(app.id) {
                return false;
            }
            let restarting = self.shared.restarting.load(Ordering::SeqCst);
            if !restarting {
                jobs.insert(app.id.to_string(), stop.clone());
            }
            restarting
        };
        if restarting {
            let result = Err("ArtCraft Launcher is restarting to finish its update. Try again once it's back.".into());
            self.shared.emit(Event::JobFinished { app: app.id.to_string(), result });
            return false;
        }
        let shared = self.shared.clone();
        let job = JobGuard { shared: shared.clone(), app_id: app.id.to_string() };
        let paths = self.paths.clone();
        let http = self.http.clone();
        let platform = self.platform;
        let options = self.options();
        let source = self.source.clone();
        std::thread::spawn(move || {
            let job = job;
            let existing = shared.installed.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned();
            let record = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).apps.get(app.id).cloned();
            let result = if uninstall {
                shared.emit(Event::Progress { app: app.id.to_string(), phase: JobPhase::Removing });
                let mut phase = |phase| shared.emit(Event::Progress { app: app.id.to_string(), phase });
                match &existing {
                    Some(inst) => install::uninstall(app, inst, record.as_ref(), &paths, &stop, &mut phase).map(|()| {
                        let mut m = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner);
                        if m.apps.remove(app.id).is_some() {
                            let _ = m.save(&paths.manifest());
                        }
                        format!("{} was removed", app.name)
                    }),
                    None => Err(format!("{} isn't installed", app.name)),
                }
            } else {
                let (approved, batch) = match &work {
                    Work::Replace(path) => (Some(path.as_str()), None),
                    Work::Install(batch) => (None, batch.clone()),
                    Work::Uninstall => (None, None),
                };
                let r = run_install(
                    app,
                    &shared,
                    &paths,
                    http.as_ref(),
                    platform,
                    &options,
                    existing.as_ref(),
                    record.as_ref(),
                    &stop,
                    approved,
                    source.as_deref(),
                    batch,
                );
                // Stopped by the connection rather than by the user: say so, and how to go on.
                match r {
                    Err(e) if e == PAUSED && stop.check().is_ok() => {
                        Err("The download paused because the connection keeps dropping. Press Resume to continue where it stopped.".into())
                    }
                    other => other,
                }
            };
            // Look again: the result is whatever is on disk now.
            let manifest = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).clone();
            let found = crate::detect::detect(app, &manifest, &crate::detect::Snapshot::take());
            // One installer call can cover several apps, and fail after some of them went in.
            let latest = shared.latest.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned();
            let result = match (result, latest) {
                (Err(e), Some(release))
                    if !uninstall && release.version.as_ref().is_some_and(|v| landed_despite(&e, existing.as_ref(), found.as_ref(), v)) =>
                {
                    log::warn!("{}: {e}; the new version is installed all the same", app.id);
                    Ok(installed_text(app, existing.is_some(), &release))
                }
                (result, _) => result,
            };
            // The manifest may change the package plan even when detection finds the same copy.
            shared.invalidate_download_size(app.id);
            publish_detected(app, found, &shared, platform);
            report_partial(app, &shared, &paths, platform);
            drop(job);
            shared.emit(Event::JobFinished { app: app.id.to_string(), result });
        });
        true
    }

    /// Starts one install per app, together (see [`Command::InstallAll`]): where the platform
    /// needs a permission prompt, the batch shares one — each member waits for the others'
    /// downloads before the system installer is asked once. Apps unknown or already running a
    /// job are left out; a batch of one installs the same as a single [`Command::Install`].
    fn install_all(&self, apps: &[String]) {
        let running = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let mut ids: Vec<&str> = Vec::new();
        for id in apps {
            let Some(app) = CraftApp::by_id(id) else { continue };
            if running.contains_key(app.id) || ids.contains(&app.id) {
                continue;
            }
            ids.push(app.id);
        }
        drop(running);
        #[cfg(any(target_os = "linux", windows))]
        let batch = (!ids.is_empty()).then(|| install::INSTALLER.gather(ids.len()));
        #[cfg(not(any(target_os = "linux", windows)))]
        let batch = None;
        let mut started = 0;
        for id in &ids {
            if self.start_job(id, Work::Install(batch.clone())) {
                started += 1;
            }
        }
        // Members that could not start after all (another job raced in for them) are not coming:
        // their places in the batch are given back, so the rest don't wait for them.
        for _ in started..ids.len() {
            if let Some(batch) = &batch {
                batch.leave();
            }
        }
    }

    /// Downloads the newer launcher, puts it in place and asks the UI to quit so it can start.
    fn update_launcher(&self) {
        let id = LAUNCHER_ID.to_string();
        let (Some(install), Some(platform)) = (self.shared.self_install.get().cloned().flatten(), self.platform) else {
            self.shared.emit(Event::JobFinished {
                app: id,
                result: Err("this copy of ArtCraft Launcher can't update itself; download the new version instead".into()),
            });
            return;
        };
        let Some(release) = self.shared.launcher_release.lock().unwrap_or_else(PoisonError::into_inner).clone() else { return };
        let stop = Arc::new(Stop::default());
        {
            let mut jobs = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            if jobs.contains_key(&id) {
                return;
            }
            jobs.insert(id.clone(), stop.clone());
        }
        let job = JobGuard { shared: self.shared.clone(), app_id: id.clone() };
        let (shared, paths, http) = (self.shared.clone(), self.paths.clone(), self.http.clone());
        std::thread::spawn(move || {
            let job = job;
            let mut last_emit = Instant::now() - Duration::from_secs(1);
            let mut phase = |p: JobPhase| {
                if matches!(p, JobPhase::Downloading { done, total } if done != total && done != 0) && last_emit.elapsed() < Duration::from_millis(50)
                {
                    return;
                }
                last_emit = Instant::now();
                shared.emit(Event::Progress { app: LAUNCHER_ID.to_string(), phase: p });
            };
            let last = || {
                let mut waiting = || shared.emit(Event::Progress { app: LAUNCHER_ID.to_string(), phase: JobPhase::Waiting });
                claim_restart(&shared.jobs, &shared.restarting, &stop, RESTART_WAIT, &mut waiting)
            };
            let result = match http.as_ref() {
                Some(http) => crate::self_update::update(&install, &release, platform, http, &paths, &stop, &mut phase, &last),
                None => Err("secure connections aren't available on this system".into()),
            };
            drop(job);
            match result {
                Ok(()) => shared.emit(Event::LauncherRestarting),
                Err(e) => {
                    shared.restarting.store(false, Ordering::SeqCst);
                    shared.emit(Event::JobFinished { app: LAUNCHER_ID.to_string(), result: Err(e) });
                }
            }
        });
    }

    fn installed(&self, app: &str) -> Option<Installed> {
        self.shared.installed.lock().unwrap_or_else(PoisonError::into_inner).get(app).cloned()
    }
}

#[derive(Clone, PartialEq, Eq)]
enum Work {
    /// Install or update. Carries the batch this job is part of, if any (see
    /// [`Command::InstallAll`]): where the platform needs a permission prompt, the batch shares one.
    Install(Option<crate::queue::Gather>),
    /// Install over the path the user agreed to overwrite (see [`Command::Replace`]).
    Replace(String),
    Uninstall,
}

fn release_matches_track(release: &Release, track: &VersionTrack, prereleases: bool) -> bool {
    match track {
        VersionTrack::Stable => prereleases || (!release.prerelease && release.version.as_ref().is_some_and(version::is_stable)),
        VersionTrack::Nightly => artcraft_launcher_core::release::is_nightly(release),
        VersionTrack::Pinned(tag) => &release.tag == tag,
    }
}

/// Whether `chosen` is a version picked by name that is older than the installed copy.
fn is_chosen_downgrade(track: &VersionTrack, chosen: &Release, existing: Option<&Installed>) -> bool {
    let VersionTrack::Pinned(tag) = track else { return false };
    let (Some(chosen_v), Some(installed_v)) = (chosen.version.as_ref(), existing.and_then(|i| i.version.as_ref())) else { return false };
    chosen.tag == *tag && version::is_newer(installed_v, chosen_v)
}

/// For a version chosen by name that is older than the installed copy: the installed version's own
/// published release, to put back if the downgrade fails. `None` for anything else, without
/// asking `list`. An error means the downgrade can't be made safe, so it must not start.
fn downgrade_previous(
    app: &CraftApp,
    track: &VersionTrack,
    chosen: &Release,
    existing: Option<&Installed>,
    list: impl FnOnce() -> Result<Vec<Release>, String>,
) -> Result<Option<Release>, String> {
    let Some(installed_v) = existing.and_then(|i| i.version.as_ref()).filter(|_| is_chosen_downgrade(track, chosen, existing)) else {
        return Ok(None);
    };
    let releases =
        list().map_err(|e| format!("couldn't find {} {installed_v} to put back if going back fails ({e}), so nothing was changed.", app.name))?;
    releases.into_iter().find(|r| r.version.as_ref() == Some(installed_v)).map(Some).ok_or_else(|| {
        format!("{} {installed_v} isn't published any more, so it couldn't be put back if going back fails. Nothing was changed.", app.name)
    })
}

/// What an install does with a Flatpak its repository updates: `flatpak update`, on Linux, and
/// only on the stable track (it follows the repository's latest build, never a pinned or nightly
/// one). `None` for any other copy, which [`install_plan`] decides; else the copy, or why the
/// update can't run. [`run_install`] and the UI's [`Event::Installable`] both ask this.
fn repository_update<'a>(app: &CraftApp, existing: Option<&'a Installed>, options: &BackendOptions) -> Option<Result<&'a Installed, String>> {
    let inst = existing.filter(|i| i.source == artcraft_launcher_core::InstallSource::Flatpak && i.externally_managed)?;
    Some(if !matches!(options.versions.get(app.id), None | Some(VersionTrack::Stable)) {
        Err(install::managed_elsewhere(app, inst, false).unwrap_or_else(|| "Update this app through its Flatpak repository".into()))
    } else if !cfg!(target_os = "linux") {
        Err(flatpak_unsupported(app))
    } else {
        Ok(inst)
    })
}

/// Why a Flatpak can't be updated on a system without Flatpak.
fn flatpak_unsupported(app: &CraftApp) -> String {
    format!("{} is a Flatpak, which is not supported on this platform", app.name)
}

/// Who asks [`install_plan`].
#[derive(Clone, Copy, Debug)]
enum Asking<'a> {
    /// An install: it asks this system what it can install now, and explains a refusal in full.
    Install,
    /// The installability answer: what this system could install a moment ago
    /// ([`Shared::system_kinds`]), and only whether a refusal would come.
    Answer(&'a install::SystemKinds),
}

/// What an install of `app` would download, worked out from what is installed and chosen
/// before anything is fetched.
struct InstallPlan {
    release: Release,
    platform: Platform,
    asset: Asset,
    kind: PackageKind,
}

/// The checks an install makes before it downloads anything: a copy installed outside the
/// launcher (a system binary, a repository package) isn't replaced; the release must be known and
/// on the chosen version track; and it must have a package that can replace the copy the way it
/// was installed. [`run_install`] and the UI's [`Event::Installable`] both ask this, so the UI
/// never offers an update the install would refuse.
fn install_plan(
    app: &CraftApp,
    latest: Option<Release>,
    platform: Option<Platform>,
    options: &BackendOptions,
    (existing, record): (Option<&Installed>, Option<&crate::manifest::Record>),
    asking: Asking<'_>,
) -> Result<InstallPlan, String> {
    let managed = existing.and_then(|e| match asking {
        Asking::Install => install::managed_elsewhere(app, e, false),
        // No pacman lookup for the wording of a refusal: only yes or no is wanted.
        Asking::Answer(_) => install::is_managed_elsewhere(app, e).then(String::new),
    });
    if let Some(msg) = managed {
        return Err(msg);
    }
    let platform = platform.ok_or("this system isn't supported")?;
    let release = latest.ok_or("the selected release hasn't been found yet. Check for updates first.")?;
    let track = options.versions.get(app.id).unwrap_or(&VersionTrack::Stable);
    if !release_matches_track(&release, track, options.prereleases) {
        return Err("the selected version isn't available yet. Check for updates before installing.".into());
    }
    let system = match asking {
        Asking::Install => None,
        Asking::Answer(system) => Some(system),
    };
    let (asset, kind) = planned_asset_on(app, &release, platform, options, (existing, record), system).ok_or_else(|| {
        if platform.os == artcraft_launcher_core::Os::Linux
            && options.install_locations.folder(app.id).is_some()
            && existing.is_none()
            && record.is_none()
        {
            "this release has no AppImage for a custom install folder".to_string()
        } else {
            format!("{} {} has no download for {}", app.name, release.version_label(), platform.label())
        }
    })?;
    Ok(InstallPlan { release, platform, asset, kind })
}

#[allow(clippy::too_many_arguments)]
fn run_install(
    app: &CraftApp,
    shared: &Shared,
    paths: &Paths,
    http: Option<&Http>,
    platform: Option<Platform>,
    options: &BackendOptions,
    existing: Option<&Installed>,
    record: Option<&crate::manifest::Record>,
    stop: &Stop,
    approved: Option<&str>,
    source: Option<&dyn ReleaseSource>,
    batch: Option<crate::queue::Gather>,
) -> Result<String, String> {
    // The ways out below that never reach the installer (a repository-managed Flatpak, a missing
    // release, the overwrite question, a paused download) free the rest of the batch at once;
    // from `install_at` on, the same guard does it there.
    let mut batch = install::BatchPlace(batch);
    if let Some(update) = repository_update(app, existing, options) {
        let inst = update?;
        #[cfg(target_os = "linux")]
        {
            shared.emit(Event::Progress { app: app.id.to_string(), phase: JobPhase::Installing });
            let mut phase = |p| shared.emit(Event::Progress { app: app.id.to_string(), phase: p });
            let default_id = app.bundle_id();
            let target_id = inst.package.as_deref().unwrap_or(&default_id);
            let scope = crate::packages::flatpak_scope(inst.installation.as_deref());
            let deployed_commit = || {
                crate::process::capture_flatpak(&["info", &scope, "--show-commit", target_id], std::time::Duration::from_secs(15))
                    .filter(|value| !value.trim().is_empty())
            };
            let before = deployed_commit();
            install::linux::update_flatpak_app(app, target_id, inst.installation.as_deref(), inst.origin.as_deref(), stop, &mut phase)?;
            let after = deployed_commit();
            return Ok(match (before, after) {
                (Some(before), Some(after)) if before.trim() == after.trim() => format!("{} is already up to date", app.name),
                (Some(_), Some(_)) => format!("{} was updated through its Flatpak repository", app.name),
                _ => format!("Flatpak finished checking {} for updates", app.name),
            });
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = inst;
            return Err(flatpak_unsupported(app));
        }
    }
    let latest = shared.latest.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned();
    let InstallPlan { release, platform, asset, kind } = install_plan(app, latest, platform, options, (existing, record), Asking::Install)?;
    let http = http.ok_or("secure connections aren't available on this system")?;
    let track = options.versions.get(app.id).unwrap_or(&VersionTrack::Stable);
    // Going back to a version chosen by name. A macOS bundle or a launcher AppImage keeps the old
    // copy until the new one is in place. Otherwise the installed version's own package is found
    // first, so a downgrade that fails can put it back. Without it nothing changes.
    let keeps_old_copy = install::keeps_old_copy(kind, existing, record);
    let previous = if keeps_old_copy {
        None
    } else {
        downgrade_previous(app, track, &release, existing, || {
            source
                .ok_or_else(|| "secure connections aren't available on this system".to_string())?
                .versions(&feed_repo(load_feed(paths).as_ref(), app))
        })?
    };
    let previous_asset = match &previous {
        Some(p) => Some(
            planned_asset(app, p, platform, options, existing, record)
                .filter(|(_, k)| *k == kind)
                .map(|(a, _)| a)
                .ok_or_else(|| format!("{} {} has no matching download to put back, so nothing was changed.", app.name, p.version_label()))?,
        ),
        None => None,
    };
    let downgrade = if keeps_old_copy {
        is_chosen_downgrade(track, &release, existing).then_some(install::Downgrade::KeepOldCopy)
    } else {
        previous.as_ref().zip(previous_asset.as_ref()).map(|(previous, asset)| install::Downgrade::Reinstall { previous, asset })
    };
    let occupied = install::occupied(app, &asset, kind, existing, record, &options.install_locations, paths)?;
    let replace = match replace_consent(occupied.as_deref(), approved) {
        Consent::Free => false,
        Consent::Given => true,
        Consent::Ask(path) => {
            shared.emit(Event::Occupied { app: app.id.to_string(), path: path.display().to_string() });
            return Err(format!("{} already exists", path.display()));
        }
    };
    let approved_path = occupied.as_deref().filter(|_| replace);
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let mut phase = |p: JobPhase| {
        // Downloads report thousands of times; the UI needs ~20 a second.
        if matches!(p, JobPhase::Downloading { done, total } if done != total && done != 0) && last_emit.elapsed() < Duration::from_millis(50) {
            return;
        }
        last_emit = Instant::now();
        shared.emit(Event::Progress { app: app.id.to_string(), phase: p });
    };
    let mut job = Job {
        http,
        paths,
        stop,
        phase: &mut phase,
        install_locations: &options.install_locations,
        replace,
        approved_path,
        downgrade,
        batch: batch.take(),
    };
    let mut commit_record = |record: &crate::manifest::Record| {
        let mut manifest = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner);
        let mut updated = manifest.clone();
        updated.apps.insert(app.id.to_string(), record.clone());
        updated.save(&paths.manifest())?;
        *manifest = updated;
        Ok(())
    };
    let new_record = install::install_at(app, &release, &asset, kind, existing, record, &mut job, Some(&mut commit_record))?;
    {
        let mut m = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner);
        let changed = match new_record {
            Some(r) => {
                m.apps.insert(app.id.to_string(), r);
                true
            }
            // Installed the platform's own way: any old launcher record is stale.
            None => m.apps.remove(app.id).is_some(),
        };
        if changed && let Err(e) = m.save(&paths.manifest()) {
            log::warn!("couldn't save the install record: {e}");
        }
    }
    Ok(installed_text(app, existing.is_some(), &release))
}

/// Whether an install may write over what is at its destination.
#[derive(Debug, PartialEq, Eq)]
enum Consent<'a> {
    /// Nothing the launcher didn't put there is in the way.
    Free,
    /// The user agreed to overwrite exactly this destination.
    Given,
    /// Something is in the way and the user hasn't agreed to overwrite it: ask.
    Ask(&'a std::path::Path),
}

/// The user's agreement to replace a file covers the path they were shown (`approved`), and only
/// that one. The release can change between the question and the answer (a new version, another
/// asset, another folder), and the install then writes somewhere else: it must not take the old
/// "yes" as leave to overwrite a different file (CWE-367).
fn replace_consent<'a>(occupied: Option<&'a std::path::Path>, approved: Option<&str>) -> Consent<'a> {
    match occupied {
        None => Consent::Free,
        Some(path) if approved == Some(path.display().to_string().as_str()) => Consent::Given,
        Some(path) => Consent::Ask(path),
    }
}

/// What a finished install tells the user.
fn installed_text(app: &CraftApp, update: bool, release: &Release) -> String {
    let verb = if update { "updated to" } else { "installed:" };
    format!("{} {verb} {}", app.name, release.version_label())
}

/// Whether an install whose job failed with `error` went in all the same. Only a failed call
/// shared by several apps (Linux packages installed together) can have installed some of them;
/// any other error, a cancellation or a pause stands, whatever is found afterwards.
fn landed_despite(error: &str, before: Option<&Installed>, after: Option<&Installed>, latest: &version::Version) -> bool {
    error != PAUSED && error != CANCELLED && error != INSTALL_CANCELLED && install::failed_in_shared_call(error) && landed(before, after, latest)
}

/// Whether an install took, whatever its job reported: `latest` is there now and wasn't before.
fn landed(before: Option<&Installed>, after: Option<&Installed>, latest: &version::Version) -> bool {
    let has_latest = |copy: Option<&Installed>| copy.and_then(|i| i.version.as_ref()).is_some_and(|v| !version::is_update(latest, v));
    has_latest(after) && !has_latest(before)
}

/// For the launcher's own update, before it replaces itself: waits (saying so through `waiting`)
/// until every other job has ended, then sets `restarting`, which keeps new jobs from starting.
/// The restart that follows would otherwise cut their installs short. The user can cancel the
/// wait (`stop`); after `limit` it gives up and names the apps still busy, since one may be
/// stuck behind a password prompt nobody has answered.
fn claim_restart(
    jobs: &Mutex<HashMap<String, Arc<Stop>>>,
    restarting: &AtomicBool,
    stop: &Stop,
    limit: Duration,
    waiting: &mut dyn FnMut(),
) -> Result<(), String> {
    let started = Instant::now();
    let mut told = false;
    loop {
        stop.check()?;
        let busy: Vec<&'static str> = {
            let jobs = jobs.lock().unwrap_or_else(PoisonError::into_inner);
            let busy = jobs.keys().filter(|id| *id != LAUNCHER_ID).map(|id| CraftApp::by_id(id).map_or("another app", |a| a.name)).collect();
            if jobs.keys().all(|id| id == LAUNCHER_ID) {
                restarting.store(true, Ordering::SeqCst);
                return Ok(());
            }
            busy
        };
        if started.elapsed() >= limit {
            return Err(restart_blocked(busy));
        }
        if !told {
            told = true;
            waiting();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Why the launcher didn't restart: `busy` apps were still installing, and what to do about it.
fn restart_blocked(mut busy: Vec<&str>) -> String {
    busy.sort_unstable();
    busy.dedup();
    let (names, were, them) = match busy.as_slice() {
        [one] => ((*one).to_string(), "was", "it"),
        _ => (busy.join(", "), "were", "them"),
    };
    format!(
        "ArtCraft Launcher didn't restart to update: {names} {were} still installing. Answer or close any password prompt, \
         let {them} finish or cancel {them}, then press Update and restart again."
    )
}

/// The file an install of `app` would download now: same format as the installed copy, else
/// the user's preference, else the best this system supports.
fn planned_asset(
    app: &CraftApp,
    release: &Release,
    platform: Platform,
    options: &BackendOptions,
    existing: Option<&Installed>,
    record: Option<&crate::manifest::Record>,
) -> Option<(Asset, PackageKind)> {
    planned_asset_on(app, release, platform, options, (existing, record), None)
}

/// [`planned_asset`], on what `system` can install when given (else asking this system now).
fn planned_asset_on(
    app: &CraftApp,
    release: &Release,
    platform: Platform,
    options: &BackendOptions,
    (existing, record): (Option<&Installed>, Option<&crate::manifest::Record>),
    system: Option<&install::SystemKinds>,
) -> Option<(Asset, PackageKind)> {
    let stems: Vec<&str> = app.stems().collect();
    let custom_linux = platform.os == artcraft_launcher_core::Os::Linux
        && !process::in_sandbox()
        && existing.is_none()
        && record.is_none()
        && options.install_locations.folder(app.id).is_some();
    let kinds = if existing.is_none() && record.is_some_and(|r| r.kind == PackageKind::PortableZip) {
        vec![PackageKind::PortableZip]
    } else if custom_linux {
        vec![artcraft_launcher_core::PackageKind::AppImage]
    } else {
        match system {
            Some(system) => install::choose_kinds_on(app, existing, record, &options.kinds, system),
            None => install::choose_kinds(app, existing, record, &options.kinds),
        }
    };
    pick_asset(release, &stems, platform, &kinds).map(|(a, k)| (a.clone(), k))
}

/// Tells the UI whether installing `app` would go ahead now ([`Event::Installable`]): the same
/// [`install_plan`] an install makes, for the copy that is installed. Nothing is said until its
/// release is known, or while nothing is installed (the UI only uses it for an installed copy).
fn report_installable(app: &CraftApp, shared: &Shared, platform: Option<Platform>) {
    let system = shared.has_copy(app).then(|| shared.system_kinds());
    let _reporting = shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
    shared.emit_all(installable_event(app, shared, platform, system));
}

/// [`report_installable`]'s answer from the shared state as it is, with the current settings:
/// whether [`install_plan`] would let an install go ahead. `system` is what this system can
/// install, asked before `reporting` was taken; when a copy appeared since, the view its detection
/// asked for is used, so nothing is probed here. The caller holds `reporting`.
fn installable_event(app: &CraftApp, shared: &Shared, platform: Option<Platform>, system: Option<install::SystemKinds>) -> Option<Event> {
    let existing = shared.installed.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned()?;
    let release = shared.latest.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned()?;
    let record = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).apps.get(app.id).cloned();
    let options = shared.options.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let installable = match repository_update(app, Some(&existing), &options) {
        // Updated with `flatpak update`: nothing is downloaded, so nothing to ask the system.
        Some(update) => update.is_ok(),
        None => {
            // A copy appeared after the caller looked: what was asked moments ago, not a probe here.
            let system = system.unwrap_or_else(|| shared.last_system_kinds());
            install_plan(app, Some(release), platform, &options, (Some(&existing), record.as_ref()), Asking::Answer(&system)).is_ok()
        }
    };
    Some(Event::Installable { app: app.id.to_string(), installable })
}

/// Records `app`'s latest release and tells the UI, with whether an install would go ahead with
/// it, in one batch: the UI never sees the release without its answer.
fn publish_latest(app: &CraftApp, latest: LatestRelease, from: Origin, shared: &Shared, platform: Option<Platform>) {
    let system = shared.has_copy(app).then(|| shared.system_kinds());
    let _reporting = shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
    if let LatestRelease::Found(r) = &latest {
        shared.store_latest(app.id, r);
    }
    let installable = installable_event(app, shared, platform, system);
    shared.emit_all(installable.into_iter().chain([Event::Latest { app: app.id.to_string(), latest, from }]));
}

/// Records what is installed of `app` (`found`) and tells the UI, with whether an install would
/// go ahead to replace it, in one batch.
fn publish_detected(app: &CraftApp, found: Option<Installed>, shared: &Shared, platform: Option<Platform>) {
    let system = found.is_some().then(|| shared.system_kinds());
    let _reporting = shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
    shared.store_detected(app.id, found.as_ref());
    let installable = installable_event(app, shared, platform, system);
    shared.emit_all(installable.into_iter().chain([Event::Detected { app: app.id.to_string(), installed: found }]));
}

/// Tells the UI whether `app` has a paused download waiting, or a finished one to install, and
/// whether an install would go ahead at all (the package choices may have changed). Worked out
/// with the settings and state as they are now, one report at a time (`planning`), so a worker
/// that started before a settings change never has the last word.
fn report_partial(app: &CraftApp, shared: &Shared, paths: &Paths, platform: Option<Platform>) {
    let _one_at_a_time = shared.planning.lock().unwrap_or_else(PoisonError::into_inner);
    // A release or detection can change during planning; retry any rejected report.
    loop {
        // Capture the revision before the options: Configure invalidates it while changing options.
        let revision = shared.download_revision(app.id);
        let options = shared.options.lock().unwrap_or_else(PoisonError::into_inner).clone();
        report_installable(app, shared, platform);
        let planned = planned_download(app, shared, paths, platform, &options);
        let download = planned.as_ref().and_then(|(dest, _, size)| Some(PausedDownload { done: http::partial_bytes(dest, *size)?, total: *size }));
        let ready = planned.as_ref().is_some_and(|(dest, _, size)| http::finished(dest, *size));
        if shared.report_download(app.id, revision, planned.as_ref().map(|(_, kind, size)| (*kind, *size)), download, ready) {
            return;
        }
    }
}

/// Where the planned download of `app` is kept, its package format and published size.
fn planned_download(
    app: &CraftApp,
    shared: &Shared,
    paths: &Paths,
    platform: Option<Platform>,
    options: &BackendOptions,
) -> Option<(std::path::PathBuf, PackageKind, u64)> {
    let release = shared.latest.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned()?;
    let existing = shared.installed.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned();
    let record = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).apps.get(app.id).cloned();
    let (asset, kind) = planned_asset(app, &release, platform?, options, existing.as_ref(), record.as_ref())?;
    artcraft_launcher_core::release::valid_file_name(&asset.name).then(|| (install::download_path(paths, &asset), kind, asset.size))
}

/// What one check asks: which feed to poll (a local mirror trumps everything), and how much to
/// lean on GitHub.
fn check_plan(http: &Http, mirror: Option<&String>, pre: bool, paths: &Paths, shared: &Arc<Shared>) -> (Option<Feed>, Fallback) {
    if let Some(base) = mirror {
        // A local mirror answers for everything: GitHub is never asked.
        (local_feed(http, base), Fallback { trust_feed: true, github_every: GITHUB_FALLBACK_EVERY })
    } else if pre {
        // The feed serves stable releases only; with pre-releases on, GitHub decides.
        (None, Fallback { trust_feed: false, github_every: EARLY_GITHUB_EVERY })
    } else {
        feed_and_fallback(http, paths, shared)
    }
}

/// Answers the apps from a shared queue over a few workers, and returns which sources answered.
#[allow(clippy::too_many_arguments)]
fn sweep(
    queue: &Mutex<Vec<&'static CraftApp>>,
    mirror: &Option<String>,
    feed: Option<&Feed>,
    fallback: Fallback,
    source: &Arc<dyn ReleaseSource>,
    shared: &Arc<Shared>,
    paths: &Paths,
    (platform, options): (Option<Platform>, &BackendOptions),
    force: bool,
    now: i64,
) -> Vec<Source> {
    let sources = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| {
                loop {
                    let Some(app) = queue.lock().unwrap_or_else(PoisonError::into_inner).pop() else { break };
                    let track =
                        if app.id == LAUNCHER_ID { &options.launcher_version } else { options.versions.get(app.id).unwrap_or(&VersionTrack::Stable) };
                    let (latest, from, used) = match mirror {
                        Some(_) => mirror_chosen(app, feed, now, track),
                        None if *track != VersionTrack::Stable => {
                            let answer = source.chosen(&feed_repo(feed, app), track);
                            (Some(answer.latest), Origin::GitHub, Some(answer.source))
                        }
                        None => latest_for(app, feed, fallback, source.as_ref(), shared, options.prereleases, force, now),
                    };
                    if let Some(used) = used {
                        sources.lock().unwrap_or_else(PoisonError::into_inner).push(used);
                    }
                    check_app(app, latest, from, shared, paths, platform);
                }
            });
        }
    });
    sources.into_inner().unwrap_or_else(PoisonError::into_inner)
}

/// Publishes one app's answer to the shared state, or handles the launcher's own update check
/// first (it doesn't appear in the grid, so it has no card of its own to publish to).
fn check_app(app: &'static CraftApp, latest: Option<LatestRelease>, from: Origin, shared: &Arc<Shared>, paths: &Paths, platform: Option<Platform>) {
    if app.id == LAUNCHER_ID {
        launcher_update(paths, latest, shared);
        return;
    }
    let Some(latest) = latest else { return };
    if !matches!(latest, LatestRelease::Failed(_)) {
        shared.answered.lock().unwrap_or_else(PoisonError::into_inner).insert(app.id.to_string());
    }
    publish_latest(app, latest, from, shared, platform);
}

/// Offers the launcher's own update when its answer is a newer release than the running one.
fn launcher_update(paths: &Paths, latest: Option<LatestRelease>, shared: &Arc<Shared>) {
    let Some(LatestRelease::Found(r)) = latest else { return };
    let (Some(newest), Some(current)) = (r.version.as_ref(), crate::self_update::current_version()) else { return };
    if version::is_newer(newest, &current) && !crate::self_update::just_updated_to(paths, &r) {
        shared.self_install.get_or_init(crate::self_update::current);
        *shared.launcher_release.lock().unwrap_or_else(PoisonError::into_inner) = Some(r.clone());
        shared.emit(Event::LauncherUpdate(r));
    }
}

/// The weakest source that answered this check; it sets the wording of the "checked with" line.
fn source_used_of(sources: &[Source]) -> Source {
    if sources.contains(&Source::Cache) {
        Source::Cache
    } else if sources.contains(&Source::GitHubPages) {
        Source::GitHubPages
    } else if sources.contains(&Source::GitHubApi) {
        Source::GitHubApi
    } else {
        Source::Feed
    }
}

/// How much the feed is trusted in one check, and how freely GitHub may be asked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Fallback {
    /// Use the feed's answers (fresh entries only). Off when the feed has seriously failed.
    trust_feed: bool,
    /// Ask GitHub about an app at most this often (seconds), unless forced.
    github_every: i64,
}

/// Asks the feed (retrying, except on the very first poll) and decides how much to lean on
/// GitHub this time:
///
/// - The very first poll (nothing has ever loaded, nothing saved): one try, then GitHub at once,
///   so the window fills.
/// - Until every app has loaded once: if the feed doesn't answer after its retries, GitHub is
///   asked fairly freely (every 5 minutes per app).
/// - After that: the feed's last answer stands in while it fails, and GitHub is asked only for
///   apps the feed doesn't list or hasn't refreshed in a day, or once the feed has seriously
///   failed (several checks in a row and an hour without an answer); then hourly per app.
fn feed_and_fallback(http: &Http, paths: &Paths, shared: &Shared) -> (Option<Feed>, Fallback) {
    let saved = load_feed(paths);
    let loaded = shared.loaded.load(Ordering::SeqCst);
    let first_time = !loaded && saved.is_none();
    let attempts = if first_time { 1 } else { FEED_ATTEMPTS };
    let fetched = fetch_feed(http, paths, &feed_url(), &shared.feed_throttle, attempts);
    let now = crate::now();
    let (fresh, answered, failures) = {
        let mut failures = shared.feed_failures.lock().unwrap_or_else(PoisonError::into_inner);
        match fetched {
            FeedFetch::Answered(feed) => {
                *failures = 0;
                *shared.feed_ok_at.lock().unwrap_or_else(PoisonError::into_inner) = Some(now);
                (Some(feed), true, 0)
            }
            FeedFetch::Failed => {
                *failures = failures.saturating_add(1);
                (None, false, *failures)
            }
            // Not asked this time: things stand as the last request left them.
            FeedFetch::Held => (None, *failures == 0 && saved.is_some(), *failures),
        }
    };
    let ok_at = *shared.feed_ok_at.lock().unwrap_or_else(PoisonError::into_inner);
    let fallback = decide_fallback(answered, loaded, first_time, failures, ok_at, now);
    (fresh.or(saved), fallback)
}

/// The policy in [`feed_and_fallback`], on its own so it can be tested.
fn decide_fallback(answered: bool, loaded: bool, first_time: bool, failures: u32, ok_at: Option<i64>, now: i64) -> Fallback {
    let github_every = if loaded { GITHUB_FALLBACK_EVERY } else { EARLY_GITHUB_EVERY };
    let seriously_failed =
        !answered && (first_time || !loaded || (failures >= SERIOUS_FEED_FAILURES && ok_at.is_none_or(|t| now - t >= SERIOUS_FEED_OUTAGE)));
    Fallback { trust_feed: !seriously_failed, github_every }
}

/// The latest release of `app`, which service it came from, and the GitHub route used (for the
/// rate-limit notice): the feed when it's trusted and vouches for the app, else GitHub if it's
/// due, else GitHub's last answer. No release when there's nothing new to say.
#[allow(clippy::too_many_arguments)]
fn latest_for(
    app: &CraftApp,
    feed: Option<&Feed>,
    fallback: Fallback,
    source: &dyn ReleaseSource,
    shared: &Shared,
    pre: bool,
    force: bool,
    now: i64,
) -> (Option<LatestRelease>, Origin, Option<Source>) {
    let repo = feed_repo(feed, app);
    if fallback.trust_feed {
        match feed.map(|f| f.answer(app, now)) {
            // The feed can lag GitHub by some minutes after a release. A check the user pressed
            // looks past it when GitHub's quota-free redirect names something newer (#91).
            Some(FeedAnswer::Fresh(found)) if !(force && feed_is_behind(source, &repo, found.as_ref())) => {
                return (Some(found.map_or(LatestRelease::None, LatestRelease::Found)), Origin::ArtCraftApi, Some(Source::Feed));
            }
            Some(FeedAnswer::Fresh(_) | FeedAnswer::AskGitHub) | None => {}
        }
    }
    let due = {
        let mut asked = shared.github_asked.lock().unwrap_or_else(PoisonError::into_inner);
        let due = force || asked.get(app.id).is_none_or(|t| now - t >= fallback.github_every);
        if due {
            asked.insert(app.id.to_string(), now);
        }
        due
    };
    if due {
        let answer = source.latest(&repo, pre);
        (Some(answer.latest), Origin::GitHub, Some(answer.source))
    } else {
        (source.cached(&repo, pre), Origin::GitHub, None)
    }
}

/// Whether GitHub names a newer stable release of `repo` than the feed's `latest` (`None`: the
/// feed says there is none). `false` whenever the redirect can't be read: the feed then stands.
fn feed_is_behind(source: &dyn ReleaseSource, repo: &str, latest: Option<&Release>) -> bool {
    let Some(newest) = source.newest_tag(repo).and_then(|tag| version::parse(&tag)) else { return false };
    match latest.and_then(|r| r.version.as_ref()) {
        Some(feed) => version::is_newer(&newest, feed),
        None => true,
    }
}

/// Until when the footer says GitHub's limit holds details back: only while it does, that is
/// when this check had to settle for the release pages or saved answers. When the feed (or the
/// API itself) answered everything, nothing is missing, however recently GitHub said no.
fn limit_notice(source_used: Source, limited_until: Option<i64>) -> Option<i64> {
    limited_until.filter(|_| matches!(source_used, Source::GitHubPages | Source::Cache))
}

/// The repo to ask GitHub about, following the feed's repo renames.
fn feed_repo(feed: Option<&Feed>, app: &CraftApp) -> String {
    feed.map_or_else(|| app.repo.to_string(), |f| f.repo_for(app))
}

/// What came of asking the feed during one check.
enum FeedFetch {
    Answered(Feed),
    /// Asked, without a good answer.
    Failed,
    /// Not asked: the [`FeedThrottle`] said to hold off.
    Held,
}

/// Asks ArtCraft's release feed up to `attempts` times, as often as `throttle` allows (waiting at
/// most [`MAX_THROTTLE_WAIT`] for it), saving a good answer.
fn fetch_feed(http: &Http, paths: &Paths, url: &str, throttle: &Mutex<FeedThrottle>, attempts: usize) -> FeedFetch {
    let mut asked = false;
    for attempt in 0..attempts.max(1) {
        let wait = throttle.lock().unwrap_or_else(PoisonError::into_inner).wait(crate::now());
        if wait > MAX_THROTTLE_WAIT {
            if !asked {
                log::info!("holding off the ArtCraft API for {wait} s");
            }
            break;
        }
        if wait > 0 {
            std::thread::sleep(Duration::from_secs(wait.unsigned_abs()));
        }
        {
            let mut throttle = throttle.lock().unwrap_or_else(PoisonError::into_inner);
            let now = crate::now();
            if throttle.wait(now) > 0 {
                break;
            }
            throttle.sending(now);
        }
        asked = true;
        let outcome = match http.get_feed(url) {
            Ok(body) => match Feed::parse(&body) {
                Some(feed) => {
                    if let Err(e) = crate::paths::write_atomic(&paths.feed_cache(), body.as_bytes()) {
                        log::warn!("couldn't save the release feed: {e}");
                    }
                    throttle.lock().unwrap_or_else(PoisonError::into_inner).finished(Outcome::Answered, crate::now(), 0.0);
                    return FeedFetch::Answered(feed);
                }
                None => {
                    log::warn!("the release feed answered with something unreadable (try {})", attempt + 1);
                    Outcome::Failed { retry_after: None }
                }
            },
            Err(e) if e.throttled => {
                log::warn!("the ArtCraft API asked us to slow down (HTTP 429); holding off");
                Outcome::Throttled { retry_after: e.retry_after }
            }
            Err(e) => {
                log::info!("release feed unavailable (try {}): {}", attempt + 1, e.why);
                Outcome::Failed { retry_after: e.retry_after }
            }
        };
        throttle.lock().unwrap_or_else(PoisonError::into_inner).finished(outcome, crate::now(), feed_throttle::jitter());
        if matches!(outcome, Outcome::Throttled { .. }) {
            break;
        }
    }
    if asked { FeedFetch::Failed } else { FeedFetch::Held }
}

/// The feed's address. `ARTCRAFT_LAUNCHER_FEED_URL` points at a staging server or a local copy
/// (only `https://`, or plain `http://` on localhost).
fn feed_url() -> String {
    std::env::var("ARTCRAFT_LAUNCHER_FEED_URL")
        .ok()
        .filter(|u| feed_url_override_allowed(u, cfg!(feature = "local-mirror")))
        .unwrap_or_else(|| artcraft_launcher_core::feed::FEED_URL.to_string())
}

fn feed_url_override_allowed(url: &str, allow_https: bool) -> bool {
    is_loopback(url) || (allow_https && url.starts_with("https://"))
}

use crate::http::is_loopback;

/// `ARTCRAFT_LAUNCHER_DOWNLOAD_BASE`: a mirror on this machine (`cargo xtask local serve`) that
/// serves `release_info.json` and the builds it lists. While it's set, releases come only from
/// there (never GitHub or ArtCraft's API), and its download links are trusted. Loopback only, so
/// nothing but the developer's own builds can be installed this way, and only in builds with the
/// `local-mirror` feature: release builds ignore the variable.
fn local_mirror() -> Option<String> {
    if !cfg!(feature = "local-mirror") {
        return None;
    }
    let base = std::env::var("ARTCRAFT_LAUNCHER_DOWNLOAD_BASE").ok()?;
    let base = base.trim();
    if !is_loopback(base) || base.contains(['?', '#', '\\']) {
        log::warn!(
            "ignoring ARTCRAFT_LAUNCHER_DOWNLOAD_BASE={base}: only http://127.0.0.1:<port>/, http://localhost:<port>/ or http://[::1]:<port>/"
        );
        return None;
    }
    Some(if base.ends_with('/') { base.to_string() } else { format!("{base}/") })
}

/// The local mirror's feed: `ARTCRAFT_LAUNCHER_FEED_URL` if set, else `release_info.json` beside
/// the builds. Asked every check (it's on this machine), and never saved over ArtCraft's.
fn local_feed(http: &Http, base: &str) -> Option<Feed> {
    let url = std::env::var("ARTCRAFT_LAUNCHER_FEED_URL").ok().filter(|u| is_loopback(u)).unwrap_or_else(|| format!("{base}release_info.json"));
    match http.get_feed(&url) {
        Ok(body) => {
            let feed = Feed::parse(&body).map(|f| f.with_mirror(base));
            if feed.is_none() {
                log::warn!("the local mirror's feed at {url} is unreadable");
            }
            feed
        }
        Err(e) => {
            log::warn!("local mirror unavailable: {} (is `cargo xtask local serve` running?)", e.why);
            None
        }
    }
}

/// `app`'s latest release according to the local mirror. An app it doesn't list has no release.
fn mirror_latest(app: &CraftApp, feed: Option<&Feed>, now: i64) -> (Option<LatestRelease>, Origin, Option<Source>) {
    let latest = match feed.map(|f| f.answer(app, now)) {
        Some(FeedAnswer::Fresh(Some(r))) => LatestRelease::Found(r),
        Some(FeedAnswer::Fresh(None) | FeedAnswer::AskGitHub) => LatestRelease::None,
        None => LatestRelease::Failed("the local mirror didn't answer".into()),
    };
    (Some(latest), Origin::ArtCraftApi, Some(Source::Feed))
}

/// A selected track never silently falls back to the mirror's stable release.
fn mirror_chosen(app: &CraftApp, feed: Option<&Feed>, now: i64, track: &VersionTrack) -> (Option<LatestRelease>, Origin, Option<Source>) {
    let (answer, origin, source) = mirror_latest(app, feed, now);
    let chosen = match (track, answer) {
        (VersionTrack::Stable, answer) => answer,
        (VersionTrack::Nightly, Some(LatestRelease::Found(r))) if artcraft_launcher_core::release::is_nightly(&r) => Some(LatestRelease::Found(r)),
        (VersionTrack::Pinned(tag), Some(LatestRelease::Found(r))) if &r.tag == tag => Some(LatestRelease::Found(r)),
        (_, Some(LatestRelease::Failed(why))) => Some(LatestRelease::Failed(why)),
        _ => Some(LatestRelease::None),
    };
    (chosen, origin, source)
}

/// The last saved feed answer, if any.
fn load_feed(paths: &Paths) -> Option<Feed> {
    std::fs::read_to_string(paths.feed_cache()).ok().and_then(|body| Feed::parse(&body))
}

/// The machine's real architecture: an x64 launcher on Windows on ARM should still pick ARM64
/// builds.
fn native_platform() -> Option<Platform> {
    let mut p = Platform::current()?;
    if p.os == Os::Windows {
        let arch = std::env::var("PROCESSOR_ARCHITEW6432").or_else(|_| std::env::var("PROCESSOR_ARCHITECTURE")).unwrap_or_default();
        if arch.eq_ignore_ascii_case("ARM64") {
            p.arch = Arch::Arm64;
        } else if arch.eq_ignore_ascii_case("AMD64") {
            p.arch = Arch::X64;
        }
    }
    Some(p)
}

/// Clears an app's "pictures on their way" mark when the thread fetching them ends, panic or not.
struct PicturesBusy {
    shared: Arc<Shared>,
    app: &'static str,
}

impl Drop for PicturesBusy {
    fn drop(&mut self) {
        self.shared.pictures_busy.lock().unwrap_or_else(PoisonError::into_inner).remove(self.app);
    }
}

impl NativeBackend {
    /// Starts fetching the pictures of an app's details page, unless they are already on their way.
    /// An app without a page, or a name that isn't an app, is ignored.
    fn fetch_screenshots(&self, app: &str) {
        let Some(details) = artcraft_launcher_core::details::AppDetails::by_id(app) else { return };
        if !self.shared.pictures_busy.lock().unwrap_or_else(PoisonError::into_inner).insert(details.id) {
            return;
        }
        let (shared, http, paths) = (self.shared.clone(), self.http.clone(), self.paths.clone());
        std::thread::spawn(move || {
            let _busy = PicturesBusy { shared: shared.clone(), app: details.id };
            crate::screenshots::fetch_all(http.as_ref(), &paths, details, |key, result| {
                shared.emit(Event::Screenshot { app: details.id.to_string(), key: key.to_string(), result });
            });
        });
    }
}

impl Backend for NativeBackend {
    fn send(&mut self, command: Command) {
        match command {
            Command::CheckForUpdates { force } => self.check(force),
            Command::Detect => self.detect_all(),
            Command::ListVersions { app } => {
                let Some(def) = (if app == LAUNCHER_ID { Some(&LAUNCHER) } else { CraftApp::by_id(&app) }) else { return };
                let source = self.source.clone();
                let shared = self.shared.clone();
                let repo = feed_repo(load_feed(&self.paths).as_ref(), def);
                std::thread::spawn(move || {
                    let result = source.ok_or("secure connections aren't available on this system".to_string()).and_then(|s| s.versions(&repo));
                    shared.emit(Event::Versions { app, result });
                });
            }
            Command::Install { app } => {
                self.start_job(&app, Work::Install(None));
            }
            Command::InstallAll { apps } => self.install_all(&apps),
            Command::Replace { app, path } => {
                self.start_job(&app, Work::Replace(path));
            }
            Command::Uninstall { app } => {
                self.start_job(&app, Work::Uninstall);
            }
            Command::Pause { app } => {
                if let Some(stop) = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).get(&app) {
                    stop.pause();
                }
            }
            Command::Cancel { app } => {
                let running = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).get(&app).cloned();
                match running {
                    Some(stop) => stop.cancel(),
                    // A paused download, or one kept after a failed install: throw the file away.
                    None => {
                        if let Some(def) = CraftApp::by_id(&app)
                            && let Some((dest, _, size)) = planned_download(def, &self.shared, &self.paths, self.platform, &self.options())
                        {
                            http::discard(&dest, size);
                        }
                        self.shared.emit(Event::Paused { app: app.clone(), download: None });
                        self.shared.emit(Event::ReadyToInstall { app, ready: false });
                    }
                }
            }
            Command::Launch { app, args } => {
                let Some(def) = CraftApp::by_id(&app) else { return };
                let result = match self.installed(&app) {
                    Some(inst) => install::launch(def, &inst, &self.paths, &args),
                    None => Err(format!("{} isn't installed", def.name)),
                };
                if let Err(e) = result {
                    self.shared.emit(Event::JobFinished { app, result: Err(e) });
                }
            }
            Command::Reveal { app } => {
                if let Some(inst) = self.installed(&app) {
                    let path = std::path::PathBuf::from(&inst.location);
                    let result = if path.exists() { crate::process::reveal(&path) } else { Err("it isn't a file on this computer".into()) };
                    if let Err(e) = result {
                        self.shared.emit(Event::JobFinished { app, result: Err(format!("Couldn't show it: {e}")) });
                    }
                }
            }
            Command::UpdateLauncher => self.update_launcher(),
            Command::FetchScreenshots { app } => self.fetch_screenshots(&app),
            Command::Configure(options) => {
                let changed_pre = {
                    let mut o = self.shared.options.lock().unwrap_or_else(PoisonError::into_inner);
                    let changed =
                        o.prereleases != options.prereleases || o.versions != options.versions || o.launcher_version != options.launcher_version;
                    *o = options;
                    // An older worker must not overwrite the plan for these settings.
                    for cached in self.shared.download_sizes.lock().unwrap_or_else(PoisonError::into_inner).values_mut() {
                        cached.invalidate();
                    }
                    changed
                };
                // Every Configure, not only on a change: the first one carries the choices saved
                // before the shared file existed, and the file may have been deleted since.
                if let Some(path) = &self.pins {
                    match crate::pins::sync(path, &self.options().versions) {
                        Ok(true) => log::info!("version choices written to {}", path.display()),
                        Ok(false) => {}
                        Err(e) => log::warn!("couldn't write the version choices: {e}"),
                    }
                }
                // Another package format means another file to download.
                self.replan();
                if changed_pre {
                    self.check(true);
                }
            }
        }
    }

    fn poll(&mut self) -> Vec<Event> {
        std::mem::take(&mut *self.shared.events.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn launcher_kind(&self) -> Option<PackageKind> {
        // Looked up (it can run package-manager queries) with a newer launcher release; never here.
        self.shared.self_install.get().and_then(Option::as_ref).map(crate::self_update::kind)
    }

    fn capabilities(&self) -> Capabilities {
        let reveal_label = match self.platform.map(|p| p.os) {
            Some(Os::MacOs) => "Show in Finder",
            Some(Os::Windows) => "Show in Explorer",
            _ => "Show in Files",
        };
        Capabilities { manage_apps: true, reveal_label, self_update: self.shared.self_install.get().is_some_and(Option::is_some) }
    }

    fn platform(&self) -> Option<Platform> {
        self.platform
    }

    fn download_size(&self, app: &str) -> Option<(PackageKind, u64)> {
        self.shared.download_sizes.lock().unwrap_or_else(PoisonError::into_inner).get(app).and_then(|cached| cached.plan)
    }

    fn now(&self) -> i64 {
        crate::now()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    use artcraft_launcher_core::InstallSource;

    use artcraft_launcher_core::protocol::LatestRelease;

    use super::*;

    use crate::source::Answer;

    /// A member of a batch that ends before it ever reaches the installer — its download failed,
    /// or it turned out to need none of the batch's prompts — frees the rest of the batch at
    /// once, rather than holding them back until the batch's wait runs out.
    #[cfg(any(target_os = "linux", windows))]
    #[test]
    fn a_batch_member_that_never_reaches_the_installer_frees_the_rest() {
        let gather = install::INSTALLER.gather(1);
        {
            let _place = install::BatchPlace(Some(gather.clone()));
            assert!(!gather.open(), "the place is held while the job is on its way");
        }
        assert!(gather.open(), "dropping the place on the way out opens the gate");
    }

    fn tagged(v: &str) -> Release {
        Release { tag: format!("v{v}"), version: version::parse(v), ..Default::default() }
    }

    /// A "Replace" answer covers the path that was shown, never a destination that changed since.
    #[test]
    fn replacing_needs_consent_for_the_exact_path_in_the_way() {
        let shown = std::path::Path::new("/home/a/Applications/photocraft-0.5.0-linux-x86_64.AppImage");
        let moved = std::path::Path::new("/home/a/Applications/photocraft-0.6.0-linux-x86_64.AppImage");
        let approved = shown.display().to_string();
        assert_eq!(replace_consent(None, None), Consent::Free);
        assert_eq!(replace_consent(None, Some(&approved)), Consent::Free, "nothing in the way: nothing to overwrite");
        assert_eq!(replace_consent(Some(shown), None), Consent::Ask(shown));
        assert_eq!(replace_consent(Some(shown), Some(&approved)), Consent::Given);
        assert_eq!(replace_consent(Some(moved), Some(&approved)), Consent::Ask(moved), "a yes for one file must not overwrite another");
    }

    #[test]
    fn a_downgrade_is_only_a_version_chosen_by_name_below_the_installed_one() {
        let installed = Installed {
            version: version::parse("0.14.0"),
            source: InstallSource::MacApp,
            location: String::new(),
            package: None,
            externally_managed: false,
            origin: None,
            installation: None,
        };
        let pinned = VersionTrack::Pinned("v0.13.0".into());
        assert!(is_chosen_downgrade(&pinned, &tagged("0.13.0"), Some(&installed)));
        assert!(!is_chosen_downgrade(&VersionTrack::Stable, &tagged("0.13.0"), Some(&installed)));
        assert!(!is_chosen_downgrade(&VersionTrack::Pinned("v0.15.0".into()), &tagged("0.15.0"), Some(&installed)));
        assert!(!is_chosen_downgrade(&pinned, &tagged("0.12.0"), Some(&installed)));
        assert!(!is_chosen_downgrade(&pinned, &tagged("0.13.0"), None));
    }

    #[test]
    fn only_a_version_chosen_by_name_below_the_installed_one_looks_for_a_package_to_put_back() {
        let app = CraftApp::by_id("filmcraft").unwrap();
        let installed = Installed {
            version: version::parse("0.14.0"),
            source: InstallSource::Msi,
            location: String::new(),
            package: None,
            externally_managed: false,
            origin: None,
            installation: None,
        };
        let pinned = VersionTrack::Pinned("v0.13.0".into());
        let never = || -> Result<Vec<Release>, String> { panic!("no list is needed") };
        // An update, the latest release, nothing installed, or a stale release: no downgrade.
        assert_eq!(downgrade_previous(app, &VersionTrack::Stable, &tagged("0.13.0"), Some(&installed), never), Ok(None));
        assert_eq!(downgrade_previous(app, &VersionTrack::Pinned("v0.15.0".into()), &tagged("0.15.0"), Some(&installed), never), Ok(None));
        assert_eq!(downgrade_previous(app, &pinned, &tagged("0.13.0"), None, never), Ok(None));
        assert_eq!(downgrade_previous(app, &pinned, &tagged("0.12.0"), Some(&installed), never), Ok(None));
        // Going back finds the installed version's own release.
        let found = downgrade_previous(app, &pinned, &tagged("0.13.0"), Some(&installed), || Ok(vec![tagged("0.14.0"), tagged("0.13.0")]));
        assert_eq!(found.unwrap().map(|r| r.tag), Some("v0.14.0".to_string()));
        // Without it, or without the list, the downgrade doesn't start.
        let gone = downgrade_previous(app, &pinned, &tagged("0.13.0"), Some(&installed), || Ok(vec![tagged("0.13.0")])).unwrap_err();
        assert!(gone.contains("FilmCraft 0.14.0 isn't published any more") && gone.contains("Nothing was changed"), "{gone}");
        let offline = downgrade_previous(app, &pinned, &tagged("0.13.0"), Some(&installed), || Err("offline".into())).unwrap_err();
        assert!(offline.contains("(offline)") && offline.contains("nothing was changed"), "{offline}");
    }

    /// Going back is a conscious choice: a check for a pinned app reports the pinned release, so
    /// a newer one never shows as an update (badge, Update all, notification or automatic install).
    #[test]
    fn a_pinned_app_is_never_offered_a_newer_release() {
        struct Published;
        impl ReleaseSource for Published {
            fn latest(&self, repo: &str, _prereleases: bool) -> crate::source::Answer {
                let found = CraftApp::by_id(APP).is_some_and(|a| a.repo == repo);
                let latest = if found { LatestRelease::Found(tagged("0.6.0")) } else { LatestRelease::None };
                crate::source::Answer { latest, source: Source::GitHubApi }
            }
            fn versions(&self, _repo: &str) -> Result<Vec<Release>, String> {
                Ok(vec![tagged("0.6.0"), tagged("0.5.0")])
            }
            fn cached(&self, _repo: &str, _prereleases: bool) -> Option<LatestRelease> {
                None
            }
            fn rate_limited_until(&self) -> Option<i64> {
                None
            }
            fn save(&self) {}
        }
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, Some(Arc::new(Published)));
        backend.http = Some(Http::new().unwrap());
        {
            let mut options = backend.shared.options.lock().unwrap();
            options.prereleases = true; // asks the source directly, never ArtCraft's API
            options.versions.insert(APP.to_string(), VersionTrack::Pinned("v0.5.0".into()));
        }
        backend.send(Command::CheckForUpdates { force: true });
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !events.iter().any(|e| matches!(e, Event::CheckFinished { .. })) {
            assert!(Instant::now() < deadline, "the check never finished");
            std::thread::sleep(Duration::from_millis(10));
            events.extend(backend.poll());
        }
        let answered = events.iter().rev().find_map(|e| match e {
            Event::Latest { app, latest: LatestRelease::Found(r), .. } if app == APP => Some(r.tag.clone()),
            _ => None,
        });
        assert_eq!(answered.as_deref(), Some("v0.5.0"), "the pinned release, not the newer one");
    }

    #[test]
    fn an_install_cannot_use_a_stale_release_from_a_different_track() {
        let mut release = Release { tag: "v0.8.0".into(), version: version::parse("v0.8.0"), ..Default::default() };
        assert!(!release_matches_track(&release, &VersionTrack::Nightly, false));
        assert!(!release_matches_track(&release, &VersionTrack::Pinned("v0.7.0".into()), false));
        assert!(release_matches_track(&release, &VersionTrack::Stable, false));
        release.tag = "v0.9.0-nightly.20261028".into();
        release.version = version::parse(&release.tag);
        release.prerelease = true;
        assert!(release_matches_track(&release, &VersionTrack::Nightly, false));
        assert!(!release_matches_track(&release, &VersionTrack::Stable, false));
    }

    #[test]
    fn feed_url_overrides_are_loopback_only_outside_local_mirror_builds() {
        assert!(feed_url_override_allowed("http://127.0.0.1:8787/release_info.json", false));
        assert!(feed_url_override_allowed("http://localhost:8787/release_info.json", false));
        assert!(!feed_url_override_allowed("https://staging.example/release_info.json", false));
        assert!(feed_url_override_allowed("https://staging.example/release_info.json", true));
        assert!(!feed_url_override_allowed("http://attacker.example/release_info.json", true));
    }

    /// GitHub, counting how often it's asked.
    #[derive(Default)]
    struct CountingGitHub(AtomicUsize);

    impl ReleaseSource for CountingGitHub {
        fn latest(&self, _repo: &str, _pre: bool) -> Answer {
            self.0.fetch_add(1, Ordering::SeqCst);
            Answer { latest: LatestRelease::None, source: Source::GitHubApi }
        }
        fn cached(&self, _repo: &str, _pre: bool) -> Option<LatestRelease> {
            None
        }
        fn rate_limited_until(&self) -> Option<i64> {
            None
        }
        fn save(&self) {}
    }

    fn shared() -> Shared {
        Shared {
            events: Mutex::new(Vec::new()),
            reporting: Mutex::new(()),
            system: Mutex::new(None),
            planning: Mutex::new(()),
            waker: Arc::new(|| {}),
            latest: Mutex::new(HashMap::new()),
            download_sizes: Mutex::new(HashMap::new()),
            installed: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            restarting: AtomicBool::new(false),
            manifest: Mutex::new(Manifest::default()),
            options: Mutex::new(BackendOptions::default()),
            checking: AtomicBool::new(false),
            detecting: AtomicBool::new(false),
            last_check: Mutex::new(None),
            launcher_release: Mutex::new(None),
            self_install: OnceLock::new(),
            github_asked: Mutex::new(HashMap::new()),
            answered: Mutex::new(std::collections::HashSet::new()),
            loaded: AtomicBool::new(false),
            feed_failures: Mutex::new(0),
            feed_ok_at: Mutex::new(None),
            feed_throttle: Mutex::new(FeedThrottle::new()),
            pictures_busy: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// A local server answering every request with `status` (and `Retry-After: 120` on a 429),
    /// counting the requests.
    fn serve(status: &'static str) -> (String, Arc<AtomicUsize>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/craft_apps/release_info", listener.local_addr().unwrap());
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 4096];
                let _ = stream.read(&mut buf);
                counted.fetch_add(1, Ordering::SeqCst);
                let extra = if status.starts_with("429") { "Retry-After: 120\r\n" } else { "" };
                let _ = write!(stream, "HTTP/1.1 {status}\r\n{extra}Content-Length: 0\r\nConnection: close\r\n\r\n");
            }
        });
        (url, hits)
    }

    #[test]
    fn a_429_stops_the_retries_and_holds_off_the_api() {
        let (url, hits) = serve("429 Too Many Requests");
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let http = Http::new().unwrap();
        let throttle = Mutex::new(FeedThrottle::load(&paths.feed_throttle(), crate::now()));
        assert!(matches!(fetch_feed(&http, &paths, &url, &throttle, FEED_ATTEMPTS), FeedFetch::Failed));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        // The next checks (and a relaunch) don't call it at all.
        assert!(matches!(fetch_feed(&http, &paths, &url, &throttle, FEED_ATTEMPTS), FeedFetch::Held));
        let relaunched = Mutex::new(FeedThrottle::load(&paths.feed_throttle(), crate::now()));
        assert!(matches!(fetch_feed(&http, &paths, &url, &relaunched, FEED_ATTEMPTS), FeedFetch::Held));
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_failing_api_is_retried_quickly_only_during_the_first_load() {
        let (url, hits) = serve("503 Service Unavailable");
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let http = Http::new().unwrap();
        let throttle = Mutex::new(FeedThrottle::new());
        // First load: three tries, a couple of seconds apart.
        assert!(matches!(fetch_feed(&http, &paths, &url, &throttle, FEED_ATTEMPTS), FeedFetch::Failed));
        assert_eq!(hits.load(Ordering::SeqCst), 3);
        // Once the server has answered, never more than one a minute.
        throttle.lock().unwrap().finished(Outcome::Answered, crate::now(), 0.0);
        assert!(matches!(fetch_feed(&http, &paths, &url, &throttle, FEED_ATTEMPTS), FeedFetch::Held));
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    const NOW: i64 = 1_791_500_000;
    /// The steady state: everything has loaded and the feed is answering.
    const LOADED: Fallback = Fallback { trust_feed: true, github_every: GITHUB_FALLBACK_EVERY };

    fn feed() -> Feed {
        Feed::parse(
            r#"{"release_info": {"apps": {
            "photocraft": {"repo": "storytold/photocraft", "latest_release": {"tag": "v0.6.0"}, "app_last_fetched": "2026-10-09T13:00:00Z"}
        }}}"#,
        )
        .unwrap()
    }

    #[test]
    fn the_limit_notice_shows_only_while_details_are_missing() {
        let until = Some(NOW + 31 * 60);
        // GitHub said no earlier, but the feed has since answered every app: nothing to warn about.
        assert_eq!(limit_notice(Source::Feed, until), None);
        assert_eq!(limit_notice(Source::GitHubApi, until), None);
        // Some apps came from the release pages or saved answers: say when the rest returns.
        assert_eq!(limit_notice(Source::GitHubPages, until), until);
        assert_eq!(limit_notice(Source::Cache, until), until);
        assert_eq!(limit_notice(Source::Cache, None), None);
    }

    #[test]
    fn the_first_poll_tries_once_and_falls_back_at_once() {
        // Nothing has ever loaded and the feed didn't answer: GitHub, freely.
        let f = decide_fallback(false, false, true, 1, None, NOW);
        assert_eq!(f, Fallback { trust_feed: false, github_every: EARLY_GITHUB_EVERY });
        // The feed answered: trust it.
        assert!(decide_fallback(true, false, true, 0, Some(NOW), NOW).trust_feed);
    }

    #[test]
    fn before_everything_loads_github_is_asked_more_eagerly() {
        // A saved feed but not everything loaded yet, and the feed isn't answering.
        let f = decide_fallback(false, false, false, 1, None, NOW);
        assert_eq!(f, Fallback { trust_feed: false, github_every: EARLY_GITHUB_EVERY });
    }

    #[test]
    fn after_loading_github_waits_for_a_serious_feed_failure() {
        // A blip or two: keep showing the feed's last answer.
        for failures in 1..SERIOUS_FEED_FAILURES {
            let f = decide_fallback(false, true, false, failures, Some(NOW - 10 * 60), NOW);
            assert_eq!(f, Fallback { trust_feed: true, github_every: GITHUB_FALLBACK_EVERY }, "{failures}");
        }
        // Several checks in a row, but it answered recently: still trusted.
        assert!(decide_fallback(false, true, false, SERIOUS_FEED_FAILURES, Some(NOW - 20 * 60), NOW).trust_feed);
        // Several checks in a row and an hour without an answer: GitHub takes over, hourly.
        let f = decide_fallback(false, true, false, SERIOUS_FEED_FAILURES, Some(NOW - SERIOUS_FEED_OUTAGE), NOW);
        assert_eq!(f, Fallback { trust_feed: false, github_every: GITHUB_FALLBACK_EVERY });
    }

    #[test]
    fn a_local_mirror_answers_for_every_app_on_its_own() {
        let photo = CraftApp::by_id("photocraft").unwrap();
        let word = CraftApp::by_id("wordcraft").unwrap();
        let feed = feed().with_mirror("http://127.0.0.1:8787/");
        // Listed: found, even long after the sync.
        let (latest, _, _) = mirror_latest(photo, Some(&feed), NOW + 30 * 86_400);
        assert!(matches!(latest, Some(LatestRelease::Found(ref r)) if r.tag == "v0.6.0"));
        // Not listed: no release (rather than asking GitHub).
        assert!(matches!(mirror_latest(word, Some(&feed), NOW).0, Some(LatestRelease::None)));
        // Not running: says so.
        assert!(matches!(mirror_latest(photo, None, NOW).0, Some(LatestRelease::Failed(_))));
        assert!(is_loopback("http://localhost:8787/") && is_loopback("http://[::1]:8787/"));
        assert!(!is_loopback("http://192.168.1.2:8787/") && !is_loopback("http://localhost.evil.example/"));
        assert!(!is_loopback("http://127.0.0.1:1@evil.example/") && !is_loopback("http://127.0.0.1:8787@evil.example:80/"));
        assert!(!is_loopback("http://127.0.0.1/") && !is_loopback("http://127.0.0.1:x/") && !is_loopback("https://127.0.0.1:8787/"));
        assert!(is_loopback("http://127.0.0.1:8787") && is_loopback("http://127.0.0.1:8787/release_info.json"));
    }

    #[test]
    fn github_is_only_the_fallback_and_is_asked_rarely() {
        let github = CountingGitHub::default();
        let shared = shared();
        let photo = CraftApp::by_id("photocraft").unwrap();
        let word = CraftApp::by_id("wordcraft").unwrap();
        let feed = feed();

        // The feed vouches for PhotoCraft: GitHub isn't asked.
        let (latest, _, used) = latest_for(photo, Some(&feed), LOADED, &github, &shared, false, false, NOW);
        assert!(matches!(latest, Some(LatestRelease::Found(ref r)) if r.tag == "v0.6.0"));
        assert_eq!(used, Some(Source::Feed));
        assert_eq!(github.0.load(Ordering::SeqCst), 0);

        // The feed doesn't list WordCraft: GitHub is asked once…
        let (_, _, used) = latest_for(word, Some(&feed), LOADED, &github, &shared, false, false, NOW);
        assert_eq!(used, Some(Source::GitHubApi));
        assert_eq!(github.0.load(Ordering::SeqCst), 1);
        // …not again within the hour…
        let (_, _, used) = latest_for(word, Some(&feed), LOADED, &github, &shared, false, false, NOW + 30 * 60);
        assert_eq!(used, None);
        assert_eq!(github.0.load(Ordering::SeqCst), 1);
        // …but Refresh asks right away, and an hour later it's due again.
        latest_for(word, Some(&feed), LOADED, &github, &shared, false, true, NOW + 31 * 60);
        assert_eq!(github.0.load(Ordering::SeqCst), 2);
        latest_for(word, Some(&feed), LOADED, &github, &shared, false, false, NOW + 31 * 60 + GITHUB_FALLBACK_EVERY);
        assert_eq!(github.0.load(Ordering::SeqCst), 3);

        // No feed answer at all (server down): GitHub, still rate-limited per app.
        let (_, _, used) = latest_for(photo, None, LOADED, &github, &shared, false, false, NOW);
        assert_eq!(used, Some(Source::GitHubApi));
        assert_eq!(github.0.load(Ordering::SeqCst), 4);

        // A feed entry more than a day old doesn't count.
        let fresh_shared = super::tests::shared();
        let (_, _, used) = latest_for(photo, Some(&feed), LOADED, &github, &fresh_shared, false, false, NOW + 2 * 86_400);
        assert_eq!(used, Some(Source::GitHubApi));
    }

    /// Refresh looks past a feed that is behind GitHub (#91).
    #[test]
    fn refresh_asks_github_when_its_redirect_names_a_newer_release_than_the_feed() {
        /// GitHub, whose quota-free redirect names `newest`.
        struct Ahead {
            newest: Option<&'static str>,
            asked: AtomicUsize,
        }
        impl ReleaseSource for Ahead {
            fn latest(&self, _repo: &str, _pre: bool) -> Answer {
                self.asked.fetch_add(1, Ordering::SeqCst);
                let found = Release { tag: "v0.7.0".into(), version: version::parse("0.7.0"), ..Release::default() };
                Answer { latest: LatestRelease::Found(found), source: Source::GitHubApi }
            }
            fn cached(&self, _repo: &str, _pre: bool) -> Option<LatestRelease> {
                None
            }
            fn rate_limited_until(&self) -> Option<i64> {
                None
            }
            fn save(&self) {}
            fn newest_tag(&self, _repo: &str) -> Option<String> {
                self.newest.map(str::to_string)
            }
        }
        let photo = CraftApp::by_id("photocraft").unwrap();
        let feed = feed(); // vouches for PhotoCraft v0.6.0
        let github = |newest| Ahead { newest, asked: AtomicUsize::new(0) };

        // GitHub names the release the feed has: Refresh takes the feed's answer, asking nothing more.
        let same = github(Some("v0.6.0"));
        let (latest, _, used) = latest_for(photo, Some(&feed), LOADED, &same, &shared(), false, true, NOW);
        assert!(matches!(latest, Some(LatestRelease::Found(ref r)) if r.tag == "v0.6.0"));
        assert_eq!((used, same.asked.load(Ordering::SeqCst)), (Some(Source::Feed), 0));

        // GitHub names a newer one: Refresh asks GitHub for it.
        let ahead = github(Some("v0.7.0"));
        let (latest, _, used) = latest_for(photo, Some(&feed), LOADED, &ahead, &shared(), false, true, NOW);
        assert!(matches!(latest, Some(LatestRelease::Found(ref r)) if r.tag == "v0.7.0"), "{latest:?}");
        assert_eq!((used, ahead.asked.load(Ordering::SeqCst)), (Some(Source::GitHubApi), 1));

        // An automatic check never probes: the feed stands, as does a Refresh when the redirect
        // can't be read, or names something older.
        let (_, _, used) = latest_for(photo, Some(&feed), LOADED, &ahead, &shared(), false, false, NOW);
        assert_eq!((used, ahead.asked.load(Ordering::SeqCst)), (Some(Source::Feed), 1));
        for newest in [None, Some("v0.5.9"), Some("not a version")] {
            let github = github(newest);
            let (_, _, used) = latest_for(photo, Some(&feed), LOADED, &github, &shared(), false, true, NOW);
            assert_eq!((used, github.asked.load(Ordering::SeqCst)), (Some(Source::Feed), 0), "{newest:?}");
        }
    }

    const APP: &str = "photocraft";

    /// A PhotoCraft release with one file per platform and format, so whichever this machine
    /// installs is there.
    fn release(version: &str) -> Release {
        let suffixes = ["-portable.zip", ".tar.gz", ".msi", ".dmg", ".AppImage", ".deb", ".rpm", ".flatpak"];
        let tokens = [
            "windows-x64",
            "windows-x86",
            "windows-arm64",
            "macos-universal",
            "linux-x86_64",
            "linux-aarch64",
            "linux-riscv64",
            "freebsd-x86_64",
            "freebsd-aarch64",
        ];
        let assets = tokens
            .iter()
            .flat_map(|t| suffixes.iter().map(move |s| format!("{APP}-{version}-{t}{s}")))
            .map(|name| Asset { url: format!("https://example.invalid/{name}"), name, size: 3, sha256: None })
            .collect();
        Release { assets, ..Release::default() }
    }

    #[cfg(windows)]
    #[test]
    fn windows_preference_selects_portable_and_existing_installs_keep_their_format() {
        let app = CraftApp::by_id(APP).unwrap();
        let platform = Platform { os: Os::Windows, arch: Arch::X64 };
        let release = release("0.5.0");
        let portable_options = BackendOptions { kinds: vec![PackageKind::PortableZip], ..Default::default() };
        assert_eq!(planned_asset(app, &release, platform, &portable_options, None, None).unwrap().1, PackageKind::PortableZip);
        let msi = Installed {
            version: None,
            source: artcraft_launcher_core::InstallSource::Msi,
            location: r"C:\Apps\PhotoCraft\photocraft.exe".into(),
            package: None,
            externally_managed: false,
            origin: None,
            installation: None,
        };
        assert_eq!(planned_asset(app, &release, platform, &portable_options, Some(&msi), None).unwrap().1, PackageKind::Msi);
        let launcher = Installed {
            version: None,
            source: artcraft_launcher_core::InstallSource::Launcher,
            location: r"D:\Craft Apps\PhotoCraft\photocraft.exe".into(),
            package: None,
            externally_managed: false,
            origin: None,
            installation: None,
        };
        let record = crate::manifest::Record {
            version: "0.4.0".into(),
            kind: PackageKind::PortableZip,
            path: launcher.location.clone().into(),
            installed_at: 1,
            files: vec![],
            needs_root: false,
        };
        let msi_options = BackendOptions { kinds: vec![PackageKind::Msi], ..Default::default() };
        assert_eq!(planned_asset(app, &release, platform, &msi_options, Some(&launcher), Some(&record)).unwrap().1, PackageKind::PortableZip);
        assert_eq!(planned_asset(app, &release, platform, &msi_options, None, Some(&record)).unwrap().1, PackageKind::PortableZip);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_custom_linux_folder_requires_appimage_without_package_fallback() {
        if process::in_sandbox() {
            return;
        }
        let app = CraftApp::by_id(APP).unwrap();
        let platform = Platform { os: Os::Linux, arch: Arch::X64 };
        let options = BackendOptions {
            kinds: vec![PackageKind::Deb],
            install_locations: artcraft_launcher_core::protocol::InstallLocations { root: Some("/custom/apps".into()), ..Default::default() },
            ..Default::default()
        };
        let mut release = release("0.5.0");
        assert_eq!(planned_asset(app, &release, platform, &options, None, None).unwrap().1, PackageKind::AppImage);
        release.assets.retain(|a| !a.name.ends_with(".AppImage"));
        assert!(planned_asset(app, &release, platform, &options, None, None).is_none());
        let existing = Installed {
            version: None,
            source: artcraft_launcher_core::InstallSource::Deb,
            location: "/usr/bin/photocraft".into(),
            package: None,
            externally_managed: false,
            origin: None,
            installation: None,
        };
        let system = BackendOptions { kinds: options.kinds.clone(), ..Default::default() };
        assert_eq!(
            planned_asset(app, &release, platform, &options, Some(&existing), None),
            planned_asset(app, &release, platform, &system, Some(&existing), None)
        );
    }

    /// A backend that knows `release("0.5.0")` and keeps its files under `root`.
    fn backend(root: &std::path::Path, platform: Platform, source: Option<Arc<dyn ReleaseSource>>) -> NativeBackend {
        let shared = shared();
        shared.latest.lock().unwrap().insert(APP.to_string(), release("0.5.0"));
        let shared = Arc::new(shared);
        NativeBackend { shared, paths: Paths::under(root), http: None, source, platform: Some(platform), pins: Some(root.join("versions.json")) }
    }

    /// What the installability answers in `events` said about [`APP`], and which `Latest` and
    /// `Detected` came with them, in order.
    fn told(events: Vec<Event>) -> Vec<String> {
        events
            .into_iter()
            .filter_map(|e| match e {
                Event::Installable { app, installable } if app == APP => Some(format!("installable {installable}")),
                Event::Latest { app, .. } if app == APP => Some("latest".to_string()),
                Event::Detected { app, .. } if app == APP => Some("detected".to_string()),
                _ => None,
            })
            .collect()
    }

    /// A copy of [`APP`] at `version`, installed the way `source` says.
    fn copy_of(version: &str, source: InstallSource, location: &str) -> Installed {
        Installed {
            version: artcraft_launcher_core::version::parse(version),
            source,
            location: location.into(),
            package: None,
            externally_managed: false,
            origin: None,
            installation: None,
        }
    }

    /// `release(v)` with its version known, so it is on the stable track.
    fn stable(v: &str) -> Release {
        Release { version: artcraft_launcher_core::version::parse(v), ..release(v) }
    }

    /// Makes the backend's view of what this system can install `available` (as if just asked),
    /// so an answer doesn't depend on the computer the test runs on.
    fn system_installs(backend: &NativeBackend, available: Vec<PackageKind>) {
        *backend.shared.system.lock().unwrap() = Some((Instant::now(), install::SystemKinds { available, can_pacman: false }));
    }

    /// Each answer tells the UI whether an install would go ahead before it says the release is
    /// out or what is installed, in the same poll, so the UI never offers (or starts) an update it
    /// can't install. A release with nothing for this computer is not installable; one with a
    /// file is. Nothing is said for an app that isn't installed (the UI doesn't use it there), so
    /// nothing is asked of the system for it either, as at startup before detection.
    #[test]
    fn installability_is_told_with_the_release_or_copy_it_depends_on() {
        let app = CraftApp::by_id(APP).unwrap();
        let platform = Platform { os: Os::MacOs, arch: Arch::Arm64 };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        system_installs(&backend, vec![PackageKind::Dmg]);

        publish_latest(app, LatestRelease::Found(stable("0.5.0")), Origin::GitHub, &backend.shared, Some(platform));
        assert_eq!(told(backend.poll()), ["latest"], "nothing installed: no answer");

        publish_detected(app, Some(copy_of("0.4.0", InstallSource::Launcher, "/Applications/PhotoCraft.app")), &backend.shared, Some(platform));
        assert_eq!(told(backend.poll()), ["installable true", "detected"]);

        let mut windows_only = stable("0.6.0");
        windows_only.assets.retain(|a| a.name.contains("windows"));
        check_app(app, Some(LatestRelease::Found(windows_only)), Origin::GitHub, &backend.shared, &backend.paths, Some(platform));
        assert_eq!(told(backend.poll()), ["installable false", "latest"]);
        check_app(app, Some(LatestRelease::Found(stable("0.6.0"))), Origin::GitHub, &backend.shared, &backend.paths, Some(platform));
        assert_eq!(told(backend.poll()), ["installable true", "latest"]);

        publish_detected(app, None, &backend.shared, Some(platform));
        assert_eq!(told(backend.poll()), ["detected"], "removed: no answer");
    }

    /// A report made when only the plan changed (a settings change, a check or an install ending)
    /// still tells the UI whether an install would go ahead, on its own, with the settings as
    /// they are then.
    #[test]
    fn a_plan_report_answers_installability_with_the_current_settings() {
        let app = CraftApp::by_id(APP).unwrap();
        let platform = Platform { os: Os::MacOs, arch: Arch::Arm64 };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        system_installs(&backend, vec![PackageKind::Dmg]);
        backend.shared.latest.lock().unwrap().insert(APP.to_string(), stable("0.6.0"));
        backend.shared.installed.lock().unwrap().insert(APP.to_string(), copy_of("0.4.0", InstallSource::Launcher, "/Applications/PhotoCraft.app"));
        let installable = |events: Vec<Event>| told(events).into_iter().filter(|t| t.starts_with("installable")).collect::<Vec<_>>();

        report_partial(app, &backend.shared, &backend.paths, Some(platform));
        assert_eq!(installable(backend.poll()), ["installable true"]);
        backend.shared.options.lock().unwrap().versions.insert(APP.to_string(), VersionTrack::Pinned("v0.7.0".into()));
        report_partial(app, &backend.shared, &backend.paths, Some(platform));
        assert_eq!(installable(backend.poll()), ["installable false"], "kept on another version: not what an install would take");
    }

    /// When a copy appears between a publisher looking and taking `reporting`, the answer uses
    /// the view of the system its detection asked for, however old: nothing is probed while
    /// `reporting` is held.
    #[test]
    fn an_answer_under_the_reporting_lock_never_probes_the_system() {
        let app = CraftApp::by_id(APP).unwrap();
        let platform = Platform { os: Os::MacOs, arch: Arch::Arm64 };
        let dir = tempfile::tempdir().unwrap();
        let backend = backend(dir.path(), platform, None);
        let asked = Instant::now().checked_sub(SYSTEM_KINDS_FOR * 2).unwrap_or_else(Instant::now);
        *backend.shared.system.lock().unwrap() = Some((asked, install::SystemKinds { available: vec![PackageKind::Dmg], can_pacman: false }));
        backend.shared.latest.lock().unwrap().insert(APP.to_string(), stable("0.6.0"));
        backend.shared.installed.lock().unwrap().insert(APP.to_string(), copy_of("0.4.0", InstallSource::Launcher, "/Applications/PhotoCraft.app"));

        let answer = {
            let _reporting = backend.shared.reporting.lock().unwrap();
            installable_event(app, &backend.shared, Some(platform), None)
        };
        assert_eq!(answer, Some(Event::Installable { app: APP.to_string(), installable: true }), "from the view it had");
        let kept = backend.shared.system.lock().unwrap().as_ref().map(|(at, _)| *at);
        assert_eq!(kept, Some(asked), "the expired view wasn't asked again under the lock");
    }

    /// However polls and publications interleave, a poll never holds a `Latest` or `Detected`
    /// without the answer queued with it, just before it.
    #[test]
    fn a_poll_never_splits_an_answer_from_its_release_or_copy() {
        let app = CraftApp::by_id(APP).unwrap();
        let platform = Platform { os: Os::MacOs, arch: Arch::Arm64 };
        let dir = tempfile::tempdir().unwrap();
        let backend = backend(dir.path(), platform, None);
        system_installs(&backend, vec![PackageKind::Dmg]);
        let installed = copy_of("0.4.0", InstallSource::Launcher, "/Applications/PhotoCraft.app");
        publish_detected(app, Some(installed.clone()), &backend.shared, Some(platform));
        let shared = backend.shared.clone();
        let publisher = std::thread::spawn(move || {
            for i in 0..300 {
                if i % 2 == 0 {
                    publish_latest(app, LatestRelease::Found(stable("0.6.0")), Origin::GitHub, &shared, Some(platform));
                } else {
                    publish_detected(app, Some(installed.clone()), &shared, Some(platform));
                }
            }
        });
        let mut seen = 0;
        while !publisher.is_finished() || !backend.shared.events.lock().unwrap().is_empty() {
            let events = std::mem::take(&mut *backend.shared.events.lock().unwrap());
            for (i, e) in events.iter().enumerate() {
                if matches!(e, Event::Latest { app, .. } | Event::Detected { app, .. } if app == APP) {
                    seen += 1;
                    let before = i.checked_sub(1).and_then(|j| events.get(j));
                    assert!(matches!(before, Some(Event::Installable { app, .. }) if app == APP), "a poll split a pair: {events:?}");
                }
            }
        }
        publisher.join().unwrap();
        assert_eq!(seen, 301, "every publication was seen");
    }

    /// The answer uses the settings as they are when it is worked out, not as a long job found
    /// them when it started: a job that ends after the version track changed answers for the new
    /// track.
    #[test]
    fn installability_follows_the_current_settings() {
        let app = CraftApp::by_id(APP).unwrap();
        let platform = Platform { os: Os::MacOs, arch: Arch::Arm64 };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        system_installs(&backend, vec![PackageKind::Dmg]);
        backend.shared.latest.lock().unwrap().insert(APP.to_string(), stable("0.6.0"));
        let installed = copy_of("0.4.0", InstallSource::Launcher, "/Applications/PhotoCraft.app");

        publish_detected(app, Some(installed.clone()), &backend.shared, Some(platform));
        assert_eq!(told(backend.poll()), ["installable true", "detected"]);

        // Kept on another version now: 0.6.0 isn't what an install would take.
        backend.shared.options.lock().unwrap().versions.insert(APP.to_string(), VersionTrack::Pinned("v0.7.0".into()));
        publish_detected(app, Some(installed), &backend.shared, Some(platform));
        assert_eq!(told(backend.poll()), ["installable false", "detected"]);
    }

    /// A Flatpak its repository updates is updated with `flatpak update` (Linux, stable track), as
    /// an install does, so the answer says yes there and keeps Reinstall, the launcher's route to
    /// that update. Kept on another version it says no, as the install refuses.
    #[test]
    fn a_repository_flatpak_is_installable_where_flatpak_update_runs() {
        let app = CraftApp::by_id(APP).unwrap();
        let platform = Platform { os: Os::Linux, arch: Arch::X64 };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        // Nothing this system could download: only `flatpak update` can say yes.
        system_installs(&backend, Vec::new());
        backend.shared.latest.lock().unwrap().insert(APP.to_string(), stable("0.5.0"));
        let flatpark = Installed { externally_managed: true, ..copy_of("0.5.0", InstallSource::Flatpak, "ai.storyteller.photocraft") };
        let options = BackendOptions::default();
        assert_eq!(repository_update(app, Some(&flatpark), &options).map(|u| u.is_ok()), Some(cfg!(target_os = "linux")));

        publish_detected(app, Some(flatpark.clone()), &backend.shared, Some(platform));
        assert_eq!(told(backend.poll()), [format!("installable {}", cfg!(target_os = "linux")), "detected".to_string()]);

        backend.shared.options.lock().unwrap().versions.insert(APP.to_string(), VersionTrack::Pinned("v0.4.0".into()));
        publish_detected(app, Some(flatpark), &backend.shared, Some(platform));
        assert_eq!(told(backend.poll()), ["installable false", "detected"], "flatpak update can't follow a pinned version");
    }

    /// A copy installed outside the launcher (FreeBSD's `/usr/local/bin/<app>`, a system binary)
    /// is never replaced, so a newer release with a matching FreeBSD tarball is not installable
    /// for it: the UI is told no, and an install is refused for the same reason. A copy the
    /// launcher installed takes the same tarball.
    #[test]
    fn a_system_binary_is_not_installable_even_with_a_matching_tarball() {
        let app = CraftApp::by_id(APP).unwrap();
        let platform = Platform { os: Os::FreeBsd, arch: Arch::X64 };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        system_installs(&backend, vec![PackageKind::TarGz]);
        let options = BackendOptions::default();
        let system = copy_of("0.4.0", InstallSource::Binary, &format!("/usr/local/bin/{APP}"));
        let release = stable("0.6.0");
        assert!(release.assets.iter().any(|a| a.name.ends_with("-freebsd-x86_64.tar.gz")), "the release has a matching tarball");

        let refused = install_plan(app, Some(release.clone()), Some(platform), &options, (Some(&system), None), Asking::Install).err();
        assert_eq!(refused, install::managed_elsewhere(app, &system, false), "an install refuses it as installed outside the launcher");

        publish_detected(app, Some(system), &backend.shared, Some(platform));
        publish_latest(app, LatestRelease::Found(release), Origin::GitHub, &backend.shared, Some(platform));
        assert_eq!(told(backend.poll()), ["installable false", "detected", "installable false", "latest"]);

        let ours = copy_of("0.4.0", InstallSource::Launcher, &format!("/usr/local/bin/{APP}"));
        publish_detected(app, Some(ours), &backend.shared, Some(platform));
        assert_eq!(told(backend.poll()), ["installable true", "detected"], "the launcher's own copy takes the tarball");
    }

    /// Reports again, and returns what the UI was last told about a kept download.
    fn ready(backend: &mut NativeBackend) -> Option<bool> {
        let app = CraftApp::by_id(APP).unwrap();
        report_partial(app, &backend.shared, &backend.paths, backend.platform);
        last_ready(backend.poll())
    }

    fn last_ready(events: Vec<Event>) -> Option<bool> {
        events.into_iter().rev().find_map(|e| match e {
            Event::ReadyToInstall { app, ready } if app == APP => Some(ready),
            _ => None,
        })
    }

    fn wait_until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !ready() {
            assert!(Instant::now() < deadline, "the worker did not finish unwinding");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn panic_on_wake() -> (Waker, mpsc::Receiver<()>) {
        let (sent, received) = mpsc::channel();
        (
            Arc::new(move || {
                let _ = sent.send(());
                panic!("injected worker panic")
            }),
            received,
        )
    }

    #[test]
    fn a_panicking_detection_worker_releases_its_busy_flag() {
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        let (waker, woke) = panic_on_wake();
        Arc::get_mut(&mut backend.shared).unwrap().waker = waker;

        backend.detect_all();
        woke.recv_timeout(Duration::from_secs(2)).unwrap();
        wait_until(|| !backend.shared.detecting.load(Ordering::SeqCst));
    }

    #[test]
    fn a_panicking_check_worker_releases_its_busy_flag() {
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, Some(Arc::new(CountingGitHub::default())));
        backend.http = Some(Http::new().unwrap());
        let (waker, woke) = panic_on_wake();
        Arc::get_mut(&mut backend.shared).unwrap().waker = waker;

        backend.check(true);
        woke.recv_timeout(Duration::from_secs(2)).unwrap();
        wait_until(|| !backend.shared.checking.load(Ordering::SeqCst));
    }

    #[test]
    fn a_panicking_job_worker_removes_its_busy_entry() {
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        let (waker, woke) = panic_on_wake();
        Arc::get_mut(&mut backend.shared).unwrap().waker = waker;

        backend.start_job(APP, Work::Uninstall);
        woke.recv_timeout(Duration::from_secs(2)).unwrap();
        wait_until(|| !backend.shared.jobs.lock().unwrap().contains_key(APP));
    }

    #[test]
    fn job_finished_follows_busy_cleanup_and_download_plan_publication() {
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        let shared: Arc<Mutex<Option<std::sync::Weak<Shared>>>> = Arc::new(Mutex::new(None));
        let shared_for_waker = shared.clone();
        let (sent, received) = mpsc::channel();
        Arc::get_mut(&mut backend.shared).unwrap().waker = Arc::new(move || {
            let Some(shared) = shared_for_waker.lock().unwrap().as_ref().and_then(std::sync::Weak::upgrade) else { return };
            let finished = shared.events.lock().unwrap().iter().any(|event| matches!(event, Event::JobFinished { app, .. } if app == APP));
            if finished {
                let busy = shared.jobs.lock().unwrap().contains_key(APP);
                let plan = shared.download_sizes.lock().unwrap().get(APP).and_then(|cached| cached.plan);
                let _ = sent.send((busy, plan));
            }
        });
        *shared.lock().unwrap() = Some(Arc::downgrade(&backend.shared));

        backend.start_job(APP, Work::Uninstall);
        let (busy, plan) = received.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(!busy);
        assert!(matches!(plan, Some((_, 3))), "JobFinished observers need the final download plan: {plan:?}");
        assert_eq!(backend.download_size(APP), plan);
    }

    /// Puts the planned download on disk, as a failed install leaves it.
    fn keep_file(backend: &NativeBackend) -> std::path::PathBuf {
        let app = CraftApp::by_id(APP).unwrap();
        let (dest, _, size) = planned_download(app, &backend.shared, &backend.paths, backend.platform, &BackendOptions::default()).unwrap();
        std::fs::create_dir_all(backend.paths.downloads()).unwrap();
        std::fs::write(&dest, vec![0u8; size as usize]).unwrap();
        dest
    }

    /// A report uses the settings as they are when it is worked out, not as the worker that sends
    /// it found them. A worker that started before the preferred format changed (and, before this
    /// was fixed, carried the settings it started with) waits behind a report in progress; by the
    /// time it reports, the kept MSI is no longer the file an install would use, and its report is
    /// the last word. (Windows only: it has two formats to choose between.)
    #[cfg(windows)]
    #[test]
    fn a_kept_download_report_follows_the_current_settings() {
        let platform = Platform { os: Os::Windows, arch: Arch::X64 };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        *backend.shared.options.lock().unwrap() = BackendOptions { kinds: vec![PackageKind::Msi], ..Default::default() };
        let app = CraftApp::by_id(APP).unwrap();
        let options = backend.shared.options.lock().unwrap().clone();
        let (dest, _, size) = planned_download(app, &backend.shared, &backend.paths, backend.platform, &options).unwrap();
        std::fs::create_dir_all(backend.paths.downloads()).unwrap();
        std::fs::write(&dest, vec![0u8; size as usize]).unwrap();
        assert_eq!(ready(&mut backend), Some(true), "the kept MSI is the planned file");

        // A report is in progress; an older worker starts and waits behind it.
        let in_progress = backend.shared.planning.lock().unwrap();
        let (shared, paths) = (backend.shared.clone(), backend.paths.clone());
        let older = std::thread::spawn(move || report_partial(app, &shared, &paths, Some(platform)));
        // The preferred format changes while it waits, then the report in progress finishes.
        backend.shared.options.lock().unwrap().kinds = vec![PackageKind::PortableZip];
        drop(in_progress);
        older.join().unwrap();
        assert_eq!(last_ready(backend.poll()), Some(false), "the older worker's report has the new settings: a portable zip is planned");
    }

    #[test]
    fn download_size_reads_the_cached_plan_and_reports_clear_missing_assets() {
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        assert_eq!(backend.download_size(APP), None);
        ready(&mut backend);
        let (kind, size) = backend.download_size(APP).unwrap();
        assert_eq!(size, 3);
        assert_eq!(backend.download_size("unknown"), None);

        let mut latest = release("0.6.0");
        for asset in &mut latest.assets {
            asset.size = 0;
        }
        backend.shared.latest.lock().unwrap().insert(APP.to_string(), latest);
        // Reading from a frame neither discovers installers nor replans the download.
        assert_eq!(backend.download_size(APP), Some((kind, 3)));
        ready(&mut backend);
        assert_eq!(backend.download_size(APP), Some((kind, 0)));

        backend.shared.latest.lock().unwrap().insert(APP.to_string(), Release::default());
        ready(&mut backend);
        assert_eq!(backend.download_size(APP), None);
    }

    #[cfg(windows)]
    #[test]
    fn download_size_respects_preferences_and_the_installed_package_format() {
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), Platform { os: Os::Windows, arch: Arch::X64 }, None);
        let mut latest = release("0.5.0");
        for asset in &mut latest.assets {
            asset.size = if asset.name.ends_with(".msi") { 100 } else { 200 };
        }
        backend.shared.latest.lock().unwrap().insert(APP.to_string(), latest);
        // A worker started planning the default MSI before the user chose portable ZIP.
        let old_revision = backend.shared.download_revision(APP);
        let options = BackendOptions { kinds: vec![PackageKind::PortableZip], ..Default::default() };
        backend.send(Command::Configure(options));
        // Configuration now replans off the UI thread; observe the new answer before testing stale reports.
        wait_until(|| backend.download_size(APP) == Some((PackageKind::PortableZip, 200)));
        backend.poll();
        assert!(!backend.shared.report_download(APP, old_revision, Some((PackageKind::Msi, 100)), None, false));
        assert!(!backend.poll().iter().any(|event| matches!(event, Event::Paused { app, .. } | Event::ReadyToInstall { app, .. } if app == APP)));
        assert_eq!(backend.download_size(APP), Some((PackageKind::PortableZip, 200)));
        backend.shared.installed.lock().unwrap().insert(
            APP.to_string(),
            Installed {
                version: None,
                source: InstallSource::Msi,
                location: r"C:\Apps\PhotoCraft\photocraft.exe".into(),
                package: None,
                externally_managed: false,
                origin: None,
                installation: None,
            },
        );
        report_partial(CraftApp::by_id(APP).unwrap(), &backend.shared, &backend.paths, backend.platform);
        assert_eq!(backend.download_size(APP), Some((PackageKind::Msi, 100)));
    }

    #[test]
    fn a_plan_report_retries_when_a_release_changes_while_it_is_worked_out() {
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        let shared: Arc<Mutex<Option<std::sync::Weak<Shared>>>> = Arc::new(Mutex::new(None));
        let shared_for_waker = shared.clone();
        let changed = AtomicBool::new(false);
        Arc::get_mut(&mut backend.shared).unwrap().waker = Arc::new(move || {
            // The installability publication is between capturing the revision and the plan.
            if !changed.swap(true, Ordering::SeqCst) {
                let shared = shared_for_waker.lock().unwrap().as_ref().unwrap().upgrade().unwrap();
                let mut latest = release("0.6.0");
                for asset in &mut latest.assets {
                    asset.size = 300;
                }
                shared.store_latest(APP, &latest);
            }
        });
        *shared.lock().unwrap() = Some(Arc::downgrade(&backend.shared));

        report_partial(CraftApp::by_id(APP).unwrap(), &backend.shared, &backend.paths, Some(platform));
        assert_eq!(backend.download_size(APP).map(|(_, size)| size), Some(300));
        let events = backend.poll();
        assert!(events.iter().any(|e| matches!(e, Event::Paused { app, download: None } if app == APP)));
        assert_eq!(last_ready(events), Some(false));
    }

    #[test]
    fn a_new_release_rejects_a_slow_workers_older_download_size() {
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        ready(&mut backend);
        let old_plan = backend.download_size(APP);
        let old_revision = backend.shared.download_revision(APP);
        let mut latest = release("0.6.0");
        for asset in &mut latest.assets {
            asset.size = 300;
        }
        check_app(
            CraftApp::by_id(APP).unwrap(),
            Some(LatestRelease::Found(latest)),
            Origin::GitHub,
            &backend.shared,
            &backend.paths,
            backend.platform,
        );
        assert_eq!(backend.download_size(APP), None);
        ready(&mut backend);
        let fresh = backend.download_size(APP).unwrap();
        assert_eq!(fresh.1, 300);
        backend.poll();
        assert!(!backend.shared.report_download(APP, old_revision, old_plan, None, false));
        assert!(backend.poll().is_empty());
        assert_eq!(backend.download_size(APP), Some(fresh));
    }

    #[test]
    fn a_kept_download_is_reported_and_can_be_thrown_away() {
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        assert_eq!(ready(&mut backend), Some(false));
        let dest = keep_file(&backend);
        assert_eq!(ready(&mut backend), Some(true));

        backend.send(Command::Cancel { app: APP.to_string() });
        assert!(!dest.exists());
        assert_eq!(last_ready(backend.poll()), Some(false));
    }

    #[test]
    fn changing_the_settings_reports_kept_downloads_again() {
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        keep_file(&backend);
        assert_eq!(ready(&mut backend), Some(true));
        // The planned file changed under the UI; only a new report can tell it.
        backend.shared.latest.lock().unwrap().insert(APP.to_string(), release("0.6.0"));
        backend.send(Command::Configure(BackendOptions::default()));
        // Worked out off the UI thread: the report follows shortly.
        let mut seen = None;
        wait_until(|| {
            seen = last_ready(backend.poll()).or(seen);
            seen.is_some()
        });
        assert_eq!(seen, Some(false));
    }

    #[test]
    fn an_unchanged_refresh_keeps_the_download_size_while_other_results_wait() {
        struct Unchanged {
            saving: mpsc::Sender<()>,
            finish: Mutex<mpsc::Receiver<()>>,
        }
        impl ReleaseSource for Unchanged {
            fn latest(&self, repo: &str, _prereleases: bool) -> Answer {
                let latest = if CraftApp::by_id(APP).is_some_and(|app| app.repo == repo) {
                    LatestRelease::Found(release("0.5.0"))
                } else {
                    LatestRelease::None
                };
                Answer { latest, source: Source::GitHubApi }
            }
            fn cached(&self, _repo: &str, _prereleases: bool) -> Option<LatestRelease> {
                None
            }
            fn rate_limited_until(&self) -> Option<i64> {
                None
            }
            fn save(&self) {
                let _ = self.saving.send(());
                // Disconnecting the sender also releases this gate if the test panics.
                let _ = self.finish.lock().unwrap().recv();
            }
        }
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let (saving, saved) = mpsc::channel();
        let (finish, finished) = mpsc::channel();
        let mut backend = backend(dir.path(), platform, Some(Arc::new(Unchanged { saving, finish: Mutex::new(finished) })));
        backend.http = Some(Http::new().unwrap());
        backend.shared.options.lock().unwrap().prereleases = true;
        ready(&mut backend);
        let before = backend.download_size(APP);
        assert!(before.is_some());
        let (wake, waking) = mpsc::channel();
        Arc::get_mut(&mut backend.shared).unwrap().waker = Arc::new(move || {
            let _ = wake.send(());
        });

        backend.send(Command::CheckForUpdates { force: true });
        saved.recv_timeout(Duration::from_secs(20)).unwrap();
        // Latest was delivered, but the batch's final report_partial loop cannot run yet.
        let during = backend.download_size(APP);
        let pending = backend.poll();
        drop(finish);
        let mut complete = false;
        while !complete {
            waking.recv_timeout(Duration::from_secs(20)).unwrap();
            complete = backend.poll().iter().any(|event| matches!(event, Event::CheckFinished { .. }));
        }
        assert!(pending.iter().any(|event| matches!(event, Event::Latest { app, latest: LatestRelease::Found(_), .. } if app == APP)));
        assert!(!pending.iter().any(|event| matches!(event, Event::CheckFinished { .. })));
        assert_eq!(during, before, "an identical release must keep its known size throughout Refresh");
    }

    #[test]
    fn unchanged_detection_keeps_the_plan_but_a_changed_copy_rejects_old_reports() {
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        ready(&mut backend);
        let plan = backend.download_size(APP);
        let revision = backend.shared.download_revision(APP);
        publish_detected(CraftApp::by_id(APP).unwrap(), None, &backend.shared, Some(platform));
        assert_eq!(backend.download_size(APP), plan);

        publish_detected(CraftApp::by_id(APP).unwrap(), Some(copy("0.4.0")), &backend.shared, Some(platform));
        assert_eq!(backend.download_size(APP), None);
        assert!(!backend.shared.report_download(APP, revision, plan, None, false));
        let revision = backend.shared.download_revision(APP);
        assert!(backend.shared.report_download(APP, revision, plan, None, false));
        publish_detected(CraftApp::by_id(APP).unwrap(), Some(copy("0.4.0")), &backend.shared, Some(platform));
        assert_eq!(backend.download_size(APP), plan);

        publish_detected(CraftApp::by_id(APP).unwrap(), Some(copy("0.5.0")), &backend.shared, Some(platform));
        assert_eq!(backend.download_size(APP), None);
        assert!(!backend.shared.report_download(APP, revision, plan, None, false));
    }

    #[test]
    fn the_settings_reach_the_file_the_apps_read() {
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        let mut options = BackendOptions::default();
        options.versions.insert("pdfcraft".into(), VersionTrack::Pinned("v0.13.0".into()));
        backend.send(Command::Configure(options));
        let pins = craft_update::pins::Pins::load(&dir.path().join("versions.json"));
        assert_eq!(pins.track("pdfcraft"), Some(craft_update::pins::VersionTrack::Pinned("v0.13.0".into())));
        backend.send(Command::Configure(BackendOptions::default()));
        let pins = craft_update::pins::Pins::load(&dir.path().join("versions.json"));
        assert_eq!(pins.choice("pdfcraft"), craft_update::pins::Choice::Stable);
    }

    #[test]
    fn a_check_that_finds_a_newer_release_ends_the_retry_offer() {
        struct Newer;
        impl ReleaseSource for Newer {
            fn latest(&self, repo: &str, _prereleases: bool) -> crate::source::Answer {
                let found = CraftApp::by_id(APP).is_some_and(|a| a.repo == repo);
                let latest = if found { LatestRelease::Found(release("0.6.0")) } else { LatestRelease::None };
                crate::source::Answer { latest, source: Source::GitHubApi }
            }
            fn cached(&self, _repo: &str, _prereleases: bool) -> Option<LatestRelease> {
                None
            }
            fn rate_limited_until(&self) -> Option<i64> {
                None
            }
            fn save(&self) {}
        }
        let Some(platform) = native_platform() else { return };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, Some(Arc::new(Newer)));
        // A check needs a client; with pre-releases on it asks only `Newer`, never ArtCraft's API.
        backend.http = Some(Http::new().unwrap());
        backend.shared.options.lock().unwrap().prereleases = true;
        keep_file(&backend);
        assert_eq!(ready(&mut backend), Some(true));

        backend.send(Command::CheckForUpdates { force: true });
        let mut events = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !events.iter().any(|e| matches!(e, Event::CheckFinished { .. })) {
            assert!(Instant::now() < deadline, "the check never finished");
            std::thread::sleep(Duration::from_millis(10));
            events.extend(backend.poll());
        }
        assert_eq!(last_ready(events), Some(false));
    }

    fn copy(v: &str) -> Installed {
        Installed {
            version: version::parse(v),
            source: InstallSource::Deb,
            location: String::new(),
            package: None,
            externally_managed: false,
            origin: None,
            installation: None,
        }
    }

    #[test]
    fn a_failed_job_counts_as_installed_only_when_the_new_version_arrived() {
        let latest = version::parse("0.5.0").unwrap();
        // One app of a shared installer call went in before another failed the call.
        assert!(landed(Some(&copy("0.4.1")), Some(&copy("0.5.0")), &latest));
        assert!(landed(None, Some(&copy("0.5.0")), &latest));
        // Nothing changed, so the error stands.
        assert!(!landed(Some(&copy("0.4.1")), Some(&copy("0.4.1")), &latest));
        assert!(!landed(None, None, &latest));
        // It was current before the job ran: the job did not install it.
        assert!(!landed(Some(&copy("0.5.0")), Some(&copy("0.5.0")), &latest));
        // A copy whose version can't be read proves nothing.
        let unreadable = Installed { version: None, ..copy("0.5.0") };
        assert!(!landed(Some(&copy("0.4.1")), Some(&unreadable), &latest));
    }

    #[test]
    fn only_a_failed_shared_call_can_count_as_installed() {
        let latest = version::parse("0.5.0").unwrap();
        let (before, after) = (Some(copy("0.4.1")), Some(copy("0.5.0")));
        let landed_after = |error: &str| landed_despite(error, before.as_ref(), after.as_ref(), &latest);
        // An app's own failure stands, even if its new version shows up (an MSI or AppImage that
        // reported an error, a cancelled or dismissed prompt, a pause).
        for error in ["the Windows Installer stopped with error 1603", "couldn't copy the AppImage", CANCELLED, INSTALL_CANCELLED, PAUSED] {
            assert!(!landed_after(error), "{error}");
        }
        // A call shared with other apps may have installed this one before it failed.
        let shared = "the system installer stopped with error 100 while handling 2 apps together. Try them one at a time.";
        assert_eq!(landed_after(shared), cfg!(target_os = "linux"));
        assert!(!landed_despite(shared, before.as_ref(), before.as_ref(), &latest));
    }

    #[test]
    fn the_launcher_gives_up_waiting_and_names_the_busy_apps() {
        let jobs = Mutex::new(HashMap::new());
        for id in [LAUNCHER_ID, "photocraft"] {
            jobs.lock().unwrap().insert(id.to_string(), Arc::new(Stop::default()));
        }
        let restarting = AtomicBool::new(false);
        let error = claim_restart(&jobs, &restarting, &Stop::default(), Duration::from_millis(300), &mut || {}).unwrap_err();
        assert!(error.contains("PhotoCraft was still installing") && error.contains("cancel it"), "{error}");
        assert!(!restarting.load(Ordering::SeqCst));
        assert!(restart_blocked(vec!["WordCraft", "FilmCraft"]).contains("FilmCraft, WordCraft were still installing"));
    }

    #[test]
    fn the_launcher_restarts_only_once_every_other_job_has_ended() {
        let jobs = Mutex::new(HashMap::new());
        for id in [LAUNCHER_ID, "photocraft", "filmcraft"] {
            jobs.lock().unwrap().insert(id.to_string(), Arc::new(Stop::default()));
        }
        let restarting = AtomicBool::new(false);
        let stop = Stop::default();
        let (waiting, waits) = mpsc::channel();
        std::thread::scope(|scope| {
            let (jobs, restarting, stop) = (&jobs, &restarting, &stop);
            let claim = scope.spawn(move || claim_restart(jobs, restarting, stop, RESTART_WAIT, &mut || waiting.send(()).unwrap()));
            waits.recv().unwrap();
            // One install ends; another is still running or queued.
            jobs.lock().unwrap().remove("photocraft");
            std::thread::sleep(Duration::from_millis(250));
            assert!(!claim.is_finished() && !restarting.load(Ordering::SeqCst));
            jobs.lock().unwrap().remove("filmcraft");
            assert_eq!(claim.join().unwrap(), Ok(()));
        });
        assert!(restarting.load(Ordering::SeqCst));
        assert!(waits.try_recv().is_err(), "it says it is waiting once");

        // With nothing else running there is nothing to wait for.
        let restarting = AtomicBool::new(false);
        assert_eq!(claim_restart(&jobs, &restarting, &stop, RESTART_WAIT, &mut || panic!("it never waits")), Ok(()));
        assert!(restarting.load(Ordering::SeqCst));
    }

    #[test]
    fn a_cancelled_launcher_update_stops_waiting_and_claims_nothing() {
        let jobs =
            Mutex::new(HashMap::from([(LAUNCHER_ID.to_string(), Arc::new(Stop::default())), ("photocraft".to_string(), Arc::new(Stop::default()))]));
        let restarting = AtomicBool::new(false);
        let stop = Stop::default();
        let (waiting, waits) = mpsc::channel();
        std::thread::scope(|scope| {
            let (jobs, restarting, stop) = (&jobs, &restarting, &stop);
            let claim = scope.spawn(move || claim_restart(jobs, restarting, stop, RESTART_WAIT, &mut || waiting.send(()).unwrap()));
            waits.recv().unwrap();
            stop.cancel();
            assert_eq!(claim.join().unwrap(), Err(CANCELLED.to_string()));
        });
        assert!(!restarting.load(Ordering::SeqCst));
    }
}
