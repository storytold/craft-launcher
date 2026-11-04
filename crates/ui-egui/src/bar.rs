//! Controls for the header and the toolbar row: a dropdown that picks one option, the grid/table
//! switch drawn as two icons, and the refresh button.
//!
//! Invariants: every colour comes from [`Tokens`]; the icons are drawn with the painter, so no
//! font or image has to carry them; nothing here owns state, the caller passes the value in.

use egui::{CornerRadius, Rect, RichText, Sense, Ui, vec2};

use crate::settings::ViewMode;
use crate::theme::{self, Tokens};
use crate::widgets::Cell;

/// A dropdown that picks one of `options`, each with an optional count after its label (which
/// takes the cell's highlight colour when it is not zero). Returns whether the choice changed.
pub fn dropdown<T: PartialEq + Copy>(ui: &mut Ui, id: &str, value: &mut T, options: &[(T, Cell<'_>)]) -> bool {
    let t = Tokens::get(ui.ctx());
    let mut changed = false;
    let text = |cell: &Cell<'_>| match cell.count {
        Some((n, _)) => format!("{} {n}", cell.label),
        None => cell.label.to_string(),
    };
    let selected = options.iter().find(|(o, _)| o == value).map(|(_, c)| text(c)).unwrap_or_default();
    egui::ComboBox::from_id_salt(id).selected_text(RichText::new(selected).size(13.0)).width(170.0).show_ui(ui, |ui| {
        for (opt, cell) in options {
            let color = cell.count.and_then(|(n, highlight)| highlight.filter(|_| n > 0)).unwrap_or(t.ink);
            let picked = ui.selectable_label(*value == *opt, RichText::new(text(cell)).color(color)).on_hover_text(cell.tip);
            if picked.clicked() && *value != *opt {
                *value = *opt;
                changed = true;
            }
        }
    });
    changed
}

/// Grid and table as two square icons in one hairline box; the chosen one is inverted. In a
/// right-to-left parent they still read grid, then table.
pub fn view_toggle(ui: &mut Ui, view: &mut ViewMode, tips: [&str; 2]) {
    let t = Tokens::get(ui.ctx());
    let rtl = ui.layout().prefer_right_to_left();
    let mut modes = [(ViewMode::Grid, tips[0]), (ViewMode::Table, tips[1])];
    if rtl {
        modes.reverse();
    }
    let mut rects = Vec::with_capacity(2);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        for (mode, tip) in modes {
            let on = *view == mode;
            let (rect, resp) = ui.allocate_exact_size(vec2(theme::CONTROL_H, theme::CONTROL_H), Sense::click());
            let (fill, ink) = match (on, resp.hovered()) {
                (true, _) => (t.invert_bg, t.invert_fg),
                (false, true) => (t.sunken, t.ink_strong),
                (false, false) => (egui::Color32::TRANSPARENT, t.muted),
            };
            let p = ui.painter();
            p.rect_filled(rect, CornerRadius::ZERO, fill);
            let c = rect.center();
            match mode {
                ViewMode::Grid => {
                    for (dx, dy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
                        p.rect_filled(Rect::from_center_size(c + vec2(dx * 3.75, dy * 3.75), vec2(6.0, 6.0)), CornerRadius::ZERO, ink);
                    }
                }
                ViewMode::Table => {
                    for dy in [-5.0, 0.0, 5.0] {
                        p.rect_filled(Rect::from_center_size(c + vec2(0.0, dy), vec2(15.0, 2.5)), CornerRadius::ZERO, ink);
                    }
                }
            }
            resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, tip));
            if resp.on_hover_text(tip).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                *view = mode;
            }
            rects.push(rect);
        }
    });
    if let (Some(first), Some(last)) = (rects.first(), rects.last()) {
        let p = ui.painter();
        p.rect_stroke(first.union(*last), CornerRadius::ZERO, t.hairline(), egui::StrokeKind::Inside);
        for r in rects.iter().skip(1) {
            let x = if rtl { r.right() } else { r.left() };
            p.vline(x, first.top()..=first.bottom(), t.hairline());
        }
    }
}

/// The refresh control: a square in the same hairline box as the grid/table switch, and lit the
/// same way on hover (sunken; the switch keeps inverted for the chosen view). While `busy` it shows
/// a spinner and ignores clicks.
pub fn refresh_button(ui: &mut Ui, tip: &str, busy: bool) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(theme::CONTROL_H, theme::CONTROL_H), if busy { Sense::hover() } else { Sense::click() });
    let hovered = resp.hovered() && !busy;
    let p = ui.painter();
    if hovered {
        p.rect_filled(rect, CornerRadius::ZERO, t.sunken);
    }
    p.rect_stroke(rect, CornerRadius::ZERO, t.hairline(), egui::StrokeKind::Inside);
    if busy {
        egui::Spinner::new().size(16.0).color(t.muted).paint_at(ui, Rect::from_center_size(rect.center(), vec2(16.0, 16.0)));
    } else {
        p.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "\u{27f3}",
            egui::FontId::proportional(17.0),
            if hovered { t.ink_strong } else { t.muted },
        );
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, !busy, tip));
    let resp = resp.on_hover_text(tip);
    if busy { resp } else { resp.on_hover_cursor(egui::CursorIcon::PointingHand) }
}
