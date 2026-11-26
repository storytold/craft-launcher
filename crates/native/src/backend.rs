//! [`NativeBackend`]: the desktop implementation of the UI's [`Backend`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use artcraft_launcher_core::catalog::{LAUNCHER, LAUNCHER_ID};
use artcraft_launcher_core::feed::{Feed, FeedAnswer};
use artcraft_launcher_core::platform::{Arch, Os, Platform, pick_asset};
use artcraft_launcher_core::protocol::{
    Backend, BackendOptions, CANCELLED, Capabilities, Command, Event, INSTALL_CANCELLED, InstallFailure, JobPhase, LatestRelease, Origin,
    PAUSED_BY_CONNECTION, PausedDownload, Source, VersionTrack, is_paused,
};
use artcraft_launcher_core::release::{Asset, Priority, Release, ReleaseQuery};
use artcraft_launcher_core::{CraftApp, Installed, PackageKind, version};

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
/// An app following the nightly is asked about at most this often: nightlies come once a night.
const NIGHTLY_TRACK_EVERY: i64 = 3 * 60 * 60;
/// A forced check this soon after the last check reuses its GitHub answers (#713).
const FORCED_REUSE: i64 = 2 * 60;
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

#[derive(Default)]
struct ProcessLifecycle {
    // Held for admission, restart claiming, and detection publication. Keys retain the
    // channel: Stable and Nightly may operate on the same app without colliding.
    jobs: Mutex<HashMap<String, Arc<Stop>>>,
    restarting: AtomicBool,
    // Bounded by admitted catalog app identities. Kept after retirement to reject ABA
    // snapshots even when rollback restores an identical manifest record.
    revisions: Mutex<HashMap<String, u64>>,
}
/// Where installed copies are looked for besides the launcher's own records: the system. Tests
/// look only at the records and a stub, so a copy installed on the machine running them (a
/// Flatpak, a distro package, an AppImage) can't change what they see.
#[derive(Clone)]
enum HostDetection {
    System,
    #[cfg(test)]
    Stub(HostStub),
}

/// What a test says is installed on the machine, besides the launcher's records.
#[cfg(test)]
type HostStub = Arc<dyn Fn(&CraftApp) -> Option<Installed> + Send + Sync>;

impl HostDetection {
    fn snapshot(&self) -> crate::detect::Snapshot {
        match self {
            Self::System => crate::detect::Snapshot::take(),
            #[cfg(test)]
            Self::Stub(_) => crate::detect::Snapshot::default(),
        }
    }

    fn detect(&self, app: &CraftApp, manifest: &Manifest, snapshot: &crate::detect::Snapshot) -> Option<Installed> {
        match self {
            Self::System => crate::detect::detect(app, manifest, snapshot),
            #[cfg(test)]
            Self::Stub(host) => crate::detect::from_manifest(app, manifest)
                .or_else(|| host(app).map(|found| crate::detect::with_recorded_release(found, manifest.releases.get(app.id)))),
        }
    }

    /// What the system itself has registered of `app`, leaving out the launcher's own records: the
    /// copy a Windows Installer package just put in.
    fn registered(&self, app: &CraftApp) -> Option<Installed> {
        match self {
            Self::System => crate::detect::detect(app, &Manifest::default(), &self.snapshot()),
            #[cfg(test)]
            Self::Stub(host) => host(app),
        }
    }
}

#[derive(PartialEq, Eq)]
enum Admission {
    Reserved,
    AlreadyActive,
    Restarting,
}

struct Shared {
    /// Immutable: Configure cannot turn the stable backend into a nightly installer.
    channel: artcraft_launcher_core::nightly::InstallChannel,
    host: HostDetection,
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
    nightly_verified: Mutex<HashMap<String, crate::nightly::VerifiedRelease>>,
    nightly_state: Mutex<crate::nightly_lookup::State>,
    #[cfg(test)]
    nightly_detection_observer: Mutex<Option<ReviewDetectionHook>>,
    download_sizes: Mutex<HashMap<String, DownloadSize>>,
    installed: Mutex<HashMap<String, Installed>>,
    jobs: Mutex<HashMap<String, Arc<Stop>>>,
    /// Process lifetime is shared; per-channel installed state and job views are not.
    lifecycle: Arc<ProcessLifecycle>,
    manifest: Mutex<Manifest>,
    options: Mutex<BackendOptions>,
    checking: AtomicBool,
    /// A detection pass is running; another request waits for the next one.
    detecting: AtomicBool,
    last_check: Mutex<Option<i64>>,
    /// When a forced check last asked GitHub (Unix seconds), for [`FORCED_REUSE`].
    last_forced: Mutex<Option<i64>>,
    /// Set while a check the user pressed reuses the last forced check's answers: it still looks
    /// past a feed that is behind GitHub, so it can't swap GitHub's newer answer for the feed's.
    reusing_forced: AtomicBool,
    /// The newer ArtCraft Launcher release, once a check has found one, with the repo that check
    /// read it from (the feed's rename, if any): its download links must be that repo's.
    launcher_release: Mutex<Option<(Release, String)>>,
    /// How this launcher is installed, when it can replace itself. Worked out on the check thread
    /// just before a newer launcher is announced: finding out can ask the host's Flatpak, which
    /// mustn't hold up the first frame.
    self_install: OnceLock<Option<crate::self_update::Install>>,
    /// Whether this copy can update itself ([`crate::self_update::is_self_updating`]: on Windows,
    /// only a signed copy). Worked out with `self_install`.
    self_updating: OnceLock<bool>,
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

struct NightlyCheckLease(Arc<Shared>);
impl Drop for NightlyCheckLease {
    fn drop(&mut self) {
        self.0.nightly_state.lock().unwrap_or_else(PoisonError::into_inner).finish();
        self.0.checking.store(false, Ordering::SeqCst);
    }
}

struct JobGuard {
    shared: Arc<Shared>,
    app_id: String,
}

impl JobGuard {
    /// Queues the job's last events and frees its slot in one hold of the admission lock, so an
    /// attempt admitted after it cannot queue an event ahead of them. The UI is woken afterwards.
    fn finish(mut self, events: impl IntoIterator<Item = Event>) {
        let app_id = std::mem::take(&mut self.app_id);
        {
            let _reporting = self.shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
            let mut process = self.shared.lifecycle.jobs.lock().unwrap_or_else(PoisonError::into_inner);
            self.shared.events.lock().unwrap_or_else(PoisonError::into_inner).extend(events);
            self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).remove(&app_id);
            process.remove(&self.shared.job_key(&app_id));
            #[cfg(test)]
            tests::note_slot_freed(&self.shared, &app_id);
        }
        (self.shared.waker)();
    }
}

impl Drop for JobGuard {
    fn drop(&mut self) {
        // `finish` already freed the slot.
        if self.app_id.is_empty() {
            return;
        }
        let _reporting = self.shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
        let mut process = self.shared.lifecycle.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).remove(&self.app_id);
        process.remove(&self.shared.job_key(&self.app_id));
        #[cfg(test)]
        tests::note_slot_freed(&self.shared, &self.app_id);
    }
}

/// How long the installability answer trusts what this system could install ([`Shared::system_kinds`]).
const SYSTEM_KINDS_FOR: Duration = Duration::from_secs(60);

impl Shared {
    fn job_key(&self, app: &str) -> String {
        if self.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly { format!("nightly:{app}") } else { app.to_owned() }
    }
    fn reserve_job(&self, app: &str, stop: Arc<Stop>) -> Admission {
        let _reporting = self.reporting.lock().unwrap_or_else(PoisonError::into_inner);
        let mut process = self.lifecycle.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let mut jobs = self.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let key = self.job_key(app);
        if jobs.contains_key(app) || process.contains_key(&key) {
            return Admission::AlreadyActive;
        }
        if self.lifecycle.restarting.load(Ordering::SeqCst) {
            return Admission::Restarting;
        }
        if self.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
            let mut revisions = self.lifecycle.revisions.lock().unwrap_or_else(PoisonError::into_inner);
            let revision = revisions.entry(key.clone()).or_default();
            *revision = revision.wrapping_add(1);
        }
        jobs.insert(app.to_owned(), stop.clone());
        process.insert(key, stop);
        Admission::Reserved
    }

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
            let Some(cached) = sizes.get_mut(app).filter(|cached| cached.revision == revision) else {
                return false;
            };
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
    /// Runs a job's worker. Each worker gets its own thread; a test swaps this to see what is in
    /// the download line at the moment each one would start.
    spawn: Spawn,
}

/// Runs a job's worker (see [`NativeBackend::spawn`]).
type Spawn = Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;

/// A rejected namespace is never passed into normal initialization. This context owns no paths,
/// manifest, HTTP client, workers or filesystem handles; it only explains the refusal in the UI.
struct RefusedNightlyBackend {
    reason: String,
    platform: Option<Platform>,
    apps: Vec<String>,
    events: Vec<Event>,
    waker: Waker,
    /// The storage notice is said once; later reports only refresh each app's status.
    noticed: bool,
}
impl RefusedNightlyBackend {
    fn new(reason: String, platform: Option<Platform>, waker: Waker) -> Self {
        let reason = format!(
            "Developer build storage is unavailable: {reason} Existing copies and data were not opened or changed. Fix the folders and restart the launcher to try again."
        );
        let apps = artcraft_launcher_core::hosted_catalog::apps()
            .iter()
            .filter(|app| app.has_separate_nightly_channel() && platform.is_none_or(|p| app.runs_on(p.os)))
            .map(|app| app.id.to_string())
            .collect();
        let mut backend = Self { reason, platform, apps, events: Vec::new(), waker, noticed: false };
        backend.report(None);
        backend
    }
    fn report(&mut self, attempted_at: Option<i64>) {
        let notice = !std::mem::replace(&mut self.noticed, true);
        for app in &self.apps {
            if notice {
                self.events.push(Event::Notice { app: app.clone(), result: Err(self.reason.clone()) });
            }
            self.events.push(Event::Installable { app: app.clone(), installable: false });
            self.events.push(Event::NightlyStatus {
                app: app.clone(),
                status: artcraft_launcher_core::protocol::NightlyStatus {
                    attempted_at,
                    stale: true,
                    message: Some(self.reason.clone()),
                    ..Default::default()
                },
            });
        }
        (self.waker)();
    }
}
impl Backend for RefusedNightlyBackend {
    /// Unusable storage doesn't make a platform without signing trust look available: Settings
    /// shows the same calm "not available yet" as a usable context would.
    fn nightly_available(&self) -> bool {
        self.platform.is_some_and(crate::nightly_profile::new_installs_available)
    }

    fn send(&mut self, command: Command) {
        match command {
            Command::Configure(_) | Command::DownloadOrder(_) => {}
            Command::CheckForUpdates { .. } => {
                let now = crate::now();
                self.report(Some(now));
                for app in &self.apps {
                    self.events.push(Event::Latest { app: app.clone(), latest: LatestRelease::Failed(self.reason.clone()), from: Origin::GitHub });
                }
                // Nothing was asked of any release source; the refusal came from local storage checks.
                self.events.push(Event::CheckFinished { at: now, source: Source::Cache, rate_limited_until: None });
            }
            _ => self.report(None),
        }
        (self.waker)();
    }
    fn poll(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { manage_apps: false, reveal_label: "Show", self_update: false, desktop_shortcuts: false }
    }
    fn platform(&self) -> Option<Platform> {
        self.platform
    }
    fn now(&self) -> i64 {
        crate::now()
    }
}

impl NativeBackend {
    /// `waker` is called (from any thread) whenever events are waiting; it should repaint the UI.
    pub fn new(waker: impl Fn() + Send + Sync + 'static) -> Self {
        let paths = Paths::new().unwrap_or_else(|e| {
            log::error!("{e}; keeping files in the temporary folder");
            Paths::under(&std::env::temp_dir().join("artcraft-launcher"))
        });
        Self::with_paths(artcraft_launcher_core::nightly::InstallChannel::Stable, waker, paths)
    }

    fn with_paths(channel: artcraft_launcher_core::nightly::InstallChannel, waker: impl Fn() + Send + Sync + 'static, paths: Paths) -> Self {
        Self::with_lifecycle(channel, waker, paths, Arc::new(ProcessLifecycle::default()))
    }
    fn with_lifecycle(
        channel: artcraft_launcher_core::nightly::InstallChannel,
        waker: impl Fn() + Send + Sync + 'static,
        paths: Paths,
        lifecycle: Arc<ProcessLifecycle>,
    ) -> Self {
        let waker: Waker = Arc::new(waker);
        let limit = if channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
            crate::github_budget::DEVELOPER_BUILDS
        } else {
            crate::github_budget::STABLE
        };
        let budget = crate::github_budget::GitHubBudget::load(paths.github_budget(), limit);
        let http = match Http::new().map(|http| http.with_budget(budget)).and_then(|http| {
            if channel == artcraft_launcher_core::nightly::InstallChannel::Nightly { http.with_nightly_test_transport() } else { Ok(http) }
        }) {
            Ok(h) => Some(h),
            Err(e) => {
                log::error!("HTTPS unavailable: {e}");
                None
            }
        };
        let source: Option<Arc<dyn ReleaseSource>> = if channel == artcraft_launcher_core::nightly::InstallChannel::Stable {
            http.clone().map(|h| Arc::new(GitHubSource::new(h, paths.release_cache())) as Arc<dyn ReleaseSource>)
        } else {
            None
        }; // Nightly never reads/writes the ordinary release-body cache.

        let shared = Arc::new(Shared {
            channel,
            host: HostDetection::System,
            #[cfg(test)]
            nightly_detection_observer: Mutex::new(None),
            events: Mutex::new(Vec::new()),
            reporting: Mutex::new(()),
            system: Mutex::new(None),
            planning: Mutex::new(()),
            waker,
            latest: Mutex::new(HashMap::new()),
            nightly_verified: Mutex::new(HashMap::new()),
            nightly_state: Mutex::new(if channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
                crate::nightly_lookup::State::load(&paths)
            } else {
                crate::nightly_lookup::State::default()
            }),
            download_sizes: Mutex::new(HashMap::new()),
            installed: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            lifecycle,
            manifest: Mutex::new(Manifest::load(&paths.manifest())),
            options: Mutex::new(BackendOptions::default()),
            checking: AtomicBool::new(false),
            detecting: AtomicBool::new(false),
            launcher_release: Mutex::new(None),
            self_install: OnceLock::new(),
            self_updating: OnceLock::new(),
            github_asked: Mutex::new(HashMap::new()),
            answered: Mutex::new(std::collections::HashSet::new()),
            loaded: AtomicBool::new(false),
            feed_failures: Mutex::new(0),
            feed_ok_at: Mutex::new(None),
            feed_throttle: Mutex::new(FeedThrottle::load(&paths.feed_throttle(), crate::now())),
            pictures_busy: Mutex::new(std::collections::HashSet::new()),
            last_check: Mutex::new(None),
            last_forced: Mutex::new(None),
            reusing_forced: AtomicBool::new(false),
        });
        let pins = if channel == artcraft_launcher_core::nightly::InstallChannel::Stable {
            craft_update::pins::Pins::default_path()
        } else {
            None // Never prepare, rewrite or export the stable apps' global version-choice file.
        };
        if let Some(path) = &pins {
            crate::pins::prepare_folder(path);
        }
        let spawn: Spawn = Arc::new(|work| {
            std::thread::spawn(work);
        });
        let backend = Self { shared, paths, http, source, platform: native_platform(), pins, spawn };
        backend.replay_cache();
        if channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
            backend.replay_nightly_status();
        }
        backend.detect_all();
        backend
    }

    /// Shows the saved answers from the last run straight away, before any network request: the
    /// release feed's where they're still fresh, GitHub's otherwise.
    fn detect_nightly_records(&self) {
        if self.shared.detecting.swap(true, Ordering::SeqCst) {
            return;
        }
        let shared = self.shared.clone();
        let busy = BusyGuard { shared: shared.clone(), flag: BusyFlag::Detecting };
        let paths = self.paths.clone();
        let platform = self.platform;
        std::thread::spawn(move || {
            let _busy = busy;
            let initial = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).clone();
            // Every app with a separate channel recovers its own interrupted transaction; a
            // failure is reported against that app, and an unreadable manifest stays a read error
            // rather than an empty one.
            let adapters: Vec<_> = platform
                .map(|platform| {
                    artcraft_launcher_core::hosted_catalog::apps()
                        .iter()
                        .chain(std::iter::once(&LAUNCHER))
                        .filter(|app| app.has_separate_nightly_channel() && app.runs_on(platform.os))
                        .filter_map(|app| crate::nightly_profile::for_app(app, platform).ok())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let refreshed = if adapters.is_empty() {
                Some(Manifest::default())
            } else {
                let mut refreshed = None;
                let mut failed = false;
                for adapter in adapters {
                    match crate::nightly_transaction::refresh(&paths, adapter) {
                        Ok(manifest) => refreshed = Some(manifest),
                        Err(error) => {
                            failed = true;
                            shared.emit(Event::Notice { app: adapter.app().into(), result: Err(error) });
                        }
                    }
                }
                // A manifest read from any app is the shared record; only when none succeeded does
                // the stored manifest's own read decide between "no copy" and "unreadable".
                if failed && refreshed.is_none() { crate::nightly_transaction::stored_manifest(&paths).ok() } else { refreshed }
            };
            // An unreadable manifest is not evidence that no copy is owned: keep the last known
            // record and publish nothing until a successful read can say what exists.
            let Some(refreshed) = refreshed else { return };
            {
                let _reporting = shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
                let mut current = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner);
                if *current == initial {
                    *current = refreshed;
                }
            }
            let valid = crate::nightly_profile::validate_paths(&paths).is_ok();
            if valid && let Err(error) = crate::nightly_profile::clean_downloads(&paths) {
                log::warn!("Nightly download-cache housekeeping was left unchanged: {error}");
            }
            for app in artcraft_launcher_core::hosted_catalog::apps()
                .iter()
                .chain(std::iter::once(&LAUNCHER))
                .filter(|app| app.has_separate_nightly_channel() && platform.is_none_or(|p| app.runs_on(p.os)))
            {
                let snapshot = nightly_detection_snapshot(&shared, app);
                let found = nightly_detection_result(app, &snapshot, &paths, platform);
                #[cfg(test)]
                if app.id == "photocraft" {
                    let hook = shared.nightly_detection_observer.lock().unwrap_or_else(PoisonError::into_inner).take();
                    if let Some(hook) = hook {
                        hook(found.as_ref().ok().and_then(Option::as_ref).is_none());
                    }
                }
                if !publish_nightly_detection_result(app, &snapshot, found, &shared, platform) {
                    // At most one re-evaluation. Continued churn keeps the worker’s newer result,
                    // rather than publishing an obsolete None or spinning while mutations run.
                    let snapshot = nightly_detection_snapshot(&shared, app);
                    let found = nightly_detection_result(app, &snapshot, &paths, platform);
                    publish_nightly_detection_result(app, &snapshot, found, &shared, platform);
                }
                report_partial(app, &shared, &paths, platform);
            }
        });
    }

    fn replay_cache(&self) {
        if self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
            return; // A stable feed or cached beta is never a nightly fallback.
        }
        let options = self.options();
        let pre = options.prereleases;
        let feed = if pre { None } else { load_feed(&self.paths) };
        let now = crate::now();
        for app in artcraft_launcher_core::hosted_catalog::apps().iter().filter(|a| self.platform.is_none_or(|p| a.runs_on(p.os))) {
            let answer = match feed.as_ref().map(|f| f.answer(app, now)) {
                Some(FeedAnswer::Fresh(Some(r))) => Some((LatestRelease::Found(r), Origin::ArtCraftApi)),
                Some(FeedAnswer::Fresh(None)) => Some((LatestRelease::None, Origin::ArtCraftApi)),
                _ => self.source.as_ref().and_then(|s| s.cached(&release_query(feed.as_ref(), app), pre)).map(|l| (l, Origin::GitHub)),
            };
            if let Some((latest, from)) = answer {
                publish_latest(app, latest, from, &self.shared, self.platform);
            }
        }
        if let Some(n) = crate::notices::load(None, &self.paths, now) {
            self.shared.emit(Event::Notices { announcements: n.announcements, alerts: n.alerts });
        }
    }

    /// Works out again, off the UI thread, what each app's install would download, and tells the
    /// UI ([`report_partial`]): inside the Flatpak sandbox that asks the host (up to 3 s an app).
    fn replan(&self) {
        let shared = self.shared.clone();
        let paths = self.paths.clone();
        let platform = self.platform;
        std::thread::spawn(move || {
            for app in artcraft_launcher_core::hosted_catalog::apps().iter().filter(|a| platform.is_none_or(|p| a.runs_on(p.os))) {
                report_partial(app, &shared, &paths, platform);
            }
        });
    }

    fn options(&self) -> BackendOptions {
        self.shared.options.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// Scans for installed Craft Apps and paused downloads on a background thread.
    fn detect_all(&self) {
        if self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
            self.detect_nightly_records();
            return;
        }
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
            let snapshot = shared.host.snapshot();
            let manifest = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).clone();
            for app in artcraft_launcher_core::hosted_catalog::apps().iter().filter(|a| platform.is_none_or(|p| a.runs_on(p.os))) {
                let found = shared.host.detect(app, &manifest, &snapshot);
                publish_detected(app, found, &shared, platform);
            }
            // Downloads paused (or cut off by quitting) in an earlier run.
            for app in artcraft_launcher_core::hosted_catalog::apps().iter().filter(|a| platform.is_none_or(|p| a.runs_on(p.os))) {
                report_partial(app, &shared, &paths, platform);
            }
            // Local MCP state: which installed apps can be exposed + the unified bridge.
            {
                let _mcp = MCP_WORK.lock().unwrap_or_else(PoisonError::into_inner);
                shared.emit(mcp_state(&shared));
            }
            drop(busy);
        });
    }

    /// Looks up the latest releases: ArtCraft's release feed first (one request for every app),
    /// GitHub for anything the feed can't vouch for (apps it doesn't list, entries more than a
    /// day old, or no answer at all), at most once an hour per app unless `force`.
    fn nightly_apps(&self) -> Vec<CraftApp> {
        let Some(platform) = self.platform else { return Vec::new() };
        artcraft_launcher_core::hosted_catalog::apps()
            .iter()
            .filter(|app| app.has_separate_nightly_channel() && app.runs_on(platform.os))
            .copied()
            .collect()
    }

    fn replay_nightly_status(&self) {
        for app in self.nightly_apps() {
            let Some(platform) = self.platform else { continue };
            let key = crate::nightly_lookup::key(&app, platform);
            let state = self.shared.nightly_state.lock().unwrap_or_else(PoisonError::into_inner);
            let has_token = self.shared.nightly_verified.lock().unwrap_or_else(PoisonError::into_inner).contains_key(app.id);
            self.shared
                .emit(Event::NightlyStatus { app: app.id.into(), status: state.record(&key).status(has_token && state.fresh(&key, crate::now())) });
        }
    }

    fn check_nightly(&self, manual: bool) {
        self.check_nightly_at(manual, crate::now());
    }

    fn check_nightly_at(&self, manual: bool, now: i64) {
        let Some(platform) = self.platform else { return };
        let apps = self.nightly_apps();
        // A scheduling failure covers the whole sweep, so its notice names the first app in it.
        let sweep_app = apps.first().map_or(LAUNCHER_ID, |app| app.id);
        let keys: Vec<_> = apps.iter().map(|app| crate::nightly_lookup::key(app, platform)).collect();
        // An app with a record in the Nightly manifest is installed here: its own Nightly is the copy
        // its user is running, so it is refreshed hourly and first in every sweep.
        let installed: std::collections::BTreeSet<String> = {
            let manifest = self.shared.manifest.lock().unwrap_or_else(PoisonError::into_inner);
            apps.iter().filter(|app| manifest.apps.contains_key(app.id)).map(|app| crate::nightly_lookup::key(app, platform)).collect()
        };
        let (chosen, generation) = {
            let options = self.shared.options.lock().unwrap_or_else(PoisonError::into_inner);
            if !options.nightly_enabled {
                return;
            }
            let mut state = self.shared.nightly_state.lock().unwrap_or_else(PoisonError::into_inner);
            if let Err(error) = state.prepare(&self.paths) {
                drop(state);
                drop(options);
                self.replay_nightly_status();
                for app in &apps {
                    report_installable(app, &self.shared, self.platform);
                }
                self.shared
                    .emit(Event::Notice { app: sweep_app.into(), result: Err(format!("Nightly scheduling metadata is unavailable: {error}")) });
                return;
            }
            let chosen = state.begin(&keys, &installed, now, manual, true);
            if chosen.is_empty() {
                drop(state);
                drop(options);
                self.replay_nightly_status();
                return;
            }
            if let Err(error) = state.save(&self.paths) {
                state.finish();
                drop(state);
                drop(options);
                self.replay_nightly_status();
                for app in &apps {
                    report_installable(app, &self.shared, self.platform);
                }
                self.shared
                    .emit(Event::Notice { app: sweep_app.into(), result: Err(format!("Could not persist Nightly scheduling metadata: {error}")) });
                return;
            }
            (chosen, state.generation)
        };
        self.shared.checking.store(true, Ordering::SeqCst);
        self.replay_nightly_status();
        let shared = self.shared.clone();
        let paths = self.paths.clone();
        let http = self.http.clone();
        (self.spawn)(Box::new(move || {
            let lease = NightlyCheckLease(shared.clone());
            shared.emit(Event::CheckStarted);
            // One sweep and one request at a time. All admitted app lookups share this deadline.
            let started = Instant::now();
            // `begin` decided the order: installed copies first, then the front of the queue.
            let by_key: std::collections::BTreeMap<String, CraftApp> =
                apps.into_iter().map(|app| (crate::nightly_lookup::key(&app, platform), app)).collect();
            // When the launcher's own budget held this sweep back, so the page can say it is waiting
            // for GitHub instead of reporting a finished check that found nothing.
            let mut held_back_until = None;
            for (index, key) in chosen.iter().enumerate() {
                let Some(app) = by_key.get(key).copied() else { continue };
                let enabled = || {
                    shared.options.lock().unwrap_or_else(PoisonError::into_inner).nightly_enabled
                        && shared.nightly_state.lock().unwrap_or_else(PoisonError::into_inner).generation == generation
                };
                if !enabled() {
                    break;
                }
                // The user's own check outranks a background one; an installed copy outranks an app
                // nobody has installed, which is what keeps discovery inside its budget.
                let priority = if manual {
                    Priority::User
                } else if installed.contains(key) {
                    Priority::Installed
                } else {
                    Priority::Discovery
                };
                let backoff = shared.nightly_state.lock().unwrap_or_else(PoisonError::into_inner).backoff(crate::now());
                let outcome = if let Some(retry_at) = backoff {
                    crate::nightly_lookup::Outcome::Indeterminate(crate::http::NightlyHttpError {
                        over_budget: false,
                        why: "Nightly discovery is waiting for server backoff.".into(),
                        retry_at: Some(retry_at),
                    })
                } else if let Some(http) = &http {
                    crate::nightly_lookup::lookup(
                        &app,
                        platform,
                        now,
                        &mut |url, max, timeout, now| http.nightly_get_as(url, max, timeout, now, priority),
                        &|| started.elapsed(),
                        &enabled,
                    )
                } else {
                    crate::nightly_lookup::Outcome::Indeterminate(crate::http::NightlyHttpError {
                        over_budget: false,
                        why: "Secure Nightly connections are unavailable.".into(),
                        retry_at: None,
                    })
                };
                if let crate::nightly_lookup::Outcome::Indeterminate(error) = &outcome
                    && error.over_budget
                {
                    // The launcher's own budget refused this lookup and the ones behind it: no app
                    // failed, so every remaining one goes back where it was and keeps its place in
                    // the queue instead of waiting out a cadence for a request never made.
                    let mut state = shared.nightly_state.lock().unwrap_or_else(PoisonError::into_inner);
                    for later in &chosen[index..] {
                        state.defer(later);
                    }
                    if let Err(error) = state.save(&paths) {
                        log::warn!("Could not persist deferred Nightly scheduling: {error}");
                    }
                    held_back_until = http.as_ref().and_then(|http| http.nightly_budget_retry_at(priority, crate::now()));
                    break;
                }
                finish_nightly_result(&app, platform, generation, outcome, &shared, &paths);
            }
            drop(lease);
            let retry = shared.nightly_state.lock().unwrap_or_else(PoisonError::into_inner).backoff(crate::now());
            shared.emit(Event::CheckFinished {
                at: crate::now(),
                source: Source::GitHubApi,
                // A server's backoff, or the launcher's own budget: both mean "ask again at this time".
                rate_limited_until: retry.or(held_back_until),
            });
        }));
    }

    fn check(&self, force: bool) {
        if self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
            self.check_nightly(force);
            return;
        }
        let (Some(source), Some(http)) = (self.source.clone(), self.http.clone()) else {
            if self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable {
                crate::hosted_catalog::load_once(None, &self.paths);
                self.shared.emit(Event::CatalogUpdated);
            }
            for app in artcraft_launcher_core::hosted_catalog::apps().iter().filter(|a| self.platform.is_none_or(|p| a.runs_on(p.os))) {
                self.shared.emit(Event::Latest {
                    app: app.id.to_string(),
                    latest: LatestRelease::Failed("secure connections aren't available".into()),
                    from: Origin::GitHub,
                });
            }
            return;
        };
        let now = crate::now();
        let last = *self.shared.last_check.lock().unwrap_or_else(PoisonError::into_inner);
        if (last.is_some_and(|t| now - t < FRESH_FOR) && !force) || self.shared.checking.swap(true, Ordering::SeqCst) {
            return;
        }
        let pressed = force;
        let force = self.shared.forced_now(force, now);
        self.shared.reusing_forced.store(pressed && !force, Ordering::SeqCst);
        let shared = self.shared.clone();
        let busy = BusyGuard { shared: shared.clone(), flag: BusyFlag::Checking };
        let paths = self.paths.clone();
        let platform = self.platform;
        let options = self.options();
        let pre = options.prereleases;
        let mirror = if self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable { local_mirror() } else { None };
        std::thread::spawn(move || {
            let busy = busy;
            shared.emit(Event::CheckStarted);
            if shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable {
                crate::hosted_catalog::load_once(Some(&http), &paths);
                shared.emit(Event::CatalogUpdated);
            }
            let (feed, fallback) = if shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
                (None, Fallback { trust_feed: false, github_every: EARLY_GITHUB_EVERY })
            } else {
                check_plan(&http, mirror.as_ref(), pre, &paths, &shared)
            };
            // Notices on their own thread: a slow notices file must never hold up release checks.
            // Stable only: the Pre-Release context has its own storage and never loads notices.
            if shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable {
                let (notice_shared, notice_http, notice_paths) = (shared.clone(), http.clone(), paths.clone());
                std::thread::spawn(move || {
                    if let Some(n) = crate::notices::load(Some(&notice_http), &notice_paths, crate::now()) {
                        notice_shared.emit(Event::Notices { announcements: n.announcements, alerts: n.alerts });
                    }
                });
            }
            let now = crate::now();
            let queue = Mutex::new(
                artcraft_launcher_core::hosted_catalog::apps()
                    .iter()
                    .filter(|a| platform.is_none_or(|p| a.runs_on(p.os)))
                    .filter(|a| {
                        shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable
                            || a.kind == artcraft_launcher_core::catalog::AppKind::Official
                    })
                    .chain(std::iter::once(&LAUNCHER))
                    .filter(|app| {
                        shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable
                            || platform.is_some_and(|platform| crate::nightly_profile::for_app(app, platform).is_ok())
                    })
                    .collect::<Vec<&CraftApp>>(),
            );
            let sources = sweep(&queue, &mirror, feed.as_ref(), fallback, &source, &shared, &paths, (platform, &options), force, now, Some(&http));
            source.save();
            {
                let answered = shared.answered.lock().unwrap_or_else(PoisonError::into_inner);
                if artcraft_launcher_core::hosted_catalog::apps()
                    .iter()
                    .filter(|a| platform.is_none_or(|p| a.runs_on(p.os)))
                    .all(|a| answered.contains(a.id))
                {
                    shared.loaded.store(true, Ordering::SeqCst);
                }
            }
            // Nightly adapters are unavailable: do not probe stable/native package machinery.
            if shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable {
                for app in artcraft_launcher_core::hosted_catalog::apps().iter().filter(|a| platform.is_none_or(|p| a.runs_on(p.os))) {
                    report_partial(app, &shared, &paths, platform);
                }
            }
            let source_used = source_used_of(&sources);
            let at = crate::now();
            *shared.last_check.lock().unwrap_or_else(PoisonError::into_inner) = Some(at);
            let rate_limited_until = limit_notice(source_used, source.rate_limited_until());
            drop(busy);
            shared.emit(Event::CheckFinished { at, source: source_used, rate_limited_until });
        });
    }

    /// Admission precedes job, download and batch reservations; each worker rechecks it.
    fn refuse_unavailable_nightly_job(&self, app_id: &str, work: &Work) -> bool {
        if self.shared.channel != artcraft_launcher_core::nightly::InstallChannel::Nightly {
            return false;
        }
        let admitted = CraftApp::by_id(app_id).ok_or_else(|| "Unknown nightly app.".to_owned()).and_then(|app| {
            let platform = self.platform.ok_or("This nightly platform is not supported.")?;
            crate::nightly_profile::for_app(app, platform)?;
            match work {
                Work::Install(None) => {
                    if !self.options().nightly_enabled {
                        return Err("Developer build installs and updates are off. Retained copies and data are kept.".into());
                    }
                    selected_nightly(app, &self.shared, platform)?;
                    Ok(())
                }
                Work::Uninstall | Work::Launch(_) => {
                    if !self.shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).apps.contains_key(app.id)
                        && !self.paths.manifest().is_file()
                    {
                        return Err("No owned nightly copy is recorded.".into());
                    }
                    Ok(())
                }
                Work::Replace(_) | Work::Install(Some(_)) | Work::Chosen(_) => {
                    Err("Nightly cannot replace a Stable/custom path or join a shared installer batch.".into())
                }
            }
        });
        if let Err(error) = admitted {
            self.shared.emit(Event::JobFinished { app: app_id.into(), result: Err(error) });
            true
        } else {
            false
        }
    }

    /// Starts the job, or returns `false` without starting one: the app is unknown, doesn't run
    /// on this platform, is already running a job, or the launcher is restarting into an update.
    fn start_job(&self, app_id: &str, work: Work) -> bool {
        let Some(spawn) = self.prepare_job(app_id, work) else {
            return false;
        };
        spawn();
        true
    }

    /// Gets a job ready without running it: the checks of [`Self::start_job`], the app's entry in
    /// the running jobs and, for an install, its place in the download line. Returns what starts
    /// its thread, or `None` where `start_job` would return `false`; dropping it instead gives the
    /// entry and the place back. A batch prepares every member before starting any, so no worker
    /// can reach a free download slot ahead of a row above it that hasn't taken its place yet.
    fn prepare_job(&self, app_id: &str, work: Work) -> Option<Box<dyn FnOnce() + Send>> {
        // This is also called by bulk installs: a guard only in start_job is bypassable.
        if self.refuse_unavailable_nightly_job(app_id, &work) {
            return None;
        }
        let uninstall = matches!(work, Work::Uninstall);
        let launching = matches!(work, Work::Launch(_));
        let app = CraftApp::by_id(app_id)?;
        if self.platform.is_some_and(|p| !app.runs_on(p.os)) {
            return None;
        }
        log::info!(
            "{}: starting {} job",
            app.name,
            if uninstall {
                "uninstall"
            } else if launching {
                "open"
            } else {
                "install/update"
            }
        );
        #[cfg(test)]
        tests::note_job_preparation("job reservation");
        let stop = Arc::new(Stop::default());
        match self.shared.reserve_job(app.id, stop.clone()) {
            Admission::AlreadyActive => return None,
            Admission::Restarting => {
                let result = Err("ArtCraft Launcher is restarting to finish its update. Try again once it's back.".into());
                self.shared.emit(Event::JobFinished { app: app.id.to_string(), result });
                return None;
            }
            Admission::Reserved => {}
        }
        let shared = self.shared.clone();
        let job = JobGuard { shared: shared.clone(), app_id: app.id.to_string() };
        let paths = self.paths.clone();
        let http = self.http.clone();
        let platform = self.platform;
        let options = self.options();
        let source = self.source.clone();
        // Taken now, before the job's thread runs, so apps started together (Update all) download
        // in screen order whichever job reaches its download first.
        let place = (!uninstall && !launching).then(|| {
            #[cfg(test)]
            tests::note_job_preparation("download queue");
            install::DOWNLOADS.join(app.id)
        });
        let spawn = self.spawn.clone();
        Some(Box::new(move || {
            spawn(Box::new(move || {
                let job = job;
                let existing = shared.installed.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned();
                let record = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).apps.get(app.id).cloned();
                // What stopped an install, for the app's card.
                let mut failure = None;
                let result = if let Work::Launch(args) = &work {
                    run_nightly_launch(app, &shared, &paths, platform, &stop, args)
                } else if uninstall && shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
                    shared.emit(Event::Progress { app: app.id.to_owned(), phase: JobPhase::Removing });
                    run_nightly_remove(app, &shared, &paths, platform, &stop)
                } else if uninstall {
                    shared.emit(Event::Progress { app: app.id.to_string(), phase: JobPhase::Removing });
                    let mut phase = |phase| shared.emit(Event::Progress { app: app.id.to_string(), phase });
                    match &existing {
                        Some(inst) => install::uninstall(app, inst, record.as_ref(), &paths, &stop, &mut phase).map(|()| {
                            let mut m = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner);
                            let released = m.releases.remove(app.id).is_some();
                            if m.apps.remove(app.id).is_some() || released {
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
                        Work::Uninstall | Work::Launch(_) | Work::Chosen(_) => (None, None),
                    };
                    let by_name = match &work {
                        Work::Chosen(chosen) => Some(chosen),
                        _ => None,
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
                        &mut failure,
                        batch,
                        place,
                        by_name,
                    );
                    // Stopped by the connection rather than by the user: say so, and how to go on.
                    // Still a pause (`protocol::is_paused`), so the UI keeps the request by name.
                    match r {
                        Err(e) if is_paused(&e) && stop.check().is_ok() => Err(PAUSED_BY_CONNECTION.into()),
                        other => other,
                    }
                };
                // Look again: the result is whatever is on disk now.
                let manifest = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).clone();
                let found = if shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
                    manifest.apps.get(app.id).and_then(|record| crate::nightly::owned_installation(app, record, &paths, platform?).ok())
                } else {
                    shared.host.detect(app, &manifest, &shared.host.snapshot())
                };
                // One installer call can cover several apps, and fail after some of them went in.
                let latest = shared.latest.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned();
                let result = match (result, latest) {
                    (Err(e), Some(release))
                        if shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable
                            && !uninstall
                            && release.version.as_ref().is_some_and(|v| landed_despite(&e, existing.as_ref(), found.as_ref(), v)) =>
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
                // One poll sees both: a Blocked alone would offer Retry while the job still runs.
                let blocked =
                    (!uninstall && !launching).then(|| Event::Blocked { app: app.id.to_string(), failure: failure.filter(|_| result.is_err()) });
                job.finish(blocked.into_iter().chain([Event::JobFinished { app: app.id.to_string(), result }]));
            }));
        }))
    }

    /// Starts one install per app, together (see [`Command::InstallAll`]): where the platform
    /// needs a permission prompt, the batch shares one — each member waits for the others'
    /// downloads before the system installer is asked once. Apps unknown or already running a
    /// job are left out; a batch of one installs the same as a single [`Command::Install`].
    fn install_all(&self, apps: &[String]) {
        if self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
            let mut seen = std::collections::HashSet::new();
            let prepared: Vec<_> = apps
                .iter()
                .filter(|id| CraftApp::by_id(id).is_some() && seen.insert(id.as_str()))
                .filter_map(|id| self.prepare_job(id, Work::Install(None)))
                .collect();
            for start in prepared {
                start();
            }
            return; // Never call INSTALLER.gather for a private portable adapter.
        }
        let running = self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        let mut ids: Vec<&str> = Vec::new();
        for id in apps {
            let Some(app) = CraftApp::by_id(id) else {
                continue;
            };
            if running.contains_key(app.id) || ids.contains(&app.id) {
                continue;
            }
            ids.push(app.id);
        }
        drop(running);
        #[cfg(any(target_os = "linux", windows))]
        let batch = (!ids.is_empty()).then(|| {
            #[cfg(test)]
            tests::note_job_preparation("installer gather");
            install::INSTALLER.gather(ids.len())
        });
        #[cfg(not(any(target_os = "linux", windows)))]
        let batch = None;
        // Every member takes its place in the download line before any worker starts.
        let ready: Vec<_> = ids.iter().filter_map(|id| self.prepare_job(id, Work::Install(batch.clone()))).collect();
        let started = ready.len();
        for spawn in ready {
            spawn();
        }
        // Members that could not start after all (another job raced in for them) are not coming:
        // their places in the batch are given back, so the rest don't wait for them.
        for _ in started..ids.len() {
            if let Some(batch) = &batch {
                batch.leave();
            }
        }
    }

    /// What updating this launcher to the newer release takes, when this copy can replace itself
    /// ([`crate::self_update::is_self_updating`]) and the release has the file the update
    /// downloads. Otherwise the UI offers the download.
    fn launcher_update_job(&self) -> Option<(crate::self_update::Install, Release, String, Platform)> {
        let install = self.shared.self_install.get().cloned().flatten()?;
        let platform = self.platform?;
        if !self.shared.self_updating.get().copied().unwrap_or(false) {
            return None;
        }
        let (release, repo) = self.shared.launcher_release.lock().unwrap_or_else(PoisonError::into_inner).clone()?;
        crate::self_update::installs_from(&install, &release, platform).then_some((install, release, repo, platform))
    }

    /// Downloads the newer launcher, puts it in place and asks the UI to quit so it can start.
    fn update_launcher(&self) {
        let id = LAUNCHER_ID.to_string();
        let refuse = || {
            self.shared.emit(Event::JobFinished {
                app: id.clone(),
                result: Err("this copy of ArtCraft Launcher can't update itself; download the new version instead".into()),
            });
        };
        if self.shared.self_install.get().cloned().flatten().is_none() || self.platform.is_none() {
            return refuse();
        }
        if self.shared.launcher_release.lock().unwrap_or_else(PoisonError::into_inner).is_none() {
            return;
        }
        let Some((install, release, repo, platform)) = self.launcher_update_job() else {
            return refuse();
        };
        let stop = Arc::new(Stop::default());
        if self.shared.reserve_job(&id, stop.clone()) != Admission::Reserved {
            return;
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
                claim_backend_restart(&shared, &stop, RESTART_WAIT, &mut waiting)
            };
            let result = match http.as_ref() {
                Some(http) => crate::self_update::update(&install, &release, &repo, platform, http, &paths, &stop, &mut phase, &last),
                None => Err("secure connections aren't available on this system".into()),
            };
            finish_launcher_update(&shared, job, result);
        });
    }

    fn nightly_context_with_paths(&self, paths: Result<Paths, String>) -> Option<Box<dyn Backend>> {
        if self.shared.channel != artcraft_launcher_core::nightly::InstallChannel::Stable {
            return None;
        }
        match paths.and_then(|paths| {
            crate::nightly_profile::validate_paths(&paths)?;
            Ok(paths)
        }) {
            Ok(paths) => Some(Box::new(self.nightly_with_paths(paths))),
            Err(reason) => Some(Box::new(RefusedNightlyBackend::new(reason, self.platform, self.shared.waker.clone()))),
        }
    }

    fn nightly_with_paths(&self, paths: Paths) -> Self {
        let waker = self.shared.waker.clone();
        Self::with_lifecycle(artcraft_launcher_core::nightly::InstallChannel::Nightly, move || waker(), paths, self.shared.lifecycle.clone())
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
    Launch(Vec<String>),
    /// Install the release the person picked by name (see [`Command::InstallChosen`]).
    Chosen(Chosen),
}

/// The release the person picked by name in the version dialog, and the release they agreed may be
/// put back instead of the installed version if going back fails ([`Event::GoBackFallback`]).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Chosen {
    tag: String,
    fallback: Option<String>,
}

/// Whether `release` may be installed on `track`. The app stays part of the call because the
/// track is its own choice; that a Nightly is never an ordinary release is now shared policy.
fn release_matches_track(_app: &CraftApp, release: &Release, track: &VersionTrack, prereleases: bool) -> bool {
    match track {
        VersionTrack::Stable => {
            artcraft_launcher_core::release::ordinary_release_allowed(release)
                && (prereleases || (!release.prerelease && release.version.as_ref().is_some_and(version::is_stable)))
        }
        VersionTrack::Nightly => artcraft_launcher_core::release::is_nightly(release),
        VersionTrack::Pinned(tag) => &release.tag == tag,
    }
}

/// Whether `chosen` is a version picked by name that is older than the installed copy: the pinned
/// version, or the release `by_name` says the person picked in the version dialog
/// ([`Command::InstallChosen`]), on any track. A release a check found on its own (the latest one)
/// never is, so a feed can't send an app back to an older build.
fn is_chosen_downgrade(track: &VersionTrack, chosen: &Release, existing: Option<&Installed>, by_name: Option<&Chosen>) -> bool {
    let named = by_name.is_some_and(|c| c.tag == chosen.tag) || matches!(track, VersionTrack::Pinned(tag) if chosen.tag == *tag);
    let (Some(chosen_v), Some(installed_v)) = (chosen.version.as_ref(), existing.and_then(|i| i.version.as_ref())) else {
        return false;
    };
    named && version::is_newer(installed_v, chosen_v)
}

/// Why going back can't start yet.
#[derive(Debug, PartialEq, Eq)]
enum NoWayBack {
    /// Nothing changed, and nothing will until something else does.
    Refused(String),
    /// Only this release can be put back
    /// ([`RestorePoint::SameNumbers`](artcraft_launcher_core::rollback::RestorePoint::SameNumbers)): the person decides.
    Ask(Box<Release>),
}

/// For a version chosen by name that is older than the installed copy: the release to put back if
/// the downgrade fails ([`artcraft_launcher_core::rollback::restore_point`]), the installed
/// version's own or, once the person agreed to it (`by_name`'s `fallback`), a pre-release with its
/// numbers. `recorded`: the launcher's own record says which release the copy is. `None` for
/// anything else, without asking `list`.
fn downgrade_previous(
    app: &CraftApp,
    track: &VersionTrack,
    chosen: &Release,
    existing: Option<&Installed>,
    recorded: bool,
    by_name: Option<&Chosen>,
    list: impl FnOnce() -> Result<Vec<Release>, String>,
) -> Result<Option<Release>, NoWayBack> {
    use artcraft_launcher_core::rollback::{RestorePoint, restore_point};
    let Some(installed) = existing.filter(|_| is_chosen_downgrade(track, chosen, existing, by_name)) else {
        return Ok(None);
    };
    let installed_v = installed.version.as_ref().map(ToString::to_string).unwrap_or_default();
    let releases = list().map_err(|e| {
        NoWayBack::Refused(format!("couldn't find {} {installed_v} to put back if going back fails ({e}), so nothing was changed.", app.name))
    })?;
    match restore_point(&releases, installed, chosen, recorded) {
        RestorePoint::Exact(previous) => Ok(Some(previous.clone())),
        RestorePoint::SameNumbers(fallback) if by_name.and_then(|c| c.fallback.as_deref()) == Some(fallback.tag.as_str()) => {
            Ok(Some(fallback.clone()))
        }
        RestorePoint::SameNumbers(fallback) => Err(NoWayBack::Ask(Box::new(fallback.clone()))),
        RestorePoint::None => Err(NoWayBack::Refused(format!(
            "{} {installed_v} isn't published any more, so it couldn't be put back if going back fails. Nothing was changed.",
            app.name
        ))),
    }
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

/// Publish under the same consent lock Configure uses, so OFF (including OFF then ON)
/// invalidates old generations before any promotion. Indeterminate keeps the old selection
/// visibly stale, but cannot refresh its successful timestamp or install capability.
fn finish_nightly_result(
    app: &CraftApp,
    platform: Platform,
    generation: u64,
    mut outcome: crate::nightly_lookup::Outcome,
    shared: &Shared,
    paths: &Paths,
) {
    use crate::nightly_lookup::Outcome;
    let _reporting = shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
    let options = shared.options.lock().unwrap_or_else(PoisonError::into_inner);
    let mut state = shared.nightly_state.lock().unwrap_or_else(PoisonError::into_inner);
    if !options.nightly_enabled || state.generation != generation {
        return;
    }
    let key = crate::nightly_lookup::key(app, platform);
    let now = crate::now();
    if let Outcome::Verified(next) = &outcome {
        let previous = state.record(&key).tag.and_then(|tag| version::parse(&tag));
        if previous.as_ref().zip(next.release().version.as_ref()).is_some_and(|(old, next)| version::is_newer(old, next)) {
            outcome = Outcome::Indeterminate(crate::http::NightlyHttpError {
                over_budget: false,
                why: "Nightly discovery would regress the previous verified SemVer selection; it was kept stale.".into(),
                retry_at: None,
            });
        }
    }
    let mut record = match state.persist_outcome(paths, &key, now, &outcome) {
        Ok(record) => record,
        Err(error) => {
            outcome = Outcome::Indeterminate(crate::http::NightlyHttpError {
                over_budget: false,
                why: format!("Could not persist Nightly result metadata: {error}"),
                retry_at: state.record(&key).retry_at,
            });
            state.record(&key)
        }
    };
    let latest = match outcome {
        Outcome::Verified(verified) => {
            let release = verified.release().clone();
            shared.nightly_verified.lock().unwrap_or_else(PoisonError::into_inner).insert(app.id.into(), *verified);
            shared.store_latest(app.id, &release);
            LatestRelease::Found(release)
        }
        Outcome::NoCompatibleBuild => {
            shared.nightly_verified.lock().unwrap_or_else(PoisonError::into_inner).remove(app.id);
            shared.latest.lock().unwrap_or_else(PoisonError::into_inner).remove(app.id);
            LatestRelease::None
        }
        Outcome::Indeterminate(error) => {
            record.complete = false;
            record.message = Some(error.why.clone());
            LatestRelease::Failed(error.why)
        }
    };
    let installable = nightly_installable(app, shared, Some(platform), record.complete && state.fresh(&key, now));
    shared.emit_all([
        Event::Installable { app: app.id.into(), installable },
        Event::Latest { app: app.id.into(), latest, from: Origin::GitHub },
        Event::NightlyStatus { app: app.id.into(), status: record.status(record.complete) },
    ]);
}

/// A verified target, current consent and the owned private program must agree before effects.
/// This reads only the Nightly namespace: it never asks a stable package manager or adoption probe.
struct NightlyInstallPlan {
    verified: crate::nightly::VerifiedRelease,
    existing: Option<Installed>,
    previous: Option<crate::manifest::Record>,
}

fn nightly_install_plan(app: &CraftApp, shared: &Shared, paths: &Paths, platform: Option<Platform>) -> Result<NightlyInstallPlan, String> {
    if !shared.options.lock().unwrap_or_else(PoisonError::into_inner).nightly_enabled {
        return Err("Developer build installs and updates are off. Retained copies and data are kept.".into());
    }
    let platform = platform.ok_or("This nightly target is not supported on this system.")?;
    let verified = selected_nightly(app, shared, platform)?;
    let adapter = crate::nightly_profile::for_app(app, platform)?;
    adapter.validate_profile(paths)?;
    if verified.adapter() != adapter {
        return Err("The verified nightly belongs to a different profile adapter.".into());
    }
    crate::nightly_profile::validate_download(paths, verified.asset())?;
    let target = verified.release().version.as_ref().ok_or("The verified nightly has no exact version.")?;
    let record = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).apps.get(app.id).cloned();
    let existing = record.as_ref().map(|record| crate::nightly::owned_installation(app, record, paths, platform)).transpose()?;
    if let Some(installed) = existing.as_ref() {
        if installed.version.as_ref().is_none_or(|current| !version::is_update(target, current)) {
            return Err("The retained nightly is already at this version or newer; manual downgrades are not available.".into());
        }
    } else {
        let program = paths.apps().join(app.id);
        match std::fs::symlink_metadata(&program) {
            Ok(_) => return Err("The private program folder already exists without an owned nightly record; it was left unchanged.".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(NightlyInstallPlan { verified, existing, previous: record })
}

/// Runs only the verified private portable transaction; its commit can never update Stable's
/// manifest. Consent is checked again at the transactional commit, including after queue waits.
fn run_nightly_install(app: &CraftApp, shared: &Shared, platform: Option<Platform>, job: &mut install::Job<'_>) -> Result<String, String> {
    #[cfg(test)]
    if let Some(hook) = REVIEW_NIGHTLY_INSTALL.with(|hook| hook.borrow_mut().take()) {
        return hook(job);
    }
    let paths = job.paths;
    let adapter = crate::nightly_profile::for_app(app, platform.ok_or("This nightly platform is not supported.")?)?;
    let lease = crate::nightly_transaction::Lease::acquire(paths, adapter)?;
    store_nightly_manifest(shared, lease.manifest()?);
    let NightlyInstallPlan { verified, existing, previous } = nightly_install_plan(app, shared, paths, platform)?;
    let platform = platform.ok_or("This nightly platform is not supported.")?;
    let mut committed = false;
    let mut commit_record = |record: &crate::manifest::Record| {
        crate::nightly::owned_installation(app, record, paths, platform)?;
        let consent = shared.options.lock().unwrap_or_else(PoisonError::into_inner);
        if !consent.nightly_enabled {
            return Err("Developer builds were turned off; the pending install was not committed.".into());
        }
        let updated = lease.publish(previous.as_ref(), Some(record))?;
        committed = true;
        drop(consent);
        store_nightly_manifest(shared, updated);
        Ok(())
    };
    let result = install::install_nightly_at(&verified, existing.as_ref(), previous.as_ref(), job, Some(&mut commit_record), &lease);
    // Even an error can follow a real atomic manifest replacement. Never report the old
    // in-memory record as authoritative after the transaction has classified that boundary.
    store_nightly_manifest(shared, lease.manifest()?);
    let record = result?;
    if record.is_none() || !committed {
        return Err("The nightly transaction did not commit its private record.".into());
    }
    Ok(installed_text(app, existing.is_some(), verified.release()))
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
    if app.has_separate_nightly_channel()
        && (artcraft_launcher_core::release::separate_channel_nightly(&release) || artcraft_launcher_core::release::is_nightly_track(track))
    {
        return Err("Nightly builds use the separate developer channel; the stable copy was left unchanged.".into());
    }
    if !release_matches_track(app, &release, track, options.prereleases) {
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

/// Plans and runs an install or update of `app` from its latest known release, and returns the
/// message to show. `place` is the job's spot in the download line, used when the download starts.
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
    failure: &mut Option<InstallFailure>,
    batch: Option<crate::queue::Gather>,
    place: Option<crate::downloads::Place<'static>>,
    by_name: Option<&Chosen>,
) -> Result<String, String> {
    if shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
        if approved.is_some() || batch.is_some() || by_name.is_some() {
            return Err("Nightly cannot use Stable replacement or shared installer-batch permission.".into());
        }
        let locations = Default::default();
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        let mut phase = |phase| {
            if matches!(phase, JobPhase::Downloading { done, total } if done != total && done != 0) && last_emit.elapsed() < Duration::from_millis(50)
            {
                return;
            }
            last_emit = Instant::now();
            shared.emit(Event::Progress { app: app.id.to_owned(), phase });
        };
        let mut job = install::Job {
            http: http.ok_or("Secure connections are not available.")?,
            paths,
            stop,
            phase: &mut phase,
            install_locations: &locations,
            replace: false,
            approved_path: None,
            downgrade: None,
            batch: None,
            place,
            failure: None,
            put_back: None,
        };
        let result = run_nightly_install(app, shared, platform, &mut job);
        *failure = job.failure.take();
        return result;
    }
    // The ways out below that never reach the installer (a missing release, the overwrite
    // question, a paused download) free the rest of the batch at once; from `install_at` on, the
    // same guard does it there.
    let mut batch = install::BatchPlace(batch);
    if let Some(update) = repository_update(app, existing, options) {
        // A repository-managed Flatpak never joins the batch's installer call: leave before its
        // own update (network, maybe a prompt) so the ready members don't wait for it.
        drop(batch);
        let inst = update?;
        // Its repository decides the version: a release picked by name can't be installed here.
        if by_name.is_some() {
            return Err(install::managed_elsewhere(app, inst, false).unwrap_or_else(|| "Update this app through its Flatpak repository".into()));
        }
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
                (Some(before), Some(after)) if before.trim() == after.trim() => {
                    format!("{} is already up to date", app.name)
                }
                (Some(_), Some(_)) => {
                    format!("{} was updated through its Flatpak repository", app.name)
                }
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
    // The person picked a release by name: install that one, or nothing.
    if let Some(chosen) = by_name.filter(|c| c.tag != release.tag) {
        return Err(format!("{} {} isn't the version to install any more, so nothing was changed. Choose the version again.", app.name, chosen.tag));
    }
    let http = http.ok_or("secure connections aren't available on this system")?;
    let track = options.versions.get(app.id).unwrap_or(&VersionTrack::Stable);
    // Going back to a version chosen by name. A macOS bundle, a launcher AppImage or Flatpak keeps the old
    // copy until the new one is in place. Otherwise the installed version's own package is found
    // first, so a downgrade that fails can put it back. Without it nothing changes.
    let keeps_old_copy = install::keeps_old_copy(kind, existing, record);
    let previous = if keeps_old_copy {
        None
    } else {
        // The launcher's own record of which release this registration is (see
        // `detect::with_recorded_release`): such a copy is never taken for a pre-release.
        // `existing` is the copy after detection, so a release candidate the launcher recorded
        // already reads as that candidate; `record_describes` matches the numbers-only version
        // Windows reports and is then false. That is harmless: `restore_point` finds the published
        // candidate itself as the exact release and never needs the same-numbers fallback.
        let recorded = {
            let manifest = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner);
            recorded_copy(&manifest, app, existing)
        };
        let previous = downgrade_previous(app, track, &release, existing, recorded, by_name, || {
            source
                .ok_or_else(|| "secure connections aren't available on this system".to_string())?
                .versions(&release_query(load_feed(paths).as_ref(), app))
        });
        match previous {
            Ok(previous) => previous,
            Err(NoWayBack::Refused(why)) => return Err(why),
            // Asked first, like an overwrite: the UI's question answers it.
            Err(NoWayBack::Ask(fallback)) => {
                // Only asked when that release has a package to put back.
                if planned_asset(app, &fallback, platform, options, existing, record).is_none_or(|(_, k)| k != kind) {
                    return Err(format!("{} {} has no matching download to put back, so nothing was changed.", app.name, fallback.version_label()));
                }
                let installed = existing.and_then(|i| i.version.as_ref()).map(ToString::to_string).unwrap_or_default();
                shared.emit(Event::GoBackFallback {
                    app: app.id.to_string(),
                    installed: installed.clone(),
                    chosen: release.tag.clone(),
                    fallback: fallback.tag.clone(),
                });
                return Err(format!("{} {installed} isn't published, so going back waits for your answer. Nothing was changed.", app.name));
            }
        }
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
        is_chosen_downgrade(track, &release, existing, by_name).then_some(install::Downgrade::KeepOldCopy)
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
        failure: None,
        batch: batch.take(),
        place,
        put_back: None,
    };
    let mut commit_record = |record: &crate::manifest::Record| {
        let mut manifest = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner);
        let mut updated = manifest.clone();
        updated.apps.insert(app.id.to_string(), record.clone());
        updated.save(&paths.manifest())?;
        *manifest = updated;
        Ok(())
    };
    let installed = install::install_at(app, &release, &asset, kind, existing, record, &mut job, Some(&mut commit_record));
    *failure = job.failure.take();
    let new_record = match installed {
        Ok(new_record) => new_record,
        Err(e) => {
            // A failed Windows Installer downgrade that put `previous` back reports which
            // registration that package is in now; only then is a release recorded for it.
            if let Some(put_back) = job.put_back.take()
                && let Some(previous) = previous.as_ref()
            {
                remember_put_back(app, shared, paths, previous, &put_back);
            }
            return Err(e);
        }
    };
    // Windows Installer keeps only the numbers of the version: remember which release this was.
    let released =
        if new_record.is_none() && kind == PackageKind::Msi { installed_release(&release, shared.host.registered(app).as_ref()) } else { None };
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
        let release_changed = match released {
            Some(r) => m.releases.insert(app.id.to_string(), r.clone()).as_ref() != Some(&r),
            None => m.releases.remove(app.id).is_some(),
        };
        if (changed || release_changed)
            && let Err(e) = m.save(&paths.manifest())
        {
            log::warn!("couldn't save the install record: {e}");
        }
    }
    Ok(installed_text(app, existing.is_some(), &release))
}

/// What the launcher keeps of the Windows Installer copy it just installed from `release` (see
/// [`InstalledRelease`](crate::manifest::InstalledRelease)): the release, tied to the registration
/// `found` that is there now, when that copy has the release's numbers. `None` when the system
/// shows no such copy, so nothing unverified is ever recorded.
fn installed_release(release: &Release, found: Option<&Installed>) -> Option<crate::manifest::InstalledRelease> {
    let found = found.filter(|f| f.source == artcraft_launcher_core::InstallSource::Msi)?;
    let (seen, full) = (found.version.as_ref()?, release.version.as_ref()?);
    let package = found.package.clone().filter(|code| !code.is_empty())?;
    ((seen.major, seen.minor, seen.patch) == (full.major, full.minor, full.patch) && (seen.pre.is_empty() || seen == full))
        .then(|| crate::manifest::InstalledRelease { tag: release.tag.clone(), version: full.to_string(), package })
}

/// Whether the launcher's record says which release the installed copy `existing` is, read
/// exactly as detection reads it ([`crate::detect::record_describes`], numbers included): only
/// then may the fallback decision conclude anything from it. A record whose numbers don't match
/// the copy describes another release, not this one.
fn recorded_copy(manifest: &Manifest, app: &CraftApp, existing: Option<&Installed>) -> bool {
    crate::detect::record_describes(existing, manifest.releases.get(app.id))
}

/// After a Windows Installer downgrade that failed and put `previous` back: the install reports
/// the registration that package is in now (the one product code, carrying the previous release's
/// numbers, that the put-back is; see [`install::PutBack`]), and that release is recorded for
/// exactly that registration. Nothing is read off the registry here, and a put-back after a
/// removal that stopped partway reports nothing at all — a registration it left behind that
/// happens to have the same numbers never gets the release, and the copy that was there before
/// keeps its own.
fn remember_put_back(app: &CraftApp, shared: &Shared, paths: &Paths, previous: &Release, put_back: &install::PutBack) {
    let Some(version) = previous.version.as_ref() else { return };
    let record =
        crate::manifest::InstalledRelease { tag: previous.tag.clone(), version: version.to_string(), package: put_back.product_code.clone() };
    let mut m = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner);
    if m.releases.get(app.id) == Some(&record) {
        return;
    }
    m.releases.insert(app.id.to_string(), record);
    if let Err(e) = m.save(&paths.manifest()) {
        log::warn!("couldn't save the install record: {e}");
    }
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
    !is_paused(error) && error != CANCELLED && error != INSTALL_CANCELLED && install::failed_in_shared_call(error) && landed(before, after, latest)
}

/// Whether an install took, whatever its job reported: `latest` is there now and wasn't before.
fn landed(before: Option<&Installed>, after: Option<&Installed>, latest: &version::Version) -> bool {
    let has_latest = |copy: Option<&Installed>| copy.and_then(|i| i.version.as_ref()).is_some_and(|v| !version::is_update(latest, v));
    has_latest(after) && !has_latest(before)
}

fn finish_launcher_update(shared: &Shared, job: JobGuard, result: Result<(), String>) {
    // A failed update still owns the launcher reservation here. Clear its claim before
    // retirement can admit a new updater; success deliberately keeps restart held.
    if result.is_err() {
        shared.lifecycle.restarting.store(false, Ordering::SeqCst);
    }
    drop(job);
    #[cfg(test)]
    nightly_lifecycle_review_tests::after_update_retirement();
    match result {
        Ok(()) => shared.emit(Event::LauncherRestarting),
        Err(e) => {
            shared.emit(Event::JobFinished { app: LAUNCHER_ID.to_string(), result: Err(e) });
        }
    }
}

/// For the launcher's own update, before it replaces itself: waits (saying so through `waiting`)
/// until every other job has ended, then sets `restarting`, which keeps new jobs from starting.
/// The restart that follows would otherwise cut their installs short. The user can cancel the
/// wait (`stop`); after `limit` it gives up and names the apps still busy, since one may be
/// stuck behind a password prompt nobody has answered.
fn claim_backend_restart(shared: &Shared, stop: &Stop, limit: Duration, waiting: &mut dyn FnMut()) -> Result<(), String> {
    claim_restart(&shared.lifecycle.jobs, &shared.lifecycle.restarting, stop, limit, waiting)
}

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
            let busy = jobs
                .keys()
                .filter(|id| *id != LAUNCHER_ID)
                .map(|id| CraftApp::by_id(id.strip_prefix("nightly:").unwrap_or(id)).map_or("another app", |a| a.name))
                .collect();
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
    let system = (shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable && shared.has_copy(app)).then(|| shared.system_kinds());
    let _reporting = shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
    shared.emit_all(installable_event(app, shared, platform, system));
}

/// [`report_installable`]'s answer from the shared state as it is, with the current settings:
/// whether [`install_plan`] would let an install go ahead. `system` is what this system can
/// install, asked before `reporting` was taken; when a copy appeared since, the view its detection
/// asked for is used, so nothing is probed here. The caller holds `reporting`.
fn installable_event(app: &CraftApp, shared: &Shared, platform: Option<Platform>, system: Option<install::SystemKinds>) -> Option<Event> {
    if shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
        // UI eligibility uses only typed tokens and the owned-detection cache. Admission and
        // workers validate the filesystem separately, before any reservation or mutation.
        let enabled = shared.options.lock().unwrap_or_else(PoisonError::into_inner).nightly_enabled;
        let fresh = platform.is_some_and(|platform| {
            shared.nightly_state.lock().unwrap_or_else(PoisonError::into_inner).fresh(&crate::nightly_lookup::key(app, platform), crate::now())
        });
        let installable = nightly_installable(app, shared, platform, enabled && fresh);
        return Some(Event::Installable { app: app.id.into(), installable });
    }
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

fn nightly_installable(app: &CraftApp, shared: &Shared, platform: Option<Platform>, enabled: bool) -> bool {
    let adapter = platform.and_then(|platform| crate::nightly_profile::for_app(app, platform).ok());
    let verified = shared.nightly_verified.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned();
    let previous = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).apps.contains_key(app.id);
    let existing = shared.installed.lock().unwrap_or_else(PoisonError::into_inner).get(app.id).cloned();
    enabled
        && adapter.zip(verified.as_ref()).is_some_and(|(adapter, verified)| {
            adapter == verified.adapter()
                && verified.release().version.as_ref().is_some_and(|target| {
                    !previous
                        || existing
                            .as_ref()
                            .and_then(|installed| installed.version.as_ref())
                            .is_some_and(|current| version::is_update(target, current))
                })
        })
}

/// Records `app`'s latest release and tells the UI, with whether an install would go ahead with
/// it, in one batch: the UI never sees the release without its answer.
fn publish_latest(app: &CraftApp, latest: LatestRelease, from: Origin, shared: &Shared, platform: Option<Platform>) {
    let system = (shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable && shared.has_copy(app)).then(|| shared.system_kinds());
    let _reporting = shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
    if let LatestRelease::Found(r) = &latest {
        shared.store_latest(app.id, r);
    } else if shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
        shared.nightly_verified.lock().unwrap_or_else(PoisonError::into_inner).remove(app.id);
        shared.latest.lock().unwrap_or_else(PoisonError::into_inner).remove(app.id);
    }
    let installable = installable_event(app, shared, platform, system);
    shared.emit_all(installable.into_iter().chain([Event::Latest { app: app.id.to_string(), latest, from }]));
}

/// Records what is installed of `app` (`found`) and tells the UI, with whether an install would
/// go ahead to replace it, in one batch.
fn publish_detected(app: &CraftApp, found: Option<Installed>, shared: &Shared, platform: Option<Platform>) {
    let system = (shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable && found.is_some()).then(|| shared.system_kinds());
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
    if shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
        let plan = nightly_install_plan(app, shared, paths, platform).ok()?;
        let asset = plan.verified.asset();
        return Some((install::download_path(paths, asset), plan.verified.adapter().kind(), asset.size));
    }
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
    } else {
        remote_plan(pre, feed_and_fallback(http, paths, shared))
    }
}

/// [`check_plan`] without a local mirror, given what the feed said. The feed serves stable
/// releases only, so with pre-releases on it can't settle an answer that may be a beta or RC. It
/// still stands in between GitHub's asks, which keep to the feed's own pace (hourly per app once
/// everything has loaded), not every check (#713).
fn remote_plan(pre: bool, (feed, fallback): (Option<Feed>, Fallback)) -> (Option<Feed>, Fallback) {
    if pre { (feed, Fallback { trust_feed: false, ..fallback }) } else { (feed, fallback) }
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
    http: Option<&Http>,
) -> Vec<Source> {
    let sources = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| {
                loop {
                    let Some(app) = queue.lock().unwrap_or_else(PoisonError::into_inner).pop() else {
                        break;
                    };
                    let track = if shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
                        &VersionTrack::Nightly
                    } else if app.id == LAUNCHER_ID {
                        &options.launcher_version
                    } else {
                        options.versions.get(app.id).unwrap_or(&VersionTrack::Stable)
                    };
                    let (latest, from, used) = match mirror {
                        Some(_) => mirror_chosen(app, feed, now, track),
                        None if *track != VersionTrack::Stable => track_answer(app, track, feed, source.as_ref(), shared, force, now),
                        None => latest_for(app, feed, fallback, source.as_ref(), shared, options.prereleases, force, now),
                    };
                    let latest = if shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
                        latest.map(|answer| match (answer, http, platform) {
                            (LatestRelease::Found(release), Some(http), Some(platform)) => match crate::nightly::verify(app, release, http, platform)
                            {
                                Ok(verified) => {
                                    let release = verified.release().clone();
                                    shared.nightly_verified.lock().unwrap_or_else(PoisonError::into_inner).insert(app.id.into(), verified);
                                    LatestRelease::Found(release)
                                }
                                Err(reason) => LatestRelease::Failed(reason),
                            },
                            (LatestRelease::Found(_), _, _) => LatestRelease::Failed("Secure nightly descriptor verification is unavailable.".into()),
                            (other, _, _) => other,
                        })
                    } else {
                        latest
                    };
                    if let Some(used) = used {
                        sources.lock().unwrap_or_else(PoisonError::into_inner).push(used);
                    }
                    check_app(app, latest, from, &feed_repo(feed, app), shared, paths, platform);
                }
            });
        }
    });
    sources.into_inner().unwrap_or_else(PoisonError::into_inner)
}

/// Publishes one app's answer to the shared state, or handles the launcher's own update check
/// first (it doesn't appear in the grid, so it has no card of its own to publish to). `repo` is the
/// repo this check asked about for `app` (the feed's rename, if any).
fn check_app(
    app: &'static CraftApp,
    latest: Option<LatestRelease>,
    from: Origin,
    repo: &str,
    shared: &Arc<Shared>,
    paths: &Paths,
    platform: Option<Platform>,
) {
    if app.id == LAUNCHER_ID && shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable {
        launcher_update(paths, latest, repo, shared);
        return;
    }
    let Some(latest) = latest else { return };
    if !matches!(latest, LatestRelease::Failed(_)) {
        shared.answered.lock().unwrap_or_else(PoisonError::into_inner).insert(app.id.to_string());
    }
    publish_latest(app, latest, from, shared, platform);
}

/// Offers the launcher's own update when its answer is a newer release than the running one,
/// keeping `repo`, the repo it was read from, for the download's link check.
fn launcher_update(paths: &Paths, latest: Option<LatestRelease>, repo: &str, shared: &Arc<Shared>) {
    let Some(LatestRelease::Found(r)) = latest else {
        return;
    };
    let (Some(newest), Some(current)) = (r.version.as_ref(), crate::self_update::current_version()) else {
        return;
    };
    if version::is_newer(newest, &current) && !crate::self_update::just_updated_to(paths, &r) {
        shared.self_install.get_or_init(crate::self_update::current);
        shared.self_updating.get_or_init(crate::self_update::is_self_updating);
        *shared.launcher_release.lock().unwrap_or_else(PoisonError::into_inner) = Some((r.clone(), repo.to_string()));
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
            // looks past it when GitHub's quota-free redirect names something newer (#91); one
            // reusing the last forced check's answers then takes GitHub's saved answer.
            Some(FeedAnswer::Fresh(found))
                if !((force || shared.reusing_forced.load(Ordering::SeqCst)) && feed_is_behind(source, &repo, found.as_ref())) =>
            {
                return (Some(found.map_or(LatestRelease::None, LatestRelease::Found)), Origin::ArtCraftApi, Some(Source::Feed));
            }
            Some(FeedAnswer::Fresh(_) | FeedAnswer::AskGitHub) | None => {}
        }
    }
    let due = {
        let mut asked = shared.github_asked.lock().unwrap_or_else(PoisonError::into_inner);
        let key = asked_key(app, pre);
        let due = force || asked.get(&key).is_none_or(|t| now - t >= fallback.github_every);
        if due {
            asked.insert(key, now);
        }
        due
    };
    if due {
        let query = ReleaseQuery::for_app(app, repo).with_priority(github_priority(app, &VersionTrack::Stable, shared, force));
        let answer = source.latest(&query, pre);
        // GitHub couldn't answer (its limit, the launcher's budget, the network), or fell back
        // to a saved release older than the feed's: the feed's stable release stands.
        let live = answer.source == Source::GitHubApi && !matches!(answer.latest, LatestRelease::Failed(_));
        if !live && let Some(stable) = feed_if_newer(feed, app, now, Some(&answer.latest)) {
            return (Some(LatestRelease::Found(stable)), Origin::ArtCraftApi, Some(Source::Feed));
        }
        (Some(answer.latest), Origin::GitHub, Some(answer.source))
    } else {
        let cached = source.cached(&ReleaseQuery::for_app(app, repo), pre);
        // A stable release the feed already lists outranks an older one (or a beta or RC) GitHub
        // named last time, without waiting for GitHub's next turn; so does it when nothing is
        // saved from GitHub for this track yet (pre-releases just turned on). A check reusing a
        // forced one's answers keeps whichever of the two is newer.
        match feed_if_newer(feed, app, now, cached.as_ref()) {
            Some(stable) => (Some(LatestRelease::Found(stable)), Origin::ArtCraftApi, Some(Source::Feed)),
            None => (cached, Origin::GitHub, None),
        }
    }
}

/// The feed's fresh stable release for `app`, when it is newer than `github`'s answer, or that
/// answer is missing, empty or failed.
fn feed_if_newer(feed: Option<&Feed>, app: &CraftApp, now: i64, github: Option<&LatestRelease>) -> Option<Release> {
    let Some(FeedAnswer::Fresh(Some(stable))) = feed.map(|f| f.answer(app, now)) else { return None };
    let newer = match github {
        Some(LatestRelease::Found(saved)) => {
            stable.version.as_ref().zip(saved.version.as_ref()).is_some_and(|(stable, saved)| version::is_newer(stable, saved))
        }
        _ => true,
    };
    newer.then_some(stable)
}

impl Shared {
    /// Whether a check asked for with `force` at `now` really skips the per-app throttles. A
    /// forced check pressed again within [`FORCED_REUSE`] of the last forced one still finishes,
    /// but reuses what that one asked GitHub. Only a forced check opens the window: one right after
    /// an automatic check (at launch, say) or a settings change still asks.
    fn forced_now(&self, force: bool, now: i64) -> bool {
        if !force {
            return false;
        }
        let mut last = self.last_forced.lock().unwrap_or_else(PoisonError::into_inner);
        if last.is_some_and(|t| (0..FORCED_REUSE).contains(&(now - t))) {
            return false;
        }
        *last = Some(now);
        true
    }
}

/// The release a pinned or nightly `track` follows. A pinned version is saved once (see
/// [`ReleaseSource::chosen`]); the nightly is asked about at most every [`NIGHTLY_TRACK_EVERY`],
/// with the saved version list standing in between (#713).
fn track_answer(
    app: &CraftApp,
    track: &VersionTrack,
    feed: Option<&Feed>,
    source: &dyn ReleaseSource,
    shared: &Shared,
    force: bool,
    now: i64,
) -> (Option<LatestRelease>, Origin, Option<Source>) {
    let query = release_query(feed, app).with_priority(github_priority(app, track, shared, force));
    if *track == VersionTrack::Nightly {
        // Its own record, apart from the stable track's: asking about one doesn't stand in for
        // the other when the app switches tracks.
        let key = format!("{}@nightly", app.id);
        let mut asked = shared.github_asked.lock().unwrap_or_else(PoisonError::into_inner);
        let due = force || asked.get(&key).is_none_or(|t| now - t >= NIGHTLY_TRACK_EVERY);
        if !due && let Some(saved) = source.saved_choice(&query, track) {
            return (Some(saved), Origin::GitHub, None);
        }
        asked.insert(key, now);
        drop(asked);
        if due {
            // A due or pressed check asks GitHub now, past the saved version list however recent.
            let answer = source.chosen_within(&query, track, 0);
            return (Some(answer.latest), Origin::GitHub, Some(answer.source));
        }
    }
    let answer = source.chosen(&query, track);
    (Some(answer.latest), Origin::GitHub, Some(answer.source))
}

/// Who a GitHub request about `app` is for, when the GitHub budget runs short.
fn github_priority(app: &CraftApp, track: &VersionTrack, shared: &Shared, force: bool) -> Priority {
    // A reused forced check still asks as the user: it can hit GitHub when the feed is behind,
    // and must not be held back as a background request.
    if force || shared.reusing_forced.load(Ordering::SeqCst) {
        Priority::User
    } else if *track == VersionTrack::Nightly {
        Priority::NightlyTrack
    } else if shared.installed.lock().unwrap_or_else(PoisonError::into_inner).contains_key(app.id) {
        Priority::Installed
    } else {
        Priority::NotInstalled
    }
}

/// The key in [`Shared::github_asked`] for `app` with pre-releases on or off: the two settings
/// get different answers from GitHub, so asking under one doesn't stand in for the other.
fn asked_key(app: &CraftApp, pre: bool) -> String {
    if pre { format!("{}+pre", app.id) } else { app.id.to_string() }
}

/// Whether GitHub names a newer stable release of `repo` than the feed's `latest` (`None`: the
/// feed says there is none). `false` whenever the redirect can't be read: the feed then stands.
fn feed_is_behind(source: &dyn ReleaseSource, repo: &str, latest: Option<&Release>) -> bool {
    let Some(newest) = source.newest_tag(repo).and_then(|tag| version::parse(&tag)) else {
        return false;
    };
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

/// What to ask GitHub about `app`: the feed may rename its repo, but the separate-channel policy
/// always follows the app itself.
fn release_query(feed: Option<&Feed>, app: &CraftApp) -> ReleaseQuery {
    ReleaseQuery::for_app(app, feed_repo(feed, app))
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

fn feed_url_override_allowed(url: &str, local_mirror: bool) -> bool {
    // A loopback feed is a staging affordance, so it needs the same opt-in as the local mirror;
    // release builds must not accept a plaintext feed chosen by the environment.
    local_mirror && (is_loopback(url) || url.starts_with("https://"))
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
        let Some(details) = artcraft_launcher_core::details::AppDetails::by_id(app) else {
            return;
        };
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
    fn has_active_jobs(&self) -> bool {
        !self.shared.jobs.lock().unwrap_or_else(PoisonError::into_inner).is_empty()
    }

    fn nightly_available(&self) -> bool {
        self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly
            && self.platform.is_some_and(crate::nightly_profile::new_installs_available)
    }

    fn nightly_context(&self) -> Option<Box<dyn Backend>> {
        // An unavailable home or aliased namespace is not an ephemeral Nightly profile and
        // must never silently select the stable context or temporary storage.
        let channel = artcraft_launcher_core::nightly::InstallChannel::Nightly;
        self.nightly_context_with_paths(Paths::new_resolved().map(|paths| paths.for_channel(channel)))
    }

    /// Carries out a command from the UI. Long work (checks, installs, removals) runs on its own
    /// thread.
    fn send(&mut self, command: Command) {
        if self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly {
            match &command {
                Command::UpdateLauncher
                | Command::CreateDesktopShortcut { .. }
                | Command::McpState
                | Command::McpSetAgents { .. }
                | Command::McpSetClient { .. }
                | Command::McpRestartClient { .. }
                | Command::ListVersions { .. } => return,
                Command::Launch { app, args } => {
                    self.start_job(app, Work::Launch(args.clone()));
                    return;
                }
                Command::Reveal { app } => {
                    let result = CraftApp::by_id(app).ok_or_else(|| "Unknown nightly app.".to_owned()).and_then(|definition| {
                        let record = self
                            .shared
                            .manifest
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .apps
                            .get(app)
                            .cloned()
                            .ok_or("No owned nightly copy is recorded.")?;
                        crate::nightly::owned_installation(
                            definition,
                            &record,
                            &self.paths,
                            self.platform.ok_or("This nightly platform is not supported.")?,
                        )?;
                        crate::process::reveal(&record.path)
                    });
                    if let Err(error) = result {
                        self.shared.emit(Event::Notice { app: app.clone(), result: Err(error) });
                    }
                    return;
                }
                _ => {}
            }
        }
        match command {
            Command::CheckForUpdates { force } => self.check(force),
            Command::Detect => self.detect_all(),
            Command::ListVersions { app } => {
                let Some(def) = (if app == LAUNCHER_ID { Some(&LAUNCHER) } else { CraftApp::by_id(&app) }) else {
                    return;
                };
                let source = self.source.clone();
                let shared = self.shared.clone();
                let query = release_query(load_feed(&self.paths).as_ref(), def);
                std::thread::spawn(move || {
                    // Opening Choose version is the user's own look: a list saved more than
                    // CHOOSER_FRESH ago is asked for again, as before the list moved to disk.
                    let result = source
                        .ok_or("secure connections aren't available on this system".to_string())
                        .and_then(|s| s.versions_within(&query, crate::source::CHOOSER_FRESH));
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
            Command::InstallChosen { app, tag, fallback } => {
                self.start_job(&app, Work::Chosen(Chosen { tag, fallback }));
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
                let Some(def) = CraftApp::by_id(&app) else {
                    return;
                };
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
            Command::CreateDesktopShortcut { app } => {
                let Some(def) = CraftApp::by_id(&app) else {
                    return;
                };
                let Some(inst) = self.installed(&app) else {
                    self.shared.emit(Event::Notice { app, result: Err(format!("{} isn't installed", def.name)) });
                    return;
                };
                let path = std::path::PathBuf::from(&inst.location);
                let name = def.name;
                let app_id = app;
                let shared = self.shared.clone();
                // PowerShell-free .lnk write can still take a moment; don't freeze the UI thread.
                std::thread::spawn(move || {
                    let result = match crate::process::create_desktop_shortcut(name, &path) {
                        Ok(()) => Ok(format!("Desktop shortcut created for {name}")),
                        Err(e) if e.starts_with("A desktop shortcut named ") => Err(e),
                        Err(e) => Err(format!("Couldn't create the desktop shortcut: {e}")),
                    };
                    shared.emit(Event::Notice { app: app_id, result });
                });
            }
            Command::UpdateLauncher => self.update_launcher(),
            Command::FetchScreenshots { app } => self.fetch_screenshots(&app),
            Command::DownloadOrder(order) => {
                if self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable {
                    install::DOWNLOADS.set_order(order);
                }
            }
            Command::Configure(mut options) => {
                if self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable {
                    // Old settings must not make an app's own updater move its stable copy to nightly.
                    options.versions.retain(|id, track| {
                        CraftApp::by_id(id).is_none_or(|app| artcraft_launcher_core::release::ordinary_track_allowed(app, track))
                    });
                    if !artcraft_launcher_core::release::ordinary_track_allowed(&LAUNCHER, &options.launcher_version) {
                        options.launcher_version = VersionTrack::Stable;
                    }
                } else {
                    options.prereleases = true;
                    options.launcher_version = VersionTrack::Nightly;
                    options.versions = artcraft_launcher_core::hosted_catalog::apps()
                        .iter()
                        .filter(|a| a.kind == artcraft_launcher_core::catalog::AppKind::Official)
                        .map(|a| (a.id.into(), VersionTrack::Nightly))
                        .collect();
                    options.install_locations = Default::default(); // No stable/custom destination may leak into this context.
                }
                let changed_pre = {
                    let mut o = self.shared.options.lock().unwrap_or_else(PoisonError::into_inner);
                    let changed = o.prereleases != options.prereleases
                        || o.versions != options.versions
                        || o.launcher_version != options.launcher_version
                        || o.nightly_enabled != options.nightly_enabled;
                    if self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable {
                        install::DOWNLOADS.set_limit(options.simultaneous_downloads.into());
                    }
                    if self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Nightly && o.nightly_enabled != options.nightly_enabled
                    {
                        self.shared.nightly_state.lock().unwrap_or_else(PoisonError::into_inner).invalidate();
                    }
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
                    self.check(self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable);
                }
            }
            cmd @ (Command::McpState | Command::McpSetAgents { .. } | Command::McpSetClient { .. } | Command::McpRestartClient { .. }) => {
                // Detection can run package-manager queries for seconds (flatpak on Linux):
                // never on the UI thread that sends commands.
                let shared = self.shared.clone();
                std::thread::spawn(move || run_mcp(&shared, cmd));
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
        Capabilities {
            manage_apps: true,
            reveal_label,
            self_update: self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable && self.launcher_update_job().is_some(),
            desktop_shortcuts: self.shared.channel == artcraft_launcher_core::nightly::InstallChannel::Stable
                && matches!(self.platform.map(|p| p.os), Some(Os::Windows)),
        }
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

/// The AI agents state, from a fresh detection of the installed apps.
fn mcp_state(shared: &Shared) -> Event {
    mcp_state_for(&detected_apps(shared))
}

/// [`mcp_state`] for apps already detected.
fn mcp_state_for(detected: &[crate::agents::InstalledApp]) -> Event {
    Event::Mcp { agents: crate::agents::state(detected) }
}

/// Serialises MCP commands and every publication of the MCP state, so a state read before a
/// command can't arrive after that command's own (newer) state.
static MCP_WORK: Mutex<()> = Mutex::new(());

/// One AI agents command, on a worker thread: detection can run package-manager queries for
/// seconds (flatpak on Linux). Commands run one at a time; the new state is sent when done.
fn run_mcp(shared: &Shared, cmd: Command) {
    let _guard = MCP_WORK.lock().unwrap_or_else(PoisonError::into_inner);
    let apps = detected_apps(shared);
    let result = match cmd {
        Command::McpSetAgents { enabled } => crate::agents::set_enabled(enabled, &apps),
        Command::McpSetClient { client, added } => crate::agents::set_client(client, added),
        Command::McpRestartClient { client } => crate::agents::restart_client(client),
        _ => Ok(()),
    };
    if let Err(message) = result {
        shared.emit(Event::McpError { message });
    }
    shared.emit(mcp_state_for(&apps));
}

/// The installed apps with their CLIs, from a fresh detection snapshot.
fn detected_apps(shared: &Shared) -> Vec<crate::agents::InstalledApp> {
    let manifest = shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let snapshot = shared.host.snapshot();
    crate::agents::installed_apps(|app| shared.host.detect(app, &manifest, &snapshot))
}

#[cfg(test)]
#[path = "backend_nightly_phase1_tests.rs"]
mod nightly_phase1_tests;

#[cfg(all(test, target_os = "linux"))]
#[path = "backend_nightly_review_tests.rs"]
mod nightly_review_tests;

#[cfg(test)]
#[path = "backend_nightly_lifecycle_review_tests.rs"]
mod nightly_lifecycle_review_tests;

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;
    use std::sync::mpsc;

    use artcraft_launcher_core::InstallSource;

    use artcraft_launcher_core::protocol::LatestRelease;

    use super::*;

    use crate::source::Answer;

    // Per-test-thread, opt-in observation of the real effect sites. It cannot race unrelated
    // tests or alter global queue order/limits. A stable positive oracle checks it is not vacuous.
    std::thread_local! {
        static JOB_PREPARATION_EFFECTS: std::cell::RefCell<Option<Vec<&'static str>>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn note_job_preparation(effect: &'static str) {
        JOB_PREPARATION_EFFECTS.with(|effects| {
            if let Some(effects) = effects.borrow_mut().as_mut() {
                effects.push(effect);
            }
        });
    }

    std::thread_local! {
        /// For each freed job slot on this thread, whether that app's `JobFinished` was queued by then.
        static SLOT_FREED: std::cell::RefCell<Option<Vec<bool>>> = const { std::cell::RefCell::new(None) };
    }

    pub(super) fn note_slot_freed(shared: &Shared, app: &str) {
        SLOT_FREED.with(|freed| {
            if let Some(freed) = freed.borrow_mut().as_mut() {
                let queued = shared.events.lock().unwrap().iter().any(|event| matches!(event, Event::JobFinished { app: a, .. } if a == app));
                freed.push(queued);
            }
        });
    }

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

    /// A repository-managed Flatpak in an "Update all" leaves the batch before its own update
    /// runs, so a slow or blocked Flatpak never holds back the system packages waiting to share
    /// the batch's prompt.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_repository_flatpak_leaves_the_batch_before_updating() {
        let gather = install::INSTALLER.gather(1);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut shared = shared();
        shared.waker = Arc::new({
            let (gather, seen) = (gather.clone(), seen.clone());
            move || seen.lock().unwrap().push(gather.open())
        });
        let app = CraftApp::by_id("photocraft").unwrap();
        let installed = Installed {
            version: version::parse("0.5.0"),
            source: InstallSource::Flatpak,
            location: String::new(),
            package: None,
            externally_managed: true,
            origin: None,
            installation: Some("user".into()),
        };
        let dir = tempfile::tempdir().unwrap();
        let stop = Stop::default();
        stop.cancel();
        let options = BackendOptions::default();
        let result = run_install(
            app,
            &shared,
            &Paths::under(dir.path()),
            None,
            None,
            &options,
            Some(&installed),
            None,
            &stop,
            None,
            None,
            &mut None,
            Some(gather.clone()),
            None,
            None,
        );
        assert_eq!(result, Err(artcraft_launcher_core::protocol::CANCELLED.into()));
        let seen = seen.lock().unwrap();
        assert!(!seen.is_empty(), "the Flatpak update reports its progress");
        assert!(seen.iter().all(|&open| open), "the batch was already free when the Flatpak update began");
    }

    fn tagged(v: &str) -> Release {
        Release { tag: format!("v{v}"), version: version::parse(v), ..Default::default() }
    }

    /// A Flatpak its repository updates can't install a release picked by name: the repository
    /// decides the version, so the request is refused before `flatpak update` runs.
    #[test]
    fn a_release_picked_by_name_never_runs_the_repository_update() {
        let seen = Arc::new(Mutex::new(0));
        let mut shared = shared();
        shared.waker = Arc::new({
            let seen = seen.clone();
            move || *seen.lock().unwrap() += 1
        });
        let app = CraftApp::by_id("photocraft").unwrap();
        let installed = Installed {
            version: version::parse("0.5.0"),
            source: InstallSource::Flatpak,
            location: String::new(),
            package: None,
            externally_managed: true,
            origin: None,
            installation: Some("user".into()),
        };
        let dir = tempfile::tempdir().unwrap();
        let picked = Chosen { tag: "v0.4.0".into(), fallback: None };
        let (options, stop) = (BackendOptions::default(), Stop::default());
        let paths = Paths::under(dir.path());
        let result =
            run_install(app, &shared, &paths, None, None, &options, Some(&installed), None, &stop, None, None, &mut None, None, None, Some(&picked));
        assert!(result.as_ref().is_err_and(|e| e.as_str() != artcraft_launcher_core::protocol::CANCELLED), "{result:?}");
        assert_eq!(*seen.lock().unwrap(), 0, "nothing started: no progress was reported");
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
        assert!(is_chosen_downgrade(&pinned, &tagged("0.13.0"), Some(&installed), None));
        assert!(!is_chosen_downgrade(&VersionTrack::Stable, &tagged("0.13.0"), Some(&installed), None));
        assert!(!is_chosen_downgrade(&VersionTrack::Pinned("v0.15.0".into()), &tagged("0.15.0"), Some(&installed), None));
        assert!(!is_chosen_downgrade(&pinned, &tagged("0.12.0"), Some(&installed), None));
        assert!(!is_chosen_downgrade(&pinned, &tagged("0.13.0"), None, None));
        // The latest release, picked by name in the version dialog, goes back; found by a check, it doesn't.
        let picked = Chosen { tag: "v0.13.0".into(), fallback: None };
        assert!(is_chosen_downgrade(&VersionTrack::Stable, &tagged("0.13.0"), Some(&installed), Some(&picked)));
        assert!(!is_chosen_downgrade(&VersionTrack::Stable, &tagged("0.12.0"), Some(&installed), Some(&picked)));
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
        assert_eq!(downgrade_previous(app, &VersionTrack::Stable, &tagged("0.13.0"), Some(&installed), false, None, never), Ok(None));
        assert_eq!(
            downgrade_previous(app, &VersionTrack::Pinned("v0.15.0".into()), &tagged("0.15.0"), Some(&installed), false, None, never),
            Ok(None)
        );
        assert_eq!(downgrade_previous(app, &pinned, &tagged("0.13.0"), None, false, None, never), Ok(None));
        assert_eq!(downgrade_previous(app, &pinned, &tagged("0.12.0"), Some(&installed), false, None, never), Ok(None));
        // Going back finds the installed version's own release.
        let found =
            downgrade_previous(app, &pinned, &tagged("0.13.0"), Some(&installed), false, None, || Ok(vec![tagged("0.14.0"), tagged("0.13.0")]));
        assert_eq!(found.unwrap().map(|r| r.tag), Some("v0.14.0".to_string()));
        // Without it, or without the list, the downgrade doesn't start.
        let refused = |result: Result<Option<Release>, NoWayBack>| match result {
            Err(NoWayBack::Refused(why)) => why,
            other => panic!("not refused: {other:?}"),
        };
        let gone = refused(downgrade_previous(app, &pinned, &tagged("0.13.0"), Some(&installed), false, None, || Ok(vec![tagged("0.13.0")])));
        assert!(gone.contains("FilmCraft 0.14.0 isn't published any more") && gone.contains("Nothing was changed"), "{gone}");
        let offline = refused(downgrade_previous(app, &pinned, &tagged("0.13.0"), Some(&installed), false, None, || Err("offline".into())));
        assert!(offline.contains("(offline)") && offline.contains("nothing was changed"), "{offline}");
    }

    fn msi_copy(v: &str) -> Installed {
        Installed {
            version: version::parse(v),
            source: InstallSource::Msi,
            location: String::new(),
            package: Some("{6B1C1C1E-0E0B-4B6B-9C59-2C1D2E3F4A5B}".into()),
            externally_managed: false,
            origin: None,
            installation: None,
        }
    }

    fn published() -> Vec<Release> {
        let rc = Release { prerelease: true, ..tagged("0.24.0-rc.1") };
        vec![rc, tagged("0.23.1"), tagged("0.23.0")]
    }

    /// The release candidate the launcher installed reads back as itself (see
    /// `detect::with_recorded_release`), so going back from it to the latest release finds the
    /// candidate to put back, whether the latest release was picked in the dialog or pinned.
    #[test]
    fn going_back_from_an_installed_release_candidate_puts_the_candidate_back() {
        let app = CraftApp::by_id("photocraft").unwrap();
        let rc = msi_copy("0.24.0-rc.1");
        let picked = Chosen { tag: "v0.23.1".into(), fallback: None };
        let back = downgrade_previous(app, &VersionTrack::Stable, &tagged("0.23.1"), Some(&rc), false, Some(&picked), || Ok(published()));
        assert_eq!(back.unwrap().map(|r| r.tag).as_deref(), Some("v0.24.0-rc.1"));
        let pinned = VersionTrack::Pinned("v0.23.0".into());
        let back = downgrade_previous(app, &pinned, &tagged("0.23.0"), Some(&rc), false, None, || Ok(published()));
        assert_eq!(back.unwrap().map(|r| r.tag).as_deref(), Some("v0.24.0-rc.1"));
        // The candidate itself, chosen again, is the installed version: nothing to go back to.
        let same = VersionTrack::Pinned("v0.24.0-rc.1".into());
        let never = || -> Result<Vec<Release>, String> { panic!("no list is needed") };
        assert_eq!(downgrade_previous(app, &same, &published()[0], Some(&rc), false, None, never), Ok(None));
    }

    /// A copy Windows registered as 0.24.0, with no release of that version and no record of what
    /// was installed: going back to the latest release asks first, then puts back the candidate
    /// with those numbers if it fails. An answer about another release asks again, and going back
    /// to anything but the latest release stays refused.
    #[test]
    fn a_stuck_installer_copy_goes_back_to_the_latest_release_only_once_the_person_agrees() {
        let app = CraftApp::by_id("photocraft").unwrap();
        let stuck = msi_copy("0.24.0");
        let latest = tagged("0.23.1");
        let picked = |fallback: Option<&str>| Chosen { tag: "v0.23.1".into(), fallback: fallback.map(String::from) };
        let ask = downgrade_previous(app, &VersionTrack::Stable, &latest, Some(&stuck), false, Some(&picked(None)), || Ok(published()));
        assert!(matches!(&ask, Err(NoWayBack::Ask(fallback)) if fallback.tag == "v0.24.0-rc.1"), "{ask:?}");
        let agreed =
            downgrade_previous(app, &VersionTrack::Stable, &latest, Some(&stuck), false, Some(&picked(Some("v0.24.0-rc.1"))), || Ok(published()));
        assert_eq!(agreed.unwrap().map(|r| r.tag).as_deref(), Some("v0.24.0-rc.1"));
        // An answer about another release isn't an answer about this one.
        let other = downgrade_previous(app, &VersionTrack::Stable, &latest, Some(&stuck), false, Some(&picked(Some("v0.23.0"))), || Ok(published()));
        assert!(matches!(other, Err(NoWayBack::Ask(_))), "{other:?}");
        // A copy the launcher recorded as the final 0.24.0 (since withdrawn) is never taken for the candidate.
        let recorded = downgrade_previous(app, &VersionTrack::Stable, &latest, Some(&stuck), true, Some(&picked(None)), || Ok(published()));
        assert!(matches!(recorded, Err(NoWayBack::Refused(_))), "{recorded:?}");
        // Not the latest release: refused as before.
        let older =
            downgrade_previous(app, &VersionTrack::Pinned("v0.23.0".into()), &tagged("0.23.0"), Some(&stuck), false, None, || Ok(published()));
        assert!(matches!(older, Err(NoWayBack::Refused(_))), "{older:?}");
    }

    /// What the launcher records after a Windows Installer install: the release it checked, tied to
    /// the registration that install left, and nothing when the system shows no copy with its numbers.
    #[test]
    fn an_installer_install_records_its_release_only_for_the_copy_it_left() {
        let rc = Release { prerelease: true, ..tagged("0.24.0-rc.1") };
        let recorded = installed_release(&rc, Some(&msi_copy("0.24.0"))).unwrap();
        assert_eq!((recorded.tag.as_str(), recorded.version.as_str()), ("v0.24.0-rc.1", "0.24.0-rc.1"));
        assert_eq!(Some(recorded.package), msi_copy("0.24.0").package);
        assert_eq!(installed_release(&rc, Some(&msi_copy("0.23.1"))), None, "another version is there");
        assert_eq!(installed_release(&rc, Some(&Installed { package: None, ..msi_copy("0.24.0") })), None, "no registration to tie it to");
        assert_eq!(installed_release(&rc, Some(&Installed { source: InstallSource::Launcher, ..msi_copy("0.24.0") })), None);
        assert_eq!(installed_release(&rc, None), None);
    }

    /// A put-back is recorded for the registration the installer reports it created, never for a
    /// leftover registration that happens to have the previous version's numbers.
    #[test]
    fn a_put_back_is_recorded_for_the_registration_the_installer_reported() {
        const CREATED: &str = "{6B1C1C1E-0E0B-4B6B-9C59-2C1D2E3F4A5B}";
        const LEFTOVER: &str = "{11111111-2222-3333-4444-555555555555}";
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let mut shared = shared();
        // A different registration with the previous version's numbers is still on the machine.
        shared.host = HostDetection::Stub(Arc::new(|_| Some(Installed { package: Some(LEFTOVER.into()), ..msi_copy("0.24.0") })));
        let app = CraftApp::by_id("photocraft").unwrap();
        let previous = Release { prerelease: true, ..tagged("0.24.0-rc.1") };
        remember_put_back(app, &shared, &paths, &previous, &install::PutBack { product_code: CREATED.into() });
        let recorded = shared.manifest.lock().unwrap().releases.get(app.id).cloned();
        let wanted = crate::manifest::InstalledRelease { tag: "v0.24.0-rc.1".into(), version: "0.24.0-rc.1".into(), package: CREATED.into() };
        assert_eq!(recorded, Some(wanted.clone()), "the reported registration, not the leftover one");
        assert_eq!(Manifest::load(&paths.manifest()).releases.get(app.id), Some(&wanted), "saved for the next detection");
        // Detection then reads that copy as the release the put-back installed.
        let read = crate::detect::with_recorded_release(Installed { package: Some(CREATED.into()), ..msi_copy("0.24.0") }, Some(&wanted));
        assert_eq!(read.version.map(|v| v.to_string()).as_deref(), Some("0.24.0-rc.1"));
    }

    /// The fallback decision asks whether the record describes the copy the way detection reads
    /// it: a record whose numbers don't match the copy decides nothing, so the same-numbered
    /// fallback is offered rather than refused.
    #[test]
    fn only_a_record_of_the_copy_itself_decides_what_going_back_may_put_back() {
        let app = CraftApp::by_id("photocraft").unwrap();
        let stuck = msi_copy("0.24.0");
        let code = stuck.package.clone().unwrap();
        let record = |tag: &str, version: &str| crate::manifest::InstalledRelease { tag: tag.into(), version: version.into(), package: code.clone() };
        let mut manifest = Manifest::default();
        let latest = tagged("0.23.1");
        let picked = Chosen { tag: "v0.23.1".into(), fallback: None };
        let outcome = |manifest: &Manifest| {
            let recorded = recorded_copy(manifest, app, Some(&stuck));
            downgrade_previous(app, &VersionTrack::Stable, &latest, Some(&stuck), recorded, Some(&picked), || Ok(published()))
        };
        // The record says the copy is the final 0.24.0 (since withdrawn): never taken for the candidate.
        manifest.releases.insert(app.id.to_string(), record("v0.24.0", "0.24.0"));
        assert!(matches!(outcome(&manifest), Err(NoWayBack::Refused(_))), "{:?}", outcome(&manifest));
        // The record's numbers don't match the copy: it describes another release, so detection
        // keeps what Windows reports and the candidate with the same numbers is put up for a yes.
        manifest.releases.insert(app.id.to_string(), record("v0.23.5", "0.23.5"));
        assert!(matches!(outcome(&manifest), Err(NoWayBack::Ask(_))), "{:?}", outcome(&manifest));
        // Another registration is another copy, whatever it reports.
        let elsewhere = crate::manifest::InstalledRelease { package: "{00000000-0000-0000-0000-000000000000}".into(), ..record("v0.24.0", "0.24.0") };
        manifest.releases.insert(app.id.to_string(), elsewhere);
        assert!(matches!(outcome(&manifest), Err(NoWayBack::Ask(_))), "{:?}", outcome(&manifest));
    }

    /// Going back is a conscious choice: a check for a pinned app reports the pinned release, so
    /// a newer one never shows as an update (badge, Update all, notification or automatic install).
    #[test]
    fn a_pinned_app_is_never_offered_a_newer_release() {
        struct Published;
        impl ReleaseSource for Published {
            fn latest(&self, query: &ReleaseQuery, _prereleases: bool) -> crate::source::Answer {
                let found = CraftApp::by_id(APP).is_some_and(|a| a.repo == query.repo);
                let latest = if found { LatestRelease::Found(tagged("0.6.0")) } else { LatestRelease::None };
                crate::source::Answer { latest, source: Source::GitHubApi }
            }
            fn versions(&self, _query: &ReleaseQuery) -> Result<Vec<Release>, String> {
                Ok(vec![tagged("0.6.0"), tagged("0.5.0")])
            }
            fn cached(&self, _query: &ReleaseQuery, _prereleases: bool) -> Option<LatestRelease> {
                None
            }
            fn rate_limited_until(&self) -> Option<i64> {
                None
            }
            fn save(&self) {}
        }
        let Some(platform) = native_platform() else {
            return;
        };
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
        assert!(!release_matches_track(CraftApp::by_id("photocraft").unwrap(), &release, &VersionTrack::Nightly, false));
        assert!(!release_matches_track(CraftApp::by_id("photocraft").unwrap(), &release, &VersionTrack::Pinned("v0.7.0".into()), false));
        assert!(release_matches_track(CraftApp::by_id("photocraft").unwrap(), &release, &VersionTrack::Stable, false));
        release.tag = "v0.9.0-nightly.20261028".into();
        release.version = version::parse(&release.tag);
        release.prerelease = true;
        assert!(release_matches_track(CraftApp::by_id("photocraft").unwrap(), &release, &VersionTrack::Nightly, false));
        assert!(!release_matches_track(CraftApp::by_id("photocraft").unwrap(), &release, &VersionTrack::Stable, false));
    }

    /// A Nightly is never the Stable track's copy: not for an app with its own Nightly
    /// channel, and not for one without it (SolidCraft).
    #[test]
    fn a_nightly_is_never_a_stable_track_release_for_an_app_without_a_separate_channel() {
        let app = CraftApp::by_id("solidcraft").unwrap();
        assert!(!app.has_separate_nightly_channel(), "the community app is the non-roster example");
        let mut nightly = Release { tag: "v0.9.0-nightly.20261028".into(), version: version::parse("v0.9.0-nightly.20261028"), ..Default::default() };
        nightly.prerelease = true;
        for prereleases in [false, true] {
            assert!(!release_matches_track(app, &nightly, &VersionTrack::Stable, prereleases), "prereleases={prereleases}");
            assert!(release_matches_track(app, &nightly, &VersionTrack::Nightly, prereleases), "the explicit Nightly track still asks for it");
        }
        let mut beta = Release { tag: "v0.9.0-beta.1".into(), version: version::parse("v0.9.0-beta.1"), ..Default::default() };
        beta.prerelease = true;
        assert!(!release_matches_track(app, &beta, &VersionTrack::Stable, false), "a plain beta stays behind the Stable track");
        assert!(release_matches_track(app, &beta, &VersionTrack::Stable, true), "with pre-releases on it is an ordinary candidate");
    }

    #[test]
    fn feed_url_overrides_are_loopback_only_outside_local_mirror_builds() {
        assert!(!feed_url_override_allowed("http://127.0.0.1:8787/release_info.json", false));
        assert!(!feed_url_override_allowed("http://localhost:8787/release_info.json", false));
        assert!(feed_url_override_allowed("http://127.0.0.1:8787/release_info.json", true));
        assert!(feed_url_override_allowed("http://localhost:8787/release_info.json", true));
        assert!(!feed_url_override_allowed("https://staging.example/release_info.json", false));
        assert!(feed_url_override_allowed("https://staging.example/release_info.json", true));
        assert!(!feed_url_override_allowed("http://attacker.example/release_info.json", true));
    }

    /// GitHub, counting how often it's asked.
    #[derive(Default)]
    struct CountingGitHub(AtomicUsize);

    impl ReleaseSource for CountingGitHub {
        fn latest(&self, _query: &ReleaseQuery, _pre: bool) -> Answer {
            self.0.fetch_add(1, Ordering::SeqCst);
            Answer { latest: LatestRelease::None, source: Source::GitHubApi }
        }
        fn cached(&self, _query: &ReleaseQuery, _pre: bool) -> Option<LatestRelease> {
            None
        }
        fn rate_limited_until(&self) -> Option<i64> {
            None
        }
        fn save(&self) {}
    }

    pub(super) fn shared() -> Shared {
        Shared {
            channel: artcraft_launcher_core::nightly::InstallChannel::Stable,
            host: HostDetection::Stub(Arc::new(|_| None)),
            nightly_detection_observer: Mutex::new(None),
            events: Mutex::new(Vec::new()),
            reporting: Mutex::new(()),
            system: Mutex::new(None),
            planning: Mutex::new(()),
            waker: Arc::new(|| {}),
            latest: Mutex::new(HashMap::new()),
            nightly_verified: Mutex::new(HashMap::new()),
            nightly_state: Mutex::new(crate::nightly_lookup::State::default()),
            download_sizes: Mutex::new(HashMap::new()),
            installed: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            lifecycle: Arc::new(ProcessLifecycle::default()),
            manifest: Mutex::new(Manifest::default()),
            options: Mutex::new(BackendOptions::default()),
            checking: AtomicBool::new(false),
            detecting: AtomicBool::new(false),
            last_check: Mutex::new(None),
            last_forced: Mutex::new(None),
            reusing_forced: AtomicBool::new(false),
            launcher_release: Mutex::new(None),
            self_install: OnceLock::new(),
            self_updating: OnceLock::new(),
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

    /// With pre-releases on, GitHub is asked at the fallback's pace, not on every check (#713),
    /// and between its asks a newer stable release from the feed replaces an older saved RC.
    #[test]
    fn with_pre_releases_github_keeps_to_its_pace_and_the_feed_fills_in_newer_stables() {
        /// GitHub, counting asks, whose last answer for the app is `saved`.
        struct SavedRc(AtomicUsize, Mutex<Option<&'static str>>);
        impl ReleaseSource for SavedRc {
            fn latest(&self, _query: &ReleaseQuery, _pre: bool) -> Answer {
                self.0.fetch_add(1, Ordering::SeqCst);
                Answer { latest: self.cached(_query, true).unwrap_or(LatestRelease::None), source: Source::GitHubApi }
            }
            fn cached(&self, _query: &ReleaseQuery, _pre: bool) -> Option<LatestRelease> {
                let tag = (*self.1.lock().unwrap())?;
                Some(LatestRelease::Found(Release { tag: tag.into(), version: version::parse(tag), ..Default::default() }))
            }
            fn rate_limited_until(&self) -> Option<i64> {
                None
            }
            fn save(&self) {}
        }
        let photo = CraftApp::by_id("photocraft").unwrap();
        let (feed, shared) = (feed(), shared());
        let pre = Fallback { trust_feed: false, ..LOADED };
        let github = SavedRc(AtomicUsize::new(0), Mutex::new(Some("v0.6.0-rc.1")));
        let tag = |answer: Option<LatestRelease>| match answer {
            Some(LatestRelease::Found(r)) => r.tag,
            other => panic!("{other:?}"),
        };

        let (latest, _, used) = latest_for(photo, Some(&feed), pre, &github, &shared, true, false, NOW);
        assert_eq!((tag(latest), used), ("v0.6.0-rc.1".to_string(), Some(Source::GitHubApi)), "the first check asks GitHub");
        // Within the hour GitHub isn't asked again, and the feed's newer stable v0.6.0 wins.
        let (latest, _, used) = latest_for(photo, Some(&feed), pre, &github, &shared, true, false, NOW + 30 * 60);
        assert_eq!((tag(latest), used), ("v0.6.0".to_string(), Some(Source::Feed)));
        // A saved RC newer than the feed's stable stays.
        *github.1.lock().unwrap() = Some("v0.7.0-rc.1");
        let (latest, _, used) = latest_for(photo, Some(&feed), pre, &github, &shared, true, false, NOW + 40 * 60);
        assert_eq!((tag(latest), used), ("v0.7.0-rc.1".to_string(), None));
        assert_eq!(github.0.load(Ordering::SeqCst), 1, "one GitHub ask in the hour");
        latest_for(photo, Some(&feed), pre, &github, &shared, true, false, NOW + GITHUB_FALLBACK_EVERY);
        assert_eq!(github.0.load(Ordering::SeqCst), 2, "due again after an hour");
        // Nothing saved from GitHub for this track (pre-releases just turned on): the feed's stable.
        *github.1.lock().unwrap() = None;
        let (latest, _, used) = latest_for(photo, Some(&feed), pre, &github, &shared, true, false, NOW + GITHUB_FALLBACK_EVERY + 10 * 60);
        assert_eq!((tag(latest), used), ("v0.6.0".to_string(), Some(Source::Feed)));
    }

    /// GitHub, counting asks, with nothing saved.
    struct CountedAsks(AtomicUsize);
    impl ReleaseSource for CountedAsks {
        fn latest(&self, _query: &ReleaseQuery, _pre: bool) -> Answer {
            self.0.fetch_add(1, Ordering::SeqCst);
            Answer { latest: LatestRelease::None, source: Source::GitHubApi }
        }
        fn cached(&self, _query: &ReleaseQuery, _pre: bool) -> Option<LatestRelease> {
            None
        }
        fn rate_limited_until(&self) -> Option<i64> {
            None
        }
        fn save(&self) {}
    }

    /// The plan for a check with pre-releases on keeps GitHub to the feed's hourly pace once
    /// everything has loaded: checks every 12 minutes ask about an app once in the hour (#713).
    #[test]
    fn with_pre_releases_the_check_plan_keeps_github_to_the_hourly_pace() {
        let photo = CraftApp::by_id("photocraft").unwrap();
        let (feed, plan) = remote_plan(true, (Some(feed()), LOADED));
        assert!(!plan.trust_feed, "the feed can't settle a beta or RC");
        let (github, shared) = (CountedAsks(AtomicUsize::new(0)), shared());
        for minutes in [0, 12, 24, 36, 48] {
            latest_for(photo, feed.as_ref(), plan, &github, &shared, true, false, NOW + minutes * 60);
        }
        assert_eq!(github.0.load(Ordering::SeqCst), 1, "a check every 12 minutes asks once in the hour");
        latest_for(photo, feed.as_ref(), plan, &github, &shared, true, false, NOW + GITHUB_FALLBACK_EVERY);
        assert_eq!(github.0.load(Ordering::SeqCst), 2, "due again after an hour");
    }

    /// Asking GitHub with pre-releases off doesn't stand in for asking with them on: turning them
    /// on asks again at once, not after the hour.
    #[test]
    fn turning_pre_releases_on_asks_github_again_within_the_hour() {
        let photo = CraftApp::by_id("photocraft").unwrap();
        let (feed, shared) = (feed(), shared());
        let plan = Fallback { trust_feed: false, ..LOADED };
        let github = CountedAsks(AtomicUsize::new(0));
        latest_for(photo, Some(&feed), plan, &github, &shared, false, false, NOW);
        latest_for(photo, Some(&feed), plan, &github, &shared, true, false, NOW + 10 * 60);
        assert_eq!(github.0.load(Ordering::SeqCst), 2, "pre-releases on is its own question");
        latest_for(photo, Some(&feed), plan, &github, &shared, true, false, NOW + 20 * 60);
        latest_for(photo, Some(&feed), plan, &github, &shared, false, false, NOW + 30 * 60);
        assert_eq!(github.0.load(Ordering::SeqCst), 2, "each setting keeps its own hourly pace");
    }

    /// A forced check right after an automatic one (the one at launch) still asks GitHub; only
    /// pressing it again within two minutes of a forced one reuses that one's answers (#713).
    #[test]
    fn only_a_recent_forced_check_makes_a_forced_check_reuse_answers() {
        let shared = shared();
        *shared.last_check.lock().unwrap() = Some(NOW - 30);
        assert!(shared.forced_now(true, NOW), "an automatic check doesn't stand in for a forced one");
        assert!(!shared.forced_now(true, NOW + 60), "pressed again at once: reused");
        assert!(!shared.forced_now(false, NOW + 61));
        assert!(shared.forced_now(true, NOW + FORCED_REUSE), "asks again after the window");
    }

    /// An app following the nightly is asked about at most every three hours, the saved version
    /// list answering in between, unless the user presses check (#713).
    #[test]
    fn the_nightly_track_is_asked_about_every_three_hours() {
        #[derive(Default)]
        struct Lists(AtomicUsize);
        impl ReleaseSource for Lists {
            fn latest(&self, _query: &ReleaseQuery, _pre: bool) -> Answer {
                Answer { latest: LatestRelease::None, source: Source::GitHubApi }
            }
            fn versions(&self, _query: &ReleaseQuery) -> Result<Vec<Release>, String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new())
            }
            fn saved_choice(&self, _query: &ReleaseQuery, _track: &VersionTrack) -> Option<LatestRelease> {
                Some(LatestRelease::None)
            }
            fn cached(&self, _query: &ReleaseQuery, _pre: bool) -> Option<LatestRelease> {
                None
            }
            fn rate_limited_until(&self) -> Option<i64> {
                None
            }
            fn save(&self) {}
        }
        let (github, shared) = (Lists::default(), shared());
        let word = CraftApp::by_id("wordcraft").unwrap();
        let asked = |at: i64, force: bool| {
            track_answer(word, &VersionTrack::Nightly, None, &github, &shared, force, at);
            github.0.load(Ordering::SeqCst)
        };
        assert_eq!(asked(NOW, false), 1);
        assert_eq!(asked(NOW + 60 * 60, false), 1, "the saved list answers within the three hours");
        assert_eq!(asked(NOW + NIGHTLY_TRACK_EVERY, false), 2);
        assert_eq!(asked(NOW + NIGHTLY_TRACK_EVERY + 60, true), 3, "a pressed check asks at once");
    }

    /// A pressed check on the Nightly track asks GitHub now, past a version list saved a minute
    /// ago; without it, that list stands in (#713). The budget of 0 refuses any request, so a
    /// failure here means GitHub was asked.
    #[test]
    fn a_pressed_nightly_check_asks_github_past_a_recent_saved_list() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("releases.json");
        let word = CraftApp::by_id("wordcraft").unwrap();
        let list = vec![Release { tag: "nightly-2026-11-23".into(), prerelease: true, ..Default::default() }];
        let cache = serde_json::json!({ "entries": {}, "versions": { word.repo: [crate::now() - 60, list] } });
        std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
        let http = Http::new().unwrap().with_budget(crate::github_budget::GitHubBudget::in_memory(0)).without_token();
        let source = crate::source::GitHubSource::new(http, path);
        let shared = shared();
        let (latest, _, used) = track_answer(word, &VersionTrack::Nightly, None, &source, &shared, false, NOW);
        assert!(matches!(latest, Some(LatestRelease::Failed(_))), "a first due check asked GitHub: {latest:?}");
        assert_eq!(used, Some(Source::GitHubApi));
        let (latest, _, _) = track_answer(word, &VersionTrack::Nightly, None, &source, &shared, false, NOW + 60);
        assert!(!matches!(latest, Some(LatestRelease::Failed(_))), "the recent list stands in within the three hours: {latest:?}");
        let (latest, _, used) = track_answer(word, &VersionTrack::Nightly, None, &source, &shared, true, NOW + 120);
        assert!(matches!(latest, Some(LatestRelease::Failed(_))), "a pressed check asked GitHub: {latest:?}");
        assert_eq!(used, Some(Source::GitHubApi));
    }

    /// A due Nightly check asks GitHub even if Choose version saved a list a moment ago, so that
    /// list cannot stand in for the next three hours (#713).
    #[test]
    fn a_due_nightly_check_asks_github_past_a_recent_version_list() {
        #[derive(Default)]
        struct Lists(Mutex<Vec<Option<i64>>>);
        impl ReleaseSource for Lists {
            fn latest(&self, _query: &ReleaseQuery, _pre: bool) -> Answer {
                Answer { latest: LatestRelease::None, source: Source::GitHubApi }
            }
            fn versions(&self, query: &ReleaseQuery) -> Result<Vec<Release>, String> {
                self.versions_within(query, crate::source::VERSIONS_FRESH)
            }
            fn versions_within(&self, _query: &ReleaseQuery, max_age: i64) -> Result<Vec<Release>, String> {
                self.0.lock().unwrap().push(Some(max_age));
                Ok(Vec::new())
            }
            fn saved_choice(&self, _query: &ReleaseQuery, _track: &VersionTrack) -> Option<LatestRelease> {
                Some(LatestRelease::Found(Release { tag: "nightly-old".into(), ..Default::default() }))
            }
            fn cached(&self, _query: &ReleaseQuery, _pre: bool) -> Option<LatestRelease> {
                None
            }
            fn rate_limited_until(&self) -> Option<i64> {
                None
            }
            fn save(&self) {}
        }
        let (github, shared) = (Lists::default(), shared());
        let word = CraftApp::by_id("wordcraft").unwrap();
        track_answer(word, &VersionTrack::Nightly, None, &github, &shared, false, NOW);
        assert_eq!(*github.0.lock().unwrap(), vec![Some(0)], "a due check asks GitHub even if a list was just saved");
        track_answer(word, &VersionTrack::Nightly, None, &github, &shared, false, NOW + 60);
        assert_eq!(github.0.lock().unwrap().len(), 1, "within the three hours the saved choice stands in");
        track_answer(word, &VersionTrack::Nightly, None, &github, &shared, false, NOW + NIGHTLY_TRACK_EVERY);
        assert_eq!(*github.0.lock().unwrap(), vec![Some(0), Some(0)]);
    }

    /// The stable and Nightly tracks keep separate records of when GitHub was asked: asking about
    /// an app's stable release doesn't put off asking about its nightly after a track switch.
    #[test]
    fn the_stable_and_nightly_tracks_keep_their_own_github_pace() {
        #[derive(Default)]
        struct Lists(AtomicUsize);
        impl ReleaseSource for Lists {
            fn latest(&self, _query: &ReleaseQuery, _pre: bool) -> Answer {
                Answer { latest: LatestRelease::None, source: Source::GitHubApi }
            }
            fn versions(&self, _query: &ReleaseQuery) -> Result<Vec<Release>, String> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(Vec::new())
            }
            fn saved_choice(&self, _query: &ReleaseQuery, _track: &VersionTrack) -> Option<LatestRelease> {
                Some(LatestRelease::None)
            }
            fn cached(&self, _query: &ReleaseQuery, _pre: bool) -> Option<LatestRelease> {
                None
            }
            fn rate_limited_until(&self) -> Option<i64> {
                None
            }
            fn save(&self) {}
        }
        let (github, shared) = (Lists::default(), shared());
        let word = CraftApp::by_id("wordcraft").unwrap();
        latest_for(word, None, Fallback { trust_feed: false, ..LOADED }, &github, &shared, false, false, NOW);
        track_answer(word, &VersionTrack::Nightly, None, &github, &shared, false, NOW + 60);
        assert_eq!(github.0.load(Ordering::SeqCst), 1, "the nightly is asked about though the stable track just was");
    }

    /// Opening Choose version asks for a list saved more than 15 minutes ago again, though the
    /// background tracks let one stand for an hour (#713).
    #[test]
    fn choose_version_asks_for_a_list_older_than_a_quarter_hour() {
        struct Ages(Mutex<Vec<Option<i64>>>);
        impl ReleaseSource for Ages {
            fn latest(&self, _query: &ReleaseQuery, _pre: bool) -> Answer {
                Answer { latest: LatestRelease::None, source: Source::GitHubApi }
            }
            fn versions(&self, _query: &ReleaseQuery) -> Result<Vec<Release>, String> {
                self.0.lock().unwrap().push(None);
                Ok(Vec::new())
            }
            fn versions_within(&self, _query: &ReleaseQuery, max_age: i64) -> Result<Vec<Release>, String> {
                self.0.lock().unwrap().push(Some(max_age));
                Ok(Vec::new())
            }
            fn cached(&self, _query: &ReleaseQuery, _pre: bool) -> Option<LatestRelease> {
                None
            }
            fn rate_limited_until(&self) -> Option<i64> {
                None
            }
            fn save(&self) {}
        }
        let Some(platform) = native_platform() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let ages = Arc::new(Ages(Mutex::new(Vec::new())));
        let mut backend = backend(dir.path(), platform, Some(ages.clone()));
        backend.send(Command::ListVersions { app: "wordcraft".into() });
        let started = Instant::now();
        while !backend.poll().iter().any(|e| matches!(e, Event::Versions { .. })) {
            assert!(started.elapsed() < Duration::from_secs(20), "no version list");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(*ages.0.lock().unwrap(), vec![Some(crate::source::CHOOSER_FRESH)]);
    }

    /// A reused forced check still asks as the user, so the budget doesn't hold it back as a
    /// background request for an app that isn't installed.
    #[test]
    fn a_reused_forced_check_keeps_the_users_priority() {
        let shared = shared();
        assert_eq!(github_priority(CraftApp::by_id("photocraft").unwrap(), &VersionTrack::Stable, &shared, false), Priority::NotInstalled);
        shared.reusing_forced.store(true, Ordering::SeqCst);
        assert_eq!(github_priority(CraftApp::by_id("photocraft").unwrap(), &VersionTrack::Stable, &shared, false), Priority::User);
    }

    /// With pre-releases on, GitHub's turn refused (by the budget, say) with nothing saved leaves
    /// the feed's stable release standing, not an error.
    #[test]
    fn a_refused_github_turn_falls_back_to_the_feeds_release() {
        struct Refused;
        impl ReleaseSource for Refused {
            fn latest(&self, _query: &ReleaseQuery, _pre: bool) -> Answer {
                Answer { latest: LatestRelease::Failed("held back".into()), source: Source::GitHubApi }
            }
            fn cached(&self, _query: &ReleaseQuery, _pre: bool) -> Option<LatestRelease> {
                None
            }
            fn rate_limited_until(&self) -> Option<i64> {
                None
            }
            fn save(&self) {}
        }
        let photo = CraftApp::by_id("photocraft").unwrap();
        let plan = remote_plan(true, (None, LOADED)).1;
        let (latest, _, used) = latest_for(photo, Some(&feed()), plan, &Refused, &shared(), true, false, NOW);
        assert!(matches!(latest, Some(LatestRelease::Found(ref r)) if r.tag == "v0.6.0"), "{latest:?}");
        assert_eq!(used, Some(Source::Feed));
    }

    /// A check reusing a forced one's answers, or a GitHub turn that fell back to a saved
    /// release, keeps the feed's stable release when it is newer than what GitHub had saved.
    #[test]
    fn an_older_saved_github_answer_doesnt_hide_the_feeds_newer_release() {
        /// GitHub answering only from the release it saved, `self.0`.
        struct Saved(&'static str);
        impl Saved {
            fn saved(&self) -> LatestRelease {
                LatestRelease::Found(Release { tag: format!("v{}", self.0), version: version::parse(self.0), ..Release::default() })
            }
        }
        impl ReleaseSource for Saved {
            fn latest(&self, _query: &ReleaseQuery, _pre: bool) -> Answer {
                Answer { latest: self.saved(), source: Source::Cache }
            }
            fn cached(&self, _query: &ReleaseQuery, _pre: bool) -> Option<LatestRelease> {
                Some(self.saved())
            }
            fn rate_limited_until(&self) -> Option<i64> {
                None
            }
            fn save(&self) {}
            fn newest_tag(&self, _repo: &str) -> Option<String> {
                Some("v0.8.0".into())
            }
        }
        let photo = CraftApp::by_id("photocraft").unwrap();
        let tag = |latest: Option<LatestRelease>| match latest {
            Some(LatestRelease::Found(r)) => r.tag,
            other => format!("{other:?}"),
        };
        // Reusing a forced check: the feed's v0.6.0 is newer than the saved v0.5.0.
        let reusing = shared();
        reusing.github_asked.lock().unwrap().insert(asked_key(photo, false), NOW - 60);
        reusing.reusing_forced.store(true, Ordering::SeqCst);
        assert_eq!(tag(latest_for(photo, Some(&feed()), LOADED, &Saved("0.5.0"), &reusing, false, false, NOW).0), "v0.6.0");
        // A refused turn answered from a saved RC older than the feed's release.
        let plan = remote_plan(true, (None, LOADED)).1;
        let (latest, _, used) = latest_for(photo, Some(&feed()), plan, &Saved("0.6.0-rc.1"), &shared(), true, false, NOW);
        assert_eq!((tag(latest), used), ("v0.6.0".to_string(), Some(Source::Feed)));
        // A saved RC newer than the feed's release stands.
        let (latest, _, _) = latest_for(photo, Some(&feed()), plan, &Saved("0.7.0-rc.1"), &shared(), true, false, NOW);
        assert_eq!(tag(latest), "v0.7.0-rc.1");
    }

    /// A check pressed again within the reuse window keeps the newer release the forced check
    /// found on GitHub, rather than the feed's older one.
    #[test]
    fn a_reused_forced_check_keeps_githubs_newer_answer_over_the_feed() {
        struct Saved;
        impl ReleaseSource for Saved {
            fn latest(&self, _query: &ReleaseQuery, _pre: bool) -> Answer {
                panic!("a reused check doesn't ask the API again")
            }
            fn cached(&self, _query: &ReleaseQuery, _pre: bool) -> Option<LatestRelease> {
                Some(LatestRelease::Found(Release { tag: "v0.7.0".into(), version: version::parse("0.7.0"), ..Release::default() }))
            }
            fn rate_limited_until(&self) -> Option<i64> {
                None
            }
            fn save(&self) {}
            fn newest_tag(&self, _repo: &str) -> Option<String> {
                Some("v0.7.0".into())
            }
        }
        let photo = CraftApp::by_id("photocraft").unwrap();
        let shared = shared();
        // The forced check a minute ago asked GitHub; this press reuses it.
        shared.github_asked.lock().unwrap().insert(asked_key(photo, false), NOW - 60);
        shared.reusing_forced.store(true, Ordering::SeqCst);
        let (latest, _, _) = latest_for(photo, Some(&feed()), LOADED, &Saved, &shared, false, false, NOW);
        assert!(matches!(latest, Some(LatestRelease::Found(ref r)) if r.tag == "v0.7.0"), "{latest:?}");
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
            fn latest(&self, _query: &ReleaseQuery, _pre: bool) -> Answer {
                self.asked.fetch_add(1, Ordering::SeqCst);
                let found = Release { tag: "v0.7.0".into(), version: version::parse("0.7.0"), ..Release::default() };
                Answer { latest: LatestRelease::Found(found), source: Source::GitHubApi }
            }
            fn cached(&self, _query: &ReleaseQuery, _pre: bool) -> Option<LatestRelease> {
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

    #[test]
    fn a_nightly_context_cannot_start_a_stable_install_or_write_version_pins() {
        let root = tempfile::tempdir().unwrap();
        let platform = Platform { os: Os::Windows, arch: artcraft_launcher_core::Arch::X64 };
        let mut backend = backend(root.path(), platform, None);
        Arc::get_mut(&mut backend.shared).unwrap().channel = artcraft_launcher_core::nightly::InstallChannel::Nightly;
        backend.pins = None;
        let stable_record = root.path().join("versions.json");
        std::fs::write(&stable_record, "stable choices stay here").unwrap();
        assert!(!backend.start_job(APP, Work::Install(None)));
        backend.send(Command::Configure(BackendOptions { nightly_enabled: true, ..Default::default() }));
        assert_eq!(std::fs::read_to_string(stable_record).unwrap(), "stable choices stay here");
        assert!(backend.shared.jobs.lock().unwrap().is_empty());
    }

    #[test]
    fn stable_install_plan_rejects_nightly_even_if_old_settings_pinned_it() {
        let app = CraftApp::by_id(APP).unwrap();
        let mut release = stable("0.6.0-nightly.20261120");
        release.prerelease = true;
        let options = BackendOptions {
            versions: std::collections::BTreeMap::from([(APP.into(), VersionTrack::Pinned(release.tag.clone()))]),
            ..Default::default()
        };
        let platform = Platform { os: Os::Windows, arch: artcraft_launcher_core::Arch::X64 };
        let result = install_plan(app, Some(release), Some(platform), &options, (None, None), Asking::Install);
        assert!(result.is_err());
    }

    #[test]
    fn nightly_install_all_refuses_before_shared_effects_with_consent_on_or_off() {
        for enabled in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let stable_record = root.path().join("stable-installed.json");
            let stable_data = root.path().join("stable-user-data");
            std::fs::write(&stable_record, b"stable record stays byte exact").unwrap();
            std::fs::write(&stable_data, b"stable user data stays byte exact").unwrap();
            let mut backend = backend(root.path(), Platform { os: Os::Windows, arch: Arch::X64 }, None);
            Arc::get_mut(&mut backend.shared).unwrap().channel = artcraft_launcher_core::nightly::InstallChannel::Nightly;
            backend.pins = None;
            backend.paths = backend.paths.for_channel(artcraft_launcher_core::nightly::InstallChannel::Nightly);
            backend.shared.options.lock().unwrap().nightly_enabled = enabled;
            backend.spawn = Arc::new(|_| panic!("nightly InstallAll must not spawn, install or detect OS applications"));
            let before = backend.shared.manifest.lock().unwrap().clone();
            JOB_PREPARATION_EFFECTS.with(|effects| *effects.borrow_mut() = Some(Vec::new()));
            backend.send(Command::InstallAll { apps: vec![APP.into()] });
            let effects = JOB_PREPARATION_EFFECTS.with(|effects| effects.borrow_mut().take().unwrap());
            assert!(effects.is_empty(), "consent={enabled}: no gather, job reservation or queue participation: {effects:?}");
            assert!(backend.shared.jobs.lock().unwrap().is_empty());
            assert!(backend.shared.installed.lock().unwrap().is_empty(), "a stable OS copy must not be adopted");
            assert_eq!(*backend.shared.manifest.lock().unwrap(), before);
            let events = backend.poll();
            assert!(events.iter().any(|event| matches!(event,Event::JobFinished {app,result:Err(_)} if app==APP)));
            assert!(
                !events.iter().any(|event| matches!(event, Event::Detected { .. } | Event::Progress { .. })),
                "no worker means no OS Snapshot/detect publication"
            );
            assert_eq!(std::fs::read(stable_record).unwrap(), b"stable record stays byte exact");
            assert_eq!(std::fs::read(stable_data).unwrap(), b"stable user data stays byte exact");
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 2, "no new data/cache/manifest directories");
            assert!(!backend.paths.data.exists());
            assert!(!backend.paths.cache.exists());
        }
    }

    #[test]
    fn nightly_common_job_preparation_refuses_before_reservations_for_every_work_kind() {
        for enabled in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let mut backend = backend(root.path(), Platform { os: Os::Windows, arch: Arch::X64 }, None);
            Arc::get_mut(&mut backend.shared).unwrap().channel = artcraft_launcher_core::nightly::InstallChannel::Nightly;
            backend.pins = None;
            backend.shared.options.lock().unwrap().nightly_enabled = enabled;
            JOB_PREPARATION_EFFECTS.with(|effects| *effects.borrow_mut() = Some(Vec::new()));
            for work in [Work::Install(None), Work::Replace("stable application".into()), Work::Uninstall] {
                assert!(backend.prepare_job(APP, work).is_none());
            }
            let effects = JOB_PREPARATION_EFFECTS.with(|effects| effects.borrow_mut().take().unwrap());
            assert!(effects.is_empty(), "common guard must precede all preparation: {effects:?}");
            assert!(backend.shared.jobs.lock().unwrap().is_empty());
            assert!(backend.shared.installed.lock().unwrap().is_empty());
            assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
        }
    }

    #[test]
    fn stable_bulk_install_still_prepares_and_observer_records_real_effect_sites() {
        let root = tempfile::tempdir().unwrap();
        let mut backend = backend(root.path(), Platform { os: Os::Windows, arch: Arch::X64 }, None);
        type Held = Vec<Box<dyn FnOnce() + Send>>;
        let held: Arc<Mutex<Held>> = Arc::default();
        backend.spawn = Arc::new({
            let held = held.clone();
            move |work| held.lock().unwrap().push(work)
        });
        JOB_PREPARATION_EFFECTS.with(|effects| *effects.borrow_mut() = Some(Vec::new()));
        backend.send(Command::InstallAll { apps: vec![APP.into()] });
        let effects = JOB_PREPARATION_EFFECTS.with(|effects| effects.borrow_mut().take().unwrap());
        #[cfg(any(target_os = "linux", windows))]
        assert_eq!(effects.first(), Some(&"installer gather"));
        assert!(effects.contains(&"job reservation"));
        assert!(effects.contains(&"download queue"));
        assert_eq!(held.lock().unwrap().len(), 1, "the stable batch still prepares its worker; do not execute it");
        assert!(backend.shared.jobs.lock().unwrap().contains_key(APP));
        drop(held.lock().unwrap().drain(..).collect::<Vec<_>>());
        assert!(backend.shared.jobs.lock().unwrap().is_empty());
    }

    /// A backend that knows `release("0.5.0")` and keeps its files under `root`.
    pub(super) fn backend(root: &std::path::Path, platform: Platform, source: Option<Arc<dyn ReleaseSource>>) -> NativeBackend {
        let shared = shared();
        shared.latest.lock().unwrap().insert(APP.to_string(), release("0.5.0"));
        let shared = Arc::new(shared);
        NativeBackend {
            shared,
            paths: Paths::under(root),
            http: None,
            source,
            platform: Some(platform),
            pins: Some(root.join("versions.json")),
            spawn: Arc::new(|work| {
                std::thread::spawn(work);
            }),
        }
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
        check_app(app, Some(LatestRelease::Found(windows_only)), Origin::GitHub, app.repo, &backend.shared, &backend.paths, Some(platform));
        assert_eq!(told(backend.poll()), ["installable false", "latest"]);
        check_app(app, Some(LatestRelease::Found(stable("0.6.0"))), Origin::GitHub, app.repo, &backend.shared, &backend.paths, Some(platform));
        assert_eq!(told(backend.poll()), ["installable true", "latest"]);

        publish_detected(app, None, &backend.shared, Some(platform));
        assert_eq!(told(backend.poll()), ["detected"], "removed: no answer");
    }

    /// The launcher offers to replace itself only when this copy can update itself and the newer
    /// release has a file to install it from. Otherwise the UI offers the download, in the format
    /// this copy was installed as. Update and Restart refuses in the same cases without starting a
    /// download.
    #[test]
    fn self_update_is_offered_only_when_the_copy_and_the_release_allow_it() {
        let platform = Platform { os: Os::Windows, arch: Arch::X64 };
        let release = |name: &str| Release {
            assets: vec![Asset { name: name.into(), size: 1, url: format!("https://example.invalid/{name}"), sha256: None }],
            ..stable("0.9.0")
        };
        let offered = |updating: bool, release: Release| {
            let dir = tempfile::tempdir().unwrap();
            let mut backend = backend(dir.path(), platform, None);
            let install = crate::self_update::Install::Portable(dir.path().join("artcraft-launcher.exe"));
            assert!(backend.shared.self_install.set(Some(install)).is_ok());
            assert!(backend.shared.self_updating.set(updating).is_ok());
            *backend.shared.launcher_release.lock().unwrap() = Some((release, String::new()));
            let offered = backend.capabilities().self_update;
            if !offered {
                backend.update_launcher();
                let events = backend.poll();
                assert!(
                    matches!(&events[..], [Event::JobFinished { app, result: Err(_) }] if app == LAUNCHER_ID),
                    "it refuses without starting a download: {events:?}"
                );
                assert!(backend.shared.jobs.lock().unwrap().is_empty());
            }
            (offered, backend.launcher_kind())
        };
        let portable = release("artcraft-launcher-0.9.0-windows-x64-portable.zip");
        let msi_only = release("artcraft-launcher-0.9.0-windows-x64.msi");
        assert_eq!(offered(true, portable.clone()), (true, Some(PackageKind::PortableZip)));
        assert_eq!(offered(false, portable), (false, Some(PackageKind::PortableZip)), "a copy that can't update itself (unsigned on Windows)");
        assert_eq!(offered(true, msi_only), (false, Some(PackageKind::PortableZip)), "a release without a file for this copy");
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
        let Some(platform) = native_platform() else {
            return;
        };
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
        let Some(platform) = native_platform() else {
            return;
        };
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
        let Some(platform) = native_platform() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        let (waker, woke) = panic_on_wake();
        Arc::get_mut(&mut backend.shared).unwrap().waker = waker;

        backend.start_job(APP, Work::Uninstall);
        woke.recv_timeout(Duration::from_secs(2)).unwrap();
        wait_until(|| !backend.shared.jobs.lock().unwrap().contains_key(APP));
    }

    /// "Update all" takes every place in the download line before any worker starts, so a worker
    /// can't reach a free slot ahead of a row above it that hasn't joined yet. The workers are
    /// held, not run, so this checks the order of the steps and doesn't race threads: starting
    /// each job as soon as it is prepared would find only the first app in line.
    #[test]
    fn update_all_takes_every_place_before_any_worker_starts() {
        let Some(platform) = native_platform() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        let ids: Vec<&str> = artcraft_launcher_core::SUITE.iter().filter(|app| app.runs_on(platform.os)).map(|app| app.id).take(3).collect();
        assert_eq!(ids.len(), 3);
        type Held = Vec<Box<dyn FnOnce() + Send>>;
        let held: Arc<Mutex<Held>> = Arc::default();
        let in_line_at_each_start: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        backend.spawn = {
            let (held, seen) = (held.clone(), in_line_at_each_start.clone());
            Arc::new(move |work| {
                seen.lock().unwrap().push(install::DOWNLOADS.waiting_apps());
                held.lock().unwrap().push(work);
            })
        };

        backend.install_all(&ids.iter().map(|id| id.to_string()).collect::<Vec<_>>());

        let seen = in_line_at_each_start.lock().unwrap().clone();
        assert_eq!(seen.len(), 3, "one worker per app");
        for waiting in &seen {
            assert!(ids.iter().all(|id| waiting.iter().any(|w| w == id)), "every app is in line before any worker starts: {waiting:?}");
        }
        assert!(ids.iter().all(|id| backend.shared.jobs.lock().unwrap().contains_key(*id)), "and each one is reserved");
        drop(held.lock().unwrap().drain(..).collect::<Vec<_>>());
        let waiting = install::DOWNLOADS.waiting_apps();
        assert!(!ids.iter().any(|id| waiting.iter().any(|w| w == id)), "dropped, they leave the line: {waiting:?}");
        assert!(!ids.iter().any(|id| backend.shared.jobs.lock().unwrap().contains_key(*id)), "and the jobs are free again");
    }

    /// Wherever the tests run: with or without a PhotoCraft installed on the machine (here a
    /// stubbed host Flatpak, newer than any release the backend knows), JobFinished observers see
    /// the job's final busy state and download plan.
    #[test]
    fn job_finished_follows_busy_cleanup_and_download_plan_publication() {
        let Some(platform) = native_platform() else {
            return;
        };
        let host_flatpak = || Installed {
            version: version::parse("99.0.0"),
            source: InstallSource::Flatpak,
            location: "ai.storyteller.photocraft".into(),
            package: Some("ai.storyteller.photocraft".into()),
            externally_managed: false,
            origin: None,
            installation: Some("system".into()),
        };
        for host_copy in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mut backend = backend(dir.path(), platform, None);
            if host_copy {
                Arc::get_mut(&mut backend.shared).unwrap().host =
                    HostDetection::Stub(Arc::new(move |app: &CraftApp| (app.id == APP).then(host_flatpak)));
            }
            let shared: Arc<Mutex<Option<std::sync::Weak<Shared>>>> = Arc::new(Mutex::new(None));
            let shared_for_waker = shared.clone();
            let paths = backend.paths.clone();
            let (sent, received) = mpsc::channel();
            Arc::get_mut(&mut backend.shared).unwrap().waker = Arc::new(move || {
                let Some(shared) = shared_for_waker.lock().unwrap().as_ref().and_then(std::sync::Weak::upgrade) else {
                    return;
                };
                let finished = shared.events.lock().unwrap().iter().any(|event| matches!(event, Event::JobFinished { app, .. } if app == APP));
                if finished {
                    let busy = shared.jobs.lock().unwrap().contains_key(APP);
                    let plan = shared.download_sizes.lock().unwrap().get(APP).and_then(|cached| cached.plan);
                    let options = shared.options.lock().unwrap().clone();
                    let current = planned_download(CraftApp::by_id(APP).unwrap(), &shared, &paths, Some(platform), &options)
                        .map(|(_, kind, size)| (kind, size));
                    let _ = sent.send((busy, plan, current));
                }
            });
            *shared.lock().unwrap() = Some(Arc::downgrade(&backend.shared));

            backend.start_job(APP, Work::Uninstall);
            // Generous for a loaded machine; the job itself takes a few milliseconds.
            let (busy, plan, current) = received.recv_timeout(Duration::from_secs(10)).unwrap();
            assert!(!busy, "host copy: {host_copy}");
            assert_eq!(plan, current, "JobFinished observers need the final download plan (host copy: {host_copy})");
            if !host_copy {
                assert!(matches!(plan, Some((_, 3))), "{plan:?}");
            }
            assert_eq!(backend.download_size(APP), plan);
        }
    }

    /// A poll that finds an install's `Blocked` also finds its `JobFinished`, so the card never
    /// offers Retry while the job is still running.
    #[test]
    fn blocked_and_job_finished_arrive_in_one_poll() {
        let Some(platform) = native_platform() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        let shared: Arc<Mutex<Option<std::sync::Weak<Shared>>>> = Arc::new(Mutex::new(None));
        let shared_for_waker = shared.clone();
        let (sent, received) = mpsc::channel();
        Arc::get_mut(&mut backend.shared).unwrap().waker = Arc::new(move || {
            let Some(shared) = shared_for_waker.lock().unwrap().as_ref().and_then(std::sync::Weak::upgrade) else {
                return;
            };
            let polled: Vec<Event> = shared.events.lock().unwrap().drain(..).collect();
            let blocked = polled.iter().any(|event| matches!(event, Event::Blocked { app, .. } if app == APP));
            let finished = polled.iter().any(|event| matches!(event, Event::JobFinished { app, .. } if app == APP));
            if blocked || finished {
                let _ = sent.send((blocked, finished));
            }
        });
        *shared.lock().unwrap() = Some(Arc::downgrade(&backend.shared));

        assert!(backend.start_job(APP, Work::Install(None)));
        // The worker then holds the last reference to the waker, so the channel closes if it
        // exits without sending.
        drop(backend);
        assert_eq!(received.recv().unwrap(), (true, true));
    }

    /// Opening an app sends no `Blocked`, so the card keeps what stopped the last install.
    #[test]
    fn opening_an_app_keeps_the_install_blocker() {
        let Some(platform) = native_platform() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        let shared: Arc<Mutex<Option<std::sync::Weak<Shared>>>> = Arc::new(Mutex::new(None));
        let shared_for_waker = shared.clone();
        let (sent, received) = mpsc::channel();
        Arc::get_mut(&mut backend.shared).unwrap().waker = Arc::new(move || {
            let Some(shared) = shared_for_waker.lock().unwrap().as_ref().and_then(std::sync::Weak::upgrade) else {
                return;
            };
            let polled: Vec<Event> = shared.events.lock().unwrap().drain(..).collect();
            for event in polled {
                if matches!(&event, Event::Blocked { app, .. } | Event::JobFinished { app, .. } if app == APP) {
                    let _ = sent.send(event);
                }
            }
        });
        *shared.lock().unwrap() = Some(Arc::downgrade(&backend.shared));

        assert!(backend.start_job(APP, Work::Launch(Vec::new())));
        // The worker then holds the last reference to the waker, so the channel closes once it
        // exits, and everything it sent can be read to the end.
        drop(backend);
        let sent: Vec<Event> = received.iter().collect();
        assert!(matches!(sent.as_slice(), [Event::JobFinished { .. }]), "{sent:?}");
    }

    /// A job's last events are queued before its slot is freed, so an attempt admitted next
    /// cannot queue its progress ahead of the old `Blocked` and `JobFinished`.
    #[test]
    fn a_job_queues_its_last_events_before_freeing_its_slot() {
        let Some(platform) = native_platform() else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let mut backend = backend(dir.path(), platform, None);
        backend.spawn = Arc::new(|work| work());
        SLOT_FREED.with(|freed| *freed.borrow_mut() = Some(Vec::new()));
        assert!(backend.start_job(APP, Work::Install(None)));
        let freed = SLOT_FREED.with(|freed| freed.borrow_mut().take()).unwrap();
        assert_eq!(freed, [true]);
    }

    /// A nightly update stopped by the running app hands its blocker to the worker, which sends
    /// it in `Blocked`.
    #[test]
    fn a_nightly_update_reports_what_blocked_it() {
        let mut shared = shared();
        shared.channel = artcraft_launcher_core::nightly::InstallChannel::Nightly;
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::under(dir.path());
        let http = Http::new().unwrap();
        let open = InstallFailure { blocker: artcraft_launcher_core::protocol::Blocker::AppOpen, details: "photocraft".into() };
        let set = open.clone();
        REVIEW_NIGHTLY_INSTALL.with(|hook| {
            *hook.borrow_mut() = Some(Box::new(move |job: &mut install::Job<'_>| {
                job.failure = Some(set);
                Err("PhotoCraft is open.".into())
            }))
        });
        let app = CraftApp::by_id(APP).unwrap();
        let mut failure = None;
        let options = BackendOptions::default();
        let stop = Stop::default();
        let result = run_install(app, &shared, &paths, Some(&http), None, &options, None, None, &stop, None, None, &mut failure, None, None, None);
        assert_eq!(result, Err("PhotoCraft is open.".into()));
        assert_eq!(failure, Some(open));
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
        let Some(platform) = native_platform() else {
            return;
        };
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
        let Some(platform) = native_platform() else {
            return;
        };
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
        let Some(platform) = native_platform() else {
            return;
        };
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
            CraftApp::by_id(APP).unwrap().repo,
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
        let Some(platform) = native_platform() else {
            return;
        };
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
        let Some(platform) = native_platform() else {
            return;
        };
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
            fn latest(&self, query: &ReleaseQuery, _prereleases: bool) -> Answer {
                let latest = if CraftApp::by_id(APP).is_some_and(|app| app.repo == query.repo) {
                    LatestRelease::Found(release("0.5.0"))
                } else {
                    LatestRelease::None
                };
                Answer { latest, source: Source::GitHubApi }
            }
            fn cached(&self, _query: &ReleaseQuery, _prereleases: bool) -> Option<LatestRelease> {
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
        let Some(platform) = native_platform() else {
            return;
        };
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
        let Some(platform) = native_platform() else {
            return;
        };
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
        let Some(platform) = native_platform() else {
            return;
        };
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
            fn latest(&self, query: &ReleaseQuery, _prereleases: bool) -> crate::source::Answer {
                let found = CraftApp::by_id(APP).is_some_and(|a| a.repo == query.repo);
                let latest = if found { LatestRelease::Found(release("0.6.0")) } else { LatestRelease::None };
                crate::source::Answer { latest, source: Source::GitHubApi }
            }
            fn cached(&self, _query: &ReleaseQuery, _prereleases: bool) -> Option<LatestRelease> {
                None
            }
            fn rate_limited_until(&self) -> Option<i64> {
                None
            }
            fn save(&self) {}
        }
        let Some(platform) = native_platform() else {
            return;
        };
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
        let paused = artcraft_launcher_core::protocol::PAUSED;
        for error in [
            "the Windows Installer stopped with error 1603",
            "couldn't copy the AppImage",
            CANCELLED,
            INSTALL_CANCELLED,
            paused,
            PAUSED_BY_CONNECTION,
        ] {
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

    #[test]
    fn a_launcher_release_without_a_file_for_this_copy_is_offered_as_a_download() {
        use crate::self_update::Install;
        // ArtCraft Launcher v0.17.0's macOS file: the disk image only.
        let name = "artcraft-launcher-0.17.0-macos-universal.dmg";
        let release = Release {
            assets: vec![Asset { name: name.into(), size: 1, url: format!("https://example.invalid/{name}"), sha256: None }],
            ..Release::default()
        };
        let mac = Platform { os: Os::MacOs, arch: Arch::Arm64 };
        for (install, updates) in
            [(Install::MacApp("/Applications/ArtCraft Launcher.app".into()), true), (Install::Standalone("/tmp/artcraft-launcher".into()), false)]
        {
            let dir = tempfile::tempdir().unwrap();
            let mut backend = backend(dir.path(), mac, None);
            backend.shared.self_install.set(Some(install.clone())).unwrap();
            backend.shared.self_updating.set(true).unwrap();
            *backend.shared.launcher_release.lock().unwrap() = Some((release.clone(), String::new()));
            assert_eq!(backend.capabilities().self_update, updates, "{install:?}");
            if !updates {
                backend.update_launcher();
                let events = backend.poll();
                assert!(
                    matches!(&events[..], [Event::JobFinished { app, result: Err(_) }] if app == LAUNCHER_ID),
                    "it refuses without starting a download: {events:?}"
                );
                assert!(backend.shared.jobs.lock().unwrap().is_empty());
            }
        }
    }
}

/// Runs in place of [`run_nightly_install`] once, on the thread that sets it.
#[cfg(test)]
type ReviewNightlyInstallHook = Box<dyn FnOnce(&mut install::Job<'_>) -> Result<String, String>>;
#[cfg(test)]
thread_local! { static REVIEW_NIGHTLY_INSTALL: std::cell::RefCell<Option<ReviewNightlyInstallHook>> = std::cell::RefCell::new(None); }
#[cfg(test)]
thread_local! { static REVIEW_BEFORE_MANIFEST_SAVE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None); }
#[cfg(test)]
pub(crate) fn review_before_manifest_save() {
    REVIEW_BEFORE_MANIFEST_SAVE.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
}

fn nightly_worker_record(
    app: &CraftApp,
    shared: &Shared,
    paths: &Paths,
    platform: Option<Platform>,
) -> Result<(crate::nightly_transaction::Lease, crate::manifest::Record, Platform), String> {
    let platform = platform.ok_or("This nightly platform is not supported.")?;
    let adapter = crate::nightly_profile::for_app(app, platform)?;
    let lease = crate::nightly_transaction::Lease::acquire(paths, adapter)?;
    let manifest = lease.manifest()?;
    let record = manifest.apps.get(app.id).cloned();
    store_nightly_manifest(shared, manifest);
    let record = record.ok_or("No owned nightly copy is recorded.")?;
    Ok((lease, record, platform))
}
fn run_nightly_remove(app: &CraftApp, shared: &Shared, paths: &Paths, platform: Option<Platform>, stop: &Stop) -> Result<String, String> {
    let (lease, record, platform) = nightly_worker_record(app, shared, paths, platform)?;
    let result = crate::nightly::remove(app, &record, paths, platform, stop, &lease, &mut || {
        stop.check()?;
        #[cfg(test)]
        REVIEW_BEFORE_NIGHTLY_PUBLISH.with(|hook| hook.borrow_mut().take().map_or(Ok(()), |hook| hook()))?;
        let manifest = lease.publish(Some(&record), None)?;
        store_nightly_manifest(shared, manifest);
        Ok(())
    });
    store_nightly_manifest(shared, lease.manifest()?);
    result?;
    Ok(format!("{} was removed; its nightly profile was kept", app.name))
}
fn run_nightly_launch(
    app: &CraftApp,
    shared: &Shared,
    paths: &Paths,
    platform: Option<Platform>,
    stop: &Stop,
    args: &[String],
) -> Result<String, String> {
    let (_lease, record, platform) = nightly_worker_record(app, shared, paths, platform)?;
    stop.check()?;
    crate::nightly::launch(app, &record, paths, platform, args, stop)?;
    Ok(format!("{} was opened", app.name))
}

fn store_nightly_manifest(shared: &Shared, manifest: Manifest) {
    let _reporting = shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
    *shared.manifest.lock().unwrap_or_else(PoisonError::into_inner) = manifest;
}
fn selected_nightly(app: &CraftApp, shared: &Shared, platform: Platform) -> Result<crate::nightly::VerifiedRelease, String> {
    let state = shared.nightly_state.lock().unwrap_or_else(PoisonError::into_inner);
    if !state.fresh(&crate::nightly_lookup::key(app, platform), crate::now()) {
        return Err("The Nightly selection is stale or indeterminate. Complete a fresh verified check before installing.".into());
    }
    shared
        .nightly_verified
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(app.id)
        .cloned()
        .ok_or_else(|| "No verified separate-profile nightly is available. Check for updates first.".into())
}
struct NightlyDetectionSnapshot {
    record: Option<crate::manifest::Record>,
    revision: u64,
    active: bool,
}
fn nightly_detection_snapshot(shared: &Shared, app: &CraftApp) -> NightlyDetectionSnapshot {
    let _reporting = shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
    let process = shared.lifecycle.jobs.lock().unwrap_or_else(PoisonError::into_inner);
    let key = shared.job_key(app.id);
    let revision = shared.lifecycle.revisions.lock().unwrap_or_else(PoisonError::into_inner).get(&key).copied().unwrap_or_default();
    NightlyDetectionSnapshot {
        record: shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).apps.get(app.id).cloned(),
        revision,
        active: process.contains_key(&key),
    }
}
fn nightly_detection_result(
    app: &CraftApp,
    snapshot: &NightlyDetectionSnapshot,
    paths: &Paths,
    platform: Option<Platform>,
) -> Result<Option<Installed>, String> {
    crate::nightly_profile::validate_paths(paths)?;
    snapshot
        .record
        .as_ref()
        .map(|record| crate::nightly::owned_installation(app, record, paths, platform.ok_or("The retained Nightly platform is unavailable.")?))
        .transpose()
}

fn publish_nightly_detection_result(
    app: &CraftApp,
    snapshot: &NightlyDetectionSnapshot,
    found: Result<Option<Installed>, String>,
    shared: &Shared,
    platform: Option<Platform>,
) -> bool {
    let (found, error) = match found {
        Ok(found) => (found, None),
        Err(error) => (None, Some(error)),
    };
    let published = publish_nightly_detection(app, snapshot, found, shared, platform);
    // Do not report a staged-away executable or an obsolete manifest as a broken retained copy.
    if published && let Some(error) = error {
        shared.emit(Event::Notice { app: app.id.into(), result: Err(error) });
    }
    published
}

fn publish_nightly_detection(
    app: &CraftApp,
    snapshot: &NightlyDetectionSnapshot,
    found: Option<Installed>,
    shared: &Shared,
    platform: Option<Platform>,
) -> bool {
    let _reporting = shared.reporting.lock().unwrap_or_else(PoisonError::into_inner);
    let process = shared.lifecycle.jobs.lock().unwrap_or_else(PoisonError::into_inner);
    let key = shared.job_key(app.id);
    let revision = shared.lifecycle.revisions.lock().unwrap_or_else(PoisonError::into_inner).get(&key).copied().unwrap_or_default();
    if snapshot.active
        || process.contains_key(&key)
        || snapshot.revision != revision
        || shared.manifest.lock().unwrap_or_else(PoisonError::into_inner).apps.get(app.id) != snapshot.record.as_ref()
    {
        return false;
    }
    shared.store_detected(app.id, found.as_ref());
    let installable = installable_event(app, shared, platform, None);
    shared.emit_all(installable.into_iter().chain([Event::Detected { app: app.id.to_owned(), installed: found }]));
    true
}

#[cfg(test)]
type ReviewDetectionHook = Box<dyn FnOnce(bool) + Send>;
#[cfg(test)]
type ReviewManifestReplaceHook = Box<dyn FnOnce() -> Result<(), String>>;
#[cfg(test)]
thread_local! {
    static REVIEW_AFTER_MANIFEST_REPLACE: std::cell::RefCell<Option<ReviewManifestReplaceHook>> = std::cell::RefCell::new(None);
    static REVIEW_BEFORE_NIGHTLY_PUBLISH: std::cell::RefCell<Option<ReviewManifestReplaceHook>> = std::cell::RefCell::new(None);
    static REVIEW_AFTER_MANIFEST_SCRATCH_CREATED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
    static REVIEW_AFTER_STAGE_REMOVED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = std::cell::RefCell::new(None);
}
#[cfg(test)]
pub(crate) fn review_after_manifest_replace() -> Result<(), String> {
    REVIEW_AFTER_MANIFEST_REPLACE.with(|hook| hook.borrow_mut().take().map_or(Ok(()), |hook| hook()))
}
#[cfg(test)]
pub(crate) fn review_after_stage_removed() {
    REVIEW_AFTER_STAGE_REMOVED.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(test)]
pub(crate) fn review_after_manifest_scratch_created() {
    REVIEW_AFTER_MANIFEST_SCRATCH_CREATED.with(|hook| {
        if let Some(hook) = hook.borrow_mut().take() {
            hook();
        }
    });
}

#[cfg(test)]
#[path = "backend_shared_selection_tests.rs"]
mod shared_selection_tests;
