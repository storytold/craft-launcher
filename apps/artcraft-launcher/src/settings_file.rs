//! The settings in eframe's own file, read and written while there is no window.
//!
//! eframe keeps its state (window size, egui memory and the launcher's settings) in `app.ron` in
//! its storage folder, but only while a window is open. Where the window closes to the tray
//! (Wayland), the launcher carries on without one, so it saves its settings to the same file
//! itself: same folder, same format, other keys left as they are.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Where eframe keeps its state for `app_id`, as it picks it for a window with that app id.
pub fn path(app_id: &str) -> Option<PathBuf> {
    eframe::storage_dir(app_id).map(|dir| dir.join("app.ron"))
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
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let tmp = path.with_extension("ron.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
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
    fn a_missing_or_broken_file_has_no_settings() {
        let dir = Scratch::new("settings-broken");
        assert_eq!(load::<Settings>(&dir.file(), "settings"), None);
        assert!(std::fs::create_dir_all(&dir.0).is_ok());
        assert!(std::fs::write(dir.file(), "not ron {").is_ok());
        assert_eq!(load::<Settings>(&dir.file(), "settings"), None);
    }
}
