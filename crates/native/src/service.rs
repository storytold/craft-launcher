//! Talking to the ArtCraft Launcher Update Service (`artcraft-launcher-updater service`).
//!
//! On Windows the Craft Apps install for every user, into Program Files, so each update needs
//! administrator rights: a UAC prompt every time. The update service is the opt-in way round it.
//! An administrator installs it once (its own per-machine MSI); it runs as LocalSystem, installs
//! updates in the background on a schedule, and takes requests from the launcher and the
//! `artcraft-launcher-updater` command line, so a user's "Update" needs no prompt.
//!
//! It listens on a fixed loopback port. A client sends one request per connection, a line of
//! JSON; the service answers with lines of JSON until [`Reply::Done`] (or the connection ends).
//!
//! What a client can ask for is deliberately small, because any local program can connect:
//!
//! - It names apps by id, nothing else: never a path, a URL or a version. The service looks up
//!   the latest release itself (from storytold's releases, as the launcher does), downloads it
//!   into its own folder (which only administrators can write to), and checks it before running
//!   it: the published SHA-256, the publisher's Authenticode signature, and that the package is
//!   that app at that version (`msi::check`).
//! - It can only update apps an administrator already installed for every user, and only to a
//!   newer version. Installing a new app still goes through the launcher and its UAC prompt.
//! - Status, check now, update, and pause or cancel a running download. That's all: whether the
//!   service installs updates in the background is the computer's setting, which only an
//!   administrator can change (`policy`), never a request.
//!
//! The worst a stranger can do is make the service look for updates sooner, install a pending
//! update sooner, or postpone one.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use artcraft_launcher_core::CraftApp;
use artcraft_launcher_core::protocol::{CANCELLED, JobPhase, PAUSED};
use artcraft_launcher_core::version::{self, Version};
use serde::{Deserialize, Serialize};

use crate::http::Stop;

/// The loopback port the service listens on (the launcher's single-instance lock has 47413).
pub const PORT: u16 = 47_414;
/// The protocol this build speaks. A request in another one is refused.
pub const PROTOCOL: u32 = 1;
/// Every request starts by saying what it's for, so a stray connection is told apart.
pub const HELLO: &str = "artcraft-launcher-updater";
/// The Windows service's name (`sc query ArtCraftLauncherUpdater`).
pub const SERVICE_NAME: &str = "ArtCraftLauncherUpdater";
/// What Windows calls it in Services.
pub const DISPLAY_NAME: &str = "ArtCraft Launcher Update Service";
/// The longest line either side reads.
pub const MAX_LINE: usize = 64 * 1024;
/// How often a busy service says it's still at work, at the least.
pub const HEARTBEAT: Duration = Duration::from_secs(5);

/// Loopback answers at once or not at all.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
/// A client gives up on a service that said nothing (not even a heartbeat) for this long.
const SILENCE_LIMIT: Duration = Duration::from_secs(90);

/// One request: what the client wants, in which protocol.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub hello: String,
    pub proto: u32,
    pub op: Op,
}

impl Request {
    pub fn new(op: Op) -> Self {
        Self { hello: HELLO.into(), proto: PROTOCOL, op }
    }

    /// Reads a request line, refusing anything that isn't one of ours in a protocol we speak.
    pub fn parse(line: &str) -> Result<Self, String> {
        let request: Self = serde_json::from_str(line).map_err(|_| "not a request this service understands".to_string())?;
        if request.hello != HELLO {
            return Err("not a request for the update service".into());
        }
        if request.proto != PROTOCOL {
            return Err(format!("protocol {} isn't spoken here (this service speaks {PROTOCOL})", request.proto));
        }
        Ok(request)
    }
}

/// What a client can ask of the service.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Op {
    /// The service's settings, its last check and what it knows of each app.
    Status,
    /// Look up the latest releases now, then answer as [`Op::Status`] does.
    Check,
    /// Install the available updates of these apps (by id), or of every app with one when empty.
    Update { apps: Vec<String> },
    /// Pause a running download of this app (kept for the next try).
    Pause { app: String },
    /// Cancel a running download of this app.
    Cancel { app: String },
}

/// One line of the service's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "event")]
pub enum Reply {
    Status(Box<Status>),
    /// What a running update is doing (also repeated as a heartbeat while a step takes long).
    Progress {
        app: String,
        phase: Phase,
    },
    /// One app's update ended.
    Finished {
        app: String,
        outcome: Outcome,
    },
    /// The request wasn't carried out, and why (turned off by the administrator, unknown app).
    Refused {
        reason: String,
    },
    /// Still at it: a heartbeat while the request waits for its turn or the releases are looked up.
    Working,
    /// Nothing more to say.
    Done,
}

/// The service's state, for `status` and the launcher.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Status {
    /// The service's version.
    pub version: String,
    /// Updates are installed in the background as they come out (else only on request).
    pub auto_update: bool,
    /// Group Policy sets [`Status::auto_update`], so it can't be changed from the launcher.
    pub auto_update_managed: bool,
    /// Clients may ask for a check or an update (else the service keeps to its schedule).
    pub on_demand: bool,
    /// Hours between scheduled checks.
    pub interval_hours: u32,
    /// The last look at the releases (Unix seconds).
    pub last_check: Option<i64>,
    /// The next scheduled one (Unix seconds).
    pub next_check: Option<i64>,
    /// The app being updated right now.
    pub busy: Option<String>,
    pub apps: Vec<AppSummary>,
}

/// One app, as the service last saw it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSummary {
    pub id: String,
    /// The installed version (for all users), if any.
    pub installed: Option<String>,
    /// The latest release's version, if known.
    pub latest: Option<String>,
    pub state: AppState,
    /// Why the state is what it is, when that needs saying (lookup failed, not installed for all users).
    pub note: Option<String>,
    /// The service's last update of this app.
    pub last_update: Option<LastUpdate>,
}

/// Where an app stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AppState {
    #[default]
    NotInstalled,
    UpToDate,
    UpdateAvailable,
    /// Couldn't tell (the lookup failed, or a version is unreadable).
    Unknown,
    /// Installed, but not in a way this updater may replace (see the note).
    Blocked,
}

/// How an update went, and when (Unix seconds).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastUpdate {
    pub at: i64,
    pub outcome: Outcome,
}

/// What a running update is doing: [`JobPhase`] on the wire.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "step")]
pub enum Phase {
    Downloading { done: u64, total: u64 },
    Reconnecting { done: u64, total: u64 },
    Verifying,
    Waiting,
    Installing,
    Removing,
}

impl From<JobPhase> for Phase {
    fn from(p: JobPhase) -> Self {
        match p {
            JobPhase::Downloading { done, total } => Self::Downloading { done, total },
            JobPhase::Reconnecting { done, total } => Self::Reconnecting { done, total },
            JobPhase::Verifying => Self::Verifying,
            JobPhase::Waiting => Self::Waiting,
            JobPhase::Installing => Self::Installing,
            JobPhase::Removing => Self::Removing,
        }
    }
}

impl From<Phase> for JobPhase {
    fn from(p: Phase) -> Self {
        match p {
            Phase::Downloading { done, total } => Self::Downloading { done, total },
            Phase::Reconnecting { done, total } => Self::Reconnecting { done, total },
            Phase::Verifying => Self::Verifying,
            Phase::Waiting => Self::Waiting,
            Phase::Installing => Self::Installing,
            Phase::Removing => Self::Removing,
        }
    }
}

/// How one app's update ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "result")]
pub enum Outcome {
    /// Installed. `restart`: Windows wants a restart to finish replacing files.
    Updated {
        version: String,
        restart: bool,
    },
    /// Nothing newer to install (`version` is what's installed).
    UpToDate {
        version: Option<String>,
    },
    /// The app is open, so it was left alone; the service tries again later.
    Running,
    /// Not an install this updater may replace (not installed for all users, not at all, or
    /// managed by something else): why.
    NotEligible {
        reason: String,
    },
    Paused,
    Cancelled,
    Failed {
        error: String,
    },
}

impl Outcome {
    /// One line for people (the command line, the log).
    pub fn describe(&self, app: &str) -> String {
        match self {
            Self::Updated { version, restart: false } => format!("{app} updated to {version}"),
            Self::Updated { version, restart: true } => format!("{app} updated to {version}; restart Windows to finish"),
            Self::UpToDate { version: Some(v) } => format!("{app} is up to date ({v})"),
            Self::UpToDate { version: None } => format!("{app} is up to date"),
            Self::Running => format!("{app} is running. Quit it, then try again."),
            Self::Paused => format!("{app}: download paused"),
            Self::Cancelled => format!("{app}: cancelled"),
            // A reason that names the app already reads as a sentence of its own.
            Self::NotEligible { reason: why } | Self::Failed { error: why } if !app.is_empty() && why.starts_with(app) => why.clone(),
            Self::NotEligible { reason: why } | Self::Failed { error: why } => format!("{app}: {why}"),
        }
    }

    /// Whether this ended the way it should (installed, or nothing to do).
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Updated { .. } | Self::UpToDate { .. })
    }
}

/// Reads newline-ended lines of at most [`MAX_LINE`] bytes from something hostile: a peer that
/// never ends a line can't make it buffer without bound, and a read that times out keeps what
/// came so far for the next call.
pub struct LineReader<R> {
    inner: BufReader<R>,
    buf: Vec<u8>,
}

impl<R: Read> LineReader<R> {
    pub fn new(inner: R) -> Self {
        Self { inner: BufReader::new(inner), buf: Vec::new() }
    }

    /// The next line without its line ending; `Ok(None)` when the read timed out first.
    pub fn read_line(&mut self) -> Result<Option<String>, String> {
        loop {
            let available = match self.inner.fill_buf() {
                Ok(bytes) => bytes,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => return Ok(None),
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(format!("the connection failed ({e})")),
            };
            if available.is_empty() {
                return Err("the connection closed".into());
            }
            let end = available.iter().position(|b| *b == b'\n');
            let take = end.map_or(available.len(), |i| i.saturating_add(1));
            let chunk = available.get(..take).unwrap_or_default();
            if self.buf.len().saturating_add(chunk.len()) > MAX_LINE.saturating_add(2) {
                return Err("the other side sent an overlong line".into());
            }
            self.buf.extend_from_slice(chunk);
            self.inner.consume(take);
            if end.is_some() {
                let mut line = std::mem::take(&mut self.buf);
                while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                    line.pop();
                }
                return String::from_utf8(line).map(Some).map_err(|_| "the other side sent text that isn't UTF-8".to_string());
            }
        }
    }
}

/// Writes one message as a line of JSON.
pub fn write_line<T: Serialize>(out: &mut impl Write, message: &T) -> Result<(), String> {
    let mut line = serde_json::to_vec(message).map_err(|e| e.to_string())?;
    line.push(b'\n');
    out.write_all(&line).and_then(|()| out.flush()).map_err(|e| format!("couldn't send to the other side ({e})"))
}

/// A connection to the update service.
pub struct Connection {
    stream: TcpStream,
    lines: LineReader<TcpStream>,
    heard: Instant,
}

impl Connection {
    /// Connects to the update service on this machine, if it's listening.
    pub fn open() -> Result<Self, String> {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, PORT));
        let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| format!("the update service isn't running ({e})"))?;
        let _ = stream.set_nodelay(true);
        let reading = stream.try_clone().map_err(|e| e.to_string())?;
        Ok(Self { stream, lines: LineReader::new(reading), heard: Instant::now() })
    }

    /// Sends the request.
    pub fn send(&mut self, op: Op) -> Result<(), String> {
        self.heard = Instant::now();
        write_line(&mut self.stream, &Request::new(op))
    }

    /// The next reply, waiting at most `wait`; `Ok(None)` when none came that soon. Fails once
    /// the service has been silent (heartbeats included) for far longer than it ever should be.
    pub fn next(&mut self, wait: Duration) -> Result<Option<Reply>, String> {
        let _ = self.stream.set_read_timeout(Some(wait.max(Duration::from_millis(1))));
        match self.lines.read_line()? {
            Some(line) => {
                self.heard = Instant::now();
                serde_json::from_str(&line).map(Some).map_err(|_| "the update service sent something unreadable".to_string())
            }
            None if self.heard.elapsed() > SILENCE_LIMIT => Err("the update service stopped answering".into()),
            None => Ok(None),
        }
    }

    /// The next reply, however long it takes (within the silence limit).
    pub fn wait(&mut self) -> Result<Reply, String> {
        loop {
            if let Some(reply) = self.next(Duration::from_secs(1))? {
                return Ok(reply);
            }
        }
    }
}

/// Whether the update service is listening on this machine.
pub fn running() -> bool {
    Connection::open().is_ok()
}

/// Asks the service for its status (`check`: after looking up the releases first).
pub fn status(check: bool) -> Result<Status, String> {
    let mut conn = Connection::open()?;
    conn.send(if check { Op::Check } else { Op::Status })?;
    loop {
        match conn.wait()? {
            Reply::Status(status) => return Ok(*status),
            Reply::Refused { reason } => return Err(reason),
            Reply::Done => return Err("the update service didn't say how it is".into()),
            Reply::Progress { .. } | Reply::Finished { .. } | Reply::Working => {}
        }
    }
}

/// Asks the service to pause or cancel `app`'s running download. Best effort.
pub fn stop_job(app: &str, cancel: bool) {
    let op = if cancel { Op::Cancel { app: app.to_string() } } else { Op::Pause { app: app.to_string() } };
    if let Err(e) = Connection::open().and_then(|mut c| c.send(op)) {
        log::warn!("couldn't ask the update service to stop {app}: {e}");
    }
}

/// What came of handing an update to the service.
#[derive(Debug, PartialEq, Eq)]
pub enum Delegated {
    /// The service took the job, and this is how it ended (the launcher's job result).
    Done(Result<String, String>),
    /// Not one for the service (it isn't running, the app isn't installed for all users, or
    /// requests are turned off): the launcher installs it the usual way.
    Declined(String),
}

/// Hands the update of `app` to the service, if one is running. `latest` is the release the
/// launcher means to install: when the service holds a different view of what's newest (the
/// user follows pre-releases, the service doesn't), the launcher installs it itself. `stop` is
/// the launcher's own Pause and Cancel; `phase` hears the service's progress.
pub fn delegate(app: &CraftApp, latest: Option<&Version>, stop: &Stop, phase: &mut dyn FnMut(JobPhase)) -> Delegated {
    let mut conn = match Connection::open() {
        Ok(c) => c,
        Err(e) => return Delegated::Declined(e),
    };
    if let Err(e) = conn.send(Op::Update { apps: vec![app.id.to_string()] }) {
        return Delegated::Declined(e);
    }
    log::info!("{}: handing the update to the {DISPLAY_NAME}", app.name);
    let mut started = false;
    let mut stop_sent = false;
    loop {
        if !stop_sent && let Err(why) = stop.check() {
            stop_job(app.id, why != PAUSED);
            stop_sent = true;
        }
        let reply = match conn.next(Duration::from_millis(200)) {
            Ok(Some(reply)) => reply,
            Ok(None) => continue,
            Err(e) if started => return Delegated::Done(Err(format!("{DISPLAY_NAME}: {e}"))),
            Err(e) => return Delegated::Declined(e),
        };
        match reply {
            Reply::Progress { phase: p, .. } => {
                started = true;
                phase(p.into());
            }
            Reply::Finished { outcome, .. } => return delegated_result(app, latest, outcome),
            Reply::Refused { reason } => return Delegated::Declined(reason),
            Reply::Done if started => return Delegated::Done(Err(format!("{DISPLAY_NAME} ended without saying how the update went"))),
            Reply::Done => return Delegated::Declined("the update service had nothing to say".into()),
            Reply::Status(_) | Reply::Working => {}
        }
    }
}

/// The launcher's job result for an `outcome` the service reported.
fn delegated_result(app: &CraftApp, latest: Option<&Version>, outcome: Outcome) -> Delegated {
    match outcome {
        Outcome::Updated { version, restart } => {
            if restart {
                log::info!("{}: Windows wants a restart to finish the update", app.name);
            }
            Delegated::Done(Ok(format!("{} updated to {version}", app.name)))
        }
        // The service sees nothing newer. If the launcher does (it follows pre-releases, or the
        // service's answer is older), it installs that itself; otherwise the app is current.
        Outcome::UpToDate { version: installed } => {
            let behind = match (latest, installed.as_deref().and_then(version::parse)) {
                (Some(want), Some(have)) => version::is_update(want, &have),
                _ => true,
            };
            match installed {
                Some(v) if !behind => Delegated::Done(Ok(format!("{} updated to {v}", app.name))),
                _ => Delegated::Declined("the update service has nothing newer".into()),
            }
        }
        Outcome::Running => Delegated::Done(Err(format!("{} is running. Quit it, then try again.", app.name))),
        Outcome::NotEligible { reason } => Delegated::Declined(reason),
        Outcome::Paused => Delegated::Done(Err(PAUSED.into())),
        Outcome::Cancelled => Delegated::Done(Err(CANCELLED.into())),
        Outcome::Failed { error } => Delegated::Done(Err(error)),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn requests_round_trip_and_strangers_are_refused() {
        for op in [
            Op::Status,
            Op::Check,
            Op::Update { apps: vec![] },
            Op::Update { apps: vec!["photocraft".into(), "wordcraft".into()] },
            Op::Pause { app: "photocraft".into() },
            Op::Cancel { app: "photocraft".into() },
        ] {
            let line = serde_json::to_string(&Request::new(op.clone())).unwrap();
            assert!(!line.contains('\n'));
            assert_eq!(Request::parse(&line).unwrap().op, op);
        }
        assert!(Request::parse(r#"{"hello":"artcraft-launcher-updater","proto":1,"op":"status"}"#).is_ok());
        assert!(Request::parse(r#"{"hello":"something-else","proto":1,"op":"status"}"#).is_err());
        assert!(Request::parse(r#"{"hello":"artcraft-launcher-updater","proto":2,"op":"status"}"#).unwrap_err().contains("protocol 2"));
        assert!(Request::parse(r#"{"hello":"artcraft-launcher-updater","proto":1,"op":"format-disk"}"#).is_err());
        assert!(Request::parse(r#"{"hello":"artcraft-launcher-updater","proto":1,"op":{"update":{"apps":"photocraft"}}}"#).is_err());
        assert!(Request::parse("GET / HTTP/1.1").is_err());
        assert!(Request::parse("").is_err());
    }

    #[test]
    fn replies_round_trip() {
        let replies = [
            Reply::Status(Box::new(Status {
                version: "0.3.2".into(),
                auto_update: true,
                auto_update_managed: true,
                on_demand: true,
                interval_hours: 6,
                last_check: Some(1_791_500_000),
                next_check: None,
                busy: Some("photocraft".into()),
                apps: vec![AppSummary {
                    id: "photocraft".into(),
                    installed: Some("0.8.1".into()),
                    latest: Some("0.8.2".into()),
                    state: AppState::UpdateAvailable,
                    note: None,
                    last_update: Some(LastUpdate { at: 1_791_400_000, outcome: Outcome::Running }),
                }],
            })),
            Reply::Progress { app: "photocraft".into(), phase: Phase::Downloading { done: 5, total: 10 } },
            Reply::Progress { app: "photocraft".into(), phase: Phase::Installing },
            Reply::Finished { app: "photocraft".into(), outcome: Outcome::Updated { version: "0.8.2".into(), restart: false } },
            Reply::Finished { app: "photocraft".into(), outcome: Outcome::Failed { error: "nope".into() } },
            Reply::Finished { app: "photocraft".into(), outcome: Outcome::UpToDate { version: None } },
            Reply::Refused { reason: "turned off".into() },
            Reply::Working,
            Reply::Done,
        ];
        for reply in replies {
            let line = serde_json::to_string(&reply).unwrap();
            assert!(!line.contains('\n'));
            assert_eq!(serde_json::from_str::<Reply>(&line).unwrap(), reply, "{line}");
        }
        // A newer service may add fields; an older client still reads what it knows.
        let status: Reply = serde_json::from_str(r#"{"event":"status","version":"9.9.9","shiny":true}"#).unwrap();
        assert!(matches!(status, Reply::Status(s) if s.version == "9.9.9" && s.apps.is_empty()));
    }

    #[test]
    fn no_request_can_change_whether_updates_install_in_the_background() {
        // That's the computer's setting, an administrator's to change (`policy`). Asking the
        // service for it, as the per-user design once did, is not something it understands.
        let asked = r#"{"hello":"artcraft-launcher-updater","proto":1,"op":{"auto-update":{"client":"0123456789abcdef","on":false}}}"#;
        assert!(Request::parse(asked).is_err());
    }

    #[test]
    fn phases_survive_the_wire() {
        for p in [
            JobPhase::Downloading { done: 1, total: 2 },
            JobPhase::Reconnecting { done: 1, total: 2 },
            JobPhase::Verifying,
            JobPhase::Waiting,
            JobPhase::Installing,
            JobPhase::Removing,
        ] {
            assert_eq!(JobPhase::from(Phase::from(p)), p);
        }
    }

    #[test]
    fn lines_are_read_whole_and_bounded() {
        let mut lines = LineReader::new(Cursor::new(b"one\r\ntwo\n\nthree".to_vec()));
        assert_eq!(lines.read_line().unwrap().as_deref(), Some("one"));
        assert_eq!(lines.read_line().unwrap().as_deref(), Some("two"));
        assert_eq!(lines.read_line().unwrap().as_deref(), Some(""));
        // A last line without its newline never counts as a whole one.
        assert!(lines.read_line().is_err());

        let mut long = vec![b'x'; MAX_LINE + 10];
        long.push(b'\n');
        assert!(LineReader::new(Cursor::new(long)).read_line().unwrap_err().contains("overlong"));
        assert!(LineReader::new(Cursor::new(vec![0xff, 0xfe, b'\n'])).read_line().is_err());
    }

    /// A reader that times out once in the middle of a line, as a socket with a read timeout does.
    struct Stutter {
        parts: Vec<std::io::Result<Vec<u8>>>,
    }

    impl Read for Stutter {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if self.parts.is_empty() {
                return Ok(0);
            }
            let part = self.parts.remove(0)?;
            out[..part.len()].copy_from_slice(&part);
            Ok(part.len())
        }
    }

    #[test]
    fn a_timeout_mid_line_keeps_what_came() {
        let timed_out = || Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "slow"));
        let mut lines = LineReader::new(Stutter { parts: vec![Ok(b"{\"event\":".to_vec()), timed_out(), Ok(b"\"done\"}\n".to_vec())] });
        assert_eq!(lines.read_line().unwrap(), None);
        assert_eq!(lines.read_line().unwrap().as_deref(), Some(r#"{"event":"done"}"#));
    }

    fn photocraft() -> &'static CraftApp {
        CraftApp::by_id("photocraft").unwrap()
    }

    #[test]
    fn the_services_outcome_becomes_the_launchers_job_result() {
        let v = |s: &str| version::parse(s).unwrap();
        let latest = v("0.8.2");
        let done = |outcome| delegated_result(photocraft(), Some(&latest), outcome);
        assert_eq!(done(Outcome::Updated { version: "0.8.2".into(), restart: true }), Delegated::Done(Ok("PhotoCraft updated to 0.8.2".into())));
        assert_eq!(done(Outcome::Running), Delegated::Done(Err("PhotoCraft is running. Quit it, then try again.".into())));
        assert_eq!(done(Outcome::Paused), Delegated::Done(Err(PAUSED.into())));
        assert_eq!(done(Outcome::Cancelled), Delegated::Done(Err(CANCELLED.into())));
        assert_eq!(done(Outcome::Failed { error: "the download doesn't match".into() }), Delegated::Done(Err("the download doesn't match".into())));
        // Not the service's to do: the launcher carries on as before.
        assert!(matches!(done(Outcome::NotEligible { reason: "installed for one user".into() }), Delegated::Declined(_)));
        // Already at the version the launcher wanted (the service just installed it).
        assert_eq!(done(Outcome::UpToDate { version: Some("0.8.2".into()) }), Delegated::Done(Ok("PhotoCraft updated to 0.8.2".into())));
        // The launcher wants something newer than the service sees (a pre-release): its own way.
        assert!(matches!(done(Outcome::UpToDate { version: Some("0.8.1".into()) }), Delegated::Declined(_)));
        assert!(matches!(done(Outcome::UpToDate { version: None }), Delegated::Declined(_)));
    }

    #[test]
    fn with_no_service_running_the_launcher_does_it_itself() {
        // Nothing listens on the port during tests (unless a real service is installed here).
        if Connection::open().is_ok() {
            return;
        }
        let stop = Stop::default();
        let result = delegate(photocraft(), None, &stop, &mut |_| {});
        assert!(matches!(result, Delegated::Declined(_)), "{result:?}");
        assert!(status(false).is_err());
    }

    #[test]
    fn outcomes_read_well() {
        assert_eq!(Outcome::Updated { version: "1.2.3".into(), restart: false }.describe("PhotoCraft"), "PhotoCraft updated to 1.2.3");
        assert_eq!(Outcome::UpToDate { version: Some("1.2.3".into()) }.describe("PhotoCraft"), "PhotoCraft is up to date (1.2.3)");
        let not_here = Outcome::NotEligible { reason: "PhotoCraft isn't installed for all users".into() };
        assert_eq!(not_here.describe("PhotoCraft"), "PhotoCraft isn't installed for all users");
        let failed = Outcome::Failed { error: "the download doesn't match its published checksum".into() };
        assert_eq!(failed.describe("PhotoCraft"), "PhotoCraft: the download doesn't match its published checksum");
        assert!(Outcome::UpToDate { version: None }.is_success() && !Outcome::Running.is_success());
    }
}
