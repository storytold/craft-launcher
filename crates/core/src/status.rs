//! What's installed plus what's released → what to tell the user.

use crate::version::{self, Version};

// How a copy got onto the machine and where it is: shared with craft-update's installers.
pub use craft_update::status::{InstallSource, Installed};

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
