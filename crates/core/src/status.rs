//! What's installed plus what's released → what to tell the user.

use serde::{Deserialize, Serialize};

use crate::version::{self, Version};

/// How an installed copy got onto the machine, which decides how to update and remove it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InstallSource {
    /// Installed by this launcher (it keeps a record and can update or remove it).
    Launcher,
    /// A macOS `.app` the user put in Applications.
    MacApp,
    /// A Windows Installer (MSI) install.
    Msi,
    /// A Debian package.
    Deb,
    /// An RPM package.
    Rpm,
    /// An Arch Linux package: the release's own (`ai.storyteller.<app>`), or `<app>`, `<app>-bin`
    /// or `<app>-git` from the AUR or a repository.
    Pacman,
    /// A Flatpak.
    Flatpak,
    /// An AppImage file found on disk.
    AppImage,
    /// A binary found on `PATH` or in a system prefix.
    Binary,
}

impl InstallSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Launcher => "ArtCraft Launcher",
            Self::MacApp => "Applications folder",
            Self::Msi => "Windows Installer",
            Self::Deb => "Debian package",
            Self::Rpm => "RPM package",
            Self::Pacman => "Arch package",
            Self::Flatpak => "Flatpak",
            Self::AppImage => "AppImage",
            Self::Binary => "System binary",
        }
    }

    /// Whether the launcher may replace or remove this copy itself. Packages owned by a system
    /// package manager (deb, rpm, Flatpak) are updated through that manager instead.
    pub fn launcher_can_update(self) -> bool {
        matches!(self, Self::Launcher | Self::MacApp | Self::Msi | Self::AppImage)
    }
}

/// An installed copy of an app.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installed {
    /// `None` when the copy is there but its version couldn't be read.
    pub version: Option<Version>,
    pub source: InstallSource,
    /// Where it lives: the `.app`, the exe, the AppImage, or the binary of a package.
    pub location: String,
    /// The package that installed it, for package-manager installs (`photocraft-bin`).
    #[serde(default)]
    pub package: Option<String>,
}

/// The status shown on an app's card or row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AppStatus {
    /// Still waiting for the first answer.
    Checking,
    /// Released and not installed.
    Available,
    /// Installed, and the installed version is the latest.
    UpToDate,
    /// Installed, and a newer version is out.
    UpdateAvailable,
    /// Installed, but the latest release couldn't be checked (offline, rate-limited).
    Installed,
    /// Not installed and nothing is published for this platform.
    Unavailable,
    /// The app has no releases yet.
    NoRelease,
}

impl AppStatus {
    pub fn is_installed(&self) -> bool {
        matches!(self, Self::UpToDate | Self::UpdateAvailable | Self::Installed)
    }
}

/// What the latest-release lookup produced for an app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Latest<'a> {
    Pending,
    Failed,
    NoRelease,
    Found { version: Option<&'a Version>, has_asset: bool },
}

pub fn status(installed: Option<&Installed>, latest: Latest<'_>) -> AppStatus {
    match (installed, latest) {
        (None, Latest::Pending) => AppStatus::Checking,
        (None, Latest::NoRelease) => AppStatus::NoRelease,
        (None, Latest::Failed) => AppStatus::Unavailable,
        (None, Latest::Found { has_asset, .. }) => {
            if has_asset {
                AppStatus::Available
            } else {
                AppStatus::Unavailable
            }
        }
        (Some(_), Latest::Pending | Latest::Failed | Latest::NoRelease) => AppStatus::Installed,
        (Some(inst), Latest::Found { version: latest, .. }) => match (latest, inst.version.as_ref()) {
            (Some(l), Some(i)) if version::is_update(l, i) => AppStatus::UpdateAvailable,
            (Some(_), Some(_)) => AppStatus::UpToDate,
            // Version unknown on one side: we can't promise "up to date".
            _ => AppStatus::Installed,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(v: &str) -> Installed {
        Installed { version: version::parse(v), source: InstallSource::Launcher, location: String::new(), package: None }
    }

    #[test]
    fn status_covers_every_case() {
        let v5 = Version::new(0, 5, 0);
        let found = |has_asset| Latest::Found { version: Some(&v5), has_asset };
        assert_eq!(status(None, Latest::Pending), AppStatus::Checking);
        assert_eq!(status(None, found(true)), AppStatus::Available);
        assert_eq!(status(None, found(false)), AppStatus::Unavailable);
        assert_eq!(status(None, Latest::NoRelease), AppStatus::NoRelease);
        assert_eq!(status(Some(&inst("0.4.0")), found(true)), AppStatus::UpdateAvailable);
        assert_eq!(status(Some(&inst("0.5.0")), found(true)), AppStatus::UpToDate);
        // Newer than the latest release (a local build): not an update, still current.
        assert_eq!(status(Some(&inst("0.6.0")), found(true)), AppStatus::UpToDate);
        assert_eq!(status(Some(&inst("0.4.0")), Latest::Failed), AppStatus::Installed);
        assert_eq!(status(Some(&inst("garbage")), found(true)), AppStatus::Installed);
        assert!(AppStatus::UpdateAvailable.is_installed() && !AppStatus::Available.is_installed());
    }
}
