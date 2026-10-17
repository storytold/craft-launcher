//! Ukrainian UI flow in a separate binary because the UI language is process-wide.
//! The backend records commands and simulates replies; it never downloads or installs apps.
//! Set ARTCRAFT_LAUNCHER_SNAPSHOTS to save screenshots; run with --test-threads=1.

use std::cell::RefCell;
use std::rc::Rc;

use artcraft_launcher_core::protocol::{Backend, Capabilities, Command, Event, JobPhase, LatestRelease, Origin, Source};
use artcraft_launcher_core::release::{Asset, Release};
use artcraft_launcher_core::status::InstallSource;
use artcraft_launcher_core::{Arch, Installed, Os, Platform, SUITE, version};
use artcraft_launcher_ui_egui::settings::ViewMode;
use artcraft_launcher_ui_egui::{Host, LauncherUi, Settings};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

// 16 October 2026, 12:00 UTC; releases are dated 14 October.
const NOW: i64 = 1_792_152_000;

struct Fake {
    events: Vec<Event>,
    sent: Rc<RefCell<Vec<Command>>>,
}

impl Backend for Fake {
    fn send(&mut self, command: Command) {
        if let Command::Install { app } = &command
            && let Some(craft) = SUITE.iter().find(|craft| craft.id == app)
        {
            // Simulate the reply after the user action, so its toast is still visible.
            self.events.push(Event::JobFinished { app: app.clone(), result: Ok(format!("{} installed: 0.5.0", craft.name)) });
        }
        self.sent.borrow_mut().push(command);
    }
    fn poll(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { manage_apps: true, reveal_label: "Show in Explorer", self_update: true }
    }
    fn platform(&self) -> Option<Platform> {
        Some(Platform { os: Os::Windows, arch: Arch::X64 })
    }
    fn now(&self) -> i64 {
        NOW
    }
}

struct NoHost;
impl Host for NoHost {}

fn events() -> Vec<Event> {
    let mut events = Vec::new();
    for app in &SUITE {
        let name = format!("{}-0.5.0-windows-x64.msi", app.id);
        let release = Release {
            tag: "v0.5.0".into(),
            version: version::parse("0.5.0"),
            name: "v0.5.0".into(),
            published_at: Some(NOW - 2 * 86_400),
            html_url: format!("https://github.com/storytold/{}/releases/tag/v0.5.0", app.id),
            notes: String::new(),
            prerelease: false,
            assets: vec![Asset {
                name: name.clone(),
                size: 150 << 20,
                url: format!("https://github.com/storytold/{}/releases/download/v0.5.0/{name}", app.id),
                sha256: None,
            }],
        };
        events.push(Event::Latest { app: app.id.into(), latest: LatestRelease::Found(release), from: Origin::ArtCraftApi });
        // One fresh install and one update make their actions unambiguous to query.
        let installed = (app.id != "vectorcraft").then(|| Installed {
            version: version::parse(if app.id == "photocraft" { "0.4.1" } else { "0.5.0" }),
            source: InstallSource::Msi,
            location: format!(r"C:\Apps\{}\{}.exe", app.name, app.id),
            package: None,
        });
        events.push(Event::Detected { app: app.id.into(), installed });
    }
    events.push(Event::Progress { app: "filmcraft".into(), phase: JobPhase::Verifying });
    events.push(Event::CheckFinished { at: NOW - 180, source: Source::Feed, rate_limited_until: None });
    events
}

fn harness(settings: Settings) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut backend = Some(Fake { events: events(), sent: sent.clone() });
    let h = Harness::builder().with_size(egui::vec2(1120.0, 900.0)).with_theme(egui::Theme::Dark).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if let Some(backend) = backend.take() {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(backend), settings.clone()));
            }
            if let Some(launcher) = state.as_mut() {
                launcher.logic(ui.ctx(), &mut NoHost);
                launcher.ui(ui, &mut NoHost);
            }
        },
        None,
    );
    (h, sent)
}

fn snapshot(h: &mut Harness<'_, Option<LauncherUi>>, name: &str) {
    let Some(dir) = std::env::var_os("ARTCRAFT_LAUNCHER_SNAPSHOTS") else { return };
    std::fs::create_dir_all(&dir).unwrap();
    h.render().unwrap().save(std::path::Path::new(&dir).join(format!("{name}.png"))).unwrap();
}

#[test]
fn ukrainian_switches_persists_and_keeps_windows_install_actions() {
    let settings =
        Settings { language: "en".into(), notify: false, theme: artcraft_launcher_ui_egui::theme::ThemeChoice::Dark, ..Settings::default() };
    let (mut h, sent) = harness(settings);
    h.run_steps(8);
    h.get_by_label("Settings").click();
    h.run_steps(3);
    h.get_by_value("English").click();
    h.run_steps(2);
    // Ukrainian is the last item, below the ComboBox's visible scroll area.
    // kittest's pointer click does not scroll a queried accessibility node into view.
    h.get_by_label("Українська").scroll_to_me();
    h.run_steps(16);
    snapshot(&mut h, "language-picker-uk-windows-dark");
    h.get_by_label("Українська").click();
    h.run_steps(3);
    assert_eq!(h.state().as_ref().unwrap().settings.language, "uk");
    h.get_by_value("Українська");
    h.get_by_label("Автоматично завантажувати й встановлювати оновлення");
    h.get_by_label("Сповіщати про оновлення встановлених застосунків");
    h.get_by_label("Встановлення у Windows");
    snapshot(&mut h, "settings-uk-windows-dark");
    h.get_by_label("Готово").click();
    h.run_steps(3);
    h.get_by_label("Launcher");
    h.get_by_label("Craft App Suite");
    h.get_by_label("Редагування зображень");
    h.get_by_label("Оновити до 0.5.0");
    h.get_by_label_contains("Перевірка контрольної суми…");
    h.get_by_label_contains("Перевірено 3 хв тому · джерела: ArtCraft API");
    assert!(h.query_all_by_label_contains("14 жовт. 2026").count() >= 1);
    snapshot(&mut h, "grid-uk-windows-dark");

    sent.borrow_mut().clear();
    h.get_by_label("Встановити").click();
    h.run_steps(3);
    assert!(!sent.borrow().iter().any(|c| matches!(c, Command::Install { .. })));
    h.get_by_label("Застосунки розміщуються в цій теці.");
    let folder = r"C:\Craft test apps";
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Тека встановлення").click();
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Тека встановлення").type_text(folder);
    h.run_steps(3);
    assert_eq!(h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Тека встановлення").value().as_deref(), Some(folder));
    assert!(h.state().as_ref().unwrap().settings.install_locations.per_app.is_empty());
    snapshot(&mut h, "install-uk-windows-dark");
    h.get_all_by_role_and_label(egui::accesskit::Role::Button, "Встановити").last().unwrap().click();
    h.run_steps(3);
    let actions: Vec<_> = sent.borrow().iter().filter(|c| matches!(c, Command::Configure(_) | Command::Install { .. })).cloned().collect();
    assert!(matches!(actions.as_slice(), [Command::Configure(options), Command::Install { app }]
        if options.install_locations.folder("vectorcraft") == Some(folder) && app == "vectorcraft"));
    h.get_by_label("VectorCraft встановлено: 0.5.0");

    h.get_by_label("Таблиця").click();
    h.run_steps(4);
    assert_eq!(h.state().as_ref().unwrap().settings.view, ViewMode::Table);
    h.get_by_label("Наявна");
    assert!(h.query_all_by_label_contains("14 жовт. 2026").count() >= 1);
    snapshot(&mut h, "table-uk-windows-dark");

    // Recreate the UI from serialized preferences, then switch back through the real picker.
    let saved = serde_json::to_string(&h.state().as_ref().unwrap().settings).unwrap();
    let restored: Settings = serde_json::from_str(&saved).unwrap();
    assert_eq!(restored.language, "uk");
    drop(h);
    let (mut reopened, _) = harness(restored);
    reopened.run_steps(8);
    reopened.get_by_label("Наявна");
    reopened.get_by_label("Налаштування").click();
    reopened.run_steps(3);
    reopened.get_by_value("Українська").click();
    reopened.run_steps(2);
    reopened.get_by_label("English").scroll_to_me();
    reopened.run_steps(16);
    reopened.get_by_label("English").click();
    reopened.run_steps(3);
    assert_eq!(reopened.state().as_ref().unwrap().settings.language, "en");
    reopened.get_by_label("Automatically download and install updates");
    reopened.get_by_label("Installing on Windows");
    reopened.get_by_label("Done").click();
    reopened.run_steps(3);
    reopened.get_by_label("Settings");
    assert!(reopened.query_by_label("Налаштування").is_none());
    assert!(reopened.query_all_by_label_contains("Oct 14, 2026").count() >= 1);
}
