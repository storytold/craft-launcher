//! The Connections dialog's list and its confirmation.

use std::time::Duration;

use egui::{RichText, Ui};

use super::{ConnectionsPort, ConnectionsUi, KeyView, Row, Screen, TestState};
use crate::i18n::fmt;
use crate::theme::Tokens;
use crate::widgets::{self, Kind};

/// A dialog title with its mono subtitle.
pub(crate) fn title(ui: &mut Ui, title: &str, subtitle: &str) {
    let t = Tokens::get(ui.ctx());
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        ui.label(widgets::heading(title, 22.0, t.ink_strong));
        if !subtitle.is_empty() {
            widgets::hud(ui, subtitle, t.faint);
        }
    });
    ui.add_space(14.0);
}

impl ConnectionsUi {
    /// Draws the dialog's contents. Returns true when the person closed it.
    pub fn show(&mut self, ui: &mut Ui, port: &mut dyn ConnectionsPort) -> bool {
        self.show_sized(ui, port, 640.0f32.min(ui.ctx().content_rect().width() - 80.0))
    }

    fn show_sized(&mut self, ui: &mut Ui, port: &mut dyn ConnectionsPort, width: f32) -> bool {
        if !self.loaded {
            self.reload(port);
        }
        self.poll(port);
        if self.is_testing() {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        ui.set_width(width);
        match &self.screen {
            Screen::List => self.list(ui, port),
            Screen::Pick => {
                self.pick(ui);
                false
            }
            Screen::Form(_) => {
                self.form(ui, port);
                false
            }
            Screen::ConfirmRemove(id) => {
                let id = id.clone();
                self.confirm(ui, &id, port);
                false
            }
        }
    }

    fn list(&mut self, ui: &mut Ui, port: &mut dyn ConnectionsPort) -> bool {
        let t = Tokens::get(ui.ctx());
        title(ui, tl!("Connections"), tl!("AI models for every Craft app"));
        if self.rows.is_empty() {
            ui.label(RichText::new(tl!("No connections yet.")).color(t.ink));
            ui.add(
                egui::Label::new(
                    RichText::new(tl!("Add one and the Craft apps can use it. Keys are kept in your system keychain, never in a plain file."))
                        .color(t.muted),
                )
                .wrap(),
            );
        } else {
            egui::ScrollArea::vertical().max_height(ui.ctx().content_rect().height() * 0.5).auto_shrink([false, true]).show(ui, |ui| {
                for row in self.rows.clone() {
                    ui.push_id(row.id.as_str(), |ui| self.row(ui, &row, port));
                    ui.add_space(8.0);
                }
            });
        }
        if let Some((text, error)) = &self.notice {
            ui.add_space(8.0);
            ui.add(egui::Label::new(RichText::new(text).color(if *error { t.bad } else { t.muted })).wrap());
        }
        ui.add_space(16.0);
        let mut close = false;
        let mut add = false;
        ui.horizontal(|ui| {
            add = widgets::button(ui, tl!("Add connection"), Kind::Primary, 0.0).clicked();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                close = widgets::button(ui, tl!("Close"), Kind::Secondary, 90.0).clicked();
            });
        });
        if add {
            self.begin_pick();
        }
        close
    }

    fn row(&mut self, ui: &mut Ui, row: &Row, port: &mut dyn ConnectionsPort) {
        let t = Tokens::get(ui.ctx());
        let (mut edit, mut remove, mut test, mut enabled) = (false, false, false, row.enabled);
        egui::Frame::new().stroke(t.hairline()).inner_margin(12).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(widgets::heading(&row.label, 16.0, if row.enabled { t.ink_strong } else { t.faint }));
                ui.add_space(6.0);
                if row.local {
                    widgets::badge(ui, tl!("This computer"), t.good);
                } else {
                    widgets::badge(ui, tl!("Leaves this computer"), t.warn);
                }
                if !row.enabled {
                    widgets::badge(ui, tl!("Turned off"), t.faint);
                }
            });
            ui.add(egui::Label::new(RichText::new(format!("{} · {}", row.provider, row.endpoint)).size(12.0).color(t.muted)).truncate());
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let (text, color) = match row.key {
                    KeyView::Saved => (tl!("Key saved"), t.good),
                    KeyView::Missing => (tl!("Key missing"), t.bad),
                    KeyView::NotNeeded => (tl!("No key needed"), t.muted),
                    KeyView::Unreadable => (tl!("Key unreadable"), t.warn),
                };
                ui.label(RichText::new(text).size(12.0).color(color));
                ui.label(RichText::new(fmt(tl!("Models: {n}"), &[("n", &row.models.to_string())])).size(12.0).color(t.muted));
                if row.bindings > 0 {
                    ui.label(RichText::new(fmt(tl!("App settings using it: {n}"), &[("n", &row.bindings.to_string())])).size(12.0).color(t.muted));
                }
            });
            if let Some(state) = self.tested.get(&row.id) {
                let (text, color) = test_text(state, t);
                if let Some(text) = text {
                    ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color)).wrap());
                }
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let busy = self.is_testing();
                ui.add_enabled_ui(!busy, |ui| test = widgets::button(ui, tl!("Test"), Kind::Secondary, 0.0).clicked());
                edit = widgets::button(ui, tl!("Edit"), Kind::Secondary, 0.0).clicked();
                remove = widgets::button(ui, tl!("Remove"), Kind::Secondary, 0.0).clicked();
                ui.add_space(6.0);
                ui.checkbox(&mut enabled, tl!("Enabled"));
            });
        });
        if test {
            self.test_saved(&row.id, port);
        }
        if edit {
            self.begin_edit(&row.id);
        }
        if remove {
            self.screen = Screen::ConfirmRemove(row.id.clone());
        }
        if enabled != row.enabled {
            self.set_enabled(&row.id, enabled, port);
        }
    }

    fn confirm(&mut self, ui: &mut Ui, id: &craft_ai_model::ConnectionId, port: &mut dyn ConnectionsPort) {
        let t = Tokens::get(ui.ctx());
        let Some(row) = self.rows.iter().find(|r| &r.id == id).cloned() else {
            self.back();
            return;
        };
        title(ui, &fmt(tl!("Remove {name}?"), &[("name", &row.label)]), "");
        if row.key == KeyView::Saved {
            ui.label(RichText::new(tl!("Its saved key is deleted too.")).color(t.muted));
        }
        if row.bindings > 0 {
            ui.label(RichText::new(tl!("App settings that use it are cleared as well.")).color(t.muted));
        }
        ui.add_space(20.0);
        let (mut yes, mut no) = (false, false);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            yes = widgets::button(ui, tl!("Remove"), Kind::Danger, 110.0).clicked();
            no = widgets::button(ui, tl!("Cancel"), Kind::Secondary, 90.0).clicked();
        });
        if yes {
            self.remove(id, port);
        } else if no {
            self.back();
        }
    }
}

/// The line for a test, and its colour; nothing while idle.
pub(super) fn test_text(state: &TestState, t: Tokens) -> (Option<String>, egui::Color32) {
    match state {
        TestState::Idle => (None, t.muted),
        TestState::Running => (Some(tl!("Testing…").to_string()), t.muted),
        TestState::Passed { models, millis } => {
            (Some(fmt(tl!("Connected. Models found: {n} ({ms} ms)"), &[("n", &models.to_string()), ("ms", &millis.to_string())])), t.good)
        }
        TestState::Failed(e) => (Some(fmt(tl!("Test failed: {e}"), &[("e", e)])), t.bad),
    }
}
