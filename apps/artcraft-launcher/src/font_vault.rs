//! The desktop side of Settings' Font Vault Management panel: the FontCore vault on disk, shared
//! with every Craft app and the `fontcore` CLI (`$FONTCORE_HOME`, else the platform data folder).

use std::path::Path;

use artcraft_launcher_ui_egui::{FontAdded, FontRow};
use fontcore::{AddOptions, AddOutcome, Error, FontEntry, Vault};

/// The vault's fonts in manifest order; a vault nobody has created yet is simply empty.
pub fn list() -> Result<Vec<FontRow>, String> {
    let root = Vault::default_root().map_err(|e| e.to_string())?;
    match Vault::open(root) {
        Ok(vault) => Ok(vault.manifest().map_err(|e| e.to_string())?.fonts.iter().map(row).collect()),
        Err(Error::NotInitialized(_)) => Ok(Vec::new()),
        Err(e) => Err(e.to_string()),
    }
}

/// Copies a font file into the vault (creating the vault on first use). FontCore checks the file
/// is a TTF, OTF or collection, hashes it, and skips it if the same bytes are already there.
pub fn add(path: &Path) -> Result<FontAdded, String> {
    let vault = Vault::default_root().and_then(Vault::init).map_err(|e| e.to_string())?;
    Ok(match vault.add(path, &AddOptions::default()).map_err(|e| e.to_string())? {
        AddOutcome::Added(entry) => FontAdded::Added(row(&entry)),
        AddOutcome::AlreadyPresent(entry) => FontAdded::AlreadyPresent(row(&entry)),
    })
}

/// Removes a font from the manifest and deletes the vault's copy of the file.
pub fn remove(id: &str) -> Result<(), String> {
    let vault = Vault::default_root().and_then(Vault::open).map_err(|e| e.to_string())?;
    vault.remove(id).map(|_| ()).map_err(|e| e.to_string())
}

fn row(entry: &FontEntry) -> FontRow {
    let name = match entry.faces.as_slice() {
        [face] => format!("{} {}", face.family, face.subfamily),
        _ => entry.family().to_string(),
    };
    let mut detail = entry.format.extension().to_uppercase();
    if let Some(license) = &entry.license {
        detail.push_str(&format!(" · {license}"));
    }
    FontRow { id: entry.id.clone(), name, detail }
}
