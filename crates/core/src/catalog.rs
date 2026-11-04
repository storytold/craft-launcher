//! The Craft App Suite: eighteen apps, including community apps such as ASUSCraft.
//!
//! The list is data so a later version can fetch it from an API; [`SUITE`] is the built-in copy.
//!
//! Text here is English, the source the UI translates from: each `tagline` has a catalog row
//! keyed by the app id (`tr_ctx(app.id, app.tagline)` in `ui-egui`).

/// Who makes an app. Official apps live under [`GITHUB_OWNER`]; a community app is maintained
/// outside ArtCraft and is labelled as such wherever it is shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppKind {
    Official,
    Community,
}

/// One app in the suite.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CraftApp {
    /// Machine name: repo, binary, package and asset stem (`photocraft`).
    pub id: &'static str,
    /// Display name (`PhotoCraft`).
    pub name: &'static str,
    /// What it's for, in a few words (English; the UI translates it by app id).
    pub tagline: &'static str,
    /// Brand colour (sRGB), for surfaces: getartcraft.com's `--app`.
    pub color: [u8; 3],
    /// The brand colour for text on a light background (the site's light `--app-ink`).
    pub ink_light: [u8; 3],
    /// The brand colour for text on a dark background (the site's dark `--app-ink`).
    pub ink_dark: [u8; 3],
    /// GitHub `owner/repo`.
    pub repo: &'static str,
    /// Older names its release assets may carry (PdfCraft shipped as PrintCraft until 0.2.1).
    pub former_ids: &'static [&'static str],
    /// Older display names its installers registered (PdfCraft's MSI said PrintCraft until 0.2.1).
    pub former_names: &'static [&'static str],
    pub kind: AppKind,
    /// Who publishes it, as the card's byline shows it (`ArtCraft`, `Tangency Labs`).
    pub publisher: &'static str,
    /// The publisher its Windows installer names, when it isn't ArtCraft's (`Learning Machines LLC`).
    pub msi_publisher: Option<&'static str>,
    /// The bundle id when it isn't `ai.storyteller.<id>` (a community app brings its own).
    pub bundle: Option<&'static str>,
    /// Older bundle ids its Flatpak shipped under (SolidCraft was `ai.storyteller.solidcraft`).
    pub former_bundles: &'static [&'static str],
    /// Its website, when it isn't a page on getartcraft.com (a community app has its own).
    pub site: Option<&'static str>,
    /// The Apple Developer team whose Developer ID signature its macOS build must carry. `None`
    /// while those builds carry no signature the launcher can check: it then opens a copy that's
    /// there and links to the releases, but installs nothing.
    pub mac_team: Option<&'static str>,
    /// Optional desktop integration metadata for apps with a known installed executable.
    pub local_app: Option<LocalApp>,
}

impl CraftApp {
    /// `ai.storyteller.<id>`: macOS bundle id, Flatpak id and Linux desktop-file id.
    pub fn bundle_id(&self) -> String {
        self.bundle.map_or_else(|| format!("ai.storyteller.{}", self.id), str::to_string)
    }

    pub fn releases_url(&self) -> String {
        format!("https://github.com/{}/releases", self.repo)
    }

    pub fn repo_url(&self) -> String {
        format!("https://github.com/{}", self.repo)
    }

    /// The app's website: its page on getartcraft.com, or a community app's own site.
    pub fn website(&self) -> String {
        self.site.map_or_else(|| format!("https://getartcraft.com/apps/{}", self.id), str::to_string)
    }

    /// A website link that is actually published for this app. Official Craft Apps use their
    /// catalog page; community apps only expose a website when one is recorded explicitly.
    pub fn website_url(&self) -> Option<String> {
        self.site.map(str::to_string).or_else(|| (self.kind == AppKind::Official).then(|| format!("https://getartcraft.com/apps/{}", self.id)))
    }

    /// A release page where one is part of the app's metadata. Release-managed suite apps retain
    /// the standard GitHub releases route; locally integrated apps opt in explicitly.
    pub fn releases_page_url(&self) -> Option<String> {
        match self.local_app {
            Some(local) => local.releases_url.map(str::to_string),
            None => Some(self.releases_url()),
        }
    }

    /// Whether the launcher installs this app from a `kind` package.
    pub fn installs(&self, kind: crate::PackageKind) -> bool {
        kind != crate::PackageKind::Dmg || self.mac_team.is_some()
    }

    /// Bundle ids this app may be installed under, current first.
    pub fn bundle_ids(&self) -> impl Iterator<Item = String> + '_ {
        std::iter::once(self.bundle_id()).chain(self.former_bundles.iter().map(|b| b.to_string()))
    }

    /// Asset stems this app's releases may use, current first.
    pub fn stems(&self) -> impl Iterator<Item = &'static str> + '_ {
        std::iter::once(self.id).chain(self.former_ids.iter().copied())
    }

    /// Display names this app's installers may have registered, current first.
    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        std::iter::once(self.name).chain(self.former_names.iter().copied())
    }

    pub fn by_id(id: &str) -> Option<&'static CraftApp> {
        SUITE.iter().find(|a| a.id == id)
    }
}

const fn app(
    id: &'static str,
    name: &'static str,
    tagline: &'static str,
    [color, ink_light, ink_dark]: [[u8; 3]; 3],
    repo: &'static str,
) -> CraftApp {
    CraftApp {
        id,
        name,
        tagline,
        color,
        ink_light,
        ink_dark,
        repo,
        former_ids: &[],
        former_names: &[],
        kind: AppKind::Official,
        publisher: "ArtCraft",
        msi_publisher: None,
        bundle: None,
        former_bundles: &[],
        site: None,
        mac_team: Some(ARTCRAFT_TEAM_ID),
        local_app: None,
    }
}

/// The Craft App Suite, in display order matching getartcraft.com, then the community apps:
pub const SUITE: [CraftApp; 18] = [
    app("photocraft", "PhotoCraft", "Image editing", [[0x2f, 0x7b, 0xf5], [0x1a, 0x5b, 0xc4], [0x7f, 0xb0, 0xff]], "storytold/photocraft"),
    app("vectorcraft", "VectorCraft", "Vector illustration", [[0xe8, 0x57, 0x3f], [0xb8, 0x3a, 0x24], [0xff, 0x8a, 0x70]], "storytold/vectorcraft"),
    app(
        "filmcraft",
        "FilmCraft",
        "Video editing, color and sound",
        [[0x8b, 0x5c, 0xf6], [0x6a, 0x3f, 0xd6], [0xb6, 0x9c, 0xff]],
        "storytold/filmcraft",
    ),
    app(
        "lightcraft",
        "LightCraft",
        "Photo library and raw development",
        [[0xf2, 0xa5, 0x16], [0x8a, 0x58, 0x00], [0xff, 0xc1, 0x4d]],
        "storytold/lightcraft",
    ),
    CraftApp {
        former_ids: &["printcraft"],
        former_names: &["PrintCraft"],
        ..app(
            "pdfcraft",
            "PdfCraft",
            "Reading, organizing and protecting PDFs",
            [[0xe8, 0x20, 0x4f], [0xb7, 0x02, 0x28], [0xf2, 0x3a, 0x62]],
            "storytold/pdfcraft",
        )
    },
    app(
        "effectcraft",
        "EffectCraft",
        "Motion graphics and visual effects",
        [[0xe0, 0x36, 0x8f], [0xb0, 0x20, 0x6c], [0xff, 0x7a, 0xb8]],
        "storytold/effectcraft",
    ),
    app(
        "designcraft",
        "DesignCraft",
        "Page layout and publishing",
        [[0x7b, 0xb5, 0x1c], [0x45, 0x70, 0x08], [0xa6, 0xe0, 0x4a]],
        "storytold/designcraft",
    ),
    app(
        "figcraft",
        "FigCraft",
        "Interface design and prototyping",
        [[0x02, 0xda, 0x96], [0x04, 0x7a, 0x56], [0x4f, 0xe8, 0xb6]],
        "storytold/figcraft",
    ),
    app("wordcraft", "WordCraft", "Writing and document design", [[0x3f, 0x5f, 0xe0], [0x2a, 0x44, 0xb8], [0x8e, 0xa2, 0xff]], "storytold/wordcraft"),
    app(
        "soundcraft",
        "SoundCraft",
        "Recording, editing and mixing audio",
        [[0x1f, 0xa5, 0xc9], [0x0e, 0x72, 0x90], [0x5f, 0xd0, 0xee]],
        "storytold/soundcraft",
    ),
    app(
        "deckcraft",
        "DeckCraft",
        "Presentations and slide shows",
        [[0xf0, 0x7a, 0x1e], [0xa8, 0x4f, 0x0a], [0xff, 0xa0, 0x60]],
        "storytold/deckcraft",
    ),
    app(
        "cadcraft",
        "CADCraft",
        "Computer-aided design and drafting",
        [[0x3e, 0xa6, 0xe8], [0x1a, 0x6f, 0xa8], [0x7c, 0xc8, 0xff]],
        "storytold/cadcraft",
    ),
    app("gridcraft", "GridCraft", "Spreadsheets and data", [[0x1f, 0x9d, 0x55], [0x12, 0x70, 0x3a], [0x4f, 0xd6, 0x8a]], "storytold/gridcraft"),
    app(
        "texturecraft",
        "TextureCraft",
        "3D texture painting",
        [[0xc2, 0x67, 0x2b], [0x9c, 0x4a, 0x15], [0xe5, 0x8a, 0x4e]],
        "storytold/texturecraft",
    ),
    CraftApp {
        // Use the project repository until its getartcraft.com page is published.
        site: Some("https://github.com/storytold/materialcraft"),
        ..app(
            "materialcraft",
            "MaterialCraft",
            "Procedural material design",
            [[0x17, 0x8a, 0x9c], [0x10, 0x65, 0x73], [0x63, 0xc5, 0xd3]],
            "storytold/materialcraft",
        )
    },
    // The raven's tile blue, with its README's deep (#1659c4) and light (#74aaff) accents for text.
    app(
        "craft-anything",
        "Craft Anything",
        "Find out how any software works, then craft your own",
        [[0x4e, 0x7b, 0xfb], [0x16, 0x59, 0xc4], [0x74, 0xaa, 0xff]],
        "storytold/craft-anything",
    ),
    CraftApp {
        kind: AppKind::Community,
        publisher: "Tangency Labs",
        msi_publisher: Some("SolidCraft contributors"),
        bundle: Some("io.tangencylabs.solidcraft"),
        former_bundles: &["ai.storyteller.solidcraft", "ai.storyteller.solvecraft"],
        site: Some("https://solidcraft.io"),
        // Signed by its own developer's team, not ArtCraft's (since 0.27, per its README).
        mac_team: Some("G5QA5U5X2S"),
        ..app(
            "solidcraft",
            "SolidCraft",
            "Parametric 3D CAD",
            [[0x46, 0xa1, 0xfb], [0x16, 0x53, 0xae], [0x8c, 0xc4, 0xff]],
            "tangency-labs/solidcraft",
        )
    },
    CraftApp {
        kind: AppKind::Community,
        publisher: "Mostafa Momen",
        site: None,
        mac_team: None,
        local_app: Some(LocalApp {
            id: "asuscraft",
            name: "ASUSCraft",
            tagline: "Complete ASUS Laptop Control Center",
            desktop_entry: "io.github.asuscontrolcenter.AsusControlCenter.desktop",
            icon: "io.github.asuscontrolcenter.AsusControlCenter",
            executable: Some("/usr/bin/asus-control-center"),
            version: None,
            release_date: None,
            releases_url: Some("https://github.com/Mostafamomen/asus-control-center/releases"),
            release_managed: true,
        }),
        former_ids: &["asus-control-center"],
        ..app(
            "asuscraft",
            "ASUSCraft",
            "Complete ASUS Laptop Control Center",
            [[0x20, 0xc8, 0xd7], [0x0c, 0x8d, 0xa2], [0x72, 0xe6, 0xeb]],
            "Mostafamomen/asus-control-center",
        )
    },
];

/// Catalog shown in the Craft Apps view.
pub fn craft_apps() -> impl Iterator<Item = &'static CraftApp> {
    SUITE.iter()
}

/// The Apple Developer team that signs every official Craft App and ArtCraft Launcher (Learning
/// Machines LLC): the macOS side of the Windows publisher check.
pub const ARTCRAFT_TEAM_ID: &str = "DJ6XS33FX8";

/// ArtCraft's website.
pub const WEBSITE_URL: &str = "https://getartcraft.com";
/// The GitHub account every official Craft App's repository lives under. The release feed can
/// only move an app within it; a community app installs from its own repo, pinned in [`SUITE`].
pub const GITHUB_OWNER: &str = "storytold";
/// ArtCraft on GitHub: every Craft App's source.
pub const GITHUB_URL: &str = "https://github.com/storytold";
/// ArtCraft's social accounts: (service, URL). Shown as small icons, less prominent than Discord.
pub const SOCIAL: [(&str, &str); 5] = [
    ("X", "https://x.com/get_artcraft"),
    ("Instagram", "https://www.instagram.com/officialartcraftstudios"),
    ("LinkedIn", "https://www.linkedin.com/company/artcraft-ai"),
    ("YouTube", "https://www.youtube.com/@OfficialArtcraftStudios"),
    ("TikTok", "https://www.tiktok.com/@artcraft.studios"),
];

/// The ArtCraft community on Discord.
pub const DISCORD_URL: &str = "https://discord.gg/artcraft";
/// Deep link into the Discord application for the ArtCraft server invite.
pub const DISCORD_APP_URL: &str = "discord://-/invite/artcraft";

/// ArtCraft Launcher itself, described like a Craft App so it can update itself with the same
/// machinery (asset names `artcraft-launcher-<ver>-…`, bundle id `ai.storyteller.artcraft-launcher`,
/// `ArtCraft Launcher.app`). Not part of [`SUITE`].
pub const LAUNCHER: CraftApp = app(
    "artcraft-launcher",
    "ArtCraft Launcher",
    "Installs and updates the Craft App Suite",
    [[0x0b, 0x0b, 0x0c], [0x0b, 0x0b, 0x0c], [0xf2, 0xf1, 0xee]],
    "storytold/craft-launcher",
);

/// The launcher's own repo, checked so it can say when a newer launcher is out.
pub const LAUNCHER_REPO: &str = "storytold/craft-launcher";
/// The launcher's own asset stem.
pub const LAUNCHER_ID: &str = "artcraft-launcher";

/// A Craft App's 128 px icon (PNG), as copied from its repo into `assets/app-icons`: the UI
/// draws it, and the Linux install puts it in the desktop menu.
pub fn app_icon_png(id: &str) -> Option<&'static [u8]> {
    Some(match id {
        "photocraft" => include_bytes!("../../../assets/app-icons/photocraft.png"),
        "vectorcraft" => include_bytes!("../../../assets/app-icons/vectorcraft.png"),
        "filmcraft" => include_bytes!("../../../assets/app-icons/filmcraft.png"),
        "lightcraft" => include_bytes!("../../../assets/app-icons/lightcraft.png"),
        "pdfcraft" => include_bytes!("../../../assets/app-icons/pdfcraft.png"),
        "effectcraft" => include_bytes!("../../../assets/app-icons/effectcraft.png"),
        "designcraft" => include_bytes!("../../../assets/app-icons/designcraft.png"),
        "wordcraft" => include_bytes!("../../../assets/app-icons/wordcraft.png"),
        "deckcraft" => include_bytes!("../../../assets/app-icons/deckcraft.png"),
        "gridcraft" => include_bytes!("../../../assets/app-icons/gridcraft.png"),
        "texturecraft" => include_bytes!("../../../assets/app-icons/texturecraft.png"),
        "materialcraft" => include_bytes!("../../../assets/app-icons/materialcraft.png"),
        "craft-anything" => include_bytes!("../../../assets/app-icons/craft-anything.png"),
        "solidcraft" => include_bytes!("../../../assets/app-icons/solidcraft.png"),
        "cadcraft" => include_bytes!("../../../assets/app-icons/cadcraft.png"),
        "soundcraft" => include_bytes!("../../../assets/app-icons/soundcraft.png"),
        "figcraft" => include_bytes!("../../../assets/app-icons/figcraft.png"),
        "asuscraft" => include_bytes!("../../../assets/app-icons/asuscraft.png"),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_app_has_its_icon() {
        for app in &SUITE {
            let png = app_icon_png(app.id).unwrap_or_else(|| panic!("no icon for {}", app.id));
            assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "{} icon isn't a PNG", app.id);
        }
        assert!(app_icon_png("notacraft").is_none());
    }

    /// Every catalog entry must have a distinct lookup key.
    #[test]
    fn suite_has_eighteen_apps_with_unique_ids() {
        assert_eq!(SUITE.len(), 18);
        let mut ids: Vec<_> = SUITE.iter().map(|a| a.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), SUITE.len());
        for a in &SUITE {
            assert_eq!(a.name.to_lowercase().replace(' ', "-"), a.id);
            match a.kind {
                AppKind::Official => assert_eq!(a.repo, format!("storytold/{}", a.id)),
                AppKind::Community => assert!(!a.repo.starts_with("storytold/"), "{} is community", a.id),
            }
        }
    }

    #[test]
    fn asuscraft_is_a_single_builtin_craftapp_with_its_icon_and_executable() {
        let app = CraftApp::by_id("asuscraft").unwrap();
        let local = app.local_app.unwrap();
        assert_eq!(app.kind, AppKind::Community);
        assert_eq!(app.name, "ASUSCraft");
        assert_eq!(app.tagline, "Complete ASUS Laptop Control Center");
        assert_eq!(local.executable, Some("/usr/bin/asus-control-center"));
        assert_eq!(local.desktop_entry, "io.github.asuscontrolcenter.AsusControlCenter.desktop");
        assert!(local.release_managed);
        assert_eq!(app.stems().collect::<Vec<_>>(), ["asuscraft", "asus-control-center"]);
        assert!(app_icon_png(app.id).unwrap().starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(craft_apps().filter(|a| a.id == "asuscraft").count(), 1);
    }

    #[test]
    fn all_builtin_craft_apps_are_visible_once() {
        let visible: Vec<_> = craft_apps().map(|app| app.id).collect();
        assert_eq!(visible.len(), SUITE.len());
        assert_eq!(visible.iter().filter(|&&id| id == "asuscraft").count(), 1);
    }

    #[test]
    fn solidcraft_is_a_community_app_with_its_own_bundle_id() {
        let sc = CraftApp::by_id("solidcraft").unwrap();
        assert_eq!(sc.kind, AppKind::Community);
        assert_eq!(sc.repo, "tangency-labs/solidcraft");
        assert_eq!(sc.bundle_id(), "io.tangencylabs.solidcraft");
        assert_eq!(sc.bundle_ids().collect::<Vec<_>>(), ["io.tangencylabs.solidcraft", "ai.storyteller.solidcraft", "ai.storyteller.solvecraft"]);
        assert_eq!(CraftApp::by_id("gridcraft").unwrap().bundle_id(), "ai.storyteller.gridcraft");
        assert_eq!(CraftApp::by_id("gridcraft").unwrap().kind, AppKind::Official);
        assert_eq!(sc.website(), "https://solidcraft.io");
        assert!(sc.installs(crate::PackageKind::Dmg) && sc.installs(crate::PackageKind::Msi) && sc.installs(crate::PackageKind::Flatpak));
        assert_eq!(sc.mac_team, Some("G5QA5U5X2S"));
        assert_eq!(CraftApp::by_id("gridcraft").unwrap().mac_team, Some(ARTCRAFT_TEAM_ID));
    }

    #[test]
    fn texturecraft_is_an_official_app() {
        let app = CraftApp::by_id("texturecraft").unwrap();
        assert_eq!(app.kind, AppKind::Official);
        assert_eq!(app.repo, "storytold/texturecraft");
        assert_eq!(app.bundle_id(), "ai.storyteller.texturecraft");
        assert_eq!(app.website(), "https://getartcraft.com/apps/texturecraft");
        assert_eq!(app.mac_team, Some(ARTCRAFT_TEAM_ID));
    }

    #[test]
    fn materialcraft_is_an_official_app() {
        let app = CraftApp::by_id("materialcraft").unwrap();
        assert_eq!(app.kind, AppKind::Official);
        assert_eq!(app.repo, "storytold/materialcraft");
        assert_eq!(app.bundle_id(), "ai.storyteller.materialcraft");
        assert_eq!(app.website(), "https://github.com/storytold/materialcraft");
        assert_eq!(app.mac_team, Some(ARTCRAFT_TEAM_ID));
    }

    #[test]
    fn craft_anything_is_an_official_app() {
        let ca = CraftApp::by_id("craft-anything").unwrap();
        assert_eq!(ca.kind, AppKind::Official);
        assert_eq!(ca.repo, "storytold/craft-anything");
        assert_eq!(ca.bundle_id(), "ai.storyteller.craft-anything");
        assert_eq!(ca.website(), "https://getartcraft.com/apps/craft-anything");
        assert_eq!(ca.mac_team, Some(ARTCRAFT_TEAM_ID));
    }

    #[test]
    fn pdfcraft_also_answers_to_printcraft() {
        let pdf = CraftApp::by_id("pdfcraft").unwrap();
        assert_eq!(pdf.stems().collect::<Vec<_>>(), ["pdfcraft", "printcraft"]);
        assert_eq!(pdf.names().collect::<Vec<_>>(), ["PdfCraft", "PrintCraft"]);
        assert_eq!(pdf.website(), "https://getartcraft.com/apps/pdfcraft");
        assert_eq!(CraftApp::by_id("wordcraft").unwrap().website(), "https://getartcraft.com/apps/wordcraft");
    }
}

/// Desktop metadata used by a Craft App that has a known Linux executable and desktop entry. An
/// app may also opt into the normal GitHub release/install lifecycle with [`Self::release_managed`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalApp {
    /// Stable machine id (used as a key in settings, search, etc.).
    pub id: &'static str,
    /// Display name shown on the tile.
    pub name: &'static str,
    /// Short description shown under the name.
    pub tagline: &'static str,
    /// Freedesktop `.desktop` file name (resolved through XDG application dirs at runtime).
    pub desktop_entry: &'static str,
    /// Icon name as declared by the `.desktop` file's `Icon=` key (resolved via the icon theme).
    pub icon: &'static str,
    /// Optional explicit executable for CraftApps integrations that must launch a known install.
    pub executable: Option<&'static str>,
    /// Version declared by the locally installed project's own metadata, when available.
    pub version: Option<&'static str>,
    /// Unix timestamp of the project's release date, when available.
    pub release_date: Option<i64>,
    /// The project's real release page.
    pub releases_url: Option<&'static str>,
    /// This Craft App has a normal GitHub release/install lifecycle as well as a known executable.
    pub release_managed: bool,
}

/// Launch-only local applications shown in the separate Local Apps section.
/// ASUSCraft is deliberately not listed here: it is a release-managed Craft App in [`SUITE`],
/// so a second shortcut would duplicate it. ChatGPT is intentionally not shown in the launcher.
pub const LOCAL_APPS: [LocalApp; 0] = [];

#[cfg(test)]
mod local_app_tests {
    use super::LOCAL_APPS;

    #[test]
    fn local_apps_are_empty() {
        assert_eq!(LOCAL_APPS.len(), 0);
    }

    #[test]
    fn asuscraft_is_not_duplicated_in_local_apps() {
        assert!(!LOCAL_APPS.iter().any(|app| app.id == "asus-control-center" || app.id == "asuscraft"));
        assert!(!LOCAL_APPS.iter().any(|app| app.desktop_entry.to_ascii_lowercase().contains("asus")));
    }
}
