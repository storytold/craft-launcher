//! The band at the top of the History page: four figures on the left and, filling the rest of the
//! width, a chart of jobs per day, so the space says something about the rhythm of the work as well
//! as the totals.

use craft_ai_model::HistoryEntry;
use craft_ai_model::history::report::{Totals, daily, totals};
use egui::{Align, Align2, Color32, FontId, Layout, Rect, Sense, Ui, UiBuilder, pos2, vec2};

use super::history::Clock;
use super::history_table::{costs_of, place};
use crate::theme::{self, Tokens};
use crate::widgets;

const BAND_H: f32 = 140.0;
const STATS_W: f32 = 300.0;
const GAP: f32 = 12.0;
/// The most days the chart shows; older days are left off the left edge.
const CHART_DAYS: usize = 31;

pub(super) fn overview(ui: &mut Ui, shown: &[&HistoryEntry], clock: Clock) {
    let t = Tokens::get(ui.ctx());
    let (band, _) = ui.allocate_exact_size(vec2(ui.available_width(), BAND_H), Sense::hover());
    let stats_w = STATS_W.min(band.width() * 0.45);
    let stats_rect = Rect::from_min_size(band.min, vec2(stats_w, band.height()));
    let chart_rect = Rect::from_min_max(pos2(stats_rect.right() + GAP, band.top()), band.max);
    stats(ui, stats_rect, shown, &t);
    chart(ui, chart_rect, shown, clock, &t);
}

/// Jobs, done, failed and reported cost in a two by two grid of hairline cells.
fn stats(ui: &mut Ui, rect: Rect, shown: &[&HistoryEntry], t: &Tokens) {
    let all = totals(shown.iter().copied());
    let cells: [(&str, String, Color32); 4] = [
        (tl!("Jobs"), all.jobs.to_string(), t.ink_strong),
        (tl!("Done"), all.done.to_string(), t.ink_strong),
        (tl!("Failed"), all.failed.to_string(), if all.failed > 0 { t.bad } else { t.ink_strong }),
        (tl!("Reported cost"), costs_of(&all), t.ink_strong),
    ];
    let (cw, ch) = (rect.width() / 2.0, rect.height() / 2.0);
    for (i, (label, value, colour)) in cells.iter().enumerate() {
        let origin = pos2(rect.left() + (i % 2) as f32 * cw, rect.top() + (i / 2) as f32 * ch);
        let cell = Rect::from_min_size(origin, vec2(cw, ch));
        place(ui, UiBuilder::new().max_rect(cell.shrink2(vec2(14.0, 10.0))).layout(Layout::top_down(Align::Min)), |ui| {
            ui.spacing_mut().item_spacing.y = 3.0;
            widgets::hud(ui, label, t.faint);
            ui.add(egui::Label::new(widgets::heading(value, 22.0, *colour)).truncate());
        });
    }
    let p = ui.painter();
    p.vline(rect.center().x, rect.y_range(), t.hairline());
    p.hline(rect.x_range(), rect.center().y, t.hairline());
    p.rect_stroke(rect, egui::CornerRadius::ZERO, t.hairline(), egui::StrokeKind::Inside);
}

/// What one day's bar says when hovered, and what a screen reader reads.
fn bar_text(day: impl std::fmt::Display, totals: &Totals) -> String {
    let mut text = format!("{day}\n{} {}, {} {}", tl!("Done"), totals.done, tl!("Failed"), totals.failed);
    if !totals.cost_micros.is_empty() {
        text.push('\n');
        text.push_str(&costs_of(totals));
    }
    text
}

/// Bars of jobs per day: done at the bottom, then unfinished or cancelled, then failed on top.
fn chart(ui: &mut Ui, rect: Rect, shown: &[&HistoryEntry], clock: Clock, t: &Tokens) {
    ui.painter().rect_stroke(rect, egui::CornerRadius::ZERO, t.hairline(), egui::StrokeKind::Inside);
    let inner = rect.shrink2(vec2(14.0, 10.0));
    place(
        ui,
        UiBuilder::new().max_rect(Rect::from_min_size(inner.min, vec2(inner.width(), 16.0))).layout(Layout::left_to_right(Align::Center)),
        |ui| {
            widgets::hud(ui, tl!("Jobs per day"), t.faint);
        },
    );
    let series = daily(shown, clock.offset_minutes, CHART_DAYS);
    if series.is_empty() {
        return;
    }
    let busiest = series.iter().map(|(_, d)| d.jobs).max().unwrap_or(1).max(1);
    let small = FontId::new(11.0, theme::mono());
    ui.painter().text(pos2(inner.right(), inner.top()), Align2::RIGHT_TOP, busiest.to_string(), small.clone(), t.faint);

    let plot = Rect::from_min_max(pos2(inner.left(), inner.top() + 26.0), pos2(inner.right(), inner.bottom() - 18.0));
    ui.painter().hline(plot.x_range(), plot.bottom() + 0.5, (1.0, t.line_strong));
    let slot = plot.width() / series.len() as f32;
    let bar_w = (slot * 0.68).clamp(2.0, 22.0);
    let unit = plot.height() / busiest as f32;
    for (i, (day, d)) in series.iter().enumerate() {
        let slot_rect = Rect::from_min_max(pos2(plot.left() + slot * i as f32, plot.top()), pos2(plot.left() + slot * (i + 1) as f32, plot.bottom()));
        let text = bar_text(day, d);
        let resp = ui.interact(slot_rect, ui.id().with(("history-bar", i)), Sense::hover());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text.replace('\n', ", ")));
        if resp.hovered() {
            ui.painter().rect_filled(slot_rect.expand2(vec2(0.0, 4.0)), egui::CornerRadius::ZERO, t.sunken);
        }
        let (left, right) = (slot_rect.center().x - bar_w / 2.0, slot_rect.center().x + bar_w / 2.0);
        let mut bottom = plot.bottom();
        let other = d.cancelled + d.unfinished;
        for (count, colour) in [(d.done, t.accent_ink), (other, t.faint), (d.failed, t.bad)] {
            if count == 0 {
                continue;
            }
            let top = bottom - count as f32 * unit;
            ui.painter().rect_filled(Rect::from_min_max(pos2(left, top), pos2(right, bottom)), egui::CornerRadius::ZERO, colour);
            bottom = top;
        }
        if d.jobs == 0 {
            ui.painter().rect_filled(
                Rect::from_min_max(pos2(left, plot.bottom() - 1.0), pos2(right, plot.bottom())),
                egui::CornerRadius::ZERO,
                t.line,
            );
        }
        resp.on_hover_text(text);
    }
    if let (Some((first, _)), Some((last, _))) = (series.first(), series.last()) {
        ui.painter().text(pos2(inner.left(), inner.bottom()), Align2::LEFT_BOTTOM, first.to_string(), small.clone(), t.faint);
        if series.len() > 1 {
            ui.painter().text(pos2(inner.right(), inner.bottom()), Align2::RIGHT_BOTTOM, last.to_string(), small, t.faint);
        }
    }
}
