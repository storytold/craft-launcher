//! Desktop suite export UI; all filesystem and network work belongs to the backend.
use crate::app::LauncherUi;
use crate::theme::Tokens;
use crate::widgets::{self, Kind};
use artcraft_launcher_core::protocol::{Command, JobPhase};
use std::collections::BTreeSet;

pub(crate) struct ExportState {
    pub selected: BTreeSet<String>,
    pub destination: String,
    pub busy: bool,
    pub progress: Option<(String, JobPhase)>,
    pub result: Option<Result<String, String>>,
}

impl Default for ExportState {
    fn default() -> Self {
        Self {
            selected: artcraft_launcher_core::SUITE.iter().map(|a| a.id.into()).collect(),
            destination: String::new(),
            busy: false,
            progress: None,
            result: None,
        }
    }
}

impl LauncherUi {
    pub(crate) fn suite_export_dialog(&mut self, ui: &mut egui::Ui) -> bool {
        let t = Tokens::get(ui.ctx());
        let top = ui.cursor().top();
        ui.set_width(520.0_f32.min((ui.ctx().content_rect().width() - 80.0).max(0.0)));
        ui.label(widgets::heading(tl!("Offline suite"), 22.0, t.ink_strong));
        ui.label(tl!("Bundle ArtCraft Launcher and selected apps for use without an internet connection."));
        if let Some(platform) = self.platform {
            ui.label(crate::i18n::fmt(
                tl!("Target: {platform}. Create each suite on its target operating system."),
                &[("platform", &platform.label())],
            ));
        }
        ui.add_space(10.0);
        let body_height = (ui.ctx().content_rect().height() - 80.0 - (ui.cursor().top() - top) - 2.0 * crate::theme::CONTROL_H - 16.0).max(0.0);
        egui::ScrollArea::vertical().id_salt("suite-export-body").max_height(body_height.min(ui.ctx().content_rect().height() * 0.53)).show(
            ui,
            |ui| {
                ui.add_enabled_ui(!self.suite_export.busy, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if widgets::button(ui, tl!("Select all"), Kind::Secondary, 0.0).clicked() {
                            self.suite_export.selected = artcraft_launcher_core::SUITE.iter().map(|a| a.id.into()).collect();
                        }
                        if widgets::button(ui, tl!("Select none"), Kind::Secondary, 0.0).clicked() {
                            self.suite_export.selected.clear();
                        }
                    });
                    for app in &artcraft_launcher_core::SUITE {
                        let mut selected = self.suite_export.selected.contains(app.id);
                        if ui.checkbox(&mut selected, app.name).changed() {
                            if selected {
                                self.suite_export.selected.insert(app.id.into());
                            } else {
                                self.suite_export.selected.remove(app.id);
                            }
                        }
                    }
                    ui.add_space(8.0);
                    let label = ui.label(tl!("New suite folder (absolute path)"));
                    ui.add(egui::TextEdit::singleline(&mut self.suite_export.destination).desired_width(f32::INFINITY)).labelled_by(label.id);
                    ui.label(tl!("The parent folder must exist. Existing folders are never replaced."));
                    ui.label(tl!("Official stable releases are verified. Your settings and projects are not copied."));
                });
                if let Some((app, phase)) = &self.suite_export.progress {
                    ui.separator();
                    ui.label(app);
                    match phase {
                        JobPhase::Downloading { done, total } | JobPhase::Reconnecting { done, total } => {
                            let ratio = if *total > 0 { (*done as f64 / *total as f64).clamp(0.0, 1.0) as f32 } else { 0.0 };
                            ui.add(egui::ProgressBar::new(ratio).animate(true));
                            ui.label(format!("{} / {}", widgets::human_bytes(*done), widgets::human_bytes(*total)));
                        }
                        JobPhase::Waiting => {
                            ui.spinner();
                            ui.label(tl!("Checking portable releases…"));
                        }
                        JobPhase::Verifying => {
                            ui.spinner();
                            ui.label(tl!("Verifying…"));
                        }
                        _ => {
                            ui.spinner();
                            ui.label(tl!("Preparing suite…"));
                        }
                    }
                }
                if let Some(result) = &self.suite_export.result {
                    ui.separator();
                    match result {
                        Ok(path) => {
                            ui.label(tl!("Suite ready. Move the entire folder to keep apps and data together."));
                            ui.label(path);
                        }
                        Err(e) if e == artcraft_launcher_core::protocol::CANCELLED => {
                            ui.label(tl!("Export cancelled"));
                        }
                        Err(e) => {
                            ui.colored_label(t.bad, crate::i18n::message(crate::i18n::current(), e));
                        }
                    }
                }
            },
        );
        ui.add_space(12.0);
        let mut close = false;
        ui.horizontal_wrapped(|ui| {
            if self.suite_export.busy {
                if widgets::button(ui, tl!("Cancel export"), Kind::Secondary, 0.0).clicked() {
                    self.send(Command::CancelSuiteExport);
                }
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
            } else {
                let enabled = !self.suite_export.selected.is_empty() && !self.suite_export.destination.trim().is_empty();
                ui.add_enabled_ui(enabled, |ui| {
                    if widgets::button(ui, tl!("Create suite"), Kind::Primary, 0.0).clicked() {
                        self.suite_export.busy = true;
                        self.suite_export.result = None;
                        self.suite_export.progress = Some((String::new(), JobPhase::Waiting));
                        self.send(Command::ExportSuite {
                            destination: self.suite_export.destination.trim().into(),
                            apps: self.suite_export.selected.iter().cloned().collect(),
                        });
                    }
                });
                if matches!(self.suite_export.result, Some(Ok(_))) && widgets::button(ui, tl!("Show suite folder"), Kind::Secondary, 0.0).clicked() {
                    self.send(Command::RevealSuiteExport);
                }
            }
            close |= widgets::button(ui, tl!("Close"), Kind::Secondary, 90.0).clicked();
        });
        close
    }
}
