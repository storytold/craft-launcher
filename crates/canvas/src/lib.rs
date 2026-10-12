//! A view belongs to a document, not an editor or an OS window.
//! Native scene/layer data and renderer resources remain in their owning adapters.
//! Cloning a view shares its camera; constructing another view of the same document
//! gives a future detached window an independent camera. This is a 2D adapter,
//! not a 3D renderer or a serialization format.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::sync::{Arc, Mutex, PoisonError};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera2d {
    pub center: [f64; 2],
    /// Physical display pixels per document unit; independent of UI scale.
    pub zoom: f64,
    pub rotation_degrees: f64,
}

impl Camera2d {
    pub fn valid(self) -> bool {
        self.center.iter().all(|v| v.is_finite() && v.abs() <= 1e12)
            && self.zoom.is_finite()
            && (1e-6..=1e6).contains(&self.zoom)
            && self.rotation_degrees.is_finite()
            && self.rotation_degrees.abs() <= 1e6
    }
}

#[derive(Clone, Debug)]
pub struct SharedView {
    pub document: u64,
    pub view: u64,
    state: Arc<Mutex<State>>,
}

#[derive(Debug, Default)]
struct State {
    camera: Option<Camera2d>,
    epoch: u64,
}

/// A revocable camera writer. A handoff invalidates old editor bindings, so a
/// late frame cannot replace the new editor's view state.
#[derive(Clone, Debug)]
pub struct Binding {
    view: SharedView,
    epoch: u64,
    initialized: bool,
    /// Adapter-local native document identity; prevents applying this camera to
    /// a different tab opened from a native editor's File menu.
    pub native_document: u64,
}

impl SharedView {
    pub fn new(document: u64, view: u64) -> Self {
        Self { document, view, state: Arc::new(Mutex::new(State::default())) }
    }

    pub fn bind(&self, native_document: u64) -> Binding {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.epoch = state.epoch.wrapping_add(1);
        Binding { view: self.clone(), epoch: state.epoch, initialized: false, native_document }
    }

    pub fn camera(&self) -> Option<Camera2d> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).camera
    }
}

impl Binding {
    /// Adopt the preceding editor's camera once. Later native menu and tool
    /// changes must not be overwritten before their frame is published.
    pub fn initial_camera(&mut self, native_document: u64) -> Option<Camera2d> {
        if self.initialized || native_document != self.native_document {
            return None;
        }
        self.initialized = true;
        self.camera(native_document)
    }

    pub fn camera(&self, native_document: u64) -> Option<Camera2d> {
        let state = self.view.state.lock().unwrap_or_else(PoisonError::into_inner);
        (native_document == self.native_document && state.epoch == self.epoch).then_some(state.camera).flatten()
    }

    /// Invalid values, another native tab, and stale editors cannot mutate the view.
    pub fn publish(&self, native_document: u64, camera: Camera2d) -> bool {
        if native_document != self.native_document || !camera.valid() {
            return false;
        }
        let mut state = self.view.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.epoch != self.epoch {
            return false;
        }
        state.camera = Some(camera);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const CAMERA: Camera2d = Camera2d { center: [320.0, 200.0], zoom: 1.5, rotation_degrees: 30.0 };

    #[test]
    fn handoff_preserves_camera_and_revokes_old_writer() {
        let view = SharedView::new(1, 1);
        let photo = view.bind(100);
        assert!(photo.publish(100, CAMERA));
        let vector = view.bind(200);
        assert_eq!(vector.camera(200), Some(CAMERA));
        assert!(!photo.publish(100, Camera2d { zoom: 2.0, ..CAMERA }));
        assert_eq!(vector.camera(200), Some(CAMERA));
        assert_eq!(vector.camera(201), None);
        assert!(!vector.publish(201, CAMERA));
    }

    #[test]
    fn views_of_same_document_have_independent_cameras() {
        let a = SharedView::new(1, 1);
        let b = SharedView::new(1, 2);
        assert!(a.bind(100).publish(100, CAMERA));
        assert_eq!(b.camera(), None);
        assert_eq!(a.clone().camera(), Some(CAMERA));
    }

    #[test]
    fn handoff_is_adopted_once_without_overwriting_native_zoom_commands() {
        let view = SharedView::new(1, 1);
        assert!(view.bind(100).publish(100, CAMERA));
        let mut next = view.bind(200);
        assert_eq!(next.initial_camera(999), None);
        assert_eq!(next.initial_camera(200), Some(CAMERA));
        assert_eq!(next.initial_camera(200), None);
        assert!(next.publish(200, Camera2d { zoom: 3.0, ..CAMERA }));
        assert_eq!(view.camera().unwrap().zoom, 3.0);
    }

    #[test]
    fn invalid_camera_keeps_last_good_view() {
        let view = SharedView::new(1, 1);
        let binding = view.bind(100);
        assert!(binding.publish(100, CAMERA));
        for zoom in [f64::NAN, f64::INFINITY, 0.0, -1.0, 1e100] {
            assert!(!binding.publish(100, Camera2d { zoom, ..CAMERA }));
        }
        assert!(!binding.publish(100, Camera2d { center: [f64::NAN, 0.0], ..CAMERA }));
        assert_eq!(view.camera(), Some(CAMERA));
    }
}
