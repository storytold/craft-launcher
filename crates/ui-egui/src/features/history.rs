//! The History page of the Set up AI window: every AI job the Craft apps have run, with the cost
//! the providers reported.
//!
//! State and behaviour live here; `history_table` draws the stat strip and the tables. The log is
//! read, cleared and exported only through [`FeaturesPort`], so this module never touches disk or
//! the clock (the web build has no port and the page never appears there). The numbers come from
//! `craft_ai_model::history::report`, the same code the apps' own history view uses.

use craft_ai_model::HistoryLog;
use craft_ai_model::history::report::{HistoryFilter, apps, models, start_of_day_ms, to_csv};
use egui::{Align, Layout, RichText, Ui};

use super::{FeaturesPort, FeaturesUi, Page};
use crate::theme::Tokens;
use crate::widgets::{self, Cell, Kind};

const MS_PER_DAY: u64 = 86_400_000;

/// The time as the desktop shell knows it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clock {
    /// Unix milliseconds now.
    pub now_ms: u64,
    /// Local offset from UTC in minutes, east positive.
    pub offset_minutes: i32,
}

/// How far back to look.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Range {
    #[default]
    All,
    Today,
    Week,
    Month,
}

impl Range {
    pub(crate) fn since(self, clock: Clock) -> Option<u64> {
        match self {
            Range::All => None,
            Range::Today => Some(start_of_day_ms(clock.now_ms, clock.offset_minutes)),
            Range::Week => Some(clock.now_ms.saturating_sub(7 * MS_PER_DAY)),
            Range::Month => Some(clock.now_ms.saturating_sub(30 * MS_PER_DAY)),
        }
    }
}

/// Which table to show.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tab {
    #[default]
    Jobs,
    Days,
    Months,
    Models,
    Apps,
}

#[derive(Default)]
pub(crate) struct HistoryUi {
    pub(crate) log: HistoryLog,
    /// The log could not be read; shown instead of the tables.
    pub(crate) error: Option<String>,
    pub(crate) app: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) range: Range,
    pub(crate) tab: Tab,
    /// "Clear history" was pressed and the question is showing.
    pub(crate) confirming: bool,
    /// A line under the tables: `(text, is_error)`.
    pub(crate) notice: Option<(String, bool)>,
    pub(crate) clock: Clock,
}

impl HistoryUi {
    /// Reads the log again. A filter left on an app or model no longer in the log is dropped, so
    /// it cannot hide everything.
    pub(crate) fn reload(&mut self, port: &mut dyn FeaturesPort) {
        self.clock = port.clock();
        match port.history() {
            Ok(log) => {
                self.log = log;
                self.error = None;
            }
            Err(message) => {
                self.log = HistoryLog::default();
                self.error = Some(message);
            }
        }
        if self.app.as_ref().is_some_and(|a| !apps(&self.log.entries).contains(a)) {
            self.app = None;
        }
        if self.model.as_ref().is_some_and(|m| !models(&self.log.entries).contains(m)) {
            self.model = None;
        }
    }

    pub(crate) fn filter(&self) -> HistoryFilter {
        HistoryFilter { app: self.app.clone(), model: self.model.clone(), since_ms: self.range.since(self.clock) }
    }

    /// Saves what is showing as a CSV file and says where.
    pub(crate) fn export(&mut self, port: &mut dyn FeaturesPort) {
        let filter = self.filter();
        let shown = filter.apply(&self.log.entries);
        self.notice = Some(match port.export_history(&to_csv(&shown)) {
            Ok(path) => (crate::i18n::fmt(tl!("Saved to {path}"), &[("path", &path)]), false),
            Err(message) => (message, true),
        });
    }

    /// Deletes the log for every app, then reads it again.
    pub(crate) fn clear(&mut self, port: &mut dyn FeaturesPort) {
        self.confirming = false;
        match port.clear_history() {
            Ok(()) => {
                self.reload(port);
                self.notice = None;
            }
            Err(message) => self.notice = Some((message, true)),
        }
    }
}

impl FeaturesUi {
    /// Opens the History page with a fresh read of the log.
    pub(crate) fn open_history(&mut self, port: &mut dyn FeaturesPort) {
        self.page = Page::History;
        self.notice = None;
        self.history.confirming = false;
        self.history.notice = None;
        self.history.reload(port);
    }

    pub(crate) fn history_page(&mut self, ui: &mut Ui, port: &mut dyn FeaturesPort) {
        let t = Tokens::get(ui.ctx());
        crate::connections::title(ui, tl!("History"), tl!("AI jobs run by your Craft apps"));
        let mut back = false;
        let mut refresh = false;
        let mut export = false;
        let mut clear = false;
        let h = &mut self.history;

        if let Some(message) = &h.error {
            ui.add(egui::Label::new(RichText::new(message).color(t.bad)).wrap());
            ui.add_space(10.0);
        } else {
            let (app_options, model_options) = (apps(&h.log.entries), models(&h.log.entries));
            ui.horizontal(|ui| {
                let options = [
                    (Range::All, Cell { label: tl!("All time"), tip: tl!("All time"), count: None }),
                    (Range::Today, Cell { label: tl!("Today"), tip: tl!("Today"), count: None }),
                    (Range::Week, Cell { label: tl!("7 days"), tip: tl!("7 days"), count: None }),
                    (Range::Month, Cell { label: tl!("30 days"), tip: tl!("30 days"), count: None }),
                ];
                widgets::cells(ui, &mut h.range, &options);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    combo(ui, "history-model", tl!("All models"), &model_options, &mut h.model);
                    combo(ui, "history-app", tl!("All apps"), &app_options, &mut h.app);
                });
            });
            ui.add_space(18.0);

            let filter = h.filter();
            let shown = filter.apply(&h.log.entries);
            if h.log.entries.is_empty() {
                ui.label(RichText::new(tl!("No AI jobs yet. They appear here once a Craft app uses AI.")).color(t.muted));
            } else if shown.is_empty() {
                ui.label(RichText::new(tl!("No jobs match these filters.")).color(t.muted));
            } else {
                super::history_overview::overview(ui, &shown, h.clock);
                ui.add_space(20.0);
                let options = [
                    (Tab::Jobs, Cell { label: tl!("Jobs"), tip: tl!("Jobs"), count: Some((shown.len(), None)) }),
                    (Tab::Days, Cell { label: tl!("By day"), tip: tl!("By day"), count: None }),
                    (Tab::Months, Cell { label: tl!("By month"), tip: tl!("By month"), count: None }),
                    (Tab::Models, Cell { label: tl!("By model"), tip: tl!("By model"), count: None }),
                    (Tab::Apps, Cell { label: tl!("By app"), tip: tl!("By app"), count: None }),
                ];
                ui.horizontal(|ui| {
                    widgets::cells(ui, &mut h.tab, &options);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(tl!("Only costs the provider reported are counted.")).size(12.0).color(t.faint))
                                .truncate(),
                        );
                    });
                });
                ui.add_space(12.0);
                match h.tab {
                    Tab::Jobs => super::history_table::jobs(ui, &shown, h.clock),
                    Tab::Days => super::history_table::days(ui, &shown, h.clock),
                    Tab::Months => super::history_table::months(ui, &shown, h.clock),
                    Tab::Models => super::history_table::models(ui, &shown),
                    Tab::Apps => super::history_table::apps(ui, &shown),
                }
            }
            if h.log.skipped > 0 {
                ui.add(
                    egui::Label::new(RichText::new(tl!("Some lines in the history could not be read and were left out.")).size(12.0).color(t.warn))
                        .wrap(),
                );
            }
        }
        if let Some((text, error)) = &h.notice {
            ui.add_space(6.0);
            ui.add(egui::Label::new(RichText::new(text).color(if *error { t.bad } else { t.muted })).wrap());
        }

        ui.add_space(22.0);
        ui.horizontal(|ui| {
            back = widgets::button(ui, tl!("Back"), Kind::Secondary, 90.0).clicked();
            refresh = widgets::button(ui, tl!("Refresh"), Kind::Secondary, 0.0).clicked();
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if h.confirming {
                    clear = widgets::button(ui, tl!("Yes, delete it"), Kind::Danger, 0.0).clicked();
                    if widgets::button(ui, tl!("Keep it"), Kind::Secondary, 0.0).clicked() {
                        h.confirming = false;
                    }
                    ui.label(RichText::new(tl!("Delete the history for every app?")).color(t.muted));
                } else {
                    ui.add_enabled_ui(!h.log.entries.is_empty(), |ui| {
                        if widgets::button(ui, tl!("Clear history"), Kind::Secondary, 0.0).clicked() {
                            h.confirming = true;
                        }
                        export = widgets::button(ui, tl!("Export CSV"), Kind::Secondary, 0.0)
                            .on_hover_text(tl!("Saves what is showing as a spreadsheet file in your Downloads folder"))
                            .clicked();
                    });
                }
            });
        });
        if back {
            self.back();
        } else if refresh {
            self.history.reload(port);
        } else if export {
            self.history.export(port);
        } else if clear {
            self.history.clear(port);
        }
    }
}

/// An app or model filter. `None` is "all".
fn combo(ui: &mut Ui, id: &str, all: &str, options: &[String], current: &mut Option<String>) {
    let shown = current.clone().unwrap_or_else(|| all.to_owned());
    egui::ComboBox::from_id_salt(id).selected_text(shown).width(150.0).show_ui(ui, |ui| {
        if ui.selectable_label(current.is_none(), all).clicked() {
            *current = None;
        }
        for option in options {
            let on = current.as_ref() == Some(option);
            if ui.selectable_label(on, option).clicked() {
                *current = Some(option.clone());
            }
        }
    });
}
