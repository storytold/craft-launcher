//! An in-memory [`ExtensionSource`] with a few sample themes, for tests and headless runs. It
//! follows the same rules as the real store: switching a theme off for an app also stops that app
//! wearing it, and an app wears a theme only if the theme is switched on for it.

use std::collections::BTreeSet;
use std::path::Path;

use craft_ext_core::id::{AppId, ExtensionId};
use craft_ext_core::manifest::Kind;
use craft_ext_core::palette::{Palette, Rgb};
use craft_ext_core::tier::Tier;
use craft_ext_core::version::Version;

use super::source::{ExtensionSource, Row};

#[derive(Clone, Debug)]
pub struct SampleSource {
    rows: Vec<Row>,
    generation: u64,
}

fn rgb(hex: u32) -> Rgb {
    let [_, r, g, b] = hex.to_be_bytes();
    Rgb(r, g, b)
}

fn palette(dark: bool, [background, surface, text, muted, line, accent]: [u32; 6]) -> Palette {
    Palette {
        dark,
        background: rgb(background),
        surface: rgb(surface),
        text: rgb(text),
        muted_text: rgb(muted),
        line: rgb(line),
        accent: rgb(accent),
        good: rgb(if dark { 0x3fd6b8 } else { 0x0a7563 }),
        warn: rgb(if dark { 0xffc14d } else { 0x8a5800 }),
        bad: rgb(if dark { 0xff6259 } else { 0xb3261e }),
    }
}

fn row(id: &str, name: &str, tier: Tier, apps: &[&str], palette: Palette) -> Option<Row> {
    let apps: Vec<AppId> = apps.iter().filter_map(|a| AppId::new(a).ok()).collect();
    Some(Row {
        id: ExtensionId::new(id).ok()?,
        version: Version::new(1, 0, 0),
        kind: Kind::Theme,
        name: name.to_string(),
        tier,
        enabled: apps.iter().cloned().collect(),
        apps,
        worn: BTreeSet::new(),
        palette: Some(palette),
    })
}

impl Default for SampleSource {
    fn default() -> Self {
        let suite = ["photocraft", "lightcraft", "pdfcraft"];
        let rows = [
            row(
                "org.storytold.midnight",
                "Midnight",
                Tier::FirstParty,
                &suite,
                palette(true, [0x101820, 0x18222d, 0xe8eef4, 0xa9b6c2, 0x35424f, 0x4aa3ff]),
            ),
            row(
                "org.storytold.paper",
                "Paper",
                Tier::FirstParty,
                &suite,
                palette(false, [0xf5f1e8, 0xece6d8, 0x1d1b16, 0x55504a, 0xcdc5b3, 0x1a5fb4]),
            ),
            row(
                "org.example.ocean",
                "Ocean",
                Tier::FromFile,
                &["photocraft"],
                palette(true, [0x0b2530, 0x12384a, 0xe6f4f7, 0xa6c8d2, 0x2c5b6e, 0x3fd6b8]),
            ),
        ];
        Self { rows: rows.into_iter().flatten().collect(), generation: 1 }
    }
}

/// Something installed that is not a theme (no colours), switched on for every app it is made for.
fn thing(kind: Kind, id: &str, name: &str, tier: Tier, apps: &[&str]) -> Option<Row> {
    let mut r = row(id, name, tier, apps, palette(true, [0; 6]))?;
    r.kind = kind;
    r.palette = None;
    Some(r)
}

impl SampleSource {
    /// A fuller list to see how the page lays out with a real person's collection: more themes (some
    /// in use), plug-ins and resources from each kind of source, and a few switched off. Names and
    /// colours are made up. The desktop app shows it only when started with `--demo-data`.
    pub fn demo() -> Self {
        let mut source = Self::default();
        let suite = ["photocraft", "lightcraft", "pdfcraft"];
        let more = [
            row(
                "org.storytold.high-contrast",
                "High Contrast",
                Tier::FirstParty,
                &suite,
                palette(true, [0x000000, 0x0d0d0d, 0xffffff, 0xd9d9d9, 0x8c8c8c, 0xffd400]),
            ),
            row(
                "org.example.forest",
                "Forest",
                Tier::FromFile,
                &["photocraft", "lightcraft"],
                palette(true, [0x10201a, 0x183228, 0xe4efe8, 0xa4bdb0, 0x2f5646, 0x6fcf97]),
            ),
            row(
                "org.example.sunset",
                "Sunset",
                Tier::FromFile,
                &["photocraft"],
                palette(false, [0xfbeee3, 0xf4dccb, 0x2a1a14, 0x6a4a3c, 0xe0bfa6, 0xd9541e]),
            ),
            row(
                "org.example.rose",
                "Rose Quartz",
                Tier::FromFile,
                &suite,
                palette(false, [0xf8eef1, 0xf0dde3, 0x2b1a20, 0x6b4a55, 0xdcbcc7, 0xb83b66]),
            ),
            row(
                "org.example.graphite",
                "Graphite",
                Tier::ReviewedThirdParty,
                &["photocraft", "pdfcraft"],
                palette(true, [0x1b1d21, 0x25282e, 0xdfe3ea, 0x9aa3b2, 0x3a3f49, 0x7aa2ff]),
            ),
        ];
        let other = [
            thing(Kind::Plugin, "org.storytold.contact-sheet", "Contact Sheet", Tier::FirstParty, &["photocraft", "lightcraft"]),
            thing(Kind::Plugin, "org.example.batch-rename", "Batch Rename", Tier::FromFile, &["photocraft", "lightcraft", "pdfcraft"]),
            thing(Kind::Plugin, "org.example.watermark-stamp", "Watermark Stamp", Tier::FromFile, &["photocraft", "pdfcraft"]),
            thing(Kind::Plugin, "org.example.palette-from-photo", "Palette From Photo", Tier::ReviewedThirdParty, &["photocraft"]),
            thing(Kind::Resource, "org.storytold.film-grain", "Film Grain Textures", Tier::FirstParty, &["photocraft", "lightcraft"]),
            thing(Kind::Resource, "org.example.paper-textures", "Paper Textures", Tier::FromFile, &["photocraft"]),
            thing(Kind::Resource, "org.example.charcoal-brushes", "Charcoal Brush Pack", Tier::FromFile, &["photocraft"]),
            thing(Kind::Resource, "org.example.print-profiles", "Print Paper Profiles", Tier::FromFile, &["lightcraft", "pdfcraft"]),
        ];
        source.rows.extend(more.into_iter().flatten().chain(other.into_iter().flatten()));
        // A few switched off for one app, and two themes in use.
        for (id, app, on) in [
            ("org.example.batch-rename", "pdfcraft", false),
            ("org.example.watermark-stamp", "pdfcraft", false),
            ("org.example.forest", "lightcraft", false),
        ] {
            if let (Ok(id), Ok(app)) = (ExtensionId::new(id), AppId::new(app)) {
                let _ = source.set_enabled(&app, &id, on);
            }
        }
        for (app, id) in [("photocraft", "org.storytold.midnight"), ("lightcraft", "org.storytold.paper"), ("pdfcraft", "org.example.rose")] {
            if let (Ok(id), Ok(app)) = (ExtensionId::new(id), AppId::new(app)) {
                let _ = source.wear(&app, Some(&id));
            }
        }
        source
    }

    fn change<T>(&mut self, f: impl FnOnce(&mut Vec<Row>) -> Result<T, String>) -> Result<T, String> {
        let out = f(&mut self.rows)?;
        self.generation += 1;
        Ok(out)
    }
}

impl ExtensionSource for SampleSource {
    fn generation(&mut self) -> u64 {
        self.generation
    }

    fn rows(&mut self) -> Result<Vec<Row>, String> {
        Ok(self.rows.clone())
    }

    fn add_file(&mut self, _path: &Path) -> Result<String, String> {
        Err("this sample list cannot add files".to_string())
    }

    fn remove(&mut self, id: &ExtensionId) -> Result<(), String> {
        self.change(|rows| {
            let before = rows.len();
            rows.retain(|r| &r.id != id);
            if rows.len() == before { Err("that extension is not installed".to_string()) } else { Ok(()) }
        })
    }

    fn set_enabled(&mut self, app: &AppId, id: &ExtensionId, on: bool) -> Result<(), String> {
        self.change(|rows| {
            let r = rows.iter_mut().find(|r| &r.id == id).ok_or("that extension is not installed")?;
            if !r.apps.contains(app) {
                return Err("that extension is not made for this app".to_string());
            }
            if on {
                r.enabled.insert(app.clone());
            } else {
                r.enabled.remove(app);
                r.worn.remove(app);
            }
            Ok(())
        })
    }

    fn wear(&mut self, app: &AppId, id: Option<&ExtensionId>) -> Result<(), String> {
        self.change(|rows| {
            if let Some(id) = id {
                let r = rows.iter().find(|r| &r.id == id).ok_or("that extension is not installed")?;
                if !r.apps.contains(app) {
                    return Err("that extension is not made for this app".to_string());
                }
                if !r.enabled.contains(app) {
                    return Err("turn the theme on for this app before choosing it".to_string());
                }
            }
            for r in rows.iter_mut() {
                r.worn.remove(app);
            }
            if let Some(r) = id.and_then(|id| rows.iter_mut().find(|r| &r.id == id)) {
                r.worn.insert(app.clone());
            }
            Ok(())
        })
    }
}
