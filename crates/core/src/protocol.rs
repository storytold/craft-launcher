//! How the UI talks to a backend: the desktop backend installs apps; the web backend can only
//! look up releases and hand out download links.
//!
//! The UI sends [`Command`]s and drains [`Event`]s once per frame, so backends do their work on
//! threads (desktop) or futures (web) and never block a frame.

use serde::{Deserialize, Serialize};

use crate::platform::{PackageKind, Platform};
use crate::release::Release;
use crate::status::Installed;

/// Asks the backend to do something. Results arrive later as [`Event`]s.
#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    /// Look up the latest releases. `force` skips the freshness window (the user pressed Refresh).
    CheckForUpdates {
        force: bool,
    },
    /// Look again at what's installed.
    Detect,
    /// Install or update an app to its latest release.
    Install {
        app: String,
    },
    /// Fetch one page of published releases without blocking the UI.
    LoadReleases {
        app: String,
        page: u32,
        force: bool,
    },
    /// Install an exact release offered by the backend, then commit its preferences on success.
    InstallRelease {
        app: String,
        tag: String,
        preferences: AppPreferences,
        /// The user confirmed that downgrading an MSI first removes the installed version.
        confirmed_msi_downgrade: bool,
    },
    PickInstallDirectory {
        app: String,
    },
    Uninstall {
        app: String,
    },
    Launch {
        app: String,
    },
    /// Show the installed app in the file manager.
    Reveal {
        app: String,
    },
    /// Pause a running download, keeping what's downloaded so far (`Install` resumes it).
    Pause {
        app: String,
    },
    /// Stop a running download, or throw away a paused one or one kept after a failed install.
    Cancel {
        app: String,
    },
    /// Download the newer ArtCraft Launcher, put it in place and restart into it.
    UpdateLauncher,
    /// Change how the backend behaves.
    Configure(BackendOptions),
}

/// Settings the backend needs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendOptions {
    /// Package formats to install, best first (empty: the platform default).
    pub kinds: Vec<PackageKind>,
    /// Offer pre-releases (`0.6.0-rc.1`) as updates.
    pub prereleases: bool,
    #[serde(default)]
    pub apps: std::collections::BTreeMap<String, AppPreferences>,
}

/// Per-app choices, independent of the detected installation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppPreferences {
    pub pinned_tag: Option<String>,
    /// Parent folder for a new portable installation. Existing installs retain their location.
    pub install_dir: Option<String>,
    pub package: Option<PackageKind>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleasePage {
    pub releases: Vec<Release>,
    pub more: bool,
}

/// The answer to "what's the latest release of this app?".
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum LatestRelease {
    Found(Release),
    /// The repo exists but has no published release.
    None,
    /// Couldn't find out (offline, rate-limited, unreadable answer).
    Failed(String),
}

/// What a running job is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobPhase {
    Downloading {
        done: u64,
        total: u64,
    },
    /// The connection dropped; trying again shortly, from where it stopped.
    Reconnecting {
        done: u64,
        total: u64,
    },
    Verifying,
    /// The system's installer is busy with another app; this one goes next.
    Waiting,
    /// Waiting on the OS installer (MSI) or copying files.
    Installing,
    Removing,
}

/// A partly downloaded file waiting to be resumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PausedDownload {
    pub done: u64,
    /// 0 when unknown.
    pub total: u64,
}

/// The `JobFinished` error a paused job ends with (not a failure).
pub const PAUSED: &str = "paused";
/// The `JobFinished` error a cancelled job ends with (not a failure).
pub const CANCELLED: &str = "cancelled";
/// The `JobFinished` error of an install whose system prompt (password, Windows Installer) the
/// user dismissed after the download finished (not a failure).
pub const INSTALL_CANCELLED: &str = "install cancelled";

/// Which service an app's release information came from (the footer names them).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Origin {
    /// ArtCraft's release feed (storyteller-web), which polls GitHub for everyone.
    ArtCraftApi,
    /// GitHub directly: the fallback.
    GitHub,
}

impl Origin {
    pub fn label(self) -> &'static str {
        match self {
            Self::ArtCraftApi => "ArtCraft API",
            Self::GitHub => "GitHub",
        }
    }
}

/// Where release information came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    /// ArtCraft's release feed (storyteller-web), which polls GitHub for everyone.
    Feed,
    /// The GitHub REST API.
    GitHubApi,
    /// github.com release pages (no API quota; used when the API is rate-limited).
    GitHubPages,
    /// Saved answers from an earlier check.
    Cache,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::Feed => "ArtCraft",
            Self::GitHubApi => "GitHub",
            Self::GitHubPages => "GitHub (release pages)",
            Self::Cache => "saved results",
        }
    }
}

/// A release-history failure, optionally eligible for an automatic retry.
#[derive(Clone, Debug, PartialEq)]
pub struct ReleaseError {
    pub message: String,
    /// A transient failure may be retried, after at least this many seconds.
    pub retry_after: Option<u64>,
}

impl From<String> for ReleaseError {
    fn from(message: String) -> Self {
        Self { message, retry_after: None }
    }
}

impl From<&str> for ReleaseError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

/// Something the backend wants the UI to know.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Releases {
        app: String,
        page: u32,
        result: Result<ReleasePage, ReleaseError>,
    },
    /// Sent only once the exact requested version has been detected on disk.
    VersionInstalled {
        app: String,
        preferences: AppPreferences,
    },
    InstallDirectory {
        app: String,
        directory: Option<String>,
    },
    Latest {
        app: String,
        latest: LatestRelease,
        /// The service this answer came from.
        from: Origin,
    },
    /// The latest ArtCraft Launcher release, when it's newer than this one.
    LauncherUpdate(Release),
    /// The new launcher is in place (or will be, once this one exits): quit now so it can start.
    LauncherRestarting,
    /// A verified launcher download has been scheduled. Commit preferences only after restart
    /// into the selected version; the exit helper may still fail to install it.
    LauncherSwitchScheduled {
        tag: String,
        preferences: AppPreferences,
    },
    /// News and alerts from ArtCraft's release feed (replaces any earlier set).
    Notices {
        announcements: Vec<crate::feed::Notice>,
        alerts: Vec<crate::feed::Notice>,
    },
    Detected {
        app: String,
        installed: Option<Installed>,
    },
    Progress {
        app: String,
        phase: JobPhase,
    },
    /// A download is paused (by the user, by a lost connection, or left over from an earlier
    /// run) and can be resumed; `None` once there's no paused download.
    Paused {
        app: String,
        download: Option<PausedDownload>,
    },
    /// Whether a finished download is waiting because its install step failed; `Install` checks
    /// it again and goes straight to installing.
    ReadyToInstall {
        app: String,
        ready: bool,
    },
    /// A job ended: `Ok` carries a short success message, `Err` what went wrong.
    JobFinished {
        app: String,
        result: Result<String, String>,
    },
    CheckStarted,
    CheckFinished {
        /// Unix seconds.
        at: i64,
        source: Source,
        /// GitHub refused more API calls until this time (Unix seconds).
        rate_limited_until: Option<i64>,
    },
}

/// What a backend can do on this platform.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capabilities {
    /// Install, update, remove and launch apps (desktop). Without it the UI offers downloads.
    pub manage_apps: bool,
    /// What "show in file manager" is called here (`Show in Finder`).
    pub reveal_label: &'static str,
    /// The launcher can update itself in place and restart (an installed build, not a dev build or
    /// a Flatpak). Without it the UI offers the download instead.
    pub self_update: bool,
}

/// The UI's view of a backend.
pub trait Backend {
    fn send(&mut self, command: Command);
    /// Events since the last call, oldest first.
    fn poll(&mut self) -> Vec<Event>;
    fn capabilities(&self) -> Capabilities;
    /// The running launcher, including its installation format.
    fn launcher_installation(&self) -> Option<Installed> {
        None
    }
    /// The machine apps would be installed on (on the web: the visitor's, best guess).
    fn platform(&self) -> Option<Platform>;
    /// Unix seconds now.
    fn now(&self) -> i64;
}
