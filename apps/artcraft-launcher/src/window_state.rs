//! Keep the last usable native window geometry instead of eframe's minimized snapshot.
//!
//! eframe 0.36.2 writes its own `window` entry before calling `App::save`. On Windows,
//! a minimized window may report a zero inner size or off-screen coordinates. Preserve
//! the last valid geometry and replace that entry when the launcher is saved.

use eframe::Storage;
use egui_winit::WindowSettings;

const WINDOW_KEY: &str = "window";

/// A missing or tiny inner size must not override our initial 1120 x 760 viewport.
pub fn is_valid(settings: &WindowSettings) -> bool {
    let Some(size) = settings.inner_size_points() else { return false };
    let [min_width, min_height] = crate::MIN_WINDOW_SIZE;
    size.x.is_finite() && size.y.is_finite() && size.x >= min_width && size.y >= min_height
}

#[derive(Default)]
pub struct WindowState {
    last_valid: Option<WindowSettings>,
}

impl WindowState {
    pub fn new(storage: Option<&dyn Storage>) -> Self {
        let last_valid = storage.and_then(|storage| eframe::get_value(storage, WINDOW_KEY)).filter(is_valid);
        Self { last_valid }
    }

    /// Remember actual native geometry only while the window can be safely measured.
    pub fn record(&mut self, frame: &eframe::Frame, zoom_factor: f32, visible: bool) {
        let Some(window) = frame.winit_window() else { return };
        if !visible || window.is_minimized() == Some(true) || window.is_visible() == Some(false) {
            return;
        }
        let geometry = WindowSettings::from_window(zoom_factor, window);
        if is_valid(&geometry) {
            self.last_valid = Some(geometry);
        }
    }

    /// eframe wrote its possibly minimized snapshot *before* `App::save`.
    pub fn save(&self, storage: &mut dyn Storage) {
        if let Some(geometry) = &self.last_valid {
            eframe::set_value(storage, WINDOW_KEY, geometry);
        } else {
            // On an initial minimized start with no valid frame, let the next launch
            // use the default 1120 x 760 size rather than a zero-sized snapshot.
            storage.remove_string(WINDOW_KEY);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct TestStorage(HashMap<String, String>);

    impl Storage for TestStorage {
        fn get_string(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }

        fn set_string(&mut self, key: &str, value: String) {
            self.0.insert(key.to_owned(), value);
        }

        fn remove_string(&mut self, key: &str) {
            self.0.remove(key);
        }

        fn flush(&mut self) {}
    }

    #[test]
    fn only_accepts_usable_size() {
        for (width, height, expected) in [
            (1120.0, 760.0, true),
            (720.0, 500.0, true),
            (0.0, 0.0, false),
            (64.0, 64.0, false),
            (719.0, 500.0, false),
            (720.0, 499.0, false),
            (-1.0, 760.0, false),
        ] {
            let serialized = format!("(inner_size_points:Some((x:{width:?},y:{height:?})))");
            let candidate = ron::from_str::<WindowSettings>(&serialized);
            assert_eq!(candidate.as_ref().ok().map(is_valid), Some(expected), "{serialized}");
        }
        assert!(!is_valid(&WindowSettings::default()));
    }

    #[test]
    fn a_valid_size_without_saved_coordinates_is_accepted() {
        let window = "(inner_size_points:Some((x:1120.0,y:760.0)))";
        let mut storage = TestStorage::default();
        storage.set_string(WINDOW_KEY, window.to_owned());
        let state = WindowState::new(Some(&storage));
        assert!(state.last_valid.is_some());
    }

    #[test]
    fn ignores_invalid_geometry_already_in_storage() {
        let mut storage = TestStorage::default();
        storage.set_string(WINDOW_KEY, "(inner_size_points:Some((x:0.0,y:0.0)))".to_owned());
        let state = WindowState::new(Some(&storage));
        state.save(&mut storage);
        assert_eq!(storage.get_string(WINDOW_KEY), None);
    }

    #[test]
    fn saved_good_geometry_replaces_a_minimized_snapshot() {
        let mut storage = TestStorage::default();
        let last_valid = "(inner_size_points:Some((x:1120.0,y:760.0)),outer_position_pixels:Some((x:150.0,y:80.0)))";
        storage.set_string(WINDOW_KEY, last_valid.to_owned());
        let state = WindowState::new(Some(&storage));
        // Simulate what eframe did just before calling App::save.
        storage.set_string(WINDOW_KEY, "(inner_size_points:Some((x:0.0,y:0.0)))".to_owned());
        state.save(&mut storage);
        let saved = storage.get_string(WINDOW_KEY).and_then(|s| ron::from_str::<WindowSettings>(&s).ok());
        assert_eq!(saved.and_then(|s| s.inner_size_points()), Some(egui::vec2(1120.0, 760.0)));
    }

    #[test]
    fn no_valid_geometry_removes_minimized_snapshot() {
        let mut storage = TestStorage::default();
        let state = WindowState::new(None);
        storage.set_string(WINDOW_KEY, "(inner_size_points:Some((x:0.0,y:0.0)))".to_owned());
        state.save(&mut storage);
        assert_eq!(storage.get_string(WINDOW_KEY), None);
    }
}
