//! macOS: the header is the window's title bar and the footer its status bar, each drawn by a pane with its own
//! context (`craft_window::panes`) at a fixed scale, so the launcher's zoom leaves them the size they are on screen,
//! as the traffic lights are.

use craft_window::panes::{Pane, Place};

use crate::i18n::Lang;
use crate::theme::{self, ThemeChoice};

/// A bar's pane, with what was last given to its context.
pub(crate) struct BarPane {
    pane: Pane,
    fonts: Option<Option<Lang>>,
    theme: Option<ThemeChoice>,
}

impl BarPane {
    pub(crate) fn new(window: &egui::Context, name: &str, place: Place) -> Self {
        let pane = Pane::new(window, name, place);
        egui_extras::install_image_loaders(pane.context());
        Self { pane, fonts: None, theme: None }
    }

    /// Draws `bar` with the window's fonts and theme (`look`) at `zoom`, over `rect` (window points), named `label` for
    /// screen readers, in a pane that lies from `rect`'s left edge and the top of `window` to its bottom right
    /// corner, so the bar's popups can open over the page. `bar` gets a `Ui` laid out over the bar's rect, in pane
    /// points.
    pub(crate) fn show(
        &mut self,
        window: &egui::Context,
        label: &str,
        zoom: f32,
        rect: egui::Rect,
        (choice, cjk): (ThemeChoice, Option<Lang>),
        bar: impl FnOnce(&mut egui::Ui, egui::Rect),
    ) {
        self.pane.set_label(label);
        let ctx = self.pane.context();
        if self.fonts != Some(cjk) {
            ctx.set_fonts(theme::font_definitions(cjk));
            self.fonts = Some(cjk);
        }
        if self.theme != Some(choice) {
            theme::apply(ctx, choice);
            self.theme = Some(choice);
        }
        let origin = egui::pos2(rect.left(), window.viewport_rect().top());
        // Window points → pane points.
        let to_pane = window.pixels_per_point() / (window.native_pixels_per_point().unwrap_or(1.0) * zoom);
        let strip = egui::Rect::from_min_max(egui::Pos2::ZERO + (rect.min - origin) * to_pane, egui::Pos2::ZERO + (rect.max - origin) * to_pane);
        self.pane.show(window, zoom, origin, strip, |ui| {
            ui.scope_builder(egui::UiBuilder::new().max_rect(strip), |ui| bar(ui, strip));
        });
    }
}
