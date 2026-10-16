//! The theme cards: the grid they sit in, the frame and picture each starts with, the built-in card,
//! and the marks that show a theme is in use and where.
//!
//! Invariants: nothing here changes the store; a theme in use is marked three ways that do not rely
//! on colour alone (an "In use" tag on the picture, a heavier outline, and the names of the apps in
//! chips), and every sentence goes through `tl!`.

use craft_ext_core::palette::Palette;
use egui::{Align, CornerRadius, Layout, Rect, RichText, Sense, Stroke, StrokeKind, Ui, pos2, vec2};

use super::builtin::BuiltIn;
use super::card;
use crate::i18n::fmt;
use crate::theme::{self, Tokens};
use crate::widgets;

pub const CARD_MIN_W: f32 = 300.0;
/// A card with buttons under its details.
pub const CARD_H: f32 = 304.0;
/// A card with no buttons (the built-in ones).
pub const CARD_H_SHORT: f32 = 216.0;
const PREVIEW_H: f32 = 128.0;
const CARD_PAD: f32 = 20.0;
/// Space between neighbouring cards, across and down.
pub const GAP: f32 = 16.0;

/// How many cards of at least [`CARD_MIN_W`] fit in `width` with a gap between them.
pub fn columns(width: f32) -> usize {
    (((width + GAP) / (CARD_MIN_W + GAP)).floor() as usize).max(1)
}

/// `count` cards of `height` in rows that fill the width, with a gap between them; `draw` paints
/// card `i` into its rectangle.
pub fn grid(ui: &mut Ui, count: usize, height: f32, mut draw: impl FnMut(&mut Ui, usize, Rect)) {
    let width = ui.available_width();
    let cols = columns(width);
    let card_w = (width - GAP * (cols as f32 - 1.0)) / cols as f32;
    let rows = count.div_ceil(cols);
    let total = rows as f32 * height + rows.saturating_sub(1) as f32 * GAP;
    let (area, _) = ui.allocate_exact_size(vec2(width, total), Sense::hover());
    for i in 0..count {
        let (r, c) = (i / cols, i % cols);
        let min = area.min + vec2(c as f32 * (card_w + GAP), r as f32 * (height + GAP));
        draw(ui, i, Rect::from_min_size(min, vec2(card_w, height)));
    }
}

/// The frame, the picture and the name every theme card starts with. `active` outlines the card in
/// the accent colour and puts an "In use" tag on the picture. Returns the space below the name.
pub fn top(ui: &mut Ui, rect: Rect, key: &str, name: &str, palette: &Palette, active: bool) -> Ui {
    let t = Tokens::get(ui.ctx());
    let outline = if active { Stroke::new(2.0, t.accent) } else { t.hairline() };
    ui.painter_at(rect.expand(1.0)).rect(rect, CornerRadius::ZERO, t.bg, outline, StrokeKind::Inside);
    let inset = if active { 2.0 } else { 1.0 };
    let picture = Rect::from_min_size(rect.min + vec2(inset, inset), vec2(rect.width() - 2.0 * inset, PREVIEW_H));
    card::paint_preview(ui, picture, palette);
    ui.painter().hline(picture.x_range(), picture.bottom(), if active { Stroke::new(1.0, t.accent) } else { t.hairline() });
    if active {
        in_use_tag(ui, picture);
    }
    ui.interact(picture, ui.id().with(("theme-preview", key)), Sense::hover())
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Image, true, fmt(tl!("Preview of {name}"), &[("name", name)])));

    let inner = Rect::from_min_max(pos2(rect.left() + CARD_PAD, picture.bottom() + 16.0), rect.max - vec2(CARD_PAD, CARD_PAD));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child.add(egui::Label::new(RichText::new(name).size(16.0).family(theme::semibold()).color(t.ink_strong)).truncate());
    child
}

/// A solid accent tag in the corner of the picture, readable on any theme's colours.
fn in_use_tag(ui: &mut Ui, picture: Rect) {
    let t = Tokens::get(ui.ctx());
    let text = tl!("In use").to_uppercase();
    let galley = ui.painter().layout_no_wrap(text.clone(), egui::FontId::new(10.5, theme::mono()), INK_ON_ACCENT);
    let rect = Rect::from_min_size(picture.min + vec2(10.0, 10.0), vec2(galley.size().x + 16.0, 22.0));
    ui.painter().rect_filled(rect, CornerRadius::ZERO, t.accent);
    ui.painter().galley(pos2(rect.left() + 8.0, rect.center().y - galley.size().y / 2.0), galley, INK_ON_ACCENT);
    ui.interact(rect, ui.id().with(("in-use-tag", picture.min.x as i32, picture.min.y as i32)), Sense::hover())
        .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, tl!("In use")));
}

/// Near black: reads on the launcher's blue in both appearances.
const INK_ON_ACCENT: egui::Color32 = egui::Color32::from_rgb(0x0b, 0x0b, 0x0c);

/// Where a theme is in use: one outlined chip with a dot for each app.
pub fn app_chips(ui: &mut Ui, names: &[String]) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(6.0, 6.0);
        for name in names {
            let galley = ui.painter().layout_no_wrap(name.clone(), egui::FontId::proportional(12.5), t.accent_ink);
            let (rect, resp) = ui.allocate_exact_size(vec2(galley.size().x + 28.0, 24.0), Sense::hover());
            ui.painter().rect(rect, CornerRadius::ZERO, t.accent.gamma_multiply(0.16), Stroke::new(1.0, t.accent), StrokeKind::Inside);
            ui.painter().circle_filled(rect.left_center() + vec2(11.0, 0.0), 3.5, t.accent);
            ui.painter().galley(pos2(rect.left() + 21.0, rect.center().y - galley.size().y / 2.0), galley, t.accent_ink);
            resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, name));
        }
    });
}

/// A theme an app already carries: shown, never changed from here.
pub fn builtin(ui: &mut Ui, b: &BuiltIn, rect: Rect) {
    let t = Tokens::get(ui.ctx());
    let mut card = top(ui, rect, &format!("{}-{}", b.app_name, b.name), b.name, &b.palette, false);
    let ui = &mut card;
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        widgets::badge(ui, if b.palette.dark { tl!("Dark") } else { tl!("Light") }, t.muted);
        ui.add(egui::Label::new(RichText::new(fmt(tl!("Built into {app}"), &[("app", b.app_name)])).size(12.0).color(t.muted)).truncate());
    });
    ui.add_space(8.0);
    ui.add(egui::Label::new(RichText::new(fmt(tl!("Choose it inside {app}"), &[("app", b.app_name)])).size(12.5).color(t.faint)).truncate());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn columns_count_the_gaps_and_never_fall_below_one() {
        assert_eq!(columns(0.0), 1);
        assert_eq!(columns(CARD_MIN_W - 1.0), 1);
        assert_eq!(columns(CARD_MIN_W), 1);
        // Two cards need two widths and one gap.
        assert_eq!(columns(2.0 * CARD_MIN_W + GAP - 0.5), 1);
        assert_eq!(columns(2.0 * CARD_MIN_W + GAP), 2);
        assert_eq!(columns(3.0 * CARD_MIN_W + 2.0 * GAP), 3);
        assert_eq!(columns(3.0 * CARD_MIN_W + 2.0 * GAP - 0.5), 2);
    }
}
