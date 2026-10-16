//! The themes the Craft apps already carry inside them, listed beside the ones people add so the
//! whole choice is in one place.
//!
//! Invariants:
//! - Display only. Choosing one of these is still done inside its own app; the launcher never
//!   writes anything about them, so nothing here can change anyone's look.
//! - The colours were read from each app's own theme code and mapped to the same palette the
//!   added themes use, so every card is drawn the same way. They are a snapshot: when an app
//!   changes a built-in theme, update the row (`builtin_tests` checks the rows stay readable and
//!   unique, not that they match the apps).
//! - No developer words in anything a person reads (`tests::plain_words` scans this file's strings).

use craft_ext_core::id::AppId;
use craft_ext_core::manifest::Kind;
use craft_ext_core::palette::{Palette, Rgb};

use super::plan::Focus;

/// One theme an app carries.
#[derive(Clone, Debug, PartialEq)]
pub struct BuiltIn {
    /// The app's id in the suite, `None` for the launcher itself.
    pub app: Option<AppId>,
    /// The app's name as a person knows it.
    pub app_name: &'static str,
    pub name: &'static str,
    pub palette: Palette,
}

type Row = (Option<&'static str>, &'static str, &'static str, bool, [u32; 8]);

/// App id, app name, theme name, dark, then background, surface, text, muted text, line, accent, warning, error.
const TABLE: &[Row] = &[
    (Some("photocraft"), "PhotoCraft", "Pro (Dark)", true, [0x282828, 0x323232, 0xdedede, 0x808080, 0x1e1e1e, 0x378ef0, 0xe8b046, 0xec5b62]),
    (Some("photocraft"), "PhotoCraft", "Pro (Medium Gray)", true, [0x282828, 0x535353, 0xeeeeee, 0xa0a0a0, 0x3e3e3e, 0x378ef0, 0xe8b046, 0xec5b62]),
    (Some("photocraft"), "PhotoCraft", "Pro (Light Gray)", false, [0xa0a0a0, 0xb8b8b8, 0x141414, 0x606060, 0x9e9e9e, 0x378ef0, 0xe8b046, 0xec5b62]),
    (Some("photocraft"), "PhotoCraft", "Pro (Light)", false, [0xd6d6d6, 0xf0f0f0, 0x141414, 0x787878, 0xd6d6d6, 0x378ef0, 0xe8b046, 0xec5b62]),
    (Some("photocraft"), "PhotoCraft", "Studio (Dark)", true, [0x0e0e0f, 0x141415, 0xececf0, 0x606068, 0x26262a, 0x8b7cf6, 0xf0be5a, 0xf06060]),
    (Some("photocraft"), "PhotoCraft", "Studio (Light)", false, [0xe2e2e6, 0xf6f6f8, 0x18181c, 0x9696a0, 0xe2e2e8, 0x6c5ce7, 0xbe8214, 0xd23c3c]),
    (Some("photocraft"), "PhotoCraft", "Classic", false, [0x808080, 0xd4d0c8, 0x000000, 0x808080, 0x808080, 0x0a246a, 0x806000, 0xa00000]),
    (Some("photocraft"), "PhotoCraft", "Solarized Dark", true, [0x002b36, 0x073642, 0x839496, 0x586e75, 0x00252f, 0x268bd2, 0xb58900, 0xdc322f]),
    (Some("photocraft"), "PhotoCraft", "Adwaita (Light)", false, [0xdededf, 0xffffff, 0x323236, 0x9b9ba0, 0xdedee1, 0x3584e4, 0x9c6e03, 0xc01c28]),
    (Some("photocraft"), "PhotoCraft", "Adwaita (Dark)", true, [0x1d1d20, 0x2e2e32, 0xffffff, 0x919194, 0x18181b, 0x3584e4, 0xf8e45c, 0xff7b63]),
    (Some("photocraft"), "PhotoCraft", "Breeze Light", false, [0xdbdee2, 0xdbdee2, 0x21252a, 0x515a64, 0xd6dade, 0x3aace8, 0x994800, 0xaa3240]),
    (Some("photocraft"), "PhotoCraft", "Breeze Dark", true, [0x111418, 0x262a30, 0xfafbfd, 0xa4aab1, 0x2e3236, 0x3aace8, 0xf47806, 0xe36f7b]),
    (Some("photocraft"), "PhotoCraft", "Soft Dark", true, [0x282830, 0x3a3a44, 0xeeeef6, 0xbebece, 0x24242c, 0x8a9ae8, 0xf0c882, 0xfaa0ac]),
    (Some("photocraft"), "PhotoCraft", "Soft Light", false, [0xdad8e0, 0xe8e6ec, 0x2c2a38, 0x504e60, 0xd2cfda, 0x6e80d8, 0x8c4800, 0xa62c3e]),
    (Some("lightcraft"), "LightCraft", "Charcoal", true, [0x1c1c1c, 0x2d2d2d, 0xe2e2e2, 0x8e8e8e, 0x3c3c3c, 0x0165dd, 0xe3a83c, 0xe04a4a]),
    (Some("lightcraft"), "LightCraft", "Midnight", true, [0x121212, 0x1e1e1e, 0xe2e2e2, 0x8e8e8e, 0x333333, 0x0165dd, 0xe3a83c, 0xe04a4a]),
    (Some("lightcraft"), "LightCraft", "Silver", false, [0xcfcfcf, 0xe6e6e6, 0x1c1c1c, 0x6a6a6a, 0xb8b8b8, 0x0a62d0, 0xb0761a, 0xc83434]),
    (Some("lightcraft"), "LightCraft", "Paper", false, [0xececec, 0xfafafa, 0x161616, 0x707070, 0xcccccc, 0x0a62d0, 0xb0761a, 0xc83434]),
    (None, "ArtCraft Launcher", "Launcher Dark", true, [0x121316, 0x060607, 0xf2f1ee, 0x989898, 0x333436, 0x2d81ff, 0xffc14d, 0xff6259]),
    (None, "ArtCraft Launcher", "Launcher Light", false, [0xf2f1ee, 0xe9e8e4, 0x101014, 0x666667, 0xcecdcb, 0x2d81ff, 0x8a5800, 0xb3261e]),
];

fn rgb(hex: u32) -> Rgb {
    let [_, r, g, b] = hex.to_be_bytes();
    Rgb(r, g, b)
}

/// Every built-in theme, apps in suite order and each app's themes in the order its own menu has them.
pub fn all() -> Vec<BuiltIn> {
    TABLE
        .iter()
        .map(|(app, app_name, name, dark, [background, surface, text, muted, line, accent, warn, bad])| BuiltIn {
            app: app.and_then(|a| AppId::new(a).ok()),
            app_name,
            name,
            palette: Palette {
                dark: *dark,
                background: rgb(*background),
                surface: rgb(*surface),
                text: rgb(*text),
                muted_text: rgb(*muted),
                line: rgb(*line),
                accent: rgb(*accent),
                good: rgb(if *dark { 0x3fd6b8 } else { 0x0a7563 }),
                warn: rgb(*warn),
                bad: rgb(*bad),
            },
        })
        .collect()
}

/// The built-in themes within a focus: only an app's own when the page was narrowed to it, none when
/// it was narrowed to something other than themes.
pub fn shown(focus: &Focus) -> Vec<BuiltIn> {
    if focus.kind.is_some_and(|k| k != Kind::Theme) {
        return Vec::new();
    }
    all().into_iter().filter(|b| focus.app.as_ref().is_none_or(|a| b.app.as_ref() == Some(a))).collect()
}

#[cfg(test)]
#[path = "builtin_tests.rs"]
mod tests;
