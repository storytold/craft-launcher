//! What the UI knows about each app.

use artcraft_launcher_core::platform::{Platform, pick_asset};
use artcraft_launcher_core::protocol::{JobPhase, LatestRelease, PausedDownload};
use artcraft_launcher_core::release::Release;
use artcraft_launcher_core::status::{self, AppStatus, Latest};
use artcraft_launcher_core::{CraftApp, Installed, LocalApp};

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
        let kinds: Vec<_> = p.download_kinds().iter().copied().filter(|k| self.app.installs(*k)).collect();
        pick_asset(r, &stems, p, &kinds).is_some()
    }

    pub fn status(&self, platform: Option<Platform>, manage_apps: bool) -> AppStatus {
        if let Some(local) = self.app.local_app.filter(|local| !local.release_managed) {
            return local.executable.map_or(
                AppStatus::Unavailable,
                |exe| {
                    if is_executable(exe) { AppStatus::Installed } else { AppStatus::Unavailable }
                },
            );
        }
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

/// Whether an explicitly registered local Craft App executable is present and runnable.
/// ASUSCraft is Linux-only; never infer installation from its source metadata.
pub fn is_executable(path: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = path;
        false
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
    if e.app.local_app.is_some_and(|local| !local.release_managed) && !status.is_installed() {
        return (t("Not installed").to_string(), Tone::Faint);
    }
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

/// UI-side representation of a launch-only local application (Stage 2: data only).
#[derive(Clone, Debug)]
pub struct LocalAppEntry {
    pub app: &'static LocalApp,
}

impl LocalAppEntry {
    pub fn new(app: &'static LocalApp) -> Self {
        Self { app }
    }
}

#[cfg(test)]
mod local_entry_tests {
    use super::{AppEntry, LocalAppEntry, entry_status_text, is_executable};
    use artcraft_launcher_core::LOCAL_APPS;

    #[test]
    fn local_apps_are_empty() {
        assert_eq!(LOCAL_APPS.len(), 0);
        assert!(!LOCAL_APPS.iter().any(|app| app.id == "asus-control-center" || app.id == "asuscraft" || app.id == "chatgpt"));
    }

    #[test]
    fn release_managed_asuscraft_uses_the_real_release_lifecycle() {
        use artcraft_launcher_core::CraftApp;
        use artcraft_launcher_core::platform::{Arch, Os, Platform};
        use artcraft_launcher_core::protocol::LatestRelease;
        let entry = AppEntry::new(CraftApp::by_id("asuscraft").unwrap());
        assert_eq!(entry.app.name, "ASUSCraft");
        assert!(entry.app.local_app.unwrap().release_managed);
        let mut entry = entry;
        entry.detected = true;
        entry.latest = Some(LatestRelease::Found(
            artcraft_launcher_core::release::parse_github_release(
                include_str!("../../core/tests/fixtures/asuscraft-release.json"),
                "Mostafamomen/asus-control-center",
            )
            .unwrap(),
        ));
        let platform = Platform { os: Os::Linux, arch: Arch::X64 };
        assert!(entry.has_download(Some(platform)));
        assert_eq!(entry.status(Some(platform), true), artcraft_launcher_core::status::AppStatus::Available);
        assert_eq!(entry_status_text(&entry, &entry.status(Some(platform), true), true).0, "Available to install");
        entry.installed = Some(artcraft_launcher_core::Installed {
            version: artcraft_launcher_core::version::parse("0.3.0"),
            source: artcraft_launcher_core::InstallSource::Rpm,
            location: "/usr/bin/asus-control-center".into(),
            package: Some("asus-control-center".into()),
        });
        assert_eq!(entry.status(Some(platform), true), artcraft_launcher_core::status::AppStatus::UpdateAvailable);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn executable_detection_requires_execute_permission() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!("asuscraft-executable-check-{}", std::process::id()));
        std::fs::write(&path, b"test").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(!is_executable(path.to_str().unwrap()));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(is_executable(path.to_str().unwrap()));
        std::fs::remove_file(path).unwrap();
    }
}
