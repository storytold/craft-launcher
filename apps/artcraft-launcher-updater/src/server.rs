//! The update service's work: a schedule that installs updates as they come out, and the requests
//! of the launcher and the command line (`artcraft_launcher_native::service` has the protocol and
//! why a client can ask for so little).
//!
//! One run at a time installs anything: the scheduled one, or a client's update; the other waits
//! its turn. Stopping the service pauses a download (the next run resumes it) and waits for a
//! running install to end, so a package is never cut off halfway.

use std::collections::{BTreeMap, HashMap};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, TryLockError};
use std::time::{Duration, Instant};

use artcraft_launcher_core::CraftApp;
use artcraft_launcher_core::protocol::{BackendOptions, JobPhase};
use artcraft_launcher_native::http::{self, Stop};
use artcraft_launcher_native::paths::Paths;
use artcraft_launcher_native::policy::Policy;
use artcraft_launcher_native::service::{self, AppState, AppSummary, LastUpdate, LineReader, Op, Outcome, Phase, Reply, Request, Status};
use artcraft_launcher_native::updater::{Engine, Found, Scope, Survey};
use serde::{Deserialize, Serialize};

/// The first scheduled check waits this long after the service starts (which Windows already
/// delays), to keep out of the way while the computer starts up…
const FIRST_CHECK_DELAY: i64 = 5 * 60;
/// …plus up to this much, so a room of computers switched on together doesn't ask in step.
const JITTER: i64 = 10 * 60;
/// When an update had to wait (the app was open, another install was running), the next try
/// comes this soon instead of at the next scheduled check.
const RETRY_AFTER: i64 = 60 * 60;
/// A client's check reuses a look at the releases this recent (seconds).
const CHECK_REUSE: i64 = 60;
/// A client's update reuses a look at the releases this recent (seconds).
const UPDATE_REUSE: i64 = 2 * 60;
/// Clients served at once; more are turned away.
const MAX_CLIENTS: usize = 8;
/// How long a client has to send its request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Download progress goes out at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);
/// When the service stops, it waits this long for a running install to end.
const STOP_GRACE: Duration = Duration::from_secs(10 * 60);

/// What outlives a restart: when the releases were last looked up, and how each app's last
/// update went.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    last_check: Option<i64>,
    last_updates: BTreeMap<String, LastUpdate>,
}

struct State {
    saved: Saved,
    /// The last look at the releases.
    survey: Option<Arc<Survey>>,
    /// An update landed since `survey`: it still describes the other apps (and `status` counts
    /// the update in), but the next run looks again rather than reuse it.
    stale: bool,
    next_check: Option<i64>,
    busy: Option<String>,
}

/// The update service.
pub struct Server {
    engine: Engine,
    /// The administrator's settings as they stand (read afresh each time, so a change needs no
    /// restart; `Policy::load`, or fixed ones in tests).
    policy: fn() -> Policy,
    state: Mutex<State>,
    state_file: PathBuf,
    /// Held by whichever run is installing.
    work: Mutex<()>,
    /// Held while the releases are looked up, so two requests at once look once.
    surveying: Mutex<()>,
    /// Pause and Cancel for the app being updated.
    jobs: Mutex<HashMap<&'static str, Arc<Stop>>>,
    clients: Arc<AtomicUsize>,
    stopping: AtomicBool,
    /// Wakes the scheduler when the service stops.
    wake: (Mutex<bool>, Condvar),
}

impl Server {
    /// A service keeping its downloads, saved answers and state in `paths`, with the
    /// administrator's settings from `policy`.
    pub fn new(paths: Paths, policy: fn() -> Policy) -> Result<Arc<Self>, String> {
        // No install folders: the service only updates, and an update stays where its copy is.
        let options = BackendOptions { prereleases: policy().prereleases, ..BackendOptions::default() };
        let engine = Engine::new(paths.clone(), Scope::Machine, options)?;
        // Leftovers: a paused download stays a month, a finished one kept after a failed install a week.
        http::clean_downloads(&paths.downloads(), Duration::from_secs(30 * 86_400), Duration::from_secs(7 * 86_400));
        let state_file = paths.data.join("state.json");
        let saved = std::fs::read(&state_file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Ok(Arc::new(Self {
            engine,
            policy,
            state: Mutex::new(State { saved, survey: None, stale: false, next_check: None, busy: None }),
            state_file,
            work: Mutex::new(()),
            surveying: Mutex::new(()),
            jobs: Mutex::new(HashMap::new()),
            clients: Arc::new(AtomicUsize::new(0)),
            stopping: AtomicBool::new(false),
            wake: (Mutex::new(false), Condvar::new()),
        }))
    }

    /// Starts the schedule and starts listening for clients.
    pub fn start(self: &Arc<Self>) {
        let server = self.clone();
        std::thread::spawn(move || server.schedule());
        let server = self.clone();
        std::thread::spawn(move || server.listen());
    }

    /// Stops: no new work starts, a download pauses, and a running install is waited for (at most
    /// [`STOP_GRACE`]). `waiting` is called every couple of seconds meanwhile.
    pub fn stop(&self, waiting: &mut dyn FnMut()) {
        self.stopping.store(true, Ordering::SeqCst);
        {
            let (lock, wake) = &self.wake;
            *lock.lock().unwrap_or_else(PoisonError::into_inner) = true;
            wake.notify_all();
        }
        for stop in self.jobs.lock().unwrap_or_else(PoisonError::into_inner).values() {
            stop.pause();
        }
        let since = Instant::now();
        loop {
            match self.work.try_lock() {
                Ok(_) | Err(TryLockError::Poisoned(_)) => break,
                Err(TryLockError::WouldBlock) if since.elapsed() >= STOP_GRACE => {
                    log::warn!("stopping while an install is still running");
                    break;
                }
                Err(TryLockError::WouldBlock) => {
                    waiting();
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
        self.save(&self.lock_state());
        log::info!("update service stopped");
    }

    fn stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn save(&self, state: &State) {
        let result = serde_json::to_vec_pretty(&state.saved)
            .map_err(|e| e.to_string())
            .and_then(|bytes| artcraft_launcher_native::paths::write_atomic(&self.state_file, &bytes));
        if let Err(e) = result {
            log::warn!("couldn't save the service's state: {e}");
        }
    }

    // ---- the schedule ---------------------------------------------------------------------------

    fn schedule(self: Arc<Self>) {
        let mut next = self.first_check(now());
        loop {
            self.lock_state().next_check = Some(next);
            if !self.sleep_until(next) {
                return;
            }
            let (_, retry) = self.scheduled_run();
            if self.stopping() {
                return;
            }
            let hours = i64::from((self.policy)().interval_hours);
            next = now() + if retry { RETRY_AFTER } else { hours * 3600 + jitter(JITTER) };
        }
    }

    /// When the first scheduled check is due: a little after the service starts, unless the last
    /// one (before a restart) was recent enough to wait out its interval.
    fn first_check(&self, now: i64) -> i64 {
        let interval = i64::from((self.policy)().interval_hours) * 3600;
        let soonest = now + FIRST_CHECK_DELAY + jitter(JITTER);
        match self.lock_state().saved.last_check {
            Some(t) if t <= now && t + interval > soonest => t + interval,
            _ => soonest,
        }
    }

    /// Sleeps until `at` (Unix seconds); `false` when the service stops first. Wakes every minute
    /// to look at the clock, which jumps when the computer sleeps.
    fn sleep_until(&self, at: i64) -> bool {
        let (lock, wake) = &self.wake;
        let mut stopped = lock.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if *stopped {
                return false;
            }
            let left = at - now();
            if left <= 0 {
                return true;
            }
            let wait = Duration::from_secs(left.clamp(1, 60).unsigned_abs());
            stopped = wake.wait_timeout(stopped, wait).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    /// One scheduled run: look up the releases and, when updates install automatically (a user
    /// turned that on in ArtCraft Launcher, or an administrator for everyone), install every one;
    /// last, the service's own. Returns each install's outcome, and whether something had to wait
    /// and deserves another try soon.
    pub fn scheduled_run(&self) -> (Vec<(&'static str, Outcome)>, bool) {
        let _turn = self.work.lock().unwrap_or_else(PoisonError::into_inner);
        if self.stopping() {
            return (Vec::new(), false);
        }
        let policy = (self.policy)();
        let survey = self.survey(0);
        if !policy.auto_update {
            log::info!(
                "{} update(s) available; background installs are off (an administrator turns them on in ArtCraft Launcher's settings)",
                survey.updates().count()
            );
            return (Vec::new(), false);
        }
        let mut outcomes = Vec::new();
        let mut retry = false;
        for found in survey.updates() {
            if self.stopping() {
                break;
            }
            let outcome = self.apply(found, &mut |_| {});
            retry |= worth_retrying(&outcome);
            outcomes.push((found.app.id, outcome));
        }
        if !self.stopping()
            && let Some(launcher) = &survey.launcher
            && let Some(asset) = self.engine.own_update(launcher)
            && let Err(e) = self.engine.start_own_update(launcher, asset, &Stop::default())
        {
            log::warn!("the update service couldn't update itself: {e}");
        }
        (outcomes, retry)
    }

    // ---- shared by the schedule and the clients ------------------------------------------------

    /// A look at the releases no older than `max_age` seconds: the last one, or a new one.
    fn survey(&self, max_age: i64) -> Arc<Survey> {
        let _one_at_a_time = self.surveying.lock().unwrap_or_else(PoisonError::into_inner);
        let recent = {
            let state = self.lock_state();
            state.survey.clone().filter(|s| max_age > 0 && !state.stale && now() - s.at <= max_age)
        };
        if let Some(recent) = recent {
            return recent;
        }
        let survey = Arc::new(self.engine.survey());
        let mut state = self.lock_state();
        state.saved.last_check = Some(survey.at);
        state.survey = Some(survey.clone());
        state.stale = false;
        self.save(&state);
        log::info!("checked for updates: {} available", survey.updates().count());
        survey
    }

    /// Installs `found`'s update, recording how it went (when there was something to try).
    fn apply(&self, found: &Found, phase: &mut dyn FnMut(JobPhase)) -> Outcome {
        let stop = Arc::new(Stop::default());
        if self.stopping() {
            stop.pause();
        }
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner).insert(found.app.id, stop.clone());
        self.lock_state().busy = Some(found.app.id.to_string());
        let outcome = self.engine.apply(found, &stop, phase);
        self.jobs.lock().unwrap_or_else(PoisonError::into_inner).remove(found.app.id);
        let mut state = self.lock_state();
        state.busy = None;
        if matches!(outcome, Outcome::Updated { .. }) {
            state.stale = true;
        }
        if was_attempt(&outcome) {
            state.saved.last_updates.insert(found.app.id.to_string(), LastUpdate { at: now(), outcome: outcome.clone() });
            self.save(&state);
        }
        log::info!("{}", outcome.describe(found.app.name));
        outcome
    }

    /// The service's state, for clients.
    fn status(&self) -> Status {
        let policy = (self.policy)();
        let state = self.lock_state();
        Status {
            version: env!("CARGO_PKG_VERSION").to_string(),
            auto_update: policy.auto_update,
            auto_update_managed: policy.auto_update_managed,
            on_demand: policy.allow_requests,
            interval_hours: policy.interval_hours,
            last_check: state.saved.last_check,
            next_check: state.next_check,
            busy: state.busy.clone(),
            apps: summaries(state.survey.as_deref(), &state.saved.last_updates),
        }
    }

    // ---- clients -------------------------------------------------------------------------------

    fn listen(self: Arc<Self>) {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, service::PORT));
        let mut told = false;
        while !self.stopping() {
            let listener = match TcpListener::bind(addr).and_then(|l| l.set_nonblocking(true).map(|()| l)) {
                Ok(l) => l,
                Err(e) => {
                    if !told {
                        log::warn!("can't take requests on {addr} ({e}); trying again every 30 s. Scheduled updates go on regardless.");
                        told = true;
                    }
                    self.sleep_until(now() + 30);
                    continue;
                }
            };
            log::info!("taking requests on {addr}");
            while !self.stopping() {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let Some(slot) = Slot::take(&self.clients) else { continue };
                        let server = self.clone();
                        std::thread::spawn(move || {
                            let _slot = slot;
                            server.serve(stream);
                        });
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
                    Err(e) => {
                        log::warn!("couldn't take a request: {e}");
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
            }
        }
    }

    fn serve(&self, stream: TcpStream) {
        // An accepted socket can inherit the listener's non-blocking mode.
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
        let Ok(reading) = stream.try_clone() else { return };
        let mut out = Out { stream, gone: false };
        let request = match LineReader::new(reading).read_line() {
            Ok(Some(line)) => Request::parse(&line),
            Ok(None) => Err("no request came".to_string()),
            Err(e) => Err(e),
        };
        match request {
            Ok(request) => self.answer(request.op, &mut out),
            Err(reason) => {
                out.send(&Reply::Refused { reason });
            }
        }
        out.send(&Reply::Done);
    }

    fn answer(&self, op: Op, out: &mut Out) {
        if !matches!(op, Op::Status) && !(self.policy)().allow_requests {
            out.send(&Reply::Refused {
                reason: "an administrator turned off update requests on this computer; updates install on the service's schedule".into(),
            });
            return;
        }
        match op {
            Op::Status => {
                out.send(&Reply::Status(Box::new(self.status())));
            }
            Op::Check => {
                if self.with_heartbeat(out, None, || self.survey(CHECK_REUSE)).is_none() {
                    log::warn!("a check for a client stopped unexpectedly");
                }
                out.send(&Reply::Status(Box::new(self.status())));
            }
            Op::Update { apps } => self.serve_update(&apps, out),
            Op::Pause { app } => self.stop_job(&app, false, out),
            Op::Cancel { app } => self.stop_job(&app, true, out),
        }
    }

    fn serve_update(&self, apps: &[String], out: &mut Out) {
        let wanted = match resolve(apps) {
            Ok(w) => w,
            Err(reason) => {
                out.send(&Reply::Refused { reason });
                return;
            }
        };
        let first = wanted.first().map(|a| a.id);
        let Some(_turn) = self.wait_for_turn(out, first) else { return };
        let Some(survey) = self.with_heartbeat(out, first, || self.survey(UPDATE_REUSE)) else {
            out.send(&Reply::Refused { reason: "the update service couldn't look up the releases".into() });
            return;
        };
        let targets: Vec<&Found> =
            if wanted.is_empty() { survey.updates().collect() } else { wanted.iter().filter_map(|a| survey.find(a.id)).collect() };
        for found in targets {
            let outcome = if self.stopping() { Outcome::Paused } else { self.apply_streaming(found, out) };
            out.send(&Reply::Finished { app: found.app.id.to_string(), outcome });
        }
    }

    /// Waits until no other run is installing, telling the client it's waiting. `None` when the
    /// client went away or the service is stopping.
    fn wait_for_turn(&self, out: &mut Out, app: Option<&str>) -> Option<MutexGuard<'_, ()>> {
        let mut told = Instant::now() - service::HEARTBEAT;
        loop {
            if self.stopping() {
                return None;
            }
            match self.work.try_lock() {
                Ok(turn) => return Some(turn),
                Err(TryLockError::Poisoned(p)) => return Some(p.into_inner()),
                Err(TryLockError::WouldBlock) => {
                    if told.elapsed() >= service::HEARTBEAT {
                        let beat = match app {
                            Some(app) => Reply::Progress { app: app.to_string(), phase: Phase::Waiting },
                            None => Reply::Working,
                        };
                        if !out.send(&beat) {
                            return None;
                        }
                        told = Instant::now();
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            }
        }
    }

    /// Runs `work`, sending the client a heartbeat while it takes long. `None` if it ended in a panic.
    fn with_heartbeat<T: Send>(&self, out: &mut Out, app: Option<&str>, work: impl FnOnce() -> T + Send) -> Option<T> {
        let (done, finished) = mpsc::channel::<()>();
        std::thread::scope(|scope| {
            let worker = scope.spawn(move || {
                let result = work();
                let _ = done.send(());
                result
            });
            while let Err(RecvTimeoutError::Timeout) = finished.recv_timeout(service::HEARTBEAT) {
                let beat = match app {
                    Some(app) => Reply::Progress { app: app.to_string(), phase: Phase::Waiting },
                    None => Reply::Working,
                };
                out.send(&beat);
            }
            worker.join().ok()
        })
    }

    /// Installs `found`'s update, streaming its progress to the client (and repeating the last
    /// step as a heartbeat while it's quiet, as during the Windows Installer's own run).
    fn apply_streaming(&self, found: &Found, out: &mut Out) -> Outcome {
        let (tx, phases) = mpsc::channel::<JobPhase>();
        let app = found.app.id.to_string();
        std::thread::scope(|scope| {
            let worker = scope.spawn(move || {
                self.apply(found, &mut |p| {
                    let _ = tx.send(p);
                })
            });
            let mut last: Option<Phase> = None;
            let mut sent = Instant::now() - PROGRESS_EVERY;
            loop {
                match phases.recv_timeout(service::HEARTBEAT) {
                    Ok(p) => {
                        let p = Phase::from(p);
                        let same_step = last.is_some_and(|l| std::mem::discriminant(&l) == std::mem::discriminant(&p));
                        last = Some(p);
                        if !same_step || sent.elapsed() >= PROGRESS_EVERY {
                            out.send(&Reply::Progress { app: app.clone(), phase: p });
                            sent = Instant::now();
                        }
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        out.send(&last.map_or(Reply::Working, |p| Reply::Progress { app: app.clone(), phase: p }));
                        sent = Instant::now();
                    }
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            worker.join().unwrap_or_else(|_| Outcome::Failed { error: "the update stopped unexpectedly".into() })
        })
    }

    fn stop_job(&self, app: &str, cancel: bool, out: &mut Out) {
        if CraftApp::by_id(app).is_none() {
            out.send(&Reply::Refused { reason: format!("{app} isn't a Craft App") });
            return;
        }
        let stop = self.jobs.lock().unwrap_or_else(PoisonError::into_inner).iter().find(|(id, _)| **id == app).map(|(_, s)| s.clone());
        if let Some(stop) = stop {
            log::info!("{app}: {} at a client's request", if cancel { "cancelling" } else { "pausing" });
            if cancel {
                stop.cancel();
            } else {
                stop.pause();
            }
        }
    }
}

/// The apps a client named, as Craft Apps (each once). Empty means every app with an update.
fn resolve(ids: &[String]) -> Result<Vec<&'static CraftApp>, String> {
    let mut apps: Vec<&'static CraftApp> = Vec::new();
    for id in ids.iter().take(artcraft_launcher_core::SUITE.len().saturating_mul(2)) {
        let app = CraftApp::by_id(id).ok_or_else(|| format!("{id} isn't a Craft App"))?;
        if !apps.iter().any(|a| a.id == app.id) {
            apps.push(app);
        }
    }
    Ok(apps)
}

/// What clients are told about each app: the last survey's view, with each app's last update,
/// and any update installed since the survey counted in.
fn summaries(survey: Option<&Survey>, last_updates: &BTreeMap<String, LastUpdate>) -> Vec<AppSummary> {
    // Not checked since the service started: what its updates left installed is all it knows.
    let Some(survey) = survey else {
        return last_updates
            .iter()
            .map(|(id, last)| {
                let (installed, state, note) = match &last.outcome {
                    Outcome::Updated { version, .. } => (Some(version.clone()), AppState::UpToDate, None),
                    _ => (None, AppState::Unknown, Some("not checked since the service started".to_string())),
                };
                AppSummary { id: id.clone(), installed, latest: None, state, note, last_update: Some(last.clone()) }
            })
            .collect();
    };
    survey
        .apps
        .iter()
        .map(|found| {
            let mut summary = found.summary();
            if let Some(last) = last_updates.get(found.app.id) {
                if let Outcome::Updated { version, .. } = &last.outcome
                    && last.at >= survey.at
                {
                    summary.installed = Some(version.clone());
                    summary.state = AppState::UpToDate;
                }
                summary.last_update = Some(last.clone());
            }
            summary
        })
        .collect()
}

/// Whether `outcome` came of an attempt to install something, which `status` reports as the app's
/// last update. "Nothing to do" (up to date, not one for this service) is no news, and mustn't hide
/// the last real update.
fn was_attempt(outcome: &Outcome) -> bool {
    !matches!(outcome, Outcome::UpToDate { .. } | Outcome::NotEligible { .. })
}

/// Whether an update that didn't happen deserves another try before the next scheduled check.
fn worth_retrying(outcome: &Outcome) -> bool {
    match outcome {
        Outcome::Running | Outcome::Paused => true,
        #[cfg(windows)]
        Outcome::Failed { error } => error == artcraft_launcher_native::install::windows::ANOTHER_INSTALL,
        _ => false,
    }
}

/// A client's half of the connection: replies go out until it goes away, then are dropped (the
/// work it asked for carries on).
struct Out {
    stream: TcpStream,
    gone: bool,
}

impl Out {
    /// Sends `reply`; `false` once the client has gone.
    fn send(&mut self, reply: &Reply) -> bool {
        if !self.gone && service::write_line(&mut self.stream, reply).is_err() {
            self.gone = true;
        }
        !self.gone
    }
}

/// One of the [`MAX_CLIENTS`] places, given back when the client is done (even by a panic).
struct Slot(Arc<AtomicUsize>);

impl Slot {
    fn take(count: &Arc<AtomicUsize>) -> Option<Self> {
        if count.fetch_add(1, Ordering::SeqCst) >= MAX_CLIENTS {
            count.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self(count.clone()))
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Unix seconds now.
fn now() -> i64 {
    artcraft_launcher_native::now()
}

/// A random number of seconds below `max`.
fn jitter(max: i64) -> i64 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_i64(now());
    hasher.write_u32(std::process::id());
    i64::try_from(hasher.finish() % max.max(1).unsigned_abs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use artcraft_launcher_core::release::Release;
    use artcraft_launcher_native::updater::Plan;

    use super::*;

    fn found(id: &str, installed: Option<&str>, plan: Plan) -> Found {
        let installed = installed.map(|v| artcraft_launcher_core::Installed {
            version: artcraft_launcher_core::version::parse(v),
            source: artcraft_launcher_core::InstallSource::Msi,
            location: String::new(),
            package: None,
        });
        let latest = Some(Release { tag: "v0.8.2".into(), version: artcraft_launcher_core::version::parse("0.8.2"), ..Release::default() });
        Found { app: CraftApp::by_id(id).unwrap(), installed, latest, plan }
    }

    #[test]
    fn clients_name_craft_apps_only_and_each_once() {
        let ids = |list: &[&str]| list.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(resolve(&ids(&[])).unwrap().len(), 0);
        let apps = resolve(&ids(&["photocraft", "wordcraft", "photocraft"])).unwrap();
        assert_eq!(apps.iter().map(|a| a.id).collect::<Vec<_>>(), ["photocraft", "wordcraft"]);
        assert_eq!(resolve(&ids(&["photocraft", "notepad"])).unwrap_err(), "notepad isn't a Craft App");
        assert!(resolve(&ids(&[r"..\..\windows\system32"])).is_err());
        // A flood of names is cut short, not looped over.
        let flood: Vec<String> = std::iter::repeat_n("photocraft".to_string(), 10_000).collect();
        assert_eq!(resolve(&flood).unwrap().len(), 1);
    }

    #[test]
    fn status_counts_in_updates_installed_since_the_last_survey() {
        let survey = Survey {
            at: 1000,
            apps: vec![
                found("photocraft", Some("0.8.1"), Plan::UpToDate),
                found("wordcraft", Some("0.6.0"), Plan::UpToDate),
                found("pdfcraft", None, Plan::NotInstalled),
            ],
            launcher: None,
        };
        let mut last = BTreeMap::new();
        let updated = Outcome::Updated { version: "0.8.2".into(), restart: false };
        last.insert("photocraft".to_string(), LastUpdate { at: 1500, outcome: updated.clone() });
        // An update from before the survey is history, not news.
        last.insert("wordcraft".to_string(), LastUpdate { at: 500, outcome: Outcome::Updated { version: "0.5.0".into(), restart: false } });
        let apps = summaries(Some(&survey), &last);
        let photo = apps.iter().find(|a| a.id == "photocraft").unwrap();
        assert_eq!((photo.installed.as_deref(), photo.state), (Some("0.8.2"), AppState::UpToDate));
        assert_eq!(photo.last_update.as_ref().map(|l| &l.outcome), Some(&updated));
        let word = apps.iter().find(|a| a.id == "wordcraft").unwrap();
        assert_eq!(word.installed.as_deref(), Some("0.6.0"));
        assert_eq!(apps.iter().find(|a| a.id == "pdfcraft").unwrap().state, AppState::NotInstalled);
        // Before any survey (just started): the remembered updates, and what they installed.
        last.insert("pdfcraft".to_string(), LastUpdate { at: 1600, outcome: Outcome::Running });
        let apps = summaries(None, &last);
        assert_eq!(apps.len(), 3);
        assert!(apps.iter().all(|a| a.last_update.is_some()));
        let photo = apps.iter().find(|a| a.id == "photocraft").unwrap();
        assert_eq!((photo.installed.as_deref(), photo.state), (Some("0.8.2"), AppState::UpToDate));
        let pdf = apps.iter().find(|a| a.id == "pdfcraft").unwrap();
        assert_eq!((pdf.installed.as_deref(), pdf.state), (None, AppState::Unknown), "not \"not installed\": it was never checked");
    }

    #[test]
    fn only_real_attempts_count_as_an_apps_last_update() {
        assert!(was_attempt(&Outcome::Updated { version: "1.0.0".into(), restart: false }));
        assert!(was_attempt(&Outcome::Failed { error: "offline".into() }));
        assert!(was_attempt(&Outcome::Running));
        assert!(!was_attempt(&Outcome::UpToDate { version: Some("1.0.0".into()) }));
        assert!(!was_attempt(&Outcome::NotEligible { reason: "installed for one user".into() }));
    }

    #[test]
    fn what_deserves_a_retry_before_the_next_check() {
        assert!(worth_retrying(&Outcome::Running));
        assert!(worth_retrying(&Outcome::Paused));
        assert!(!worth_retrying(&Outcome::Failed { error: "the download doesn't match its published checksum".into() }));
        assert!(!worth_retrying(&Outcome::Updated { version: "1.0.0".into(), restart: false }));
        assert!(!worth_retrying(&Outcome::Cancelled));
        #[cfg(windows)]
        assert!(worth_retrying(&Outcome::Failed { error: artcraft_launcher_native::install::windows::ANOTHER_INSTALL.into() }));
    }

    #[test]
    fn client_slots_run_out_and_come_back() {
        let count = Arc::new(AtomicUsize::new(0));
        let slots: Vec<Slot> = (0..MAX_CLIENTS).filter_map(|_| Slot::take(&count)).collect();
        assert_eq!(slots.len(), MAX_CLIENTS);
        assert!(Slot::take(&count).is_none());
        drop(slots);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        assert!(Slot::take(&count).is_some());
    }

    #[test]
    fn jitter_stays_in_range() {
        for _ in 0..100 {
            assert!((0..600).contains(&jitter(600)));
        }
        assert_eq!(jitter(0), 0);
    }

    /// A whole conversation over a real socket: a stranger is refused, a status request is
    /// answered, an unknown app is refused, and the service stops cleanly.
    #[test]
    fn talks_to_clients_over_loopback() {
        let dir = tempfile::tempdir().unwrap();
        let server = Server::new(Paths::under(dir.path()), Policy::default).unwrap();
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let talk = |line: &str| -> Vec<Reply> {
            let mut stream = TcpStream::connect(addr).unwrap();
            std::io::Write::write_all(&mut stream, line.as_bytes()).unwrap();
            let (accepted, _) = listener.accept().unwrap();
            server.serve(accepted);
            let mut lines = LineReader::new(stream);
            let mut replies = Vec::new();
            while let Ok(Some(line)) = lines.read_line() {
                replies.push(serde_json::from_str(&line).unwrap());
            }
            replies
        };
        let replies = talk("GET / HTTP/1.1\n");
        assert!(matches!(replies.as_slice(), [Reply::Refused { .. }, Reply::Done]), "{replies:?}");

        let status = serde_json::to_string(&Request::new(Op::Status)).unwrap();
        let replies = talk(&format!("{status}\n"));
        let [Reply::Status(status), Reply::Done] = replies.as_slice() else { panic!("{replies:?}") };
        assert_eq!(status.version, env!("CARGO_PKG_VERSION"));
        assert!(status.apps.is_empty() && status.busy.is_none());

        let update = serde_json::to_string(&Request::new(Op::Update { apps: vec!["notepad".into()] })).unwrap();
        let replies = talk(&format!("{update}\n"));
        assert_eq!(replies, [Reply::Refused { reason: "notepad isn't a Craft App".into() }, Reply::Done]);

        let cancel = serde_json::to_string(&Request::new(Op::Cancel { app: "photocraft".into() })).unwrap();
        assert_eq!(talk(&format!("{cancel}\n")), [Reply::Done]);

        // Background installs are the administrator's setting: a client can't change them (the
        // per-user design once let it), and status reports them as the registry has them.
        let asked = r#"{"hello":"artcraft-launcher-updater","proto":1,"op":{"auto-update":{"client":"0123456789abcdef","on":true}}}"#;
        assert!(matches!(talk(&format!("{asked}\n")).as_slice(), [Reply::Refused { .. }, Reply::Done]));
        assert!(!status.auto_update && !status.auto_update_managed, "off until an administrator turns them on");

        let mut called = 0;
        server.stop(&mut || called += 1);
        assert_eq!(called, 0, "nothing was installing");
        assert!(dir.path().join("data").join("state.json").is_file());
    }

    #[test]
    fn status_says_what_the_administrator_set() {
        fn set_by_group_policy() -> Policy {
            Policy { auto_update: true, auto_update_managed: true, ..Policy::default() }
        }
        let dir = tempfile::tempdir().unwrap();
        let server = Server::new(Paths::under(dir.path()), set_by_group_policy).unwrap();
        let status = server.status();
        assert!(status.auto_update && status.auto_update_managed);
    }

    #[test]
    fn a_state_file_from_the_per_user_design_still_loads() {
        let old: Saved = serde_json::from_str(r#"{"last_check":5,"last_updates":{},"auto_update_clients":{"0123456789abcdef":7}}"#).unwrap();
        assert_eq!(old.last_check, Some(5));
    }
}
