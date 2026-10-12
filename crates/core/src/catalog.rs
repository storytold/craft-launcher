//! The Craft App Suite: the twelve apps the launcher knows about, and the community apps listed
//! beside them.
//!
//! The list is data so a later version can fetch it from an API; [`SUITE`] is the built-in copy.
//!
//! Text here is English, the source the UI translates from: each `tagline` has a catalog row
//! keyed by the app id (`tr_ctx(app.id, app.tagline)` in `ui-egui`).

use serde::{Deserialize, Serialize};

/// Which generation of the suite an app belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AppGroup {
    /// The seven original apps, each with a page on getartcraft.com.
    Classic,
    /// The five apps added after them.
    New,
    /// Apps made outside the ArtCraft team that ship releases the Craft apps' way (same asset
    /// names, `SHA256SUMS.txt`, packaging), listed after the suite.
    Community,
}

impl AppGroup {
    pub fn label(self) -> &'static str {
        match self {
            Self::Classic => "Classic",
            Self::New => "New",
            Self::Community => "Community",
        }
    }
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
    /// The familiar app it reimplements, for orientation.
    pub like: &'static str,
    pub group: AppGroup,
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
    /// Its reverse-DNS id when that isn't `ai.storyteller.<id>` (community apps use their own).
    pub reverse_dns: Option<&'static str>,
    /// The publisher its Windows installer registers (Apps & features).
    pub publisher: &'static str,
    /// Its macOS `CFBundleIdentifier` when that differs from [`CraftApp::bundle_id`] (Apple
    /// allows no underscore there, so `io.github.engr_software.*` is `io.github.engr-software.*`).
    pub mac_bundle_id: Option<&'static str>,
}

/// The publisher of the ArtCraft team's apps.
pub const PUBLISHER: &str = "Learning Machines LLC";

impl CraftApp {
    /// The product it's inspired by, with its registered-trademark sign (`Adobe Photoshop®`).
    pub fn inspired_by(&self) -> String {
        format!("{}®", self.like)
    }

    /// `ai.storyteller.<id>` (or the app's own [`CraftApp::reverse_dns`]): macOS bundle id,
    /// Flatpak id and Linux desktop-file id.
    pub fn bundle_id(&self) -> String {
        self.reverse_dns.map_or_else(|| format!("ai.storyteller.{}", self.id), str::to_string)
    }

    /// The `CFBundleIdentifier` of its macOS app.
    pub fn mac_bundle_id(&self) -> String {
        self.mac_bundle_id.map_or_else(|| self.bundle_id(), str::to_string)
    }

    pub fn releases_url(&self) -> String {
        format!("https://github.com/{}/releases", self.repo)
    }

    pub fn repo_url(&self) -> String {
        format!("https://github.com/{}", self.repo)
    }

    /// The app's page on getartcraft.com (classic apps only).
    pub fn website(&self) -> Option<String> {
        (self.group == AppGroup::Classic).then(|| format!("https://getartcraft.com/apps/{}", self.id))
    }

    /// Asset stems this app's releases may use, current first.
    pub fn stems(&self) -> impl Iterator<Item = &'static str> + '_ {
        std::iter::once(self.id).chain(self.former_ids.iter().copied())
    }

    pub fn by_id(id: &str) -> Option<&'static CraftApp> {
        SUITE.iter().find(|a| a.id == id)
    }
}

const fn app(
    id: &'static str,
    name: &'static str,
    tagline: &'static str,
    like: &'static str,
    group: AppGroup,
    [color, ink_light, ink_dark]: [[u8; 3]; 3],
    repo: &'static str,
) -> CraftApp {
    CraftApp {
        id,
        name,
        tagline,
        like,
        group,
        color,
        ink_light,
        ink_dark,
        repo,
        former_ids: &[],
        reverse_dns: None,
        publisher: PUBLISHER,
        mac_bundle_id: None,
    }
}

/// The Craft App Suite, in display order: the seven classic apps, the five new ones, then the
/// community apps.
pub const SUITE: [CraftApp; 13] = [
    app(
        "photocraft",
        "PhotoCraft",
        "Image editing",
        "Adobe Photoshop",
        AppGroup::Classic,
        [[0x2f, 0x7b, 0xf5], [0x1a, 0x5b, 0xc4], [0x7f, 0xb0, 0xff]],
        "storytold/photocraft",
    ),
    app(
        "vectorcraft",
        "VectorCraft",
        "Vector illustration",
        "Adobe Illustrator",
        AppGroup::Classic,
        [[0xe8, 0x57, 0x3f], [0xb8, 0x3a, 0x24], [0xff, 0x8a, 0x70]],
        "storytold/vectorcraft",
    ),
    app(
        "filmcraft",
        "FilmCraft",
        "Video editing, color and sound",
        "Adobe Premiere Pro",
        AppGroup::Classic,
        [[0x8b, 0x5c, 0xf6], [0x6a, 0x3f, 0xd6], [0xb6, 0x9c, 0xff]],
        "storytold/filmcraft",
    ),
    app(
        "lightcraft",
        "LightCraft",
        "Photo library and raw development",
        "Adobe Lightroom",
        AppGroup::Classic,
        [[0xf2, 0xa5, 0x16], [0x8a, 0x58, 0x00], [0xff, 0xc1, 0x4d]],
        "storytold/lightcraft",
    ),
    CraftApp {
        former_ids: &["printcraft"],
        ..app(
            "pdfcraft",
            "PdfCraft",
            "Reading, organizing and protecting PDFs",
            "Adobe Acrobat",
            AppGroup::Classic,
            [[0x12, 0xa5, 0x8a], [0x0a, 0x75, 0x63], [0x3f, 0xd6, 0xb8]],
            "storytold/pdfcraft",
        )
    },
    app(
        "effectcraft",
        "EffectCraft",
        "Motion graphics and visual effects",
        "Adobe After Effects",
        AppGroup::Classic,
        [[0xe0, 0x36, 0x8f], [0xb0, 0x20, 0x6c], [0xff, 0x7a, 0xb8]],
        "storytold/effectcraft",
    ),
    app(
        "designcraft",
        "DesignCraft",
        "Page layout and publishing",
        "Adobe InDesign",
        AppGroup::Classic,
        [[0x7b, 0xb5, 0x1c], [0x45, 0x70, 0x08], [0xa6, 0xe0, 0x4a]],
        "storytold/designcraft",
    ),
    app(
        "wordcraft",
        "WordCraft",
        "Writing and document design",
        "Microsoft Word",
        AppGroup::New,
        [[0x3f, 0x5f, 0xe0], [0x2a, 0x44, 0xb8], [0x8e, 0xa2, 0xff]],
        "storytold/wordcraft",
    ),
    app(
        "deckcraft",
        "DeckCraft",
        "Presentations and slide shows",
        "Microsoft PowerPoint",
        AppGroup::New,
        [[0xf0, 0x7a, 0x1e], [0xa8, 0x4f, 0x0a], [0xff, 0xa0, 0x60]],
        "storytold/deckcraft",
    ),
    app(
        "gridcraft",
        "GridCraft",
        "Spreadsheets and data",
        "Microsoft Excel",
        AppGroup::New,
        [[0x1f, 0x9d, 0x55], [0x12, 0x70, 0x3a], [0x4f, 0xd6, 0x8a]],
        "storytold/gridcraft",
    ),
    app(
        "cadcraft",
        "CADCraft",
        "Computer-aided design and drafting",
        "Autodesk AutoCAD",
        AppGroup::New,
        [[0x3e, 0xa6, 0xe8], [0x1a, 0x6f, 0xa8], [0x7c, 0xc8, 0xff]],
        "storytold/cadcraft",
    ),
    app(
        "soundcraft",
        "SoundCraft",
        "Recording, editing and mixing audio",
        "Avid Pro Tools",
        AppGroup::New,
        [[0x1f, 0xa5, 0xc9], [0x0e, 0x72, 0x90], [0x5f, 0xd0, 0xee]],
        "storytold/soundcraft",
    ),
    CraftApp {
        reverse_dns: Some("io.github.engr_software.markupcraft"),
        mac_bundle_id: Some("io.github.engr-software.markupcraft"),
        publisher: "engr-software",
        ..app(
            "markupcraft",
            "MarkupCraft",
            "PDF markup, measurement and takeoff",
            "Bluebeam Revu",
            AppGroup::Community,
            [[0xe9, 0x77, 0x2e], [0xa8, 0x4c, 0x12], [0xff, 0xa2, 0x66]],
            "engr-software/markupcraft",
        )
    },
];

/// Shown wherever the products the apps are inspired by are named.
pub const TRADEMARK_NOTICE: &str = "Adobe, Photoshop, Illustrator, Premiere Pro, Lightroom, Acrobat, After Effects and InDesign are either \
registered trademarks or trademarks of Adobe in the United States and/or other countries. Microsoft, Word, PowerPoint and Excel are \
trademarks of the Microsoft group of companies. Autodesk and AutoCAD are registered trademarks or trademarks of Autodesk, Inc. Avid \
and Pro Tools are trademarks or registered trademarks of Avid Technology, Inc. Bluebeam and Revu are registered trademarks of Bluebeam, Inc. \
All other trademarks are the property of their \
respective owners. These names are used only to describe what each Craft App is for; the Craft App Suite is independent and is not \
affiliated with, sponsored by or endorsed by any of these companies.";

/// ArtCraft's website.
pub const WEBSITE_URL: &str = "https://getartcraft.com";
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

/// ArtCraft Launcher itself, described like a Craft App so it can update itself with the same
/// machinery (asset names `artcraft-launcher-<ver>-…`, bundle id `ai.storyteller.artcraft-launcher`,
/// `ArtCraft Launcher.app`). Not part of [`SUITE`].
pub const LAUNCHER: CraftApp = app(
    "artcraft-launcher",
    "ArtCraft Launcher",
    "Installs and updates the Craft App Suite",
    "",
    AppGroup::New,
    [[0x0b, 0x0b, 0x0c], [0x0b, 0x0b, 0x0c], [0xf2, 0xf1, 0xee]],
    "storytold/craft-launcher",
);

/// The launcher's own repo, checked so it can say when a newer launcher is out.
pub const LAUNCHER_REPO: &str = "storytold/craft-launcher";
/// The launcher's own asset stem.
pub const LAUNCHER_ID: &str = "artcraft-launcher";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suite_has_seven_classic_and_five_new_apps_with_unique_ids() {
        assert_eq!(SUITE.iter().filter(|a| a.group == AppGroup::Classic).count(), 7);
        assert_eq!(SUITE.iter().filter(|a| a.group == AppGroup::New).count(), 5);
        let mut ids: Vec<_> = SUITE.iter().map(|a| a.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), SUITE.len());
        for a in &SUITE {
            assert_eq!(a.name.to_lowercase(), a.id);
            if a.group == AppGroup::Community {
                // A community app keeps its own repo, reverse-DNS id and publisher.
                assert!(!a.repo.starts_with("storytold/") && a.repo.ends_with(&format!("/{}", a.id)), "{}", a.repo);
                assert!(a.reverse_dns.is_some_and(|r| !r.starts_with("ai.storyteller.")), "{}", a.id);
                assert_ne!(a.publisher, PUBLISHER, "{}", a.id);
                assert_eq!(a.website(), None);
            } else {
                assert_eq!(a.repo, format!("storytold/{}", a.id));
                assert_eq!((a.reverse_dns, a.publisher), (None, PUBLISHER));
            }
        }
        // Community apps come after the suite.
        let first = SUITE.iter().position(|a| a.group == AppGroup::Community).unwrap_or(SUITE.len());
        assert!(SUITE[first..].iter().all(|a| a.group == AppGroup::Community));
    }

    #[test]
    fn markupcraft_uses_its_own_ids() {
        let m = CraftApp::by_id("markupcraft").unwrap();
        assert_eq!(m.bundle_id(), "io.github.engr_software.markupcraft");
        assert_eq!(m.mac_bundle_id(), "io.github.engr-software.markupcraft");
        assert_eq!(m.releases_url(), "https://github.com/engr-software/markupcraft/releases");
        assert_eq!(m.inspired_by(), "Bluebeam Revu®");
        assert_eq!(CraftApp::by_id("photocraft").unwrap().bundle_id(), "ai.storyteller.photocraft");
    }

    #[test]
    fn every_inspiring_product_is_covered_by_the_trademark_notice() {
        for a in &SUITE {
            assert!(a.inspired_by().ends_with('®'));
            // The company and the product name both appear in the notice.
            let (company, product) = a.like.split_once(' ').unwrap();
            assert!(TRADEMARK_NOTICE.contains(company), "{company}");
            assert!(TRADEMARK_NOTICE.contains(product), "{product}");
        }
    }

    #[test]
    fn pdfcraft_also_answers_to_printcraft() {
        let pdf = CraftApp::by_id("pdfcraft").unwrap();
        assert_eq!(pdf.stems().collect::<Vec<_>>(), ["pdfcraft", "printcraft"]);
        assert_eq!(pdf.website().as_deref(), Some("https://getartcraft.com/apps/pdfcraft"));
        assert_eq!(CraftApp::by_id("wordcraft").unwrap().website(), None);
    }
}
