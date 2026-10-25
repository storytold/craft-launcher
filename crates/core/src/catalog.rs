//! The apps the launcher knows about: Zendaya's Photozen.
//!
//! The list is data so a later version can fetch it from an API; [`SUITE`] is the built-in copy.
//!
//! Text here is English, the source the UI translates from: each `tagline` has a catalog row
//! keyed by the app id (`tr_ctx(app.id, app.tagline)` in `ui-egui`).

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
    /// The `<name>` in its bundle id `ai.storyteller.<name>`: the id, unless the app kept an older one.
    pub bundle_name: &'static str,
}

impl CraftApp {
    /// `ai.storyteller.<bundle_name>`: macOS bundle id, Flatpak id and Linux desktop-file id.
    pub fn bundle_id(&self) -> String {
        format!("ai.storyteller.{}", self.bundle_name)
    }

    pub fn releases_url(&self) -> String {
        format!("https://github.com/{}/releases", self.repo)
    }

    pub fn repo_url(&self) -> String {
        format!("https://github.com/{}", self.repo)
    }

    /// The app's page on getartcraft.com.
    pub fn website(&self) -> String {
        format!("https://getartcraft.com/apps/{}", self.id)
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
    CraftApp { id, name, tagline, color, ink_light, ink_dark, repo, former_ids: &[], former_names: &[], bundle_name: id }
}

/// The apps the launcher installs: Zendaya's Photozen only.
pub const SUITE: [CraftApp; 1] = [PHOTOZEN];

/// Photozen, Zendaya's image editor (a PhotoCraft fork). Its releases ship `photozen-<ver>-…`
/// assets; older builds of the fork still carry PhotoCraft's `photocraft-…` names and
/// `photocraft.exe`. It keeps PhotoCraft's bundle id, so macOS and Flatpak installs still match.
pub const PHOTOZEN: CraftApp = CraftApp {
    former_ids: &["photocraft"],
    former_names: &["PhotoCraft"],
    bundle_name: "photocraft",
    ..app("photozen", "Photozen", "Image editing", [[0x2f, 0x7b, 0xf5], [0x1a, 0x5b, 0xc4], [0x7f, 0xb0, 0xff]], "zendaya-tech/photocraft")
};

/// ArtCraft's website.
pub const WEBSITE_URL: &str = "https://getartcraft.com";
/// The GitHub account every app's repository lives under: the only one the launcher
/// installs from, whatever the release feed says.
pub const GITHUB_OWNER: &str = "zendaya-tech";
/// Zendaya on GitHub: every app's source.
pub const GITHUB_URL: &str = "https://github.com/zendaya-tech";
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
pub const LAUNCHER: CraftApp = CraftApp {
    former_names: &["ArtCraft Launcher"],
    ..app(
        "artcraft-launcher",
        "Zendaya Launcher",
        "Installs and updates Zendaya applications",
        [[0x0b, 0x0b, 0x0c], [0x0b, 0x0b, 0x0c], [0xf2, 0xf1, 0xee]],
        "zendaya-tech/zendaya-launcher",
    )
};

/// The launcher's own repo, checked so it can say when a newer launcher is out.
pub const LAUNCHER_REPO: &str = "zendaya-tech/zendaya-launcher";
/// The launcher's own asset stem.
pub const LAUNCHER_ID: &str = "artcraft-launcher";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_suite_is_photozen_alone() {
        assert_eq!(SUITE.len(), 1);
        let photozen = CraftApp::by_id("photozen").unwrap();
        assert_eq!(photozen.name, "Photozen");
        assert_eq!(photozen.repo, "zendaya-tech/photocraft");
        assert!(photozen.repo.starts_with(&format!("{GITHUB_OWNER}/")));
        assert_eq!(photozen.releases_url(), "https://github.com/zendaya-tech/photocraft/releases");
        assert!(CraftApp::by_id("photocraft").is_none());
    }

    #[test]
    fn photozen_also_answers_to_photocraft_assets_and_bundle_id() {
        assert_eq!(PHOTOZEN.stems().collect::<Vec<_>>(), ["photozen", "photocraft"]);
        assert_eq!(PHOTOZEN.names().collect::<Vec<_>>(), ["Photozen", "PhotoCraft"]);
        assert_eq!(PHOTOZEN.bundle_id(), "ai.storyteller.photocraft");
        assert_eq!(LAUNCHER.bundle_id(), "ai.storyteller.artcraft-launcher");
    }
}
