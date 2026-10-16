//! Drawing the History page: the stat strip, the jobs table and the per-day and per-month tables.
//!
//! The tables are laid out by hand, one hairline row at a time, so the columns line up and the
//! jobs table can draw only the rows in view (a busy log runs to thousands of jobs).

use craft_ai_model::history::report::{Totals, amount_text, by_app, by_day, by_model, by_month, time_text};
use craft_ai_model::{ErrorKind, HistoryEntry, Locality, Outcome};
use egui::{Align, Color32, Layout, Rect, RichText, Sense, Ui, UiBuilder, vec2};

use super::history::Clock;
use crate::i18n::fmt;
use crate::theme::{self, Tokens};
use crate::widgets;

const ROW_H: f32 = 54.0;
const GROUP_ROW_H: f32 = 42.0;
const HEAD_H: f32 = 30.0;
const VISIBLE_ROWS: f32 = 6.0;
const PAD: f32 = 10.0;

/// Draws `add` inside `builder`'s rect without moving the parent's cursor. (`scope_builder` moves
/// it to the bottom of whatever was drawn, which pulls everything after it back up into the cell.)
pub(super) fn place(ui: &mut Ui, builder: UiBuilder, add: impl FnOnce(&mut Ui)) {
    let mut child = ui.new_child(builder);
    add(&mut child);
}

/// Runs `add` with no vertical gap between items, so a table's rows touch and each is exactly its
/// row height tall (`show_rows` adds the item spacing to every row).
fn rows_flush(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        add(ui);
    });
}

/// The mono face for numbers, so digits line up down a column.
fn mono(text: impl Into<String>, size: f32, color: Color32) -> RichText {
    RichText::new(text.into()).family(theme::mono()).size(size).color(color)
}

fn cost_text(micros: u64, currency: &str) -> String {
    format!("{} {currency}", amount_text(micros))
}

/// "2.5 s", or whole seconds once past a minute and a half.
fn seconds(ms: u64) -> String {
    if ms < 90_000 { format!("{}.{} s", ms / 1000, ms % 1000 / 100) } else { format!("{} s", ms / 1000) }
}

fn failure_reason(kind: ErrorKind) -> &'static str {
    match kind {
        ErrorKind::Quota => tl!("Out of credit"),
        ErrorKind::RateLimited => tl!("Rate limited"),
        ErrorKind::Auth => tl!("Sign-in problem"),
        ErrorKind::Network => tl!("Network problem"),
        ErrorKind::Provider => tl!("Provider error"),
        _ => tl!("Error"),
    }
}

/// The reported cost per currency, joined, or "-" when nothing was reported.
pub(super) fn costs_of(t: &Totals) -> String {
    if t.cost_micros.is_empty() {
        return "-".to_owned();
    }
    t.cost_micros.iter().map(|(c, m)| cost_text(*m, c)).collect::<Vec<_>>().join(", ")
}

/// Column widths for the jobs table; "what" takes the rest.
struct Cols {
    when: f32,
    what: f32,
    app: f32,
    result: f32,
    took: f32,
    cost: f32,
}

impl Cols {
    fn new(total: f32) -> Self {
        let (when, app, result, took, cost) = (142.0, 92.0, 120.0, 78.0, 100.0);
        Self { when, what: (total - when - app - result - took - cost).max(120.0), app, result, took, cost }
    }

    /// Each column's rect within a row, padded.
    fn rects(&self, row: Rect) -> [Rect; 6] {
        let widths = [self.when, self.what, self.app, self.result, self.took, self.cost];
        let mut x = row.left();
        widths.map(|w| {
            let r = Rect::from_min_size(egui::pos2(x + PAD, row.top()), vec2((w - 2.0 * PAD).max(0.0), row.height()));
            x += w;
            r
        })
    }
}

fn header(ui: &mut Ui, cols: &Cols, labels: [&str; 6]) {
    let t = Tokens::get(ui.ctx());
    let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), HEAD_H), Sense::hover());
    for (rect, label) in cols.rects(row).into_iter().zip(labels) {
        place(ui, UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)), |ui| {
            widgets::hud(ui, label, t.faint);
        });
    }
    ui.painter().hline(row.x_range(), row.bottom() - 0.5, (1.0, t.line_strong));
}

/// Every job, newest first, drawing only the rows in view.
pub(super) fn jobs(ui: &mut Ui, shown: &[&HistoryEntry], clock: Clock) {
    let t = Tokens::get(ui.ctx());
    let cols = Cols::new(ui.available_width());
    header(ui, &cols, [tl!("When"), tl!("What"), tl!("App"), tl!("Result"), tl!("Took"), tl!("Cost")]);
    rows_flush(ui, |ui| {
        egui::ScrollArea::vertical().id_salt("history-jobs").max_height(ROW_H * VISIBLE_ROWS).auto_shrink([false, true]).show_rows(
            ui,
            ROW_H,
            shown.len(),
            |ui, range| {
                for entry in shown.iter().skip(range.start).take(range.len()) {
                    job_row(ui, &cols, entry, clock, &t);
                }
            },
        );
    });
}

fn job_row(ui: &mut Ui, cols: &Cols, entry: &HistoryEntry, clock: Clock, t: &Tokens) {
    let s = &entry.start;
    let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::hover());
    if resp.hovered() {
        ui.painter().rect_filled(row, egui::CornerRadius::ZERO, t.sunken);
    }
    ui.painter().hline(row.x_range(), row.bottom() - 0.5, (1.0, t.line));
    let when = time_text(s.at_ms, clock.offset_minutes);
    let outcome = entry.outcome();
    resp.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("{when}, {}, {}, {}", s.app, s.role_label, outcome_label(outcome)))
    });
    let [c_when, c_what, c_app, c_result, c_took, c_cost] = cols.rects(row);
    let left = Layout::left_to_right(Align::Center);
    let stacked = Layout::top_down(Align::Min);

    place(ui, UiBuilder::new().max_rect(c_when).layout(left), |ui| {
        ui.add(egui::Label::new(mono(&when, 12.0, t.muted)).truncate());
    });
    place(ui, UiBuilder::new().max_rect(c_what.shrink2(vec2(0.0, 6.0))).layout(stacked), |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.add(egui::Label::new(RichText::new(&s.role_label).color(t.ink_strong)).truncate());
        let sub = fmt(tl!("{model} on {connection}"), &[("model", &s.model), ("connection", &s.connection_label)]);
        ui.add(egui::Label::new(RichText::new(sub).size(12.0).color(t.muted)).truncate());
    });
    place(ui, UiBuilder::new().max_rect(c_app.shrink2(vec2(0.0, 6.0))).layout(stacked), |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        ui.add(egui::Label::new(RichText::new(&s.app).color(t.ink)).truncate());
        let (place, colour) = match s.locality {
            Locality::Local => (tl!("Local"), t.good),
            Locality::Remote => (tl!("Remote"), t.faint),
        };
        widgets::hud(ui, place, colour);
    });
    place(ui, UiBuilder::new().max_rect(c_result.shrink2(vec2(0.0, 6.0))).layout(stacked), |ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let (colour, reason) = match outcome {
            Some(Outcome::Done) => (t.good, None),
            Some(Outcome::Failed { kind }) => (t.bad, Some(failure_reason(*kind))),
            Some(Outcome::Cancelled) => (t.faint, None),
            None => (t.warn, None),
        };
        widgets::badge(ui, &outcome_label(outcome), colour);
        if let Some(reason) = reason {
            ui.add(egui::Label::new(RichText::new(reason).size(11.0).color(t.muted)).truncate());
        }
    });
    place(ui, UiBuilder::new().max_rect(c_took).layout(left), |ui| {
        let text = entry.duration_ms().map_or_else(|| "-".to_owned(), seconds);
        ui.add(egui::Label::new(mono(text, 12.0, t.muted)).truncate());
    });
    place(ui, UiBuilder::new().max_rect(c_cost).layout(Layout::right_to_left(Align::Center)), |ui| match entry.reported_cost() {
        Some((micros, currency)) => {
            ui.add(egui::Label::new(mono(cost_text(micros, currency), 12.0, t.ink_strong)).truncate());
        }
        None => {
            ui.label(mono("-", 12.0, t.faint));
        }
    });
}

fn outcome_label(outcome: Option<&Outcome>) -> String {
    match outcome {
        Some(Outcome::Done) => tl!("Done").to_owned(),
        Some(Outcome::Failed { .. }) => tl!("Failed").to_owned(),
        Some(Outcome::Cancelled) => tl!("Cancelled").to_owned(),
        None => tl!("Not finished").to_owned(),
    }
}

/// Totals per calendar day, newest first.
pub(super) fn days(ui: &mut Ui, shown: &[&HistoryEntry], clock: Clock) {
    let rows: Vec<(String, Totals)> = by_day(shown, clock.offset_minutes).into_iter().map(|(d, t)| (d.to_string(), t)).collect();
    groups(ui, tl!("Day"), true, &rows);
}

/// Totals per calendar month, newest first.
pub(super) fn months(ui: &mut Ui, shown: &[&HistoryEntry], clock: Clock) {
    let rows: Vec<(String, Totals)> = by_month(shown, clock.offset_minutes).into_iter().map(|(m, t)| (m.to_string(), t)).collect();
    groups(ui, tl!("Month"), true, &rows);
}

/// Totals per model, busiest first: which models are doing the work, how fast, at what cost.
pub(super) fn models(ui: &mut Ui, shown: &[&HistoryEntry]) {
    groups(ui, tl!("Model"), false, &by_model(shown));
}

/// Totals per app, busiest first.
pub(super) fn apps(ui: &mut Ui, shown: &[&HistoryEntry]) {
    groups(ui, tl!("App"), false, &by_app(shown));
}

fn groups(ui: &mut Ui, first: &str, numeric_names: bool, rows: &[(String, Totals)]) {
    let t = Tokens::get(ui.ctx());
    let total = ui.available_width();
    let (jobs, outcomes, average, cost) = (170.0, 120.0, 88.0, 140.0);
    let widths = [(total - jobs - outcomes - average - cost).max(110.0), jobs, outcomes, average, cost];
    let rects = |row: Rect| {
        let mut x = row.left();
        widths.map(|w| {
            let r = Rect::from_min_size(egui::pos2(x + PAD, row.top()), vec2((w - 2.0 * PAD).max(0.0), row.height()));
            x += w;
            r
        })
    };
    let (head, _) = ui.allocate_exact_size(vec2(total, HEAD_H), Sense::hover());
    for (rect, label) in rects(head).into_iter().zip([first, tl!("Jobs"), tl!("Done"), tl!("Average"), tl!("Cost")]) {
        place(ui, UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)), |ui| {
            widgets::hud(ui, label, t.faint);
        });
    }
    ui.painter().hline(head.x_range(), head.bottom() - 0.5, (1.0, t.line_strong));
    let busiest = rows.iter().map(|(_, t)| t.jobs).max().unwrap_or(1).max(1);
    rows_flush(ui, |ui| {
        egui::ScrollArea::vertical().id_salt("history-groups").max_height(GROUP_ROW_H * (VISIBLE_ROWS + 1.0)).auto_shrink([false, true]).show_rows(
            ui,
            GROUP_ROW_H,
            rows.len(),
            |ui, range| {
                for (label, totals) in rows.iter().skip(range.start).take(range.len()) {
                    let (row, resp) = ui.allocate_exact_size(vec2(ui.available_width(), GROUP_ROW_H), Sense::hover());
                    if resp.hovered() {
                        ui.painter().rect_filled(row, egui::CornerRadius::ZERO, t.sunken);
                    }
                    ui.painter().hline(row.x_range(), row.bottom() - 0.5, (1.0, t.line));
                    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, format!("{label}, {}", totals.jobs)));
                    let [c_name, c_jobs, c_done, c_average, c_cost] = rects(row);
                    let left = Layout::left_to_right(Align::Center);
                    place(ui, UiBuilder::new().max_rect(c_name).layout(left), |ui| {
                        let text = if numeric_names { mono(label, 12.5, t.ink_strong) } else { RichText::new(label).color(t.ink_strong) };
                        ui.add(egui::Label::new(text).truncate());
                    });
                    place(ui, UiBuilder::new().max_rect(c_jobs).layout(left), |ui| {
                        ui.label(mono(totals.jobs.to_string(), 12.5, t.ink));
                        // A slim bar, the busiest row at full width: a quick read of the proportions.
                        let (bar, _) = ui.allocate_exact_size(vec2(72.0, 4.0), Sense::hover());
                        ui.painter().rect_filled(bar, egui::CornerRadius::ZERO, t.line);
                        let filled = Rect::from_min_size(bar.min, vec2(bar.width() * totals.jobs as f32 / busiest as f32, bar.height()));
                        ui.painter().rect_filled(filled, egui::CornerRadius::ZERO, t.accent_ink);
                    });
                    place(ui, UiBuilder::new().max_rect(c_done).layout(left), |ui| {
                        ui.label(mono(totals.done.to_string(), 12.5, t.ink));
                        if totals.failed > 0 {
                            ui.label(mono(format!("{} {}", totals.failed, tl!("Failed").to_lowercase()), 11.0, t.bad));
                        }
                    });
                    place(ui, UiBuilder::new().max_rect(c_average).layout(left), |ui| {
                        let text = totals.average_ms().map_or_else(|| "-".to_owned(), seconds);
                        ui.add(egui::Label::new(mono(text, 12.0, t.muted)).truncate());
                    });
                    place(ui, UiBuilder::new().max_rect(c_cost).layout(left), |ui| {
                        ui.add(egui::Label::new(mono(costs_of(totals), 12.5, t.ink_strong)).truncate());
                    });
                }
            },
        );
    });
}
