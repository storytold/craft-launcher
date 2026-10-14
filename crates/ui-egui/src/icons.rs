//! Bundled images: each Craft App's icon (in the style the user picked) and the launcher's own.

use artcraft_launcher_core::IconStyle;
use egui::ImageSource;

/// A Craft App's icon: [`IconStyle::Default`] is the app's own 128 px icon (copied from its
/// repository), a pack is the bundled artwork `assets/app-icons/alternate_icons/` holds.
pub fn app(id: &str, style: IconStyle) -> Option<ImageSource<'static>> {
    let Some(pack) = style.pack() else { return own(id) };
    let art = artcraft_launcher_core::icons::artwork(id, pack)?;
    Some(ImageSource::Bytes { uri: format!("bytes://app-icons/{pack}/{id}.png").into(), bytes: egui::load::Bytes::Static(art.png) })
}

/// The app's own icon, as its release ships it.
fn own(id: &str) -> Option<ImageSource<'static>> {
    Some(match id {
        "photocraft" => egui::include_image!("../../../assets/app-icons/photocraft.png"),
        "vectorcraft" => egui::include_image!("../../../assets/app-icons/vectorcraft.png"),
        "filmcraft" => egui::include_image!("../../../assets/app-icons/filmcraft.png"),
        "lightcraft" => egui::include_image!("../../../assets/app-icons/lightcraft.png"),
        "pdfcraft" => egui::include_image!("../../../assets/app-icons/pdfcraft.png"),
        "effectcraft" => egui::include_image!("../../../assets/app-icons/effectcraft.png"),
        "designcraft" => egui::include_image!("../../../assets/app-icons/designcraft.png"),
        "wordcraft" => egui::include_image!("../../../assets/app-icons/wordcraft.png"),
        "deckcraft" => egui::include_image!("../../../assets/app-icons/deckcraft.png"),
        "gridcraft" => egui::include_image!("../../../assets/app-icons/gridcraft.png"),
        "cadcraft" => egui::include_image!("../../../assets/app-icons/cadcraft.png"),
        "soundcraft" => egui::include_image!("../../../assets/app-icons/soundcraft.png"),
        "figcraft" => egui::include_image!("../../../assets/app-icons/figcraft.png"),
        _ => return None,
    })
}

/// The label of an icon style, in the language the UI is drawn in: `Default`, or a pack's English
/// label from `packs.tsv`, translated like any other string.
pub fn icon_style_label(style: IconStyle) -> &'static str {
    match style.pack().and_then(artcraft_launcher_core::icons::pack) {
        Some(pack) => crate::i18n::t(pack.label),
        None => crate::i18n::t("Default"),
    }
}

/// The ArtCraft wordmark, as supplied: dark ink for light backgrounds, white for dark ones.
/// An ArtCraft trademark, not covered by the MIT/Apache license (`docs/brand/LICENSE-brand.txt`).
pub fn artcraft_logo(dark_background: bool) -> ImageSource<'static> {
    if dark_background {
        egui::include_image!("../../../docs/brand/artcraft-logo-white.svg")
    } else {
        egui::include_image!("../../../docs/brand/artcraft-logo.svg")
    }
}

/// The ArtCraft wordmark's width / height (its SVG view box: 1038.05 × 108).
pub const ARTCRAFT_LOGO_ASPECT: f32 = 1038.05 / 108.0;

/// A social service's logo (Simple Icons, white, for tinting), by its name in `catalog::SOCIAL`.
pub fn social(service: &str) -> Option<ImageSource<'static>> {
    Some(match service {
        "X" => egui::include_image!("../../../assets/social/x.svg"),
        "YouTube" => egui::include_image!("../../../assets/social/youtube.svg"),
        "TikTok" => egui::include_image!("../../../assets/social/tiktok.svg"),
        "Instagram" => egui::include_image!("../../../assets/social/instagram.svg"),
        "LinkedIn" => egui::include_image!("../../../assets/social/linkedin.svg"),
        _ => return None,
    })
}

/// The ArtCraft "A" mark in blue, as supplied: the launcher's app icon and getartcraft.com's favicon.
pub fn artcraft_mark() -> ImageSource<'static> {
    egui::include_image!("../../../docs/brand/artcraft-mark.svg")
}
