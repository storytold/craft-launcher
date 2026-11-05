//! German layout runs in its own process: the UI language is global, and ui.rs drives English
//! widgets concurrently. Do not switch languages inside that test binary.

use artcraft_launcher_core::protocol::{Backend, Capabilities, Command, Event};
use artcraft_launcher_core::{Arch, Os, Platform, SUITE};
use artcraft_launcher_ui_egui::{Host, LauncherUi, Settings};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

struct Reports(Vec<Event>);
impl Backend for Reports {
    fn send(&mut self, command: Command) {
        if let Command::GenerateDiagnostics { request, .. } = command {
            let apps: Vec<_> = SUITE
                .iter()
                .map(|app| {
                    serde_json::json!({
                        "app": app.id, "installed_version": "0.8.2", "install_source": "Msi",
                        "location": "[path]", "local_location_exists": true,
                    })
                })
                .collect();
            self.0.push(Event::DiagnosticsGenerated {
                request,
                result: Ok(serde_json::to_string_pretty(&serde_json::json!({
                    "schema": "artcraft-launcher.diagnostics/v1", "apps": apps,
                }))
                .unwrap()),
            });
        }
    }
    fn poll(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.0)
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { manage_apps: true, reveal_label: "Show in Explorer", self_update: false }
    }
    fn platform(&self) -> Option<Platform> {
        Some(Platform { os: Os::Windows, arch: Arch::X64 })
    }
    fn now(&self) -> i64 {
        1_791_500_000
    }
}
struct NoHost;
impl Host for NoHost {}

#[test]
fn german_diagnostic_preview_and_actions_fit_the_minimum_desktop_window() {
    let mut h = Harness::builder().with_size(egui::vec2(720.0, 500.0)).with_theme(egui::Theme::Dark).build_ui_state(
        |ui, state: &mut Option<LauncherUi>| {
            if state.is_none() {
                *state = Some(LauncherUi::new(
                    ui.ctx(),
                    Box::new(Reports(Vec::new())),
                    Settings { language: "de".into(), notify: false, ..Default::default() },
                ));
            }
            let launcher = state.as_mut().unwrap();
            launcher.logic(&ui.ctx().clone(), &mut NoHost);
            launcher.ui(ui, &mut NoHost);
        },
        None,
    );
    h.run_steps(6);
    h.get_by_label("Einstellungen").click();
    h.run_steps(4);
    h.get_by_label("Diagnosebericht").scroll_to_me();
    h.run_steps(3);
    h.get_by_label("Diagnosebericht").click();
    h.run_steps(4);
    for label in ["Bericht kopieren", "Bericht speichern", "Bericht aktualisieren", "Schließen"] {
        let rect = h.get_by_label(label).rect();
        assert!(rect.top() >= 0.0 && rect.bottom() <= 500.0 && rect.left() >= 0.0 && rect.right() <= 720.0, "{label}: {rect:?}");
    }
    if let Some(dir) = std::env::var_os("ARTCRAFT_LAUNCHER_SNAPSHOTS") {
        h.render().unwrap().save(std::path::Path::new(&dir).join("diagnostics-small-german-dark.png")).unwrap();
    }
}
