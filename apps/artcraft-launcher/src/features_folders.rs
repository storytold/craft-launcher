//! The desktop shell's side of the Model folders page: reads and changes the person's list of
//! model folders in the shared store, behind the UI's [`FeaturesPort`] methods.
//!
//! Nothing here changes a model file. Adding a folder reads it; removing one only forgets it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use artcraft_launcher_ui_egui::features::FolderCard;
use craft_ai_client::FolderDesk;
use craft_ai_comfy_env::FolderScan;
use craft_ai_model::{FolderOrigin, ModelFolder};
use craft_ai_native::{paths, shared::SharedStore};

pub struct DesktopFolders {
    desk: Option<FolderDesk>,
    suggested: Option<PathBuf>,
}

impl DesktopFolders {
    /// The standard store, proposing `ArtCraft\models` beside the AI files.
    pub fn new() -> Self {
        Self::with(SharedStore::at_default_location().ok().map(FolderDesk::new), paths::suggested_models_dir())
    }

    pub fn with(desk: Option<FolderDesk>, suggested: Option<PathBuf>) -> Self {
        Self { desk, suggested }
    }

    fn desk(&self) -> Result<&FolderDesk, String> {
        self.desk.as_ref().ok_or_else(|| "The AI settings file could not be found on this computer.".to_owned())
    }

    /// The folders, default first. A list that cannot be read is shown as empty.
    pub fn cards(&self) -> Vec<FolderCard> {
        let Ok(desk) = self.desk() else { return Vec::new() };
        let mut cards: Vec<FolderCard> = desk.list().unwrap_or_default().iter().map(card).collect();
        cards.sort_by_key(|c| !c.default);
        cards
    }

    /// The proposed folder, only while there is nowhere for models to go.
    pub fn suggested(&self) -> Option<String> {
        let has_one = self.desk().ok().and_then(|d| d.list().ok()).is_some_and(|l| !l.is_empty());
        self.suggested.as_ref().filter(|_| !has_one).map(|p| p.to_string_lossy().into_owned())
    }

    pub fn add(&self, path: &str, create: bool) -> Result<String, String> {
        let desk = self.desk()?;
        let path = Path::new(path);
        let added = if create { desk.add_new(path, None) } else { desk.add(path, None) }.map_err(|e| e.message)?;
        Ok(describe(&added.scan))
    }

    pub fn rescan(&self, id: &str) -> Result<String, String> {
        self.desk()?.rescan(id).map(|scan| describe(&scan)).map_err(|e| e.message)
    }

    pub fn set_default(&self, id: &str) -> Result<(), String> {
        self.desk()?.set_default(id).map_err(|e| e.message)
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.desk()?.remove(id).map_err(|e| e.message)
    }
}

fn card(folder: &ModelFolder) -> FolderCard {
    FolderCard {
        id: folder.id.clone(),
        label: folder.label.clone(),
        path: folder.path.clone(),
        default: folder.default,
        read_only: folder.origin != FolderOrigin::Chosen,
        missing: !Path::new(&folder.path).is_dir(),
        kinds: folder.kinds.iter().map(|(kind, dirs)| (kind.clone(), dirs.len())).collect(),
    }
}

/// One sentence on what a scan found.
fn describe(scan: &FolderScan) -> String {
    if scan.files.is_empty() {
        return "Added. No model files in it yet.".to_owned();
    }
    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    for file in &scan.files {
        if let Some(kind) = &file.kind {
            *by_kind.entry(kind).or_default() += 1;
        }
    }
    let kinds = by_kind.iter().map(|(kind, n)| format!("{n} {kind}")).collect::<Vec<_>>().join(", ");
    let mut text = format!("Found {} model files", scan.files.len());
    if !kinds.is_empty() {
        text.push_str(&format!(": {kinds}"));
    }
    text.push('.');
    if scan.unrecognised > 0 {
        text.push_str(&format!(" {} could not be told apart and were left out.", scan.unrecognised));
    }
    if scan.truncated {
        text.push_str(" The folder is very large, so only part of it was read.");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> (tempfile::TempDir, tempfile::TempDir, DesktopFolders) {
        let store = tempfile::tempdir().unwrap();
        let models = tempfile::tempdir().unwrap();
        let shared = SharedStore::new(store.path().join("connections.json"));
        let suggested = store.path().join("ArtCraft").join("models");
        let folders = DesktopFolders::with(Some(FolderDesk::new(shared)), Some(suggested));
        (store, models, folders)
    }

    fn put(root: &Path, rel: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"x").unwrap();
    }

    #[test]
    fn the_proposed_folder_is_offered_only_until_there_is_a_folder() {
        let (_s, models, f) = world();
        assert!(f.suggested().is_some());
        f.add(&models.path().to_string_lossy(), false).unwrap();
        assert!(f.suggested().is_none());
    }

    #[test]
    fn accepting_the_proposal_makes_the_folder_and_it_becomes_the_default() {
        let (_s, _m, f) = world();
        let proposed = f.suggested().unwrap();
        assert!(!Path::new(&proposed).exists());
        let said = f.add(&proposed, true).unwrap();
        assert!(Path::new(&proposed).is_dir());
        assert!(said.contains("No model files"), "{said}");
        let cards = f.cards();
        assert_eq!((cards.len(), cards[0].default, cards[0].missing, cards[0].read_only), (1, true, false, false));
    }

    #[test]
    fn what_was_found_is_said_in_one_sentence() {
        let (_s, models, f) = world();
        put(models.path(), "vae/a.safetensors");
        put(models.path(), "vae/b.safetensors");
        put(models.path(), "loras/c.safetensors");
        put(models.path(), "misc/d.safetensors");
        let said = f.add(&models.path().to_string_lossy(), false).unwrap();
        assert_eq!(said, "Found 4 model files: 1 loras, 2 vae. 1 could not be told apart and were left out.");
    }

    #[test]
    fn a_folder_that_has_gone_is_marked_missing_and_the_default_is_listed_first() {
        let (_s, models, f) = world();
        let other = tempfile::tempdir().unwrap();
        f.add(&models.path().to_string_lossy(), false).unwrap();
        f.add(&other.path().to_string_lossy(), false).unwrap();
        let second = f.cards().into_iter().find(|c| !c.default).unwrap();
        f.set_default(&second.id).unwrap();
        assert_eq!(f.cards()[0].id, second.id);
        drop(other);
        assert!(f.cards().iter().any(|c| c.missing));
        assert!(f.rescan(&f.cards().iter().find(|c| c.missing).unwrap().id.clone()).is_err(), "a folder that is gone cannot be read");
    }

    #[test]
    fn errors_are_plain_sentences_and_a_missing_store_is_not_a_crash() {
        let (_s, models, f) = world();
        assert!(f.add("relative", false).is_err());
        assert!(f.remove("nope").is_err());
        f.add(&models.path().to_string_lossy(), false).unwrap();
        assert!(f.add(&models.path().to_string_lossy(), false).unwrap_err().contains("already"));
        let none = DesktopFolders::with(None, None);
        assert!(none.cards().is_empty() && none.suggested().is_none());
        assert!(none.add("/x", false).unwrap_err().contains("settings file"));
    }
}
