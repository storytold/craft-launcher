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
    Source,
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

struct Shared {
    events: Mutex<Vec<Event>>,
    waker: Waker,
    latest: Mutex<HashMap<String, Release>>,
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

impl Shared {
    fn emit(&self, event: Event) {
        self.events.lock().unwrap_or_else(PoisonError::into_inner).push(event);
        (self.waker)();
    }
}

pub struct NativeBackend {
    shared: Arc<Shared>,
    paths: Paths,
    http: Option<Http>,
    source: Option<Arc<dyn ReleaseSource>>,
    platform: Option<Platform>,
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
            waker,
            latest: Mutex::new(HashMap::new()),
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
        let backend = Self { shared, paths, http, source, platform: native_platform() };
        backend.replay_cache();
        backend.detect_all();
        backend
    }

    /// Shows the saved answers from the last run straight away, before any network request: the
    /// release feed's where they're still fresh, GitHub's otherwise.
    fn replay_cache(&self) {
        let pre = self.options().prereleases;
        let feed = if pre { None } else { load_feed(&self.paths) };
        let now = crate::now();
        for app in &SUITE {
            let answer = match feed.as_ref().map(|f| f.answer(app, now)) {
                Some(FeedAnswer::Fresh(Some(r))) => Some((LatestRelease::Found(r), Origin::ArtCraftApi)),
                Some(FeedAnswer::Fresh(None)) => Some((LatestRelease::None, Origin::ArtCraftApi)),
                _ => self.source.as_ref().and_then(|s| s.cached(&feed_repo(feed.as_ref(), app), pre)).map(|l| (l, Origin::GitHub)),
            };
            if let Some((latest, from)) = answer {
                if let LatestRelease::Found(r) = &latest {
                    self.shared.latest.lock().unwrap_or_else(PoisonError::into_inner).insert(app.id.to_string(), r.clone());
                }
                self.shared.emit(Event::Latest { app: app.id.to_string(), latest, from });
            }
        }
        if let Some(f) = feed {
            self.shared.emit(Event::Notices { announcements: f.announcements, alerts: f.alerts });
        }
    }

    fn options(&self) -> BackendOptions {
        self.shared.options.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    fn detect_all(&self) {
        if self.shared.detecting.swap(true, Ordering::SeqCst) {
            return;
        }
        let shared = self.shared.clone();
        let paths = self.paths.clone();
        let platform = self.platform;
        let options = self.options();
        std::thread::spawn(move || {
            // Leftovers from interrupted installs or older versions of the launcher. A paused
            // download stays a month; a finished one kept after a failed install, a week.
            http::clean_downloads(&paths.downloads(), Duration::from_secs(30 * 86_400), Duration::from_secs(7 * 86_400));
            let snapshot = crate::detect::Snapshot::take();
            let manifest = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).clone();
            for app in &SUITE {
                let found = crate::detect::detect(app, &manifest, &snapshot);
                {
                    let mut map = shared.installed.lock().unwrap_or_else(PoisonError::into_inner);
                    match &found {
                        Some(i) => map.insert(app.id.to_string(), i.clone()),
                        None => map.remove(app.id),
                    };
                }
                shared.emit(Event::Detected { app: app.id.to_string(), installed: found });
            }
            // Downloads paused (or cut off by quitting) in an earlier run.
            for app in &SUITE {
                report_partial(app, &shared, &paths, platform, &options);
            }
            shared.detecting.store(false, Ordering::SeqCst);
        });
    }

    /// Looks up the latest releases: ArtCraft's release feed first (one request for every app),
    /// GitHub for anything the feed can't vouch for (apps it doesn't list, entries more than a
    /// day old, or no answer at all), at most once an hour per app unless `force`.
    fn check(&self, force: bool) {
        let (Some(source), Some(http)) = (self.source.clone(), self.http.clone()) else {
            for app in &SUITE {
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
        let paths = self.paths.clone();
        let platform = self.platform;
        let pre = self.options().prereleases;
        let mirror = local_mirror();
        std::thread::spawn(move || {
            shared.emit(Event::CheckStarted);
            let (feed, fallback) = if let Some(base) = &mirror {
                // A local mirror answers for everything: GitHub is never asked.
                (local_feed(&http, base), Fallback { trust_feed: true, github_every: GITHUB_FALLBACK_EVERY })
            } else if pre {
                // The feed serves stable releases only; with pre-releases on, GitHub decides.
                (None, Fallback { trust_feed: false, github_every: EARLY_GITHUB_EVERY })
            } else {
                feed_and_fallback(&http, &paths, &shared)
            };
            if let Some(f) = &feed {
                shared.emit(Event::Notices { announcements: f.announcements.clone(), alerts: f.alerts.clone() });
            }
            let now = crate::now();
            let queue = Mutex::new(SUITE.iter().chain(std::iter::once(&LAUNCHER)).collect::<Vec<&CraftApp>>());
            let sources = Mutex::new(Vec::new());
            std::thread::scope(|scope| {
                for _ in 0..WORKERS {
                    scope.spawn(|| {
                        loop {
                            let Some(app) = queue.lock().unwrap_or_else(PoisonError::into_inner).pop() else { break };
                            let (latest, from, used) = match &mirror {
                                Some(_) => mirror_latest(app, feed.as_ref(), now),
                                None => latest_for(app, feed.as_ref(), fallback, source.as_ref(), &shared, pre, force, now),
                            };
                            if let Some(used) = used {
                                sources.lock().unwrap_or_else(PoisonError::into_inner).push(used);
                            }
                            if app.id == LAUNCHER_ID {
                                // Is there a newer ArtCraft Launcher?
                                if let Some(LatestRelease::Found(r)) = latest
                                    && let (Some(newest), Some(current)) = (r.version.as_ref(), crate::self_update::current_version())
                                    && version::is_newer(newest, &current)
                                    && !crate::self_update::just_updated_to(&paths, &r)
                                {
                                    shared.self_install.get_or_init(crate::self_update::current);
                                    *shared.launcher_release.lock().unwrap_or_else(PoisonError::into_inner) = Some(r.clone());
                                    shared.emit(Event::LauncherUpdate(r));
                                }
                                continue;
                            }
                            let Some(latest) = latest else { continue };
                            if let LatestRelease::Found(r) = &latest {
                                shared.latest.lock().unwrap_or_else(PoisonError::into_inner).insert(app.id.to_string(), r.clone());
                            }
                            if !matches!(latest, LatestRelease::Failed(_)) {
                                shared.answered.lock().unwrap_or_else(PoisonError::into_inner).insert(app.id.to_string());
                            }
                            shared.emit(Event::Latest { app: app.id.to_string(), latest, from });
                        }
                    });
                }
            });
            source.save();
            {
                let answered = shared.answered.lock().unwrap_or_else(PoisonError::into_inner);
                if SUITE.iter().all(|a| answered.contains(a.id)) {
                    shared.loaded.store(true, Ordering::SeqCst);
                }
            }
            // A newer release changes which file an install would use.
            let options = shared.options.lock().unwrap_or_else(PoisonError::into_inner).clone();
            for app in &SUITE {
                report_partial(app, &shared, &paths, platform, &options);
            }
            let sources = sources.into_inner().unwrap_or_else(PoisonError::into_inner);
            let source_used = if sources.contains(&Source::Cache) {
                Source::Cache
            } else if sources.contains(&Source::GitHubPages) {
                Source::GitHubPages
            } else if sources.contains(&Source::GitHubApi) {
                Source::GitHubApi
            } else {
                Source::Feed
            };
            let at = crate::now();
            *shared.last_check.lock().unwrap_or_else(PoisonError::into_inner) = Some(at);
            shared.checking.store(false, Ordering::SeqCst);
            let rate_limited_until = limit_notice(source_used, source.rate_limited_until());
            shared.emit(Event::CheckFinished { at, source: source_used, rate_limited_until });
        });
    }

    fn start_job(&self, app_id: &str, work: Work) {
        let uninstall = work == Work::Uninstall;
        let Some(app) = CraftApp::by_id(app_id) else { return };
        log::info!("{}: starting {} job", app.name, if uninstall { "uninstall" } else { "install/update" });
        let stop = Arc::new(Stop::default());
        let restarting = {
            let mut jobs = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            if jobs.contains_key(app.id) {
                return;
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
            return;
        }
        let shared = self.shared.clone();
        let paths = self.paths.clone();
        let http = self.http.clone();
        let platform = self.platform;
        let options = self.options();
        std::thread::spawn(move || {
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
                    work == Work::Replace,
                );
                // Stopped by the connection rather than by the user: say so, and how to go on.
                match r {
                    Err(e) if e == PAUSED && stop.check().is_ok() => {
                        Err("The download paused because the connection keeps dropping. Press Resume to continue where it stopped.".into())
                    }
                    other => other,
                }
            };
            report_partial(app, &shared, &paths, platform, &options);
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
            {
                let mut map = shared.installed.lock().unwrap_or_else(PoisonError::into_inner);
                match &found {
                    Some(i) => map.insert(app.id.to_string(), i.clone()),
                    None => map.remove(app.id),
                };
            }
            shared.emit(Event::Detected { app: app.id.to_string(), installed: found });
            shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).remove(app.id);
            shared.emit(Event::JobFinished { app: app.id.to_string(), result });
        });
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
        let (shared, paths, http) = (self.shared.clone(), self.paths.clone(), self.http.clone());
        std::thread::spawn(move || {
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
            shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).remove(LAUNCHER_ID);
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Work {
    Install,
    Replace,
    Uninstall,
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
    replace: bool,
) -> Result<String, String> {
    if let Some(msg) = existing.and_then(|e| install::managed_elsewhere(app, e, false)) {
        return Err(msg);
    }
    let http = http.ok_or("secure connections aren't available on this system")?;
    let platform = platform.ok_or("this system isn't supported")?;
    let release = shared
        .latest
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(app.id)
        .cloned()
        .ok_or("the latest release hasn't been found yet. Check for updates first.")?;
    let (asset, kind) = planned_asset(app, &release, platform, options, existing, record).ok_or_else(|| {
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
    if !replace && let Some(path) = install::occupied(app, &asset, kind, existing, record, &options.install_locations, paths)? {
        shared.emit(Event::Occupied { app: app.id.to_string(), path: path.display().to_string() });
        return Err(format!("{} already exists", path.display()));
    }
    let mut last_emit = Instant::now() - Duration::from_secs(1);
    let mut phase = |p: JobPhase| {
        // Downloads report thousands of times; the UI needs ~20 a second.
        if matches!(p, JobPhase::Downloading { done, total } if done != total && done != 0) && last_emit.elapsed() < Duration::from_millis(50) {
            return;
        }
        last_emit = Instant::now();
        shared.emit(Event::Progress { app: app.id.to_string(), phase: p });
    };
    let mut job = Job { http, paths, stop, phase: &mut phase, install_locations: &options.install_locations, replace };
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
        install::choose_kinds(existing, record, &options.kinds)
    };
    pick_asset(release, &stems, platform, &kinds).map(|(a, k)| (a.clone(), k))
}

/// Tells the UI whether `app` has a paused download waiting, or a finished one to install.
fn report_partial(app: &CraftApp, shared: &Shared, paths: &Paths, platform: Option<Platform>, options: &BackendOptions) {
    let planned = planned_download(app, shared, paths, platform, options);
    let download = planned.as_ref().and_then(|(dest, size)| Some(PausedDownload { done: http::partial_bytes(dest, *size)?, total: *size }));
    shared.emit(Event::Paused { app: app.id.to_string(), download });
    let ready = planned.is_some_and(|(dest, size)| http::finished(&dest, size));
    shared.emit(Event::ReadyToInstall { app: app.id.to_string(), ready });
}

/// Where the planned download of `app` is kept, and its size.
fn planned_download(
    app: &CraftApp,
    shared: &Shared,
    paths: &Paths,
    platform: Option<Platform>,
    options: &BackendOptions,
) -> Option<(std::path::PathBuf, u64)> {
    let release = shared.latest.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned()?;
    let existing = shared.installed.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned();
    let record = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).apps.get(app.id).cloned();
    let (asset, _) = planned_asset(app, &release, platform?, options, existing.as_ref(), record.as_ref())?;
    artcraft_launcher_core::release::valid_file_name(&asset.name).then(|| (install::download_path(paths, &asset), asset.size))
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
    if fallback.trust_feed {
        match feed.map(|f| f.answer(app, now)) {
            Some(FeedAnswer::Fresh(Some(r))) => return (Some(LatestRelease::Found(r)), Origin::ArtCraftApi, Some(Source::Feed)),
            Some(FeedAnswer::Fresh(None)) => return (Some(LatestRelease::None), Origin::ArtCraftApi, Some(Source::Feed)),
            Some(FeedAnswer::AskGitHub) | None => {}
        }
    }
    let repo = feed_repo(feed, app);
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
        .filter(|u| u.starts_with("https://") || is_loopback(u))
        .unwrap_or_else(|| artcraft_launcher_core::feed::FEED_URL.to_string())
}

/// `http://<loopback host>:<port>` and an optional path: the host must be exactly 127.0.0.1,
/// localhost or [::1] (no `user@` in front of another host), with a numeric port.
fn is_loopback(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let Some((host, port)) = authority.rsplit_once(':') else { return false };
    matches!(host, "127.0.0.1" | "localhost" | "[::1]") && !port.is_empty() && port.parse::<u16>().is_ok()
}

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
            Command::Install { app } => self.start_job(&app, Work::Install),
            Command::Replace { app } => self.start_job(&app, Work::Replace),
            Command::Uninstall { app } => self.start_job(&app, Work::Uninstall),
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
                            && let Some((dest, size)) = planned_download(def, &self.shared, &self.paths, self.platform, &self.options())
                        {
                            http::discard(&dest, size);
                        }
                        self.shared.emit(Event::Paused { app: app.clone(), download: None });
                        self.shared.emit(Event::ReadyToInstall { app, ready: false });
                    }
                }
            }
            Command::Launch { app } => {
                let Some(def) = CraftApp::by_id(&app) else { return };
                let result = match self.installed(&app) {
                    Some(inst) => install::launch(def, &inst, &self.paths),
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
                    let changed = o.prereleases != options.prereleases;
                    *o = options;
                    changed
                };
                // Another package format means another file to download.
                let options = self.options();
                for app in &SUITE {
                    report_partial(app, &self.shared, &self.paths, self.platform, &options);
                }
                if changed_pre {
                    self.check(true);
                }
            }
        }
    }

    fn poll(&mut self) -> Vec<Event> {
        std::mem::take(&mut *self.shared.events.lock().unwrap_or_else(PoisonError::into_inner))
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
            waker: Arc::new(|| {}),
            latest: Mutex::new(HashMap::new()),
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

    const APP: &str = "photocraft";

    /// A PhotoCraft release with one file per platform and format, so whichever this machine
    /// installs is there.
    fn release(version: &str) -> Release {
        let suffixes = ["-portable.zip", ".tar.gz", ".msi", ".dmg", ".AppImage", ".deb", ".rpm", ".flatpak"];
        let tokens =
            ["windows-x64", "windows-x86", "windows-arm64", "macos-universal", "linux-x86_64", "linux-aarch64", "linux-riscv64", "freebsd-x86_64"];
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
        };
        assert_eq!(planned_asset(app, &release, platform, &portable_options, Some(&msi), None).unwrap().1, PackageKind::Msi);
        let launcher = Installed {
            version: None,
            source: artcraft_launcher_core::InstallSource::Launcher,
            location: r"D:\Craft Apps\PhotoCraft\photocraft.exe".into(),
            package: None,
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
        let existing =
            Installed { version: None, source: artcraft_launcher_core::InstallSource::Deb, location: "/usr/bin/photocraft".into(), package: None };
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
        NativeBackend { shared, paths: Paths::under(root), http: None, source, platform: Some(platform) }
    }

    /// Reports again, and returns what the UI was last told about a kept download.
    fn ready(backend: &mut NativeBackend) -> Option<bool> {
        let app = CraftApp::by_id(APP).unwrap();
        report_partial(app, &backend.shared, &backend.paths, backend.platform, &BackendOptions::default());
        last_ready(backend.poll())
    }

    fn last_ready(events: Vec<Event>) -> Option<bool> {
        events.into_iter().rev().find_map(|e| match e {
            Event::ReadyToInstall { app, ready } if app == APP => Some(ready),
            _ => None,
        })
    }

    /// Puts the planned download on disk, as a failed install leaves it.
    fn keep_file(backend: &NativeBackend) -> std::path::PathBuf {
        let app = CraftApp::by_id(APP).unwrap();
        let (dest, size) = planned_download(app, &backend.shared, &backend.paths, backend.platform, &BackendOptions::default()).unwrap();
        std::fs::create_dir_all(backend.paths.downloads()).unwrap();
        std::fs::write(&dest, vec![0u8; size as usize]).unwrap();
        dest
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
        assert_eq!(last_ready(backend.poll()), Some(false));
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
        Installed { version: version::parse(v), source: InstallSource::Deb, location: String::new(), package: None }
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
