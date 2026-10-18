//! Every table column header must be drawn in full, in every language: a localized label longer
//! than the column ("Zainstalowana" - PL) widens its column instead of being cut off at the edge.
//! One test: the UI language is process-wide. `ARTCRAFT_LAUNCHER_SNAPSHOTS=<dir>` also saves
//! screenshots (run with `-- --test-threads=1`).

use std::cell::Cell;
use std::rc::Rc;

use artcraft_launcher_core::protocol::{Backend, Capabilities, Command, Event, LatestRelease, Origin, Source};
use artcraft_launcher_core::release::{Asset, Release};
use artcraft_launcher_core::status::InstallSource;
use artcraft_launcher_core::{Arch, Installed, Os, Platform, SUITE, version};
use artcraft_launcher_ui_egui::i18n::{Lang, tr, tr_ctx};
use artcraft_launcher_ui_egui::settings::ViewMode;
use artcraft_launcher_ui_egui::{Host, LauncherUi, Settings, theme};
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
        Capabilities { manage_apps: true, reveal_label: "Show in Finder", self_update: true }
    }
    fn platform(&self) -> Option<Platform> {
        Some(Platform { os: Os::MacOs, arch: Arch::Arm64 })
    }
    fn now(&self) -> i64 {
        self.clock.get()
    }
}

struct NoHost;
impl Host for NoHost {}

fn release(id: &str, v: &str, at: i64) -> Release {
    let name = format!("{id}-{v}-macos-universal.dmg");
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
        // One app with an update, one up to date: their rows carry the full set of badges and
        // buttons, so the columns are measured against real content.
        let installed = match app.id {
            "photocraft" | "lightcraft" => {
                let v = if app.id == "photocraft" { "0.4.1" } else { "0.5.0" };
                Some(Installed {
                    version: version::parse(v),
                    source: InstallSource::MacApp,
                    location: format!("/Applications/{}.app", app.name),
                    package: None,
                })
            }
            _ => None,
        };
        ev.push(Event::Detected { app: app.id.into(), installed });
    }
    ev.push(Event::CheckFinished { at: NOW - 180, source: Source::Feed, rate_limited_until: None });
    ev
}

fn harness(language: &str) -> Harness<'static, Option<LauncherUi>> {
    let mut backend = Some(Fake { events: events(), clock: Rc::new(Cell::new(NOW)) });
    let settings = Settings { view: ViewMode::Table, notify: false, language: language.into(), ..Settings::default() };
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

fn snapshot(h: &mut Harness<'_, Option<LauncherUi>>, name: &str) {
    let Some(dir) = std::env::var_os("ARTCRAFT_LAUNCHER_SNAPSHOTS") else { return };
    let img = h.render().unwrap();
    img.save(std::path::Path::new(&dir).join(format!("{name}.png"))).unwrap();
}

/// What a table header needs in points: its label in the HUD face, upper case, plus the sort
/// arrow. This mirrors what `views::table` gives the column as its floor and what the header
/// cell claims as its width; the two must always agree.
fn header_needs(ctx: &egui::Context, label: &str) -> f32 {
    let format = egui::TextFormat {
        font_id: egui::FontId::new(theme::HUD_SIZE, theme::mono()),
        color: egui::Color32::PLACEHOLDER,
        extra_letter_spacing: theme::HUD_SPACING,
        ..Default::default()
    };
    let job = egui::text::LayoutJob::single_section(label.to_uppercase(), format);
    ctx.fonts_mut(|f| f.layout_job(job)).size().x + theme::HEADER_ARROW_W
}

#[test]
fn table_headers_fit_in_every_language() {
    artcraft_launcher_ui_egui::i18n::set_test_lang(Lang::EN);
    for lang in Lang::all() {
        let mut h = harness(lang.code());
        h.run_steps(8);
        snapshot(&mut h, &format!("table-headers-{}", lang.code()));
        // A CJK language is drawn in English where the system has no CJK font (`effective_lang`):
        // the headers then read English too.
        let shown = if lang.needs_cjk_font() && !theme::cjk_font_available(lang) { Lang::EN } else { lang };
        for label in [
            tr(shown, "App"),
            tr(shown, "Status"),
            tr_ctx(shown, "column", "Installed"),
            tr_ctx(shown, "column", "Latest"),
            tr_ctx(shown, "column", "Released"),
        ] {
            let needs = header_needs(&h.ctx, label);
            let node = h.get_by_label(label);
            let has = node.rect().width();
            assert!(has + 1.0 >= needs, "{}: \"{label}\" needs {needs:.1} points but its column is {has:.1}", lang.code());
        }
    }
}
