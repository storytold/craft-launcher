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

impl SampleSource {
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
