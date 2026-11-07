//! Optional read-only status in the existing app details view. No persisted
//! connection settings, automatic polling, broker lifecycle or native editing.
use crate::{
    app::LauncherUi,
    widgets::{self, Kind},
};
use artcraft_launcher_core::{craftlink::Binding, protocol::Command};

#[derive(Default)]
pub(crate) struct State {
    port: String,
    credential_file: String,
    request: u64,
    busy: bool,
    result: Option<Result<Vec<Binding>, String>>,
}
impl State {
    fn inspect(&mut self) -> Option<Command> {
        if self.busy {
            return None;
        }
        let port = self.port.trim().parse::<u16>().ok().filter(|p| *p > 0);
        let Some(port) = port.filter(|_| !self.credential_file.trim().is_empty()) else {
            self.result = Some(Err(String::new()));
            return None;
        };
        self.request = self.request.checked_add(1)?;
        self.busy = true;
        self.result = None;
        Some(Command::InspectCraftLinks { request: self.request, port, credential_file: self.credential_file.trim().into() })
    }
    fn clear(&mut self) {
        self.request = self.request.saturating_add(1);
        self.busy = false;
        self.result = None;
        self.credential_file.clear();
    }
    pub(crate) fn apply(&mut self, request: u64, result: Result<Vec<Binding>, String>) {
        if request == self.request && self.busy {
            self.busy = false;
            self.result = Some(result);
        }
    }
}
fn status(value: &str) -> &'static str {
    match value {
        "current" => crate::tl!("Current"),
        "stale" => crate::tl!("Stale"),
        "conflicted" => crate::tl!("Conflicted"),
        "missing" => crate::tl!("Missing"),
        "applying" => crate::tl!("Applying"),
        "locally-modified" => crate::tl!("Local changes"),
        "frozen" => crate::tl!("Frozen"),
        _ => crate::tl!("Unknown"),
    }
}
impl LauncherUi {
    pub(crate) fn craftlink_section(&mut self, ui: &mut egui::Ui, app: &str) {
        let mut command = None;
        ui.collapsing("CraftLink", |ui| {
            ui.label(crate::tl!("Broker port"));
            ui.add(egui::TextEdit::singleline(&mut self.craftlink.port).char_limit(5));
            ui.label(crate::tl!("Credential file"));
            ui.add(egui::TextEdit::singleline(&mut self.craftlink.credential_file).char_limit(4096));
            ui.horizontal(|ui| {
                if ui.add_enabled_ui(!self.craftlink.busy, |ui| widgets::button(ui, crate::tl!("Inspect links"), Kind::Primary, 0.0)).inner.clicked()
                {
                    command = self.craftlink.inspect();
                }
                if widgets::button(ui, crate::tl!("Cancel"), Kind::Secondary, 0.0).clicked() {
                    self.craftlink.clear();
                    command = None;
                }
            });
            if self.craftlink.busy {
                ui.label(crate::tl!("Waiting for CraftLink"));
            }
            if let Some(result) = &self.craftlink.result {
                match result {
                    Err(_) => {
                        ui.label(crate::tl!("CraftLink is unavailable"));
                    }
                    Ok(rows) => {
                        let rows = rows.iter().filter(|row| row.profile == app).collect::<Vec<_>>();
                        if rows.is_empty() {
                            ui.label(crate::tl!("No linked objects"));
                        }
                        for row in rows {
                            ui.monospace(format!("{}  #{}  {}", row.object.get(..8).unwrap_or(&row.object), row.applied, status(&row.status)));
                        }
                    }
                }
            }
        });
        if let Some(command) = command {
            self.act_command(command);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn craftlink_requests_are_transient_and_stale_results_are_ignored() {
        let mut state = State { port: "12345".into(), credential_file: "test-credential-file".into(), ..Default::default() };
        let command = state.inspect().unwrap();
        assert!(state.inspect().is_none());
        let Command::InspectCraftLinks { request, .. } = command else { panic!("wrong command") };
        state.apply(request + 1, Ok(vec![]));
        assert!(state.busy);
        state.apply(request, Ok(vec![]));
        assert!(!state.busy);
        assert!(state.result.as_ref().unwrap().is_ok());
        state.inspect();
        state.clear();
        state.apply(request + 1, Err("late".into()));
        assert!(state.result.is_none());
        assert!(state.credential_file.is_empty());
    }
    #[test]
    fn craftlink_invalid_port_never_dispatches() {
        let mut state = State { port: "65536".into(), credential_file: "fixture".into(), ..Default::default() };
        assert!(state.inspect().is_none());
        assert!(!state.busy);
    }
    #[test]
    fn craftlink_panel_renders_only_the_selected_application() {
        use artcraft_launcher_core::protocol::{Backend, Capabilities, Event};
        use egui_kittest::{Harness, kittest::Queryable};
        struct Fake;
        impl Backend for Fake {
            fn platform(&self) -> Option<artcraft_launcher_core::Platform> {
                None
            }
            fn send(&mut self, _: Command) {}
            fn poll(&mut self) -> Vec<Event> {
                vec![]
            }
            fn capabilities(&self) -> Capabilities {
                Capabilities { manage_apps: true, reveal_label: "", self_update: false }
            }
            fn now(&self) -> i64 {
                0
            }
        }
        struct Host;
        impl crate::Host for Host {}
        crate::i18n::set_test_lang(crate::i18n::Lang::EN);
        let mut harness = Harness::builder().with_size(egui::vec2(720.0, 420.0)).build_ui_state(
            |ui, state: &mut Option<LauncherUi>| {
                if state.is_none() {
                    let mut launcher =
                        LauncherUi::new(ui.ctx(), Box::new(Fake), crate::Settings { language: "en".into(), notify: false, ..Default::default() });
                    launcher.craftlink.result = Some(Ok(vec![
                        Binding {
                            object: "11111111-1111-4111-8111-111111111111".into(),
                            profile: "figcraft".into(),
                            applied: 7,
                            status: "current".into(),
                        },
                        Binding {
                            object: "22222222-2222-4222-8222-222222222222".into(),
                            profile: "vectorcraft".into(),
                            applied: 2,
                            status: "missing".into(),
                        },
                    ]));
                    *state = Some(launcher);
                }
                if let Some(launcher) = state {
                    launcher.logic(&ui.ctx().clone(), &mut Host);
                    launcher.craftlink_section(ui, "figcraft");
                }
            },
            None,
        );
        harness.run_steps(6);
        harness.get_by_label("CraftLink").click();
        harness.run_steps(4);
        harness.get_by_label("Broker port");
        harness.get_by_label_contains("11111111");
        assert!(harness.query_by_label_contains("22222222").is_none());
        if let Some(dir) = std::env::var_os("ARTCRAFT_LAUNCHER_SNAPSHOTS") {
            harness.render().unwrap().save(std::path::Path::new(&dir).join("craftlink-status.png")).unwrap();
        }
        let settings = &harness.state().as_ref().unwrap().settings;
        assert!(!serde_json::to_string(settings).unwrap().contains("credential"));
    }
}
