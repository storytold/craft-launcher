//! Where the launcher keeps its own files: craft-update's [`Paths`] for the launcher, plus the
//! files only the launcher keeps ([`LauncherPaths`]).

use std::path::PathBuf;

pub use craft_update::paths::{Paths, write_atomic};

/// The launcher's folders: `ArtCraft Launcher` (macOS, Windows) or `artcraft-launcher` in the
/// per-user data and cache folders, or `ArtCraftLauncherData` beside the executable when a
/// `portable.txt` is there (the Windows portable zip ships one).
pub fn for_launcher() -> Result<Paths, String> {
    Paths::for_app(&craft_update::suite::LAUNCHER)
}

/// The files only the launcher keeps.
pub trait LauncherPaths {
    fn release_cache(&self) -> PathBuf;
    /// The last answer from ArtCraft's release feed, for an instant first paint and as a fallback.
    fn feed_cache(&self) -> PathBuf;
    /// How long to hold off calling ArtCraft's API (after failures or HTTP 429), across restarts.
    fn feed_throttle(&self) -> PathBuf;
    /// A picture of an app's details page, saved after its first download: under the revision of
    /// the pictures repository it was fetched from, so a new revision never reads an old picture.
    fn screenshot(&self, app: &str, key: &str) -> PathBuf;
    /// The record of what the launcher installed.
    fn manifest(&self) -> PathBuf;
}

impl LauncherPaths for Paths {
    fn release_cache(&self) -> PathBuf {
        self.cache.join("releases.json")
    }

    fn feed_cache(&self) -> PathBuf {
        self.cache.join("feed.json")
    }

    fn feed_throttle(&self) -> PathBuf {
        self.cache.join("feed-throttle.json")
    }

    fn screenshot(&self, app: &str, key: &str) -> PathBuf {
        self.cache.join("screenshots").join(artcraft_launcher_core::details::SCREENSHOTS_REV).join(app).join(format!("{key}.jpg"))
    }

    fn manifest(&self) -> PathBuf {
        self.data.join("installed.json")
    }
}

/// Portable mode's folder, when a `portable.txt` beside the executable asks for it: where
/// [`for_launcher`] puts everything, for the settings file that goes beside it.
pub fn portable_root() -> Option<PathBuf> {
    Paths::portable_root(&craft_update::suite::LAUNCHER)
}
