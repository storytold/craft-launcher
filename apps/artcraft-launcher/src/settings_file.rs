//! The settings in eframe's own file, read and written while there is no window.
//!
//! eframe keeps its state (window size, egui memory and the launcher's settings) in `app.ron` in
//! its storage folder, but only while a window is open. Where the window closes to the tray
//! (Wayland), the launcher carries on without one, so it saves its settings to the same file
//! itself: same folder, same format, other keys left as they are.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Where eframe kept its state before it was told otherwise: the folder it picks from the app id.
/// Only to read a file left there by an earlier version (see [`adopt`]).
pub fn legacy_path(app_id: &str) -> Option<PathBuf> {
    eframe::storage_dir(app_id).map(|dir| dir.join("app.ron"))
}

/// Move the settings an earlier version left in eframe's own folder to `path`, once. Does nothing
/// when there is nothing to move or `path` already exists, so a later run never overwrites newer
/// settings with older ones. The old file is left where it is: an older launcher started again
/// still finds it, and it is small.
///
/// Written through a temporary file in the destination folder, as every other write to this file
/// is. A copy that created `path` and then failed would leave a half-written file that the next
/// start takes for adopted settings, and the launcher would read its defaults from then on with
/// the real settings still sitting in the old folder.
pub fn adopt(path: &Path, legacy: Option<&Path>) -> bool {
    let Some(legacy) = legacy else { return false };
    if path.exists() || !legacy.is_file() || legacy == path {
        return false;
    }
    let bytes = match std::fs::read(legacy) {
        Ok(bytes) => bytes,
        Err(e) => {
            log::warn!("{}: {e}", legacy.display());
            return false;
        }
    };
    match artcraft_launcher_native::paths::write_atomic(path, &bytes) {
        Ok(()) => {
            log::info!("adopted the settings from {}", legacy.display());
            true
        }
        Err(e) => {
            log::warn!("{} -> {}: {e}", legacy.display(), path.display());
            false
        }
    }
}

/// The value stored under `key`, if the file has one that still reads.
pub fn load<T: serde::de::DeserializeOwned>(path: &Path, key: &str) -> Option<T> {
    eframe::get_value(&RonFile::read(path), key)
}

/// Stores `value` under `key`, keeping what else the file holds.
pub fn save<T: serde::Serialize>(path: &Path, key: &str, value: &T) -> Result<(), String> {
    let mut file = RonFile::read(path);
    eframe::set_value(&mut file, key, value);
    file.write(path)
}

/// eframe's file: a RON map of keys to RON strings.
#[derive(Default)]
struct RonFile(HashMap<String, String>);

impl RonFile {
    /// What the file holds; empty when it is missing or doesn't read, as eframe takes it.
    fn read(path: &Path) -> Self {
        let Ok(text) = std::fs::read_to_string(path) else { return Self::default() };
        match ron::from_str(&text) {
            Ok(kv) => Self(kv),
            Err(e) => {
                log::warn!("{}: {e}", path.display());
                Self::default()
            }
        }
    }

    /// Writes a new file and moves it into place, so a failed write leaves the old one whole.
    fn write(&self, path: &Path) -> Result<(), String> {
        let text = ron::ser::to_string_pretty(&self.0, ron::ser::PrettyConfig::default()).map_err(|e| e.to_string())?;
        artcraft_launcher_native::paths::write_atomic(path, text.as_bytes())
    }
}

impl eframe::Storage for RonFile {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }

    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_string(), value);
    }

    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }

    fn flush(&mut self) {}
}

#[cfg(test)]
mod tests {
    use artcraft_launcher_ui_egui::Settings;

    use super::*;

    /// A file in a folder of its own, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("artcraft-launcher-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }
        fn file(&self) -> PathBuf {
            self.0.join("app.ron")
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn settings_saved_with_no_file_read_back() {
        let dir = Scratch::new("settings-round-trip");
        let settings = Settings { last_check: Some(1_791_500_000), notify: false, ..Settings::default() };
        assert_eq!(save(&dir.file(), "settings", &settings), Ok(()));
        assert_eq!(load::<Settings>(&dir.file(), "settings"), Some(settings));
    }

    #[test]
    fn saving_keeps_what_eframe_stored() {
        let dir = Scratch::new("settings-keeps-window");
        assert!(std::fs::create_dir_all(&dir.0).is_ok());
        // As eframe writes it: a map of keys to RON strings.
        let window = r#"(inner_position_pixels:Some((x:10.0,y:20.0)),fullscreen:false)"#;
        let eframe_file = format!("{{\n    \"window\": {window:?},\n    \"settings\": \"(notify:true)\",\n}}");
        assert!(std::fs::write(dir.file(), eframe_file).is_ok());
        assert_eq!(load::<Settings>(&dir.file(), "settings").map(|s| s.notify), Some(true));

        let settings = Settings { notify: false, ..Settings::default() };
        assert_eq!(save(&dir.file(), "settings", &settings), Ok(()));
        assert_eq!(load::<Settings>(&dir.file(), "settings"), Some(settings));
        assert_eq!(RonFile::read(&dir.file()).0.get("window").map(String::as_str), Some(window));
    }

    #[test]
    fn saving_settings_leaves_an_unrelated_temporary_file_alone() {
        let dir = Scratch::new("settings-temp-collision");
        std::fs::create_dir_all(&dir.0).unwrap();
        let tmp = dir.file().with_extension("ron.tmp");
        std::fs::write(&tmp, b"another writer's data").unwrap();
        let settings = Settings { notify: false, ..Settings::default() };
        save(&dir.file(), "settings", &settings).unwrap();
        assert_eq!(load::<Settings>(&dir.file(), "settings"), Some(settings));
        assert_eq!(std::fs::read(&tmp).unwrap(), b"another writer's data");
    }

    /// Settings an earlier version left in eframe's own folder are taken over once, and never
    /// over newer ones (#775).
    #[test]
    fn settings_left_in_eframes_folder_are_adopted_once() {
        let old = Scratch::new("settings-adopt-old");
        let new = Scratch::new("settings-adopt-new");
        let was = Settings { notify: false, ..Settings::default() };
        std::fs::create_dir_all(&old.0).unwrap();
        save(&old.file(), "settings", &was).unwrap();

        // Nothing where the launcher now keeps them: the old file is taken over.
        assert!(adopt(&new.file(), Some(&old.file())));
        assert_eq!(load::<Settings>(&new.file(), "settings"), Some(was.clone()));
        // The old file stays, so an older launcher started again still finds its settings.
        assert!(old.file().is_file());

        // Already there: left alone, however old the other file is.
        let now = Settings { notify: true, ..Settings::default() };
        save(&new.file(), "settings", &now).unwrap();
        assert!(!adopt(&new.file(), Some(&old.file())));
        assert_eq!(load::<Settings>(&new.file(), "settings"), Some(now));

        // A destination that cannot be written leaves nothing behind for the next start to take
        // for adopted settings: here the folder's place is taken by a file. (This holds however
        // the bytes are written -- the partial-copy case it guards against needs a write that
        // fails after creating the file, which no portable test can provoke.)
        let blocked = Scratch::new("settings-adopt-blocked");
        std::fs::create_dir_all(&blocked.0).unwrap();
        let wall = blocked.0.join("wall");
        std::fs::write(&wall, b"not a folder").unwrap();
        let under = wall.join("app.ron");
        assert!(!adopt(&under, Some(&old.file())));
        assert!(!under.exists(), "no half-written settings file");
        assert_eq!(std::fs::read(&wall).unwrap(), b"not a folder", "and nothing else was replaced");

        // Nothing to take over, and the same file twice, are both no-ops rather than errors.
        let empty = Scratch::new("settings-adopt-empty");
        assert!(!adopt(&empty.file(), Some(&Scratch::new("settings-adopt-absent").file())));
        assert!(!adopt(&empty.file(), None));
        assert!(!adopt(&old.file(), Some(&old.file())));
    }

    /// The settings sit with the launcher's other files, so a portable install keeps them beside
    /// its executable rather than in the user's profile (#775).
    #[test]
    fn the_settings_file_is_in_the_launchers_own_folder() {
        let root = std::env::temp_dir().join(format!("artcraft-launcher-portable-{}", std::process::id()));
        let paths = artcraft_launcher_native::paths::Paths::under(&root);
        assert_eq!(paths.settings(), paths.data.join("app.ron"));
        assert!(paths.settings().starts_with(&root), "{:?}", paths.settings());
        assert!(paths.settings().starts_with(paths.logs().parent().unwrap()), "beside the logs");
    }

    #[test]
    fn a_missing_or_broken_file_has_no_settings() {
        let dir = Scratch::new("settings-broken");
        assert_eq!(load::<Settings>(&dir.file(), "settings"), None);
        assert!(std::fs::create_dir_all(&dir.0).is_ok());
        assert!(std::fs::write(dir.file(), "not ron {").is_ok());
        assert_eq!(load::<Settings>(&dir.file(), "settings"), None);
    }
}
