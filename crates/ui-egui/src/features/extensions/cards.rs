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
pub const CARD_H: f32 = 336.0;
/// A card with no buttons (the built-in ones).
pub const CARD_H_SHORT: f32 = 220.0;
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

/// Like [`grid`], but card `i` is `heights[i]` tall: a row is as tall as its tallest card and the
/// shorter ones keep their own height, so a card with less to show has no empty tail.
pub fn grid_sized(ui: &mut Ui, heights: &[f32], mut draw: impl FnMut(&mut Ui, usize, Rect)) {
    let width = ui.available_width();
    let cols = columns(width);
    let card_w = (width - GAP * (cols as f32 - 1.0)) / cols as f32;
    let row_heights: Vec<f32> = heights.chunks(cols).map(|row| row.iter().copied().fold(0.0, f32::max)).collect();
    let total = row_heights.iter().sum::<f32>() + row_heights.len().saturating_sub(1) as f32 * GAP;
    let (area, _) = ui.allocate_exact_size(vec2(width, total), Sense::hover());
    let mut y = area.top();
    for (r, row_h) in row_heights.iter().enumerate() {
        for c in 0..cols {
            let i = r * cols + c;
            let Some(height) = heights.get(i) else { break };
            let min = pos2(area.left() + c as f32 * (card_w + GAP), y);
            draw(ui, i, Rect::from_min_size(min, vec2(card_w, *height)));
        }
        y += row_h + GAP;
    }
}

/// The frame and the name of a card with no picture: the details start right under the name.
pub fn top_plain(ui: &mut Ui, rect: Rect, name: &str) -> Ui {
    let t = Tokens::get(ui.ctx());
    ui.painter_at(rect.expand(1.0)).rect(rect, CornerRadius::ZERO, t.bg, t.hairline(), StrokeKind::Inside);
    let inner = Rect::from_min_max(pos2(rect.left() + CARD_PAD, rect.top() + CARD_PAD), rect.max - vec2(CARD_PAD, CARD_PAD));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    child.add(egui::Label::new(RichText::new(name).size(16.0).family(theme::semibold()).color(t.ink_strong)).truncate());
    child
}

/// The frame, the picture and the name every theme card starts with. `active` outlines the card in
/// the accent colour and puts an "In use" tag on the picture. Returns the space below the name.
pub fn top(ui: &mut Ui, rect: Rect, key: &str, name: &str, palette: &Palette, active: bool) -> Ui {
    top_with(ui, rect, key, name, active, |ui, picture| card::paint_preview(ui, picture, palette))
}

/// [`top`] with the picture painted by `paint_picture` into the rectangle it is given.
pub fn top_with(ui: &mut Ui, rect: Rect, key: &str, name: &str, active: bool, paint_picture: impl FnOnce(&mut Ui, Rect)) -> Ui {
    let t = Tokens::get(ui.ctx());
    let outline = if active { Stroke::new(2.0, t.accent) } else { t.hairline() };
    ui.painter_at(rect.expand(1.0)).rect(rect, CornerRadius::ZERO, t.bg, outline, StrokeKind::Inside);
    let inset = if active { 2.0 } else { 1.0 };
    let picture = Rect::from_min_size(rect.min + vec2(inset, inset), vec2(rect.width() - 2.0 * inset, PREVIEW_H));
    paint_picture(ui, picture);
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

const CHIP_H: f32 = 26.0;
const CHIP_GAP: f32 = 6.0;

/// Which chips go on which row. Chips fill each row in order; at most `max_rows` rows are used, and
/// when some do not fit the last row ends with a "+N" chip (`more_w` wide) standing for them. Returns
/// the rows (indices into `widths`) and how many chips are left out. A chip wider than the row still
/// gets a row to itself, so every chip is either shown or counted.
pub fn pack(widths: &[f32], avail: f32, max_rows: usize, more_w: f32) -> (Vec<Vec<usize>>, usize) {
    let max_rows = max_rows.max(1);
    let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
    let mut used = 0.0_f32;
    for (i, w) in widths.iter().enumerate() {
        let need = if used == 0.0 { *w } else { used + CHIP_GAP + w };
        if need > avail && used > 0.0 {
            rows.push(Vec::new());
            used = 0.0;
        }
        if rows.len() > max_rows {
            rows.truncate(max_rows);
            break;
        }
        if let Some(row) = rows.last_mut() {
            used = if row.is_empty() { *w } else { used + CHIP_GAP + w };
            row.push(i);
        }
    }
    let shown: usize = rows.iter().map(Vec::len).sum();
    let hidden = widths.len() - shown;
    if hidden > 0 {
        // Make room for the "+N" chip at the end of the last row.
        while let Some(row) = rows.last_mut() {
            let width: f32 = row.iter().map(|i| widths[*i]).sum::<f32>() + CHIP_GAP * row.len().saturating_sub(1) as f32;
            if row.len() <= 1 || width + CHIP_GAP + more_w <= avail {
                break;
            }
            row.pop();
        }
    }
    let shown: usize = rows.iter().map(Vec::len).sum();
    (rows, widths.len() - shown)
}

/// Where a theme is in use: one outlined chip with a dot for each app, in at most `max_rows` rows,
/// with "+N" for any that do not fit.
pub fn app_chips(ui: &mut Ui, names: &[String], max_rows: usize) {
    let t = Tokens::get(ui.ctx());
    let font = egui::FontId::proportional(13.0);
    let galleys: Vec<_> = names.iter().map(|n| ui.painter().layout_no_wrap(n.clone(), font.clone(), t.accent_ink)).collect();
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 30.0).collect();
    let painter = ui.painter().clone();
    let more = |n: usize| painter.layout_no_wrap(format!("+{n}"), font.clone(), t.accent_ink);
    let (rows, hidden) = pack(&widths, ui.available_width(), max_rows, more(names.len()).size().x + 20.0);
    let height = rows.len() as f32 * CHIP_H + rows.len().saturating_sub(1) as f32 * CHIP_GAP;
    let (area, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let chip = |ui: &mut Ui, rect: Rect, dot: bool| {
        ui.painter().rect(rect, CornerRadius::ZERO, t.accent.gamma_multiply(0.16), Stroke::new(1.0, t.accent), StrokeKind::Inside);
        if dot {
            ui.painter().circle_filled(rect.left_center() + vec2(12.0, 0.0), 3.5, t.accent);
        }
    };
    for (r, row) in rows.iter().enumerate() {
        let mut x = area.left();
        let y = area.top() + r as f32 * (CHIP_H + CHIP_GAP);
        for i in row {
            let rect = Rect::from_min_size(pos2(x, y), vec2(widths[*i], CHIP_H));
            chip(ui, rect, true);
            ui.painter().galley(pos2(rect.left() + 23.0, rect.center().y - galleys[*i].size().y / 2.0), galleys[*i].clone(), t.accent_ink);
            ui.interact(rect, ui.id().with(("app-chip", names[*i].as_str(), x as i32, y as i32)), Sense::hover())
                .widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, &names[*i]));
            x += widths[*i] + CHIP_GAP;
        }
        if hidden > 0 && r + 1 == rows.len() {
            let galley = more(hidden);
            let rect = Rect::from_min_size(pos2(x, y), vec2(galley.size().x + 20.0, CHIP_H));
            chip(ui, rect, false);
            ui.painter().galley(pos2(rect.left() + 10.0, rect.center().y - galley.size().y / 2.0), galley, t.accent_ink);
        }
    }
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

    #[test]
    fn chips_that_fit_share_a_row_and_none_is_left_out() {
        assert_eq!(pack(&[], 200.0, 2, 30.0), (vec![vec![]], 0));
        assert_eq!(pack(&[80.0], 200.0, 2, 30.0), (vec![vec![0]], 0));
        assert_eq!(pack(&[80.0, 80.0], 200.0, 2, 30.0), (vec![vec![0, 1]], 0));
        // Two chips and the gap are 166 wide; a third would make 252.
        assert_eq!(pack(&[80.0, 80.0, 80.0], 200.0, 2, 30.0), (vec![vec![0, 1], vec![2]], 0));
        // Exactly full is still a fit.
        assert_eq!(pack(&[97.0, 97.0], 200.0, 1, 30.0), (vec![vec![0, 1]], 0));
        assert_eq!(pack(&[97.0, 98.0], 200.0, 2, 30.0).0.len(), 2);
    }

    #[test]
    fn what_does_not_fit_is_counted_and_the_plus_chip_has_room() {
        // Five chips, one row of 200: two fit (166); a "+N" chip 20 wide fits beside them (192).
        assert_eq!(pack(&[80.0; 5], 200.0, 1, 20.0), (vec![vec![0, 1]], 3));
        // A wider one (40) does not, so one chip steps aside for it.
        assert_eq!(pack(&[80.0; 5], 200.0, 1, 40.0), (vec![vec![0]], 4));
        assert_eq!(pack(&[80.0; 5], 200.0, 2, 40.0), (vec![vec![0, 1], vec![2]], 2));
        // Shown plus hidden is always everything, and rows never exceed the limit.
        for n in 0..9 {
            for rows_max in 1..4 {
                let (rows, hidden) = pack(&vec![90.0; n], 200.0, rows_max, 40.0);
                assert_eq!(rows.iter().map(Vec::len).sum::<usize>() + hidden, n);
                assert!(rows.len() <= rows_max);
                assert!(rows.iter().all(|r| {
                    let w: f32 = r.len() as f32 * 90.0 + CHIP_GAP * r.len().saturating_sub(1) as f32;
                    w <= 200.0 || r.len() == 1
                }));
            }
        }
    }

    #[test]
    fn a_chip_wider_than_the_row_still_gets_its_own_row() {
        assert_eq!(pack(&[300.0, 50.0], 200.0, 3, 30.0), (vec![vec![0], vec![1]], 0));
        assert_eq!(pack(&[300.0], 200.0, 1, 30.0), (vec![vec![0]], 0));
    }
}
