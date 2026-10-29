//! How the UI talks to a backend: the desktop backend installs apps; the web backend can only
//! look up releases and hand out download links.
//!
//! The UI sends [`Command`]s and drains [`Event`]s once per frame, so backends do their work on
//! threads (desktop) or futures (web) and never block a frame.

use serde::{Deserialize, Serialize};

use crate::platform::{PackageKind, Platform};
use crate::release::Release;
use crate::status::Installed;

// What a job is doing, how it ended, where new installs go and what a release lookup found:
// shared with every Craft app's installer through craft-update.
pub use craft_update::protocol::{CANCELLED, INSTALL_CANCELLED, InstallLocations, JobPhase, LatestRelease, PAUSED, Source};

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
    /// `Install`, writing over what [`Event::Occupied`] reported.
    Replace {
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
    /// Fetch the pictures of an app's details page (`core::details`), one [`Event::Screenshot`]
    /// each, the lead picture first. Pictures already saved on this machine arrive without
    /// touching the network. Asking again while they are on their way does nothing.
    FetchScreenshots {
        app: String,
    },
    /// Change how the backend behaves.
    Configure(BackendOptions),
}

/// Settings the backend needs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackendOptions {
    pub install_locations: InstallLocations,
    /// Package formats to install, best first (empty: the platform default).
    pub kinds: Vec<PackageKind>,
    /// Offer pre-releases (`0.6.0-rc.1`) as updates.
    pub prereleases: bool,
}

/// A partly downloaded file waiting to be resumed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PausedDownload {
    pub done: u64,
    /// 0 when unknown.
    pub total: u64,
}

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

/// Something the backend wants the UI to know.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
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
    /// A new install stopped before downloading because `path` already exists and the launcher
    /// didn't put it there. `Command::Replace` installs over it.
    Occupied {
        app: String,
        path: String,
    },
    /// A job ended: `Ok` carries a short success message, `Err` what went wrong.
    JobFinished {
        app: String,
        result: Result<String, String>,
    },
    /// One picture of an app's details page, or why it couldn't be had (offline, not published,
    /// not a JPEG). `key` is the [`crate::details::Shot::key`].
    Screenshot {
        app: String,
        key: String,
        result: Result<Vec<u8>, String>,
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
    /// a Flatpak from anywhere but the release bundle). Without it the UI offers the download
    /// instead. A backend may only know this once it has found a newer launcher, so the UI reads
    /// it again on [`Event::LauncherUpdate`].
    pub self_update: bool,
}

/// The UI's view of a backend.
pub trait Backend {
    fn send(&mut self, command: Command);
    /// Events since the last call, oldest first.
    fn poll(&mut self) -> Vec<Event>;
    fn capabilities(&self) -> Capabilities;
    /// The machine apps would be installed on (on the web: the visitor's, best guess).
    fn platform(&self) -> Option<Platform>;
    /// Unix seconds now.
    fn now(&self) -> i64;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_folders_inherit_the_default_unless_the_app_has_an_override() {
        let mut locations = InstallLocations::default();
        assert_eq!(locations.folder("photocraft"), None);
        locations.root = Some("/Volumes/Craft".into());
        locations.per_app.insert("photocraft".into(), "/Volumes/Photo".into());
        assert_eq!(locations.folder("photocraft"), Some("/Volumes/Photo"));
        assert_eq!(locations.folder("filmcraft"), Some("/Volumes/Craft"));
        locations.per_app.remove("photocraft");
        assert_eq!(locations.folder("photocraft"), Some("/Volumes/Craft"));
    }

    #[test]
    fn old_backend_options_keep_system_install_locations() {
        let options: BackendOptions = serde_json::from_str(r#"{"kinds":[],"prereleases":true}"#).unwrap();
        assert!(options.prereleases);
        assert_eq!(options.install_locations, InstallLocations::default());
    }
}
