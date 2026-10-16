//! The Advanced page: add a server, and choose which connected model each tool uses.
//!
//! Like the History page it is a scrolling body with a footer pinned under it, so Back is always
//! in reach. Each tool that has a connected model is a card holding its picker and its test.

use egui::{RichText, Ui};

use super::home::{Act, server_rows};
use super::{FeaturesPort, FeaturesUi, Outcome, Snapshot};
use crate::connections::Row;
use crate::theme::Tokens;
use crate::widgets::{self, Kind};

impl FeaturesUi {
    /// The scrolling part: servers, then one card per tool that has a model to choose between.
    pub(super) fn advanced_body(&mut self, ui: &mut Ui, snapshot: &Snapshot, port: &mut dyn FeaturesPort, rows: &[Row]) -> (Outcome, Act) {
        let t = Tokens::get(ui.ctx());
        crate::connections::title(ui, tl!("Advanced"), tl!("Servers, and which model each tool uses"));
        ui.add(
            egui::Label::new(
                RichText::new(tl!("Tools pick the best model that is connected on their own. Change one here, or test that it works."))
                    .color(t.muted),
            )
            .wrap(),
        );
        ui.add_space(18.0);
        let mut outcome = Outcome::None;
        let mut act = Act::None;

        widgets::hud(ui, tl!("Other servers"), t.faint);
        ui.add_space(6.0);
        // Servers already connected come first, so a connection that worked is visible here.
        let connected = self.unmatched(rows);
        if let Some(next) = server_rows(ui, &connected) {
            act = next;
        }
        egui::Frame::new().stroke(t.hairline()).inner_margin(14).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::button(ui, tl!("Add another server"), Kind::Secondary, 0.0).clicked() {
                        outcome = Outcome::AddServer;
                    }
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.add(
                            egui::Label::new(
                                RichText::new(tl!("Ollama, LM Studio, ComfyUI or any OpenAI-compatible server")).size(12.0).color(t.muted),
                            )
                            .wrap(),
                        );
                    });
                });
            });
        });

        ui.add_space(20.0);
        widgets::hud(ui, tl!("Models"), t.faint);
        ui.add_space(6.0);
        let mut shown = 0;
        for feature in self.features.clone() {
            if !snapshot.tools.iter().any(|s| s.feature == feature.id && !s.choices.is_empty()) {
                continue;
            }
            shown += 1;
            egui::Frame::new().stroke(t.hairline()).inner_margin(14).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(widgets::heading(crate::i18n::t(&feature.title), 16.0, t.ink_strong));
                ui.add_space(6.0);
                if let Some(next) = self.tool_block(ui, &feature, snapshot, port) {
                    outcome = next;
                }
            });
            ui.add_space(8.0);
        }
        if shown == 0 {
            egui::Frame::new().stroke(t.hairline()).inner_margin(14).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(tl!("No tool has a connected model yet.")).color(t.muted));
            });
        }
        ui.add_space(4.0);
        (outcome, act)
    }

    /// The footer pinned under the body: what the last action said, and Back.
    pub(super) fn advanced_footer(&mut self, ui: &mut Ui) {
        let t = Tokens::get(ui.ctx());
        if let Some((text, error)) = &self.notice {
            ui.add(egui::Label::new(RichText::new(text).color(if *error { t.bad } else { t.muted })).wrap());
            ui.add_space(8.0);
        }
        if widgets::button(ui, tl!("Back"), Kind::Secondary, 90.0).clicked() {
            self.back();
        }
    }
}
