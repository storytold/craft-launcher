//! The settings in eframe's own file, read and written while there is no window.
//!
//! eframe keeps its state (window size, egui memory and the launcher's settings) in `app.ron` in
//! its storage folder, but only while a window is open. Where the window closes to the tray
//! (Wayland), the launcher carries on without one, so it saves its settings to the same file
//! itself: same folder, same format, other keys left as they are.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use eframe::Storage;

/// Where eframe keeps its state for `app_id`, as it picks it for a window with that app id.
pub fn path(app_id: &str) -> Option<PathBuf> {
    eframe::storage_dir(app_id).map(|dir| dir.join("app.ron"))
}

/// The value stored under `key`, if the file has one that still reads.
pub fn load<T: serde::de::DeserializeOwned>(path: &Path, key: &str) -> Option<T> {
    eframe::get_value(&RonFile::read(path), key)
}

/// Remove window geometry that cannot safely be restored by eframe, without touching user
/// settings or egui memory. Called before eframe opens the window on each launch.
pub fn discard_invalid_window(path: &Path) -> Result<bool, String> {
    const WINDOW_KEY: &str = "window";
    let mut file = RonFile::read(path);
    if file.get_string(WINDOW_KEY).is_none() {
        return Ok(false);
    }
    let valid = eframe::get_value::<egui_winit::WindowSettings>(&file, WINDOW_KEY).is_some_and(|settings| crate::window_state::is_valid(&settings));
    if valid {
        return Ok(false);
    }
    file.remove_string(WINDOW_KEY);
    file.write(path)?;
    Ok(true)
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

    #[test]
    fn discards_minimized_window_but_keeps_other_settings() {
        let dir = Scratch::new("invalid-window");
        let window = "(inner_size_points:Some((x:0.0,y:0.0)),outer_position_pixels:Some((x:-32000.0,y:-32000.0)))";
        let mut file = RonFile::default();
        file.set_string("window", window.to_owned());
        file.set_string("settings", "(notify:true)".to_owned());
        file.set_string("egui", "remember my layout".to_owned());
        assert_eq!(file.write(&dir.file()), Ok(()));
        assert_eq!(discard_invalid_window(&dir.file()), Ok(true));
        let restored = RonFile::read(&dir.file());
        assert_eq!(restored.get_string("window"), None);
        assert_eq!(restored.get_string("settings").as_deref(), Some("(notify:true)"));
        assert_eq!(restored.get_string("egui").as_deref(), Some("remember my layout"));
    }

    #[test]
    fn keeps_valid_window_geometry_unchanged() {
        let dir = Scratch::new("valid-window");
        let window = "(inner_size_points:Some((x:1120.0,y:760.0)),outer_position_pixels:Some((x:150.0,y:80.0)))";
        let mut file = RonFile::default();
        file.set_string("window", window.to_owned());
        assert_eq!(file.write(&dir.file()), Ok(()));
        assert_eq!(discard_invalid_window(&dir.file()), Ok(false));
        assert_eq!(RonFile::read(&dir.file()).get_string("window").as_deref(), Some(window));
    }

    #[test]
    fn no_window_state_does_not_create_storage_file() {
        let dir = Scratch::new("no-window");
        assert_eq!(discard_invalid_window(&dir.file()), Ok(false));
        assert!(!dir.0.exists());
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
