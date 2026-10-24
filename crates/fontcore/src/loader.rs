use std::fs;
use std::path::PathBuf;

use crate::error::IoContext;
use crate::{AppId, Error, FaceInfo, FontEntry, Result, Vault};

/// The fonts a suite app should register at launch.
#[derive(Debug, Clone, Default)]
pub struct FontSet {
    pub fonts: Vec<ResolvedFont>,
    /// Entries meant for this app whose file is missing or whose path is
    /// unsafe. Apps should log these rather than fail to start.
    pub unavailable: Vec<FontEntry>,
}

/// A manifest entry together with the absolute path of its font file.
#[derive(Debug, Clone)]
pub struct ResolvedFont {
    pub entry: FontEntry,
    pub path: PathBuf,
}

impl ResolvedFont {
    /// The faces to register; for collections, pass each face's `index` to
    /// the text engine.
    pub fn faces(&self) -> impl Iterator<Item = &FaceInfo> {
        self.entry.active_faces()
    }

    /// Reads the font file's bytes from the vault.
    pub fn read(&self) -> Result<Vec<u8>> {
        fs::read(&self.path).at(&self.path)
    }
}

/// Loads the fonts available to `app` (a reverse-DNS id such as
/// `ai.storytold.photocraft`) from the default vault.
///
/// A vault that has not been initialized yields an empty set, so apps can
/// call this unconditionally on launch.
pub fn load_for_app(app: &str) -> Result<FontSet> {
    let app = AppId::new(app)?;
    match Vault::open(Vault::default_root()?) {
        Ok(vault) => vault.fonts_for_app(&app),
        Err(Error::NotInitialized(_)) => Ok(FontSet::default()),
        Err(e) => Err(e),
    }
}

impl Vault {
    /// Resolves the fonts scoped to `app` (or to every app) that have at
    /// least one enabled face.
    pub fn fonts_for_app(&self, app: &AppId) -> Result<FontSet> {
        let mut set = FontSet::default();
        for entry in self.manifest()?.fonts {
            if !entry.available_to(app) {
                continue;
            }
            match entry.path_in(self.root()) {
                Some(path) if path.is_file() => set.fonts.push(ResolvedFont { entry, path }),
                _ => set.unavailable.push(entry),
            }
        }
        Ok(set)
    }
}
