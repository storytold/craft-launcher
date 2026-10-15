//! What the UI knows about each app.

use artcraft_launcher_core::platform::{PackageKind, Platform, pick_asset};
use artcraft_launcher_core::protocol::{JobPhase, LatestRelease, PausedDownload};
use artcraft_launcher_core::release::Release;
use artcraft_launcher_core::status::{self, AppStatus, Latest};
use artcraft_launcher_core::{CraftApp, Installed};

use crate::i18n::t;

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
        self.installed.as_ref().map(|i| i.version.as_ref().map(ToString::to_string).unwrap_or_else(|| t("Installed").to_string()))
    }

    /// How the installed copy arrived, for the badge: the release format when known,
    /// else where it was found (`None` when nothing is installed).
    pub fn installed_kind_label(&self) -> Option<&'static str> {
        let installed = self.installed.as_ref()?;
        Some(installed.kind.map(|k| k.label()).unwrap_or(installed.source.label()))
    }

    /// What `Install` would download now: the preferred formats the backend can install,
    /// then what it can install, first published asset wins. Empty `installable` (an old
    /// backend) falls back to the platform's download list. A label only; the backend
    /// decides the real one at install time.
    pub fn planned_kind_label(&self, platform: Option<Platform>, preferred: &[PackageKind], installable: &[PackageKind]) -> Option<&'static str> {
        let (release, platform) = (self.release()?, platform?);
        let stems: Vec<&str> = self.app.stems().collect();
        let base: &[PackageKind] = if installable.is_empty() { platform.download_kinds() } else { installable };
        let mut kinds: Vec<PackageKind> = preferred.iter().copied().filter(|k| base.contains(k)).collect();
        let rest: Vec<PackageKind> = base.iter().copied().filter(|k| !kinds.contains(k)).collect();
        kinds.extend(rest);
        kinds.into_iter().find_map(|k| pick_asset(release, &stems, platform, &[k]).map(|(_, kind)| kind.label()))
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

pub fn status_text(status: &AppStatus, manage_apps: bool) -> (String, Tone) {
    match status {
        AppStatus::Checking => (t("Checking…").to_string(), Tone::Faint),
        AppStatus::Available if manage_apps => (t("Available to install").to_string(), Tone::Info),
        AppStatus::Available => (t("Available").to_string(), Tone::Info),
        AppStatus::UpToDate => (t("Installed · up to date").to_string(), Tone::Good),
        AppStatus::UpdateAvailable => (t("Update available").to_string(), Tone::Warn),
        AppStatus::Installed => (t("Installed").to_string(), Tone::Neutral),
        AppStatus::Unavailable => (t("Not available here").to_string(), Tone::Faint),
        AppStatus::NoRelease => (t("Coming soon").to_string(), Tone::Faint),
    }
}

/// [`status_text`], but saying so when a download is paused or reconnecting.
pub fn entry_status_text(e: &AppEntry, status: &AppStatus, manage_apps: bool) -> (String, Tone) {
    match (e.job, e.paused) {
        (Some(JobPhase::Reconnecting { .. }), _) => (t("Reconnecting…").to_string(), Tone::Warn),
        (Some(JobPhase::Downloading { .. }), _) if status.is_installed() => (t("Updating…").to_string(), Tone::Info),
        (Some(JobPhase::Downloading { .. }), _) => (t("Downloading…").to_string(), Tone::Info),
        (None, Some(_)) if status.is_installed() => (t("Update paused").to_string(), Tone::Warn),
        (None, Some(_)) => (t("Download paused").to_string(), Tone::Warn),
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
