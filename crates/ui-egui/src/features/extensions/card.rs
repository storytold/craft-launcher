//! A theme card's picture: a small mock window painted from the theme's own colours, so no
//! screenshots are needed and the preview can never disagree with the theme.

use craft_ext_core::palette::{Palette, Rgb};
use egui::{Color32, CornerRadius, Rect, Stroke, Ui, pos2, vec2};

pub fn color(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

/// Paints the preview into `rect`, clipped to it.
pub fn paint_preview(ui: &Ui, rect: Rect, p: &Palette) {
    let paint = ui.painter_at(rect);
    let square = CornerRadius::ZERO;
    let bar = |origin: egui::Pos2, width: f32, height: f32, c: Rgb| {
        paint.rect_filled(Rect::from_min_size(origin, vec2(width, height)), square, color(c));
    };
    paint.rect_filled(rect, square, color(p.background));

    // A side panel with three entries.
    let side = Rect::from_min_max(rect.min, pos2(rect.left() + rect.width() * 0.28, rect.bottom()));
    paint.rect_filled(side, square, color(p.surface));
    paint.vline(side.right() - 0.5, side.y_range(), Stroke::new(1.0, color(p.line)));
    for i in 0..3 {
        bar(side.min + vec2(12.0, 16.0 + 16.0 * i as f32), side.width() * 0.5, 5.0, p.muted_text);
    }

    // The page: a heading, two lines of text, a button.
    let page = rect.min + vec2(rect.width() * 0.28 + 16.0, 16.0);
    bar(page, rect.width() * 0.3, 8.0, p.text);
    bar(page + vec2(0.0, 20.0), rect.width() * 0.4, 5.0, p.muted_text);
    bar(page + vec2(0.0, 32.0), rect.width() * 0.32, 5.0, p.muted_text);
    bar(page + vec2(0.0, 52.0), 52.0, 16.0, p.accent);

    // A panel on the page, and the three status colours.
    let panel = Rect::from_min_size(page + vec2(0.0, 80.0), vec2(rect.width() * 0.4, 28.0));
    paint.rect_filled(panel, square, color(p.surface));
    paint.rect_stroke(panel, square, Stroke::new(1.0, color(p.line)), egui::StrokeKind::Inside);
    for (i, c) in [p.good, p.warn, p.bad].into_iter().enumerate() {
        bar(pos2(rect.right() - 22.0 - 14.0 * i as f32, rect.top() + 16.0), 8.0, 8.0, c);
    }
}
