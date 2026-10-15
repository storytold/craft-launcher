//! Report preview. The backend owns all I/O; the browser never offers this desktop action.

use artcraft_launcher_core::diagnostics::{DiagnosticContext, MAX_REPORT_BYTES};
use artcraft_launcher_core::protocol::Command;
use egui::RichText;

use crate::app::{Dialog, LauncherUi};
use crate::i18n::fmt;
use crate::theme::{self, Tokens};
use crate::widgets::{self, Kind};

#[derive(Default)]
pub(crate) struct State {
    pub request: u64,
    pub collecting: bool,
    pub saving: bool,
    pub include_logs: bool,
    pub report: Option<String>,
    pub error: Option<String>,
    pub saved: Option<String>,
}

impl State {
    pub fn generated(&mut self, request: u64, result: Result<String, String>) {
        if request != self.request {
            return;
        }
        self.collecting = false;
        match result {
            Ok(report) if report.len() <= MAX_REPORT_BYTES => {
                self.report = Some(report);
                self.error = None;
            }
            _ => {
                self.report = None;
                self.error = Some(tl!("Couldn't prepare the report. Try again.").into());
            }
        }
    }

    pub fn saved(&mut self, request: u64, result: Result<String, String>) {
        if request != self.request {
            return;
        }
        self.saving = false;
        match result {
            Ok(path) => {
                self.saved = Some(path);
                self.error = None;
            }
            Err(_) => {
                self.error = Some(tl!("Couldn't save the report. Check disk space and folder permissions.").into());
            }
        }
    }
}

impl LauncherUi {
    pub(crate) fn open_diagnostics(&mut self) {
        self.dialog = Some(Dialog::Diagnostics);
        // Reopening an in-flight request just shows it; never launch duplicate workers.
        if !self.diagnostics.collecting && !self.diagnostics.saving {
            self.refresh_diagnostics();
        }
    }

    fn refresh_diagnostics(&mut self) {
        self.diagnostics.request = self.diagnostics.request.saturating_add(1);
        self.diagnostics.report = None;
        self.diagnostics.error = None;
        self.diagnostics.saved = None;
        self.diagnostics.collecting = true;
        self.send(Command::GenerateDiagnostics {
            request: self.diagnostics.request,
            context: DiagnosticContext {
                language: crate::i18n::current().code().into(),
                auto_update: self.settings.auto_update,
                prereleases: self.settings.prereleases,
                checking: self.checking,
                last_check: self.settings.last_check,
                last_source: self.last_source,
                rate_limited_until: self.rate_limited_until,
                include_logs: self.diagnostics.include_logs,
            },
        });
    }

    pub(crate) fn diagnostics_dialog(&mut self, ui: &mut egui::Ui) -> bool {
        let t = Tokens::get(ui.ctx());
        let viewport = ui.ctx().content_rect();
        ui.set_width(680.0_f32.min((viewport.width() - 80.0).max(0.0)));
        ui.set_max_height((viewport.height() - 80.0).max(0.0));
        ui.label(widgets::heading(tl!("Diagnostic report"), 22.0, t.ink_strong));
        ui.add_space(8.0);
        ui.label(RichText::new(tl!("Review this local report before sharing it. Nothing is uploaded automatically.")).color(t.muted));
        ui.label(
            RichText::new(tl!("Paths, URLs and recognizable secrets are removed. Other personal details may remain in error messages."))
                .size(12.0)
                .color(t.muted),
        );
        ui.add_space(8.0);
        let busy = self.diagnostics.collecting || self.diagnostics.saving;
        if ui.add_enabled(!busy, egui::Checkbox::new(&mut self.diagnostics.include_logs, tl!("Include recent warning and error logs"))).changed() {
            self.refresh_diagnostics();
        }
        ui.add_space(8.0);
        // Leave a stable footer visible, even on short windows; scroll the selectable JSON.
        let height = (viewport.height() * 0.45).min((ui.available_height() - 150.0).max(40.0));
        egui::ScrollArea::both().id_salt("diagnostic-preview").max_height(height).auto_shrink([false, true]).show(ui, |ui| {
            if self.diagnostics.collecting {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(tl!("Preparing report…"));
                });
            } else if let Some(report) = &self.diagnostics.report {
                let mut preview = report.as_str();
                ui.add(egui::TextEdit::multiline(&mut preview).font(egui::FontId::new(12.0, theme::mono())).desired_width(f32::INFINITY));
            }
        });
        if let Some(error) = &self.diagnostics.error {
            ui.label(RichText::new(error).color(t.warn));
        }
        if self.diagnostics.saving {
            ui.label(tl!("Saving report…"));
        }
        if let Some(path) = &self.diagnostics.saved {
            ui.add(egui::Label::new(fmt(tl!("Saved to {path}"), &[("path", path)])).wrap());
        }
        ui.add_space(12.0);
        let ready = self.diagnostics.report.is_some() && !busy;
        let mut close = false;
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled_ui(ready, |ui| widgets::button(ui, tl!("Copy report"), Kind::Secondary, 0.0)).inner.clicked()
                && let Some(report) = &self.diagnostics.report
            {
                ui.ctx().copy_text(report.clone());
                let ctx = ui.ctx().clone();
                self.toast(&ctx, tl!("Report copied"), false);
            }
            if ui.add_enabled_ui(ready, |ui| widgets::button(ui, tl!("Save report"), Kind::Primary, 0.0)).inner.clicked()
                && let Some(report) = self.diagnostics.report.clone()
            {
                self.diagnostics.saving = true;
                self.diagnostics.error = None;
                self.diagnostics.saved = None;
                self.send(Command::SaveDiagnostics { request: self.diagnostics.request, report });
            }
            if ui.add_enabled_ui(!busy, |ui| widgets::button(ui, tl!("Refresh report"), Kind::Secondary, 0.0)).inner.clicked() {
                self.refresh_diagnostics();
            }
            if self.diagnostics.saved.is_some() && widgets::button(ui, crate::i18n::t(self.caps.reveal_label), Kind::Secondary, 0.0).clicked() {
                self.send(Command::RevealDiagnostics);
            }
            close = widgets::button(ui, tl!("Close"), Kind::Secondary, 0.0).clicked();
        });
        close
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn late_results_cannot_replace_the_preview_or_its_export_status() {
        let mut state = State { request: 2, collecting: true, saving: true, ..Default::default() };
        state.generated(1, Ok("old report".into()));
        state.saved(1, Ok("old path".into()));
        assert!(state.collecting && state.saving && state.report.is_none() && state.saved.is_none());
        state.generated(2, Ok("reviewed report".into()));
        state.saved(2, Err("disk full".into()));
        assert!(!state.collecting && !state.saving && state.saved.is_none() && state.error.is_some());
        assert_eq!(state.report.as_deref(), Some("reviewed report"));
    }
}
