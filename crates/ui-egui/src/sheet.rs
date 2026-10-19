//! The shape the launcher's large dialogs (Settings, release notes) share, so they sit
//! beside the app page as one family: a card that nearly fills the window, a header with a wash of
//! the brand colour under a 2 px rule, a body that scrolls (the bar in the brand colour), and
//! sections laid out in two columns once there is room.
//!
//! Invariants: every colour comes from [`Tokens`]; nothing here owns state; the card is never
//! larger than the window, and a window too narrow for two columns gets one.

use egui::{Color32, Context, CornerRadius, Frame, ImageSource, Margin, Ui, Vec2, vec2};

use crate::theme::{self, Tokens, tint};
use crate::widgets;

/// The size of a large dialog: the window less a margin all round, no larger than `max`.
pub(crate) fn size(ctx: &Context, max: Vec2) -> Vec2 {
    let screen = ctx.content_rect();
    vec2((screen.width() - 64.0).clamp(1.0, max.x), (screen.height() - 48.0).clamp(1.0, max.y))
}

/// The modal's frame: no inner margin, so the header can run to the edges.
pub(crate) fn frame(t: &Tokens) -> Frame {
    Frame::new().fill(t.bg).stroke(egui::Stroke::new(1.0, t.line_strong)).shadow(crate::theme::shadow(t)).inner_margin(0)
}

/// The header: an icon, the title and a small caption, with whatever `right` puts at the right
/// edge. Returns its height.
pub(crate) fn hero(ui: &mut Ui, icon: Option<ImageSource<'static>>, title: &str, caption: &str, right: impl FnOnce(&mut Ui)) -> f32 {
    let t = Tokens::get(ui.ctx());
    let top = ui.cursor().top();
    let shown =
        Frame::new().fill(tint(t.accent, if t.dark { 0.09 } else { 0.07 })).inner_margin(Margin::symmetric(theme::PAD as i8, 18)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                if let Some(icon) = icon {
                    ui.add(egui::Image::new(icon).fit_to_exact_size(vec2(44.0, 44.0)));
                    ui.add_space(6.0);
                }
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 4.0;
                    ui.label(widgets::heading(title, theme::SHEET_TITLE, t.ink_strong));
                    if !caption.is_empty() {
                        widgets::hud(ui, caption, t.faint);
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), right);
            });
        });
    let r = shown.response.rect;
    ui.painter().rect_filled(egui::Rect::from_min_size(r.min, vec2(r.width(), 2.0)), CornerRadius::ZERO, t.accent);
    ui.painter().hline(r.x_range(), r.bottom() - 0.5, t.hairline());
    ui.cursor().top() - top
}

/// A solid scroll bar in `bar` (the brand colour, or an app's on its page) on a tint of it, so it
/// stands out from the page; egui's floating one is invisible until the pointer is over it.
/// Returns the visuals to put back inside the scroll area's contents (the bar's colours are for the
/// bar alone).
pub(crate) fn accent_scroll(ui: &mut Ui, t: &Tokens, bar: Color32) -> egui::Visuals {
    ui.spacing_mut().scroll = egui::style::ScrollStyle::solid();
    ui.spacing_mut().scroll.bar_width = 10.0;
    let page = ui.visuals().clone();
    let v = ui.visuals_mut();
    v.extreme_bg_color = t.bg.blend(tint(bar, if t.dark { 0.14 } else { 0.10 }));
    v.widgets.inactive.bg_fill = bar.gamma_multiply(0.8);
    v.widgets.hovered.bg_fill = bar;
    v.widgets.active.bg_fill = bar;
    page
}

/// Two columns of equal width a `gap` apart when `wide`; otherwise `left` then `right`, stacked.
pub(crate) fn columns(ui: &mut Ui, wide: bool, gap: f32, left: impl FnOnce(&mut Ui), right: impl FnOnce(&mut Ui)) {
    if !wide {
        left(ui);
        right(ui);
        return;
    }
    let w = ((ui.available_width() - gap) / 2.0).floor();
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        ui.vertical(|ui| {
            ui.set_width(w);
            left(ui);
        });
        ui.vertical(|ui| {
            ui.set_width(w);
            right(ui);
        });
    });
}

/// A titled box: a hairline frame with a small caps heading, for one group of settings.
pub(crate) fn panel(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui)) {
    let t = Tokens::get(ui.ctx());
    Frame::new().stroke(t.hairline()).inner_margin(theme::BOX_MARGIN).show(ui, |ui| {
        ui.set_width(ui.available_width());
        widgets::hud(ui, title, t.faint);
        ui.add_space(8.0);
        body(ui);
    });
    ui.add_space(14.0);
}
