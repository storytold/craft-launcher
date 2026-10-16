//! The shared extensions store as the Extensions page sees it: a thin layer over `craft-ext-store`.
//!
//! Invariants: nothing here opens a network connection; every failure comes back as a short
//! sentence a person can read (the store's own wording, which is plain by its own tests); a theme
//! whose file no longer reads or passes the colour checks simply has no preview, it never panics.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use artcraft_launcher_ui_egui::features::extensions::{ExtensionSource, Row};
use craft_ext_core::id::{AppId, ExtensionId};
use craft_ext_core::manifest::{Kind, THEME_FILE};
use craft_ext_core::palette::{MAX_THEME_BYTES, Palette, ThemeFile};
use craft_ext_core::tier::Tier;
use craft_ext_core::version::Version;
use craft_ext_store::location;
use craft_ext_store::reader::Reader;
use craft_ext_store::store::Store;

pub struct DesktopStore {
    store: Store,
    reader: Reader,
    /// Palettes already read, by extension and version. A package folder is never edited after install.
    palettes: HashMap<(ExtensionId, Version), Palette>,
}

impl DesktopStore {
    /// The store in its usual place (`CRAFT_EXT_DIR` overrides it), shared with every Craft app.
    pub fn open() -> Result<Self, String> {
        location::default_root().map(Self::at).map_err(|e| e.to_string())
    }

    pub fn at(root: PathBuf) -> Self {
        Self { store: Store::at(&root), reader: Reader::open(root), palettes: HashMap::new() }
    }

    fn palette(&mut self, id: &ExtensionId, version: Version) -> Option<Palette> {
        if let Some(found) = self.palettes.get(&(id.clone(), version)) {
            return Some(found.clone());
        }
        let path = self.store.package_dir(id, version).join(THEME_FILE);
        let mut bytes = Vec::new();
        File::open(path).ok()?.take(MAX_THEME_BYTES as u64 + 1).read_to_end(&mut bytes).ok()?;
        let palette = ThemeFile::parse(&bytes).ok()?.palette;
        self.palettes.insert((id.clone(), version), palette.clone());
        Some(palette)
    }
}

impl ExtensionSource for DesktopStore {
    fn generation(&mut self) -> u64 {
        self.reader.generation()
    }

    fn rows(&mut self) -> Result<Vec<Row>, String> {
        let state = self.store.state().map_err(|e| e.to_string())?;
        let mut rows = Vec::with_capacity(state.installed.len());
        for i in &state.installed {
            let palette = if i.kind == Kind::Theme { self.palette(&i.id, i.version) } else { None };
            rows.push(Row {
                id: i.id.clone(),
                version: i.version,
                kind: i.kind,
                name: i.name.clone(),
                tier: i.tier,
                apps: i.apps.clone(),
                enabled: i.apps.iter().filter(|a| state.is_enabled(a, &i.id)).cloned().collect(),
                worn: state.active_theme.iter().filter(|(_, chosen)| **chosen == i.id).map(|(app, _)| app.clone()).collect(),
                palette,
            });
        }
        self.palettes.retain(|(id, version), _| state.installed.iter().any(|i| &i.id == id && i.version == *version));
        Ok(rows)
    }

    fn add_file(&mut self, path: &Path) -> Result<String, String> {
        self.store.install_file(path, Tier::FromFile).map(|installed| installed.name).map_err(|e| e.to_string())
    }

    fn remove(&mut self, id: &ExtensionId) -> Result<(), String> {
        self.store.uninstall(id).map_err(|e| e.to_string())
    }

    fn set_enabled(&mut self, app: &AppId, id: &ExtensionId, on: bool) -> Result<(), String> {
        self.store.set_enabled(app, id, on).map_err(|e| e.to_string())
    }

    fn wear(&mut self, app: &AppId, id: Option<&ExtensionId>) -> Result<(), String> {
        self.store.set_active_theme(app, id).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests;
