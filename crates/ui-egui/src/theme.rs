//! Colours, type and shapes, matching getartcraft.com: hard edges, hairline rules, Archivo
//! display type, Inter body text and Geist Mono labels. Widgets read [`Tokens::get`]; nothing
//! hard-codes a colour.

use std::sync::Arc;

use egui::epaint::text::VariationCoords;
use egui::{Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, FontTweak, Stroke, TextStyle, Visuals};
use serde::{Deserialize, Serialize};

use crate::i18n::Lang;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeChoice {
    /// Follow the system's light or dark setting.
    #[default]
    System,
    Dark,
    Light,
}

impl ThemeChoice {
    pub const ALL: [Self; 3] = [Self::System, Self::Dark, Self::Light];

    /// The English key translated at draw time into the current UI language
    /// (the shell sets it per frame via `i18n::set_current`). Same-crate lookup,
    /// pure and wasm-safe; no new dependency. Call sites stay unchanged.
    pub fn label(self) -> &'static str {
        match self {
            Self::System => crate::i18n::t("Match system"),
            Self::Dark => crate::i18n::t("Dark"),
            Self::Light => crate::i18n::t("Light"),
        }
    }
}

/// Colour tokens for one appearance: the website's (`globals.css`), by the same names.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tokens {
    pub dark: bool,
    /// Page and card surface.
    pub bg: Color32,
    /// Badges, wells and row hovers.
    pub sunken: Color32,
    /// Body text.
    pub ink: Color32,
    /// Headings.
    pub ink_strong: Color32,
    /// Secondary text.
    pub muted: Color32,
    /// Labels and tertiary text.
    pub faint: Color32,
    /// Hairlines.
    pub line: Color32,
    /// Outlined buttons and stronger rules.
    pub line_strong: Color32,
    /// The brand blue, for surfaces.
    pub accent: Color32,
    /// The brand blue, for text and icons.
    pub accent_ink: Color32,
    /// Primary buttons and hover inversion.
    pub invert_bg: Color32,
    pub invert_fg: Color32,
    pub good: Color32,
    pub warn: Color32,
    pub bad: Color32,
}

const fn ink_alpha(rgb: u32, alpha: u8) -> Color32 {
    let [_, r, g, b] = rgb.to_be_bytes();
    Color32::from_rgba_unmultiplied_const(r, g, b, alpha)
}

const fn hex(rgb: u32) -> Color32 {
    let [_, r, g, b] = rgb.to_be_bytes();
    Color32::from_rgb(r, g, b)
}

impl Tokens {
    pub const DARK: Self = Self {
        dark: true,
        bg: hex(0x121316),
        sunken: hex(0x060607),
        ink: hex(0xf2f1ee),
        ink_strong: hex(0xffffff),
        muted: ink_alpha(0xf2f1ee, 153),
        faint: ink_alpha(0xf2f1ee, 97),
        line: ink_alpha(0xf2f1ee, 38),
        line_strong: ink_alpha(0xf2f1ee, 102),
        accent: hex(0x2d81ff),
        accent_ink: hex(0x74aaff),
        invert_bg: hex(0xffffff),
        invert_fg: hex(0x0b0b0c),
        // A green and LightCraft's amber from the site's app inks, so status colours come from its palette.
        good: hex(0x3fd6b8),
        warn: hex(0xffc14d),
        bad: hex(0xff6259),
    };

    pub const LIGHT: Self = Self {
        dark: false,
        bg: hex(0xf2f1ee),
        sunken: hex(0xe9e8e4),
        ink: hex(0x101014),
        ink_strong: hex(0x000000),
        muted: ink_alpha(0x101014, 158),
        faint: ink_alpha(0x101014, 107),
        line: ink_alpha(0x101014, 41),
        line_strong: ink_alpha(0x101014, 102),
        accent: hex(0x2d81ff),
        accent_ink: hex(0x1659c4),
        invert_bg: hex(0x101014),
        invert_fg: hex(0xf2f1ee),
        good: hex(0x0a7563),
        warn: hex(0x8a5800),
        bad: hex(0xb3261e),
    };

    pub fn get(ctx: &egui::Context) -> Self {
        if ctx.theme() == egui::Theme::Dark { Self::DARK } else { Self::LIGHT }
    }

    pub fn hairline(&self) -> Stroke {
        Stroke::new(1.0, self.line)
    }

    /// An app's text colour (`--app-ink`): readable on this appearance's background.
    pub fn app_ink(&self, app: &artcraft_launcher_core::CraftApp) -> Color32 {
        rgb(if self.dark { app.ink_dark } else { app.ink_light })
    }
}

/// Mixes `c` into the background at `alpha` (0â€“1): tinted washes.
pub fn tint(c: Color32, alpha: f32) -> Color32 {
    let a = (alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), a)
}

pub fn rgb(c: [u8; 3]) -> Color32 {
    Color32::from_rgb(c[0], c[1], c[2])
}

/// The shadow under menus, dialogs and toasts.
pub fn shadow(t: &Tokens) -> egui::epaint::Shadow {
    egui::epaint::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(if t.dark { 140 } else { 36 }) }
}

/// The dim over the page behind a drawer; `amount` runs from 0 (none) to 1.
pub fn scrim(t: &Tokens, amount: f32) -> Color32 {
    let alpha = if t.dark { 170.0 } else { 105.0 } * amount.clamp(0.0, 1.0);
    Color32::from_black_alpha(alpha.round() as u8)
}

/// Behind an enlarged picture: near-black in both appearances, as pictures are judged on black.
pub fn backdrop() -> Color32 {
    Color32::from_black_alpha(232)
}

/// A frame standing in for a picture, over [`backdrop`].
pub fn backdrop_panel() -> Color32 {
    Color32::from_white_alpha(14)
}

/// Ink on [`backdrop`], at `alpha` out of 255.
pub fn backdrop_ink(alpha: u8) -> Color32 {
    Color32::from_white_alpha(alpha)
}

fn visuals(t: &Tokens) -> Visuals {
    let mut v = if t.dark { Visuals::dark() } else { Visuals::light() };
    v.panel_fill = t.bg;
    v.window_fill = t.bg;
    v.extreme_bg_color = t.sunken;
    v.text_edit_bg_color = Some(t.sunken);
    v.faint_bg_color = t.sunken;
    v.code_bg_color = t.sunken;
    v.window_stroke = Stroke::new(1.0, t.line_strong);
    v.window_corner_radius = CornerRadius::ZERO;
    v.menu_corner_radius = CornerRadius::ZERO;
    v.window_shadow = shadow(t);
    v.popup_shadow = shadow(t);
    v.override_text_color = Some(t.ink);
    v.hyperlink_color = t.accent_ink;
    v.selection.bg_fill = t.accent;
    v.selection.stroke = Stroke::new(1.0, Color32::WHITE);
    v.warn_fg_color = t.warn;
    v.error_fg_color = t.bad;
    for (w, fill, stroke, fg) in [
        (&mut v.widgets.noninteractive, t.bg, t.line, t.muted),
        (&mut v.widgets.inactive, t.bg, t.line, t.ink),
        (&mut v.widgets.hovered, t.sunken, t.line_strong, t.ink_strong),
        (&mut v.widgets.active, t.sunken, t.line_strong, t.ink_strong),
        (&mut v.widgets.open, t.sunken, t.line_strong, t.ink_strong),
    ] {
        w.bg_fill = fill;
        w.weak_bg_fill = fill;
        w.bg_stroke = Stroke::new(1.0, stroke);
        w.corner_radius = CornerRadius::ZERO;
        w.fg_stroke = Stroke::new(1.0, fg);
        w.expansion = 0.0;
    }
    v
}

/// Applies both appearances (egui switches between them with the theme preference).
pub fn apply(ctx: &egui::Context, choice: ThemeChoice) {
    ctx.set_visuals_of(egui::Theme::Dark, visuals(&Tokens::DARK));
    ctx.set_visuals_of(egui::Theme::Light, visuals(&Tokens::LIGHT));
    ctx.set_theme(match choice {
        ThemeChoice::System => egui::ThemePreference::System,
        ThemeChoice::Dark => egui::ThemePreference::Dark,
        ThemeChoice::Light => egui::ThemePreference::Light,
    });
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        // Between a checkbox (or radio button) and its label.
        style.spacing.icon_spacing = 10.0;
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = CONTROL_H;
        style.spacing.menu_margin = egui::Margin::same(4);
        style.spacing.window_margin = egui::Margin::same(4);
        style.text_styles = [
            (TextStyle::Heading, FontId::new(24.0, display())),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
            (TextStyle::Button, FontId::new(13.5, medium())),
            (TextStyle::Small, FontId::new(12.0, FontFamily::Proportional)),
            (TextStyle::Monospace, FontId::new(13.0, FontFamily::Monospace)),
        ]
        .into();
    });
}

/// Height of buttons, inputs and filter cells (the site's medium button).
pub const CONTROL_H: f32 = 34.0;

/// The site's `.hud-label`: 11 px Geist Mono 500, upper case, 0.15 em apart.
pub const HUD_SIZE: f32 = 11.0;
pub const HUD_SPACING: f32 = HUD_SIZE * 0.15;
/// Buttons: 11 px Geist Mono 700, upper case, 0.12 em apart.
pub const BUTTON_SPACING: f32 = HUD_SIZE * 0.12;

// ---- layout ------------------------------------------------------------------------------------
// The app page and the large dialogs (Settings, release notes) share these, so they read as one
// family; the toolbar and the footer use the same steps.

/// Padding inside the app page and the dialogs, round their headers, bodies and footers.
pub const PAD: f32 = 24.0;
/// The gap between the two columns of a page or dialog.
pub const COLUMN_GAP: f32 = 32.0;
/// Padding inside a hairline box: a group of settings, a feature, a release fact.
pub const BOX_MARGIN: egui::Margin = egui::Margin::symmetric(14, 12);
/// Below this width the toolbar moves the sort and "Update all" to a second row.
pub const TOOLBAR_COMPACT_BELOW: f32 = 640.0;
/// From this width the large dialogs read in two columns.
pub const DIALOG_COLUMNS_FROM: f32 = 760.0;
/// From this width the app page reads in two columns (its pictures need the room).
pub const PAGE_COLUMNS_FROM: f32 = 900.0;
/// Title sizes, largest first: an app page's name, a large dialog's title, a small prompt's title.
pub const PAGE_TITLE: f32 = 32.0;
pub const SHEET_TITLE: f32 = 26.0;
pub const DIALOG_TITLE: f32 = 22.0;
/// A row of icon links (the footer, Settings): the room round each logo, and the gap between their
/// targets; neighbours' logos sit `2 × ICON_LINK_PAD + ICON_LINK_GAP` apart.
pub const ICON_LINK_PAD: f32 = 6.0;
pub const ICON_LINK_GAP: f32 = 2.0;

/// Inter 500.
pub fn medium() -> FontFamily {
    FontFamily::Name("medium".into())
}

/// Inter 600.
pub fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Archivo 620 at 118 % width: the site's `.font-display` headings and app wordmarks.
pub fn display() -> FontFamily {
    FontFamily::Name("display".into())
}

/// Geist Mono 500: labels and badges.
pub fn mono() -> FontFamily {
    FontFamily::Name("mono".into())
}

/// Geist Mono 700: buttons and app number tabs.
pub fn mono_bold() -> FontFamily {
    FontFamily::Name("mono-bold".into())
}

/// A font registered under `name`: its file, its variation coordinates and its family.
type Face<'a> = (&'a str, &'static [u8], &'a [([u8; 4], f32)], FontFamily);

/// Registers the site's three variable fonts, each at the weights (and width) it uses there.
/// `cjk`: the CJK language to draw, whose system font is added behind everything else, or `None`
/// for a language that needs no CJK font.
pub fn install_fonts(ctx: &egui::Context, cjk: Option<Lang>) {
    ctx.set_fonts(font_definitions(cjk));
}

/// The font stack both the native and the web app use.
///
/// It starts from egui's `default_fonts` and enables `monochrome_emoji_fonts` at the workspace
/// level, so arbitrary release-note, feed and alert text keeps the monochrome emoji coverage 0.36
/// provided on every target, web included. The fixed gear and refresh controls stay painter-drawn
/// [`crate::widgets::Glyph`]s rather than font glyphs. The one optional extra is a system CJK face,
/// added only when the desktop selected a CJK language; neither a CJK face nor a system-font
/// provider is bundled.
fn font_definitions(cjk: Option<Lang>) -> FontDefinitions {
    const INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Variable.ttf");
    const ARCHIVO: &[u8] = include_bytes!("../../../assets/fonts/Archivo-Variable.ttf");
    const GEIST_MONO: &[u8] = include_bytes!("../../../assets/fonts/GeistMono-Variable.ttf");
    let mut fonts = FontDefinitions::default();
    // egui's own Latin faces and the small monochrome icon face stay behind ours.
    let fallback: Vec<String> = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let faces: [Face<'_>; 7] = [
        ("Inter", INTER, &[(*b"wght", 400.0), (*b"opsz", 14.0)], FontFamily::Proportional),
        ("Inter-Medium", INTER, &[(*b"wght", 500.0), (*b"opsz", 14.0)], medium()),
        ("Inter-SemiBold", INTER, &[(*b"wght", 600.0), (*b"opsz", 14.0)], semibold()),
        ("Archivo-Display", ARCHIVO, &[(*b"wght", 620.0), (*b"wdth", 118.0)], display()),
        ("GeistMono", GEIST_MONO, &[(*b"wght", 400.0)], FontFamily::Monospace),
        ("GeistMono-Medium", GEIST_MONO, &[(*b"wght", 500.0)], mono()),
        ("GeistMono-Bold", GEIST_MONO, &[(*b"wght", 700.0)], mono_bold()),
    ];
    for (name, bytes, coords, family) in faces {
        // No subpixel binning: these fonts carry no hinting instructions, so a glyph drawn at a
        // fractional x smears each stem across two pixels, leaving small text blurry at 100 %.
        let tweak = FontTweak { coords: VariationCoords::new(coords.iter().copied()), subpixel_binning: Some(false), ..Default::default() };
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(bytes).tweak(tweak)));
        let mut stack = vec![name.to_owned()];
        stack.extend(fallback.iter().cloned());
        fonts.families.insert(family, stack);
    }
    // The CJK face, behind everything else, from the system: only when asked for, as the font is
    // large.
    if let Some(lang) = cjk
        && let Some(font) = cjk_font(lang)
    {
        fonts.font_data.insert("CJK".to_owned(), font);
        for stack in fonts.families.values_mut() {
            stack.push("CJK".to_owned());
        }
    }
    fonts
}

/// The system's font for a CJK language, read once per language (tens of MB). Desktop only: the
/// web build has none, so CJK text can't be drawn there.
pub fn cjk_font(lang: Lang) -> Option<Arc<FontData>> {
    #[cfg(test)]
    {
        let overridden = CJK_FONT_OVERRIDE.with(|slot| slot.borrow().as_ref().and_then(|overrides| overrides.get(lang.code()).cloned()));
        if let Some(font) = overridden {
            return font;
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        CJK_FONTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(lang.code()).or_insert_with(|| load_cjk(lang)).clone()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = lang;
        None
    }
}

/// Loads the system font for the CJK language `lang`: the face for its script, or `None` where the
/// system has none. Nothing falls back to another script's face — a face for a different script
/// leaves its characters as tofu — so a language without its font stays in English, like any other.
#[cfg(not(target_arch = "wasm32"))]
fn load_cjk(lang: Lang) -> Option<Arc<FontData>> {
    crate::cjk_font::load(lang.script()?).map(Arc::new)
}

/// The system fonts looked for so far, by language code.
#[cfg(not(target_arch = "wasm32"))]
static CJK_FONTS: std::sync::Mutex<std::collections::BTreeMap<&str, Option<Arc<FontData>>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

#[cfg(test)]
type CjkFontOverrides = std::collections::BTreeMap<&'static str, Option<Arc<FontData>>>;

// Test-only per-thread answer for cjk_font, scoped by CjkFontOverride.
#[cfg(test)]
thread_local! {
    static CJK_FONT_OVERRIDE: std::cell::RefCell<Option<CjkFontOverrides>> =
        const { std::cell::RefCell::new(None) };
}

/// Scopes a test-only answer for [`cjk_font`], restoring the previous answer on drop.
#[cfg(test)]
pub(crate) struct CjkFontOverride {
    previous: Option<CjkFontOverrides>,
}

#[cfg(test)]
impl CjkFontOverride {
    /// The system has a face for `lang`: a real bundled face with no CJK glyphs stands in, so a
    /// test can prove the font gate installed a face without faking CJK coverage or depending on
    /// the machine.
    pub(crate) fn available(lang: Lang) -> Self {
        const INTER: &[u8] = include_bytes!("../../../assets/fonts/Inter-Variable.ttf");
        Self::install(lang, Some(Arc::new(FontData::from_static(INTER))))
    }

    /// The system has no face for `lang`.
    pub(crate) fn unavailable(lang: Lang) -> Self {
        Self::install(lang, None)
    }

    fn install(lang: Lang, font: Option<Arc<FontData>>) -> Self {
        let mut overrides = std::collections::BTreeMap::new();
        overrides.insert(lang.code(), font);
        let previous = CJK_FONT_OVERRIDE.with(|slot| slot.borrow_mut().replace(overrides));
        Self { previous }
    }
}

#[cfg(test)]
impl Drop for CjkFontOverride {
    fn drop(&mut self) {
        CJK_FONT_OVERRIDE.with(|slot| *slot.borrow_mut() = self.previous.take());
    }
}

/// Has the system font for `lang` already been loaded? Never loads it.
pub fn cjk_font_loaded(lang: Lang) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        CJK_FONTS.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(lang.code()).is_some_and(Option::is_some)
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = lang;
        false
    }
}

/// Can the CJK language `lang` be drawn? Loads the system font on the first call ([`cjk_font`]).
pub fn cjk_font_available(lang: Lang) -> bool {
    cjk_font(lang).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The app's bundled font set is deliberate and bounded: our three families, egui's Latin
    /// fallbacks, its small monochrome icon face and the two attributed monochrome emoji faces.
    /// The same function is used by the desktop and web builds, so this upper bound holds for both.
    #[test]
    fn production_fonts_are_latin_plus_attributed_monochrome_fallbacks() {
        let fonts = font_definitions(None);
        for expected in ["Inter", "Inter-Medium", "Inter-SemiBold", "Archivo-Display", "GeistMono", "GeistMono-Medium", "GeistMono-Bold"] {
            assert!(fonts.font_data.contains_key(expected), "missing bundled face {expected}: {:?}", fonts.font_data.keys().collect::<Vec<_>>());
        }
        for fallback in ["Ubuntu-Light", "Hack", "egui-icons", "NotoEmoji-Regular", "emoji-icon-font"] {
            assert!(fonts.font_data.contains_key(fallback), "missing egui fallback {fallback}");
        }
        assert_eq!(fonts.font_data.len(), 12, "unexpected extra faces: {:?}", fonts.font_data.keys().collect::<Vec<_>>());
        for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace, medium(), semibold(), display(), mono(), mono_bold()] {
            let stack = fonts.families.get(&family).unwrap_or_else(|| panic!("family {family:?} missing"));
            assert_eq!(
                stack.last().map(String::as_str),
                Some("emoji-icon-font"),
                "{family:?} must end at the bundled monochrome emoji fallback, without system fonts: {stack:?}"
            );
        }
    }

    /// Arbitrary network text must not depend on a system font: release notes, feed notices and
    /// alerts can contain emoji. This is a strict fixture over representative dynamic text, not
    /// just the fixed controls (whose gear/refresh glyphs are painter-drawn). It also pins the
    /// bundled font-data upper bound, so a stray system face cannot make coverage pass.
    #[test]
    fn bundled_font_stack_covers_representative_dynamic_release_and_feed_text() {
        use egui::{FontFamily, FontId};
        const DYNAMIC_TEXT: &str = concat!(
            "What's Changed: Faster exports 🚀 and fewer bugs 🐛 — New templates 🎨, ",
            "warnings ⚠ and a celebration 🎉 — ArtCraft news: ✨ ✅ ❌ 💡 🔒"
        );
        let fonts = font_definitions(None);
        assert_eq!(
            fonts.font_data.len(),
            12,
            "the coverage test is only meaningful for the bundled stack: {:?}",
            fonts.font_data.keys().collect::<Vec<_>>()
        );
        let ctx = egui::Context::default();
        install_fonts(&ctx, None);
        let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
        output.textures_delta.clear();
        for family in [FontFamily::Proportional, FontFamily::Monospace, medium(), semibold(), display(), mono(), mono_bold()] {
            ctx.fonts_mut(|fonts| {
                assert!(
                    fonts.has_glyphs(&FontId::new(14.0, family.clone()), DYNAMIC_TEXT),
                    "{family:?} lacks a glyph in the representative dynamic notes/feed fixture"
                );
            });
        }
    }
}
