//! The Korean UI, in its own test binary: the UI language is process-wide, so it can't share one
//! with the English UI tests. Korean is drawn with the system's Korean font; where there is
//! none (CI, minimal Linux installs), the UI stays in English and says why.
//! `ARTCRAFT_LAUNCHER_SNAPSHOTS=<dir>` also saves screenshots (run with `-- --test-threads=1`).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use artcraft_launcher_core::protocol::{Backend, Capabilities, Command, Event, JobPhase, LatestRelease, Origin, PausedDownload, Source};
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
    sent: Rc<RefCell<Vec<Command>>>,
    manage: bool,
    clock: Rc<Cell<i64>>,
}

impl Backend for Fake {
    fn send(&mut self, command: Command) {
        self.sent.borrow_mut().push(command);
    }
    fn poll(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { manage_apps: self.manage, reveal_label: "Show in Finder", self_update: self.manage }
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
        notes: "## What's Changed\n* Faster things by @a in https://github.com/storytold/x/pull/1".into(),
        prerelease: false,
        assets: vec![Asset {
            name: name.clone(),
            size: 150 << 20,
            url: format!("https://github.com/storytold/{id}/releases/download/v{v}/{name}"),
            sha256: None,
        }],
    }
}

/// A realistic mix: updates, up-to-date apps, a download in progress, one app with no release.
fn events() -> Vec<Event> {
    let mut ev = Vec::new();
    let versions = [
        ("photocraft", "0.5.0"),
        ("vectorcraft", "0.7.0"),
        ("filmcraft", "0.4.0"),
        ("lightcraft", "0.4.0"),
        ("pdfcraft", "0.4.0"),
        ("effectcraft", "0.6.0"),
        ("designcraft", "0.4.0"),
        ("wordcraft", "0.3.0"),
        ("deckcraft", "0.3.0"),
        ("gridcraft", "0.3.0"),
        ("cadcraft", "0.3.0"),
    ];
    for (i, (id, v)) in versions.iter().enumerate() {
        ev.push(Event::Latest {
            app: (*id).into(),
            latest: LatestRelease::Found(release(id, v, NOW - 86_400 - i as i64 * 600)),
            from: Origin::ArtCraftApi,
        });
    }
    ev.push(Event::Latest { app: "soundcraft".into(), latest: LatestRelease::None, from: Origin::ArtCraftApi });
    let installed = [("photocraft", "0.4.1"), ("lightcraft", "0.4.0"), ("wordcraft", "0.3.0"), ("effectcraft", "0.5.0")];
    for app in &SUITE {
        let inst = installed.iter().find(|(id, _)| *id == app.id).map(|(_, v)| Installed {
            version: version::parse(v),
            source: InstallSource::MacApp,
            location: format!("/Applications/{}.app", app.name),
            package: None,
        });
        ev.push(Event::Detected { app: app.id.into(), installed: inst });
    }
    ev.push(Event::Progress { app: "filmcraft".into(), phase: JobPhase::Downloading { done: 48 << 20, total: 131 << 20 } });
    ev.push(Event::Paused { app: "deckcraft".into(), download: Some(PausedDownload { done: 30 << 20, total: 120 << 20 }) });
    ev.push(Event::Progress { app: "gridcraft".into(), phase: JobPhase::Reconnecting { done: 10 << 20, total: 90 << 20 } });
    ev.push(Event::CheckFinished { at: NOW - 180, source: Source::GitHubApi, rate_limited_until: None });
    ev
}

fn harness_language(
    view: ViewMode,
    theme: egui::Theme,
    manage: bool,
    language: &str,
) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let sent = Rc::new(RefCell::new(Vec::new()));
    let sent2 = sent.clone();
    let mut backend = Some(Fake { events: events(), sent: sent2, manage, clock: Rc::new(Cell::new(NOW)) });
    let settings = Settings { view, notify: false, language: language.into(), ..Settings::default() };
    let harness = Harness::builder().with_size(egui::vec2(1180.0, 900.0)).with_theme(theme).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                let mut s = settings.clone();
                s.theme = if theme == egui::Theme::Dark {
                    artcraft_launcher_ui_egui::theme::ThemeChoice::Dark
                } else {
                    artcraft_launcher_ui_egui::theme::ThemeChoice::Light
                };
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), s));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut NoHost);
                l.ui(ui, &mut NoHost);
            }
        },
        None,
    );
    (harness, sent)
}

fn snapshot(h: &mut Harness<'_, Option<LauncherUi>>, name: &str) {
    let Some(dir) = std::env::var_os("ARTCRAFT_LAUNCHER_SNAPSHOTS") else { return };
    let img = h.render().unwrap();
    img.save(std::path::Path::new(&dir).join(format!("{name}.png"))).unwrap();
}

// One test: the UI language is process-wide.
#[test]
fn korean_switches_persists_and_keeps_install_actions() {
    use artcraft_launcher_ui_egui::i18n::Lang;
    let (mut h, sent) = harness_language(ViewMode::Grid, egui::Theme::Dark, true, "en");
    h.run_steps(6);
    h.get_by_label("Settings").click();
    h.run_steps(3);
    h.get_by_value("English").click();
    h.run_steps(2);
    // Opening Settings doesn't load the (large) Korean font: the picker names it in English.
    assert!(!artcraft_launcher_ui_egui::theme::cjk_font_loaded(Lang::KO));
    // The list is long enough to scroll, so the last option may be out of view: click it through
    // accesskit, which reaches widgets that aren't visible.
    h.get_by_label("Korean").click_accesskit();
    h.run_steps(3);
    let settings = h.state().as_ref().unwrap().settings.clone();
    assert_eq!(settings.language, "ko");
    let saved = serde_json::to_string(&settings).unwrap();
    let restored: Settings = serde_json::from_str(&saved).unwrap();
    assert_eq!(restored.language, "ko");
    if !artcraft_launcher_ui_egui::theme::cjk_font_available(Lang::KO) {
        // No font for Korean: choosing it keeps the UI in English, and Settings says why.
        assert_eq!(h.state().as_ref().unwrap().settings.language, "ko");
        h.get_by_label_contains("No font for this language was found");
        h.get_by_label("Notify me when an installed app has an update");
        snapshot(&mut h, "settings-ko-no-font");
        return;
    }
    // Choosing it loaded the font: now the picker shows both names.
    h.get_by_value("Korean (한국어)");
    h.get_by_label("설치된 앱에 업데이트가 있으면 알림");
    h.run_steps(20);
    snapshot(&mut h, "settings-ko-dark");
    h.get_by_label("완료").click();
    h.run_steps(3);
    h.get_by_label("이미지 편집");
    h.get_by_label_contains("3분 전 확인 · ArtCraft API 사용");
    h.get_by_label("0.5.0 버전으로 업데이트");
    // Product names stay in English.
    h.get_by_label("Launcher");
    snapshot(&mut h, "grid-ko-dark");
    h.get_by_label("모두 업데이트(2개)").click();
    h.run_steps(2);
    let installs: Vec<_> = sent.borrow().iter().filter_map(|c| if let Command::Install { app } = c { Some(app.clone()) } else { None }).collect();
    assert_eq!(installs, ["photocraft", "effectcraft"]);

    let (mut table, _) = harness_language(ViewMode::Table, egui::Theme::Light, true, &restored.language);
    table.run_steps(6);
    table.get_by_label("설치됨");
    assert!(table.query_all_by_label_contains("년 10월").count() >= 1);
    snapshot(&mut table, "table-ko-light");
    let (mut web, _) = harness_language(ViewMode::Grid, egui::Theme::Dark, false, &restored.language);
    web.run_steps(6);
    assert!(web.query_all_by_label("macOS용 다운로드").count() >= 3);
}
