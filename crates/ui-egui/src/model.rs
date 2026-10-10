//! What the UI knows about each app.

use artcraft_launcher_core::platform::{Platform, pick_asset};
use artcraft_launcher_core::protocol::{JobPhase, LatestRelease, PausedDownload};
use artcraft_launcher_core::release::Release;
use artcraft_launcher_core::status::{self, AppStatus, Latest};
use artcraft_launcher_core::{CraftApp, Installed};

#[derive(Clone, Debug)]
pub struct AppEntry {
    pub app: &'static CraftApp,
    pub latest: Option<LatestRelease>,
    pub installed: Option<Installed>,
    /// Detection has answered at least once.
    pub detected: bool,
    pub job: Option<JobPhase>,
    /// The service `latest` came from.
    pub origin: Option<artcraft_launcher_core::protocol::Origin>,
    /// A download waiting to be resumed.
    pub paused: Option<PausedDownload>,
    /// A finished download waiting for another try at installing it.
    pub ready: bool,
}

impl AppEntry {
    pub fn new(app: &'static CraftApp) -> Self {
        Self { app, latest: None, installed: None, detected: false, job: None, origin: None, paused: None, ready: false }
    }

    pub fn release(&self) -> Option<&Release> {
        match &self.latest {
            Some(LatestRelease::Found(r)) => Some(r),
            _ => None,
        }
    }

    /// Whether the latest release has something for `platform`.
    pub fn has_download(&self, platform: Option<Platform>) -> bool {
        let (Some(r), Some(p)) = (self.release(), platform) else { return false };
        let stems: Vec<&str> = self.app.stems().collect();
        pick_asset(r, &stems, p, p.download_kinds()).is_some()
    }

    pub fn status(&self, platform: Option<Platform>, manage_apps: bool) -> AppStatus {
        if manage_apps && !self.detected {
            return AppStatus::Checking;
        }
        let latest = match &self.latest {
            None => Latest::Pending,
            Some(LatestRelease::Failed(_)) => Latest::Failed,
            Some(LatestRelease::None) => Latest::NoRelease,
            Some(LatestRelease::Found(r)) => Latest::Found { version: r.version.as_ref(), has_asset: self.has_download(platform) },
        };
        status::status(self.installed.as_ref(), latest)
    }

    pub fn installed_label(&self) -> Option<String> {
        self.installed.as_ref().map(|i| i.version.as_ref().map_or_else(|| "installed".to_string(), ToString::to_string))
    }

    pub fn latest_label(&self) -> Option<String> {
        self.release().map(Release::version_label)
    }

    pub fn failure(&self) -> Option<&str> {
        match &self.latest {
            Some(LatestRelease::Failed(why)) => Some(why),
            _ => None,
        }
    }
}

/// The colour role of a status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Good,
    Warn,
    Info,
    Neutral,
    Faint,
}

pub fn status_text(status: &AppStatus, manage_apps: bool) -> (&'static str, Tone) {
    match status {
        AppStatus::Checking => ("Checking…", Tone::Faint),
        AppStatus::Available if manage_apps => ("Available to install", Tone::Info),
        AppStatus::Available => ("Available", Tone::Info),
        AppStatus::UpToDate => ("Installed · up to date", Tone::Good),
        AppStatus::UpdateAvailable => ("Update available", Tone::Warn),
        AppStatus::Installed => ("Installed", Tone::Neutral),
        AppStatus::Unavailable => ("Not available here", Tone::Faint),
        AppStatus::NoRelease => ("Coming soon", Tone::Faint),
    }
}

/// [`status_text`], but saying so when a download is paused or reconnecting.
pub fn entry_status_text(e: &AppEntry, status: &AppStatus, manage_apps: bool) -> (&'static str, Tone) {
    match (e.job, e.paused) {
        (Some(JobPhase::Reconnecting { .. }), _) => ("Reconnecting…", Tone::Warn),
        (Some(JobPhase::Downloading { .. }), _) if status.is_installed() => ("Updating…", Tone::Info),
        (Some(JobPhase::Downloading { .. }), _) => ("Downloading…", Tone::Info),
        (None, Some(_)) if status.is_installed() => ("Update paused", Tone::Warn),
        (None, Some(_)) => ("Download paused", Tone::Warn),
        _ => status_text(status, manage_apps),
    }
}

/// Sort rank for "by status": things needing attention first.
pub fn status_rank(status: &AppStatus) -> u8 {
    match status {
        AppStatus::UpdateAvailable => 0,
        AppStatus::Installed => 1,
        AppStatus::UpToDate => 2,
        AppStatus::Available => 3,
        AppStatus::Checking => 4,
        AppStatus::Unavailable => 5,
        AppStatus::NoRelease => 6,
    }
}
