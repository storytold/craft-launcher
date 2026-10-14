//! The user's choices, saved between runs by the shell.

use std::collections::BTreeMap;

use artcraft_launcher_core::{CraftApp, IconStyle, PackageKind, SUITE};
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
}

impl SortKey {
    /// The direction a first click sorts in: newest and highest first for dates and versions,
    /// A to Z for text.
    pub fn starts_descending(self) -> bool {
        matches!(self, Self::Installed | Self::Latest | Self::Released)
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
    /// fallback), so automatic checks run every five minutes.
    pub fn seconds(self) -> Option<i64> {
        match self {
            Self::Hour | Self::FourHours | Self::Day => Some(5 * 60),
            Self::Never => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
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
    /// The icon style given to every app that has no entry of its own in `app_icons`.
    #[serde(default)]
    pub icon_style: IconStyle,
    /// The apps the user gave an icon style of their own, by app id.
    #[serde(default)]
    pub app_icons: BTreeMap<String, IconStyle>,
}

impl Settings {
    /// Machine-readable code for "follow the system language".
    pub const LANGUAGE_AUTO: &'static str = "auto";

    /// The icon style to draw and install for `app`: the app's own entry if it has one, else the
    /// suite-wide choice — falling back to the app's own icon for a style no pack was drawn for
    /// that app in.
    pub fn icon_for(&self, app: &CraftApp) -> IconStyle {
        let style = self.app_icons.get(app.id).copied().unwrap_or(self.icon_style);
        if app.supports_icon_style(style) { style } else { IconStyle::Default }
    }

    /// Every app's icon style, as the backend wants them (`BackendOptions::icon_styles`).
    pub fn icon_styles(&self) -> BTreeMap<String, IconStyle> {
        SUITE.iter().map(|app| (app.id.to_string(), self.icon_for(app))).collect()
    }

    /// Whether any app is drawn differently from the suite-wide choice.
    pub fn has_icon_overrides(&self) -> bool {
        SUITE.iter().any(|app| self.app_icons.get(app.id).copied().is_some_and(|s| s != self.icon_style))
    }

    /// Set `app`'s icon style: the suite-wide one (dropping any entry of its own) or a choice.
    pub fn set_app_icon(&mut self, app: &CraftApp, style: IconStyle) {
        if style == self.icon_style {
            self.app_icons.remove(app.id);
        } else {
            self.app_icons.insert(app.id.to_string(), style);
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
            preferred_kinds: default_preferred_kinds(),
            dismissed_notices: std::collections::BTreeSet::new(),
            notified: BTreeMap::new(),
            last_check: None,
            hidden: std::collections::BTreeSet::new(),
            icon_style: IconStyle::Default,
            app_icons: BTreeMap::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use artcraft_launcher_core::{CraftApp, IconStyle, SUITE};

    use super::Settings;

    /// The first pack that was drawn for `app`, so the test never names one that may be renamed.
    fn a_pack_for(app: &str) -> IconStyle {
        IconStyle::Pack(artcraft_launcher_core::icons::packs_for(app).next().unwrap().id)
    }

    #[test]
    fn the_suite_wide_choice_applies_wherever_an_app_has_no_choice_of_its_own() {
        let photo = CraftApp::by_id("photocraft").unwrap();
        let word = CraftApp::by_id("wordcraft").unwrap();
        let (dark, light) = (a_pack_for(photo.id), IconStyle::Pack(artcraft_launcher_core::icons::PACKS[1].id));
        let mut settings = Settings::default();
        assert_eq!(settings.icon_for(photo), IconStyle::Default);
        settings.icon_style = dark;
        // Every app the pack was drawn for follows the suite-wide choice…
        assert_eq!(settings.icon_for(photo), dark);
        // …and an app without artwork keeps its own icon, rather than a style it has none for.
        assert_eq!(settings.icon_for(word), IconStyle::Default);
        // An app set apart from the rest keeps its own choice.
        settings.app_icons.insert("photocraft".into(), light);
        assert_eq!(settings.icon_for(photo), light);
        assert!(settings.has_icon_overrides());
        // Going back to the suite-wide choice drops the entry rather than storing it.
        settings.set_app_icon(photo, dark);
        assert!(settings.app_icons.is_empty());
        assert!(!settings.has_icon_overrides());
        // What the backend is told is what the grid draws.
        let styles = settings.icon_styles();
        assert_eq!(styles.len(), SUITE.len());
        assert_eq!(styles.get("photocraft"), Some(&dark));
        assert_eq!(styles.get("wordcraft"), Some(&IconStyle::Default));
    }
}
