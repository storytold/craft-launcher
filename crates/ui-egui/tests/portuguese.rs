//! The Brazilian Portuguese (pt-BR) UI test and snapshot generator.

use std::cell::Cell;
use std::rc::Rc;

use artcraft_launcher_core::protocol::{Backend, Capabilities, Command, Event, JobPhase, LatestRelease, Origin, Source};
use artcraft_launcher_core::release::{Asset, Release};
use artcraft_launcher_core::status::InstallSource;
use artcraft_launcher_core::{Arch, Installed, Os, Platform, SUITE, version};
use artcraft_launcher_ui_egui::settings::ViewMode;
use artcraft_launcher_ui_egui::{Host, LauncherUi, Settings};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

const NOW: i64 = 1_791_500_000;

struct Fake {
    events: Vec<Event>,
    clock: Rc<Cell<i64>>,
}

impl Backend for Fake {
    fn send(&mut self, _command: Command) {}
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
        self.clock.get()
    }
}

struct NoHost;
impl Host for NoHost {}

fn release(id: &str, v: &str, at: i64) -> Release {
    let name = format!("{id}-{v}-windows-x64.msi");
    Release {
        tag: format!("v{v}"),
        version: version::parse(v),
        name: format!("v{v}"),
        published_at: Some(at),
        html_url: format!("https://github.com/storytold/{id}/releases/tag/v{v}"),
        notes: String::new(),
        prerelease: false,
        assets: vec![Asset {
            name: name.clone(),
            size: 150 << 20,
            url: format!("https://github.com/storytold/{id}/releases/download/v{v}/{name}"),
            sha256: None,
        }],
    }
}

fn events() -> Vec<Event> {
    let mut ev = Vec::new();
    for (i, app) in SUITE.iter().enumerate() {
        let latest = LatestRelease::Found(release(app.id, "0.5.0", NOW - 40 * 86_400 - i as i64 * 600));
        ev.push(Event::Latest { app: app.id.into(), latest, from: Origin::ArtCraftApi });
        let installed = matches!(app.id, "photocraft" | "lightcraft").then(|| Installed {
            version: version::parse(if app.id == "photocraft" { "0.4.1" } else { "0.5.0" }),
            source: InstallSource::Msi,
            location: format!("C:\\Program Files\\{}\\{}.exe", app.name, app.id),
            package: None,
        });
        ev.push(Event::Detected { app: app.id.into(), installed });
    }
    ev.push(Event::Progress { app: "filmcraft".into(), phase: JobPhase::Verifying });
    ev.push(Event::CheckFinished { at: NOW - 180, source: Source::Feed, rate_limited_until: None });
    ev.push(Event::JobFinished { app: "lightcraft".into(), result: Ok("LightCraft updated to 0.5.0".into()) });
    ev
}

fn harness(view: ViewMode) -> Harness<'static, Option<LauncherUi>> {
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let mut backend = Some(Fake { events: events(), clock: Rc::new(Cell::new(NOW)) });
    let settings =
        Settings { view, notify: false, language: "pt-BR".into(), theme: artcraft_launcher_ui_egui::theme::ThemeChoice::Dark, ..Settings::default() };
    Harness::builder().with_size(egui::vec2(1180.0, 900.0)).with_theme(egui::Theme::Dark).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), settings.clone()));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut NoHost);
                l.ui(ui, &mut NoHost);
            }
        },
        None,
    )
}

#[test]
fn portuguese_ui_translates_labels_dates_and_messages() {
    let mut h = harness(ViewMode::Grid);
    h.run_steps(8);
    h.get_by_label("Launcher");
    h.get_by_label("Craft App Suite");
    h.get_by_label("Edição de imagens");
    h.get_by_label("Atualizar para 0.5.0");
    h.get_by_label_contains("Conferindo a soma de verificação…");
    h.get_by_label_contains("Verificado há 3 min · via ArtCraft API");
    h.get_by_label("LightCraft atualizado para 0.5.0");

    let mut table = harness(ViewMode::Table);
    table.run_steps(8);
    table.get_by_label("Instalada");
    assert!(table.query_all_by_label_contains("de ago. de 2026").count() >= 1);
}
