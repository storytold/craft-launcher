//! The Turkish UI, in its own test binary: the UI language is process-wide, so it can't share one
//! with the English UI tests. `ARTCRAFT_LAUNCHER_SNAPSHOTS=<dir>` also saves screenshots.

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
    os: Os,
}

impl Backend for Fake {
    fn send(&mut self, _command: Command) {}
    fn poll(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { manage_apps: true, reveal_label: "Show in Files", self_update: true }
    }
    fn platform(&self) -> Option<Platform> {
        Some(Platform { os: self.os, arch: Arch::Arm64 })
    }
    fn now(&self) -> i64 {
        self.clock.get()
    }
}

struct DesktopHost;
impl Host for DesktopHost {
    fn has_tray(&self) -> bool {
        true
    }
    fn login_item(&self) -> Option<bool> {
        Some(false)
    }
}

fn release(id: &str, v: &str, at: i64) -> Release {
    let name = format!("{id}-{v}-linux-x86_64.tar.gz");
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
            source: InstallSource::Launcher,
            location: format!("/opt/{}.tar.gz", app.name),
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
    let mut backend = Some(Fake { events: events(), clock: Rc::new(Cell::new(NOW)), os: Os::Linux });
    let settings =
        Settings { view, notify: false, language: "tr".into(), theme: artcraft_launcher_ui_egui::theme::ThemeChoice::Dark, ..Settings::default() };
    Harness::builder().with_size(egui::vec2(1180.0, 900.0)).with_theme(egui::Theme::Dark).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), settings.clone()));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut DesktopHost);
                l.ui(ui, &mut DesktopHost);
            }
        },
        None,
    )
}

#[test]
fn turkish_ui_translates_labels_dates_and_messages() {
    let mut h = harness(ViewMode::Grid);
    h.run_steps(8);
    // Product names stay in English.
    h.get_by_label("Launcher");
    h.get_by_label("Craft App Suite");
    h.get_by_label("Görsel düzenleme");
    h.get_by_label("0.5.0 sürümüne güncelle");
    h.get_by_label_contains("Sağlama toplamı doğrulanıyor…");
    h.get_by_label_contains("Denetlendi: 3 dk. önce · ArtCraft API");
    h.get_by_label("LightCraft, 0.5.0 sürümüne güncellendi");

    let mut table = harness(ViewMode::Table);
    table.run_steps(8);
    table.get_by_label("Yüklü");
    assert!(table.query_all_by_label_contains("Ağu 2026").count() >= 1);
}
