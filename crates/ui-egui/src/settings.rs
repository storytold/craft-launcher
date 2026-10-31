//! The user's choices, saved between runs by the shell.

use std::collections::BTreeMap;

use artcraft_launcher_core::PackageKind;
use serde::{Deserialize, Serialize};

use crate::i18n::t;
use crate::theme::ThemeChoice;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ViewMode {
    #[default]
    Grid,
    Table,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Filter {
    #[default]
    All,
    // These three no longer appear in the toolbar but must remain deserializable from old settings.
    Installed,
    Updates,
    NotInstalled,
    /// Apps the user chose to hide from the other lists.
    Hidden,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortKey {
    /// Suite order: classic apps, then new. Settings saved with the removed "Inspired by" sort load as this.
    #[default]
    #[serde(alias = "InspiredBy")]
    Suite,
    Name,
    Status,
    /// Installed version.
    Installed,
    /// Latest released version.
    Latest,
    /// Release date.
    Released,
    /// How often the app has been opened from here, most first.
    Frequent,
}

impl SortKey {
    /// The direction a first click sorts in: newest and highest first for dates and versions,
    /// A to Z for text.
    pub fn starts_descending(self) -> bool {
        matches!(self, Self::Installed | Self::Latest | Self::Released | Self::Frequent)
    }
}

/// How often to look for updates in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CheckEvery {
    Hour,
    FourHours,
    Day,
    Never,
}

impl CheckEvery {
    /// The choices offered. `FourHours` and `Day` are kept so saved settings still load; they now
    /// mean "automatically" too.
    pub const ALL: [Self; 2] = [Self::Hour, Self::Never];

    pub fn label(self) -> String {
        match self {
            Self::Hour | Self::FourHours | Self::Day => t("Automatically").to_string(),
            Self::Never => t("Only when I ask").to_string(),
        }
    }

    /// How often to look. Checks ask ArtCraft's release feed (one cheap request; GitHub only as a
    /// fallback), so automatic checks run every twelve minutes to reduce GitHub API traffic.
    pub fn seconds(self) -> Option<i64> {
        match self {
            Self::Hour | Self::FourHours | Self::Day => Some(12 * 60),
            Self::Never => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub install_locations: artcraft_launcher_core::protocol::InstallLocations,
    pub view: ViewMode,
    pub filter: Filter,
    pub sort: SortKey,
    pub sort_descending: bool,
    pub theme: ThemeChoice,
    /// Interface language: `auto` (follow the system) or a language code from the registry
    /// (`en`, `ru`, `zh-CN`, `de`, `fr`, `pt-BR`, `pl`).
    pub language: String,
    /// Closing the window keeps the launcher in the tray (menu bar) instead of quitting.
    pub close_to_tray: bool,
    /// macOS: showing the window brings it to the current desktop (Space) instead of switching to
    /// the desktop it was on.
    pub show_on_current_desktop: bool,
    pub check_every: CheckEvery,
    /// Automatically download and install updates when found.
    #[serde(default)]
    pub auto_update: bool,
    /// Show a system notification when an installed app has an update.
    pub notify: bool,
    pub prereleases: bool,
    /// Version track per app. Missing entries follow stable releases.
    pub versions: BTreeMap<String, artcraft_launcher_core::protocol::VersionTrack>,
    pub launcher_version: artcraft_launcher_core::protocol::VersionTrack,
    /// Linux: the package format to prefer for new installs (empty: the system's own).
    pub preferred_kinds: Vec<PackageKind>,
    /// Announcements and alerts from the release feed the user dismissed (by notice id).
    pub dismissed_notices: std::collections::BTreeSet<String>,
    /// Updates already announced, so each version is announced once: app id → version.
    pub notified: BTreeMap<String, String>,
    /// Unix seconds of the last finished check.
    pub last_check: Option<i64>,
    /// Catalog ids of apps hidden from the main lists (`photocraft`, …).
    pub hidden: std::collections::BTreeSet<String>,
    /// The one-time notice that closing the window keeps the launcher in the tray has been shown.
    pub tray_hint_shown: bool,
    /// How many times each app was opened from the launcher (catalog id → count), for "Frequently used".
    pub launches: BTreeMap<String, u32>,
}

impl Settings {
    /// Machine-readable code for "follow the system language".
    pub const LANGUAGE_AUTO: &str = "auto";

    /// Clear saved catalog filters whose controls were removed from the toolbar.
    pub fn migrate(&mut self) {
        if !matches!(self.filter, Filter::All | Filter::Hidden) {
            self.filter = Filter::All;
        }
    }

    /// Display name for a language `code`: a language's name in itself, from the registry
    /// ([`LANGUAGES`](crate::i18n::LANGUAGES)). A language needing a CJK font is named in English,
    /// with its own name added only while `installed` — the CJK face in the current font stack — is
    /// that language (no other face draws it); "Auto" for `auto` and unknown codes, like
    /// [`Lang::from_pref`](crate::i18n::Lang::from_pref).
    pub fn language_label(code: &str, installed: Option<crate::i18n::Lang>) -> String {
        match crate::i18n::Lang::from_code(code) {
            Some(lang) if lang.needs_cjk_font() && installed == Some(lang) => format!("{} ({})", lang.english_name(), lang.name()),
            Some(lang) if lang.needs_cjk_font() => lang.english_name().to_string(),
            Some(lang) => lang.name().to_string(),
            None => t("Auto").to_string(),
        }
    }
}

/// Whether this process is running inside a Flatpak container sandbox.
pub fn is_flatpak_sandbox() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("FLATPAK_ID").is_some() || std::path::Path::new("/.flatpak-info").exists()
    }
    #[cfg(not(target_os = "linux"))]
    false
}

/// Whether this process was launched from an AppImage.
pub fn is_appimage() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::env::var_os("APPIMAGE").is_some()
    }
    #[cfg(not(target_os = "linux"))]
    false
}

/// Default package kinds preferred on Linux based on how the launcher itself was installed.
pub fn default_preferred_kinds() -> Vec<PackageKind> {
    if is_flatpak_sandbox() {
        vec![PackageKind::Flatpak]
    } else if is_appimage() {
        vec![PackageKind::AppImage]
    } else {
        Vec::new()
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            install_locations: Default::default(),
            view: ViewMode::Grid,
            filter: Filter::All,
            sort: SortKey::Suite,
            sort_descending: false,
            theme: ThemeChoice::System,
            language: "auto".to_string(),
            close_to_tray: true,
            show_on_current_desktop: true,
            check_every: CheckEvery::Hour,
            auto_update: false,
            notify: true,
            prereleases: false,
            versions: BTreeMap::new(),
            launcher_version: Default::default(),
            preferred_kinds: default_preferred_kinds(),
            dismissed_notices: std::collections::BTreeSet::new(),
            notified: BTreeMap::new(),
            last_check: None,
            hidden: std::collections::BTreeSet::new(),
            tray_hint_shown: false,
            launches: BTreeMap::new(),
        }
    }
}
