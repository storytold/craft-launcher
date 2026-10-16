//! Community sources stay opt-in, persisted, and outside official auto-updates.
use artcraft_launcher_core::community::{CommunityAction, CommunitySource, CommunityStatus};
use artcraft_launcher_core::protocol::{Backend, Capabilities, Command, Event};
use artcraft_launcher_core::{Arch, Os, Platform};
use artcraft_launcher_ui_egui::{Host, LauncherUi, Settings};
use egui_kittest::{Harness, kittest::Queryable};
use std::cell::RefCell;
use std::rc::Rc;

struct Fake {
    sent: Rc<RefCell<Vec<Command>>>,
    events: Vec<Event>,
    installed: bool,
}
impl Backend for Fake {
    fn send(&mut self, command: Command) {
        if let Command::Community { source, action } = &command {
            if *action == CommunityAction::Install {
                self.installed = true;
            }
            let release = source.parse_release(r#"{"tag_name":"v1.0.0","assets":[{"name":"pdfcraft-1.0.0-windows-x64-portable.zip","size":10,"browser_download_url":"https://github.com/example/pdf-fork/releases/download/v1.0.0/pdfcraft-1.0.0-windows-x64-portable.zip"}]}"#).unwrap();
            self.events.push(Event::CommunityFinished {
                source: source.clone(),
                status: Box::new(CommunityStatus { release: Some(release), installed: self.installed.then(|| "1.0.0".into()), error: None }),
            });
        }
        self.sent.borrow_mut().push(command);
    }
    fn poll(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { manage_apps: true, reveal_label: "Show in folder", self_update: false }
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
fn harness(settings: Settings) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut backend = Some(Fake { sent: sent.clone(), events: Vec::new(), installed: false });
    let h = Harness::builder().with_size(egui::vec2(1180.0, 1000.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if let Some(b) = backend.take() {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), settings.clone()));
            }
            if let Some(app) = state {
                app.logic(ui.ctx(), &mut NoHost);
                app.ui(ui, &mut NoHost);
            }
        },
        None,
    );
    (h, sent)
}
fn open_sources(h: &mut Harness<'_, Option<LauncherUi>>) {
    h.run_steps(6);
    h.get_by_label("Settings").click();
    h.run_steps(4);
    h.get_by_label("Sources").click();
    h.run_steps(4);
}
#[test]
fn adding_a_source_requires_trust_and_persists_without_installing() {
    let (mut h, sent) = harness(Settings { language: "en".into(), auto_update: true, ..Settings::default() });
    open_sources(&mut h);
    for (label, value) in [("GitHub repository", "https://github.com/Example/Pdf-Fork"), ("Display name", "PDF fork"), ("Package name", "pdfcraft")] {
        h.get_by_label(label).focus();
        h.run_steps(1);
        h.get_by_label(label).type_text(value);
        h.run_steps(2);
    }
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Add source").click();
    h.run_steps(3);
    assert!(h.state().as_ref().unwrap().settings.sources.is_empty());
    h.get_by_label("Confirm that you trust this publisher.");
    h.get_by_label("I trust this publisher and want to add this unofficial source.").click();
    h.run_steps(2);
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Add source").click();
    h.run_steps(6);
    let settings = &h.state().as_ref().unwrap().settings;
    assert_eq!(settings.sources.len(), 1);
    assert_eq!(settings.sources[0].repo, "example/pdf-fork");
    let saved = serde_json::to_string(settings).unwrap();
    assert_eq!(serde_json::from_str::<Settings>(&saved).unwrap().sources, settings.sources);
    assert!(!sent.borrow().iter().any(|c| matches!(c, Command::Community { action: CommunityAction::Install, .. })));
    assert!(serde_json::from_str::<Settings>("{}").unwrap().sources.is_empty());
}
#[test]
fn installs_only_on_click_and_disabling_or_removing_never_uninstalls() {
    let source = CommunitySource::new("example/pdf-fork", "PDF fork", "pdfcraft").unwrap();
    let (mut h, sent) = harness(Settings { language: "en".into(), sources: vec![source.clone()], auto_update: true, ..Settings::default() });
    open_sources(&mut h);
    h.get_by_label("Unofficial");
    assert!(!sent.borrow().iter().any(|c| matches!(c, Command::Community { action: CommunityAction::Install, .. })));
    if let Some(dir) = std::env::var_os("ARTCRAFT_LAUNCHER_SNAPSHOTS") {
        h.render().unwrap().save(std::path::Path::new(&dir).join("community-sources.png")).unwrap();
    }
    h.get_by_label("Install / update").click();
    h.run_steps(1);
    assert!(h.state().as_ref().unwrap().busy());
    h.run_steps(5);
    assert!(sent.borrow().contains(&Command::Community { source: source.clone(), action: CommunityAction::Install }));
    h.get_by_label("Open").click();
    h.run_steps(4);
    assert!(sent.borrow().contains(&Command::Community { source, action: CommunityAction::Launch }));
    sent.borrow_mut().clear();
    h.get_by_label("Enabled").click();
    h.run_steps(4);
    assert!(!h.state().as_ref().unwrap().settings.sources[0].enabled);
    assert!(!sent.borrow().iter().any(|c| matches!(c, Command::Community { .. } | Command::Uninstall { .. })));
    h.get_by_label("Remove source").click();
    h.run_steps(4);
    assert!(h.state().as_ref().unwrap().settings.sources.is_empty());
    assert!(!sent.borrow().iter().any(|c| matches!(c, Command::Uninstall { .. })));
}
