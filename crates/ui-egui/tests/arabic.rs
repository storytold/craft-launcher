//! The Arabic UI, in its own test binary: the UI language is process-wide, so it can't share one
//! with the English UI tests. Arabic is drawn with the system's Arabic font and the window is
//! mirrored; where there is no font (minimal Linux installs), the UI stays in English and says why.
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
            externally_managed: false,
            origin: None,
            installation: None,
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
fn arabic_switches_persists_mirrors_and_keeps_install_actions() {
    use artcraft_launcher_ui_egui::i18n::Lang;
    let (mut h, sent) = harness_language(ViewMode::Grid, egui::Theme::Dark, true, "en");
    h.run_steps(6);
    h.get_by_label("Settings").click();
    h.run_steps(3);
    h.get_by_value("English").click();
    h.run_steps(2);
    // Opening Settings doesn't load the Arabic font: the picker names it in English.
    assert!(!artcraft_launcher_ui_egui::theme::cjk_font_loaded(Lang::AR));
    h.get_by_label("Arabic").click_accesskit();
    h.run_steps(3);
    if !artcraft_launcher_ui_egui::theme::cjk_font_available(Lang::AR) {
        // No font for Arabic: choosing it keeps the UI in English, left to right, and Settings says why.
        assert_eq!(h.state().as_ref().unwrap().settings.language, "ar");
        h.get_by_label_contains("No font for this language was found");
        assert!(!h.ctx.global_style().right_to_left);
        snapshot(&mut h, "settings-ar-no-font");
        return;
    }
    h.get_by_value("Arabic (العربية)");
    h.get_by_label("إعلامي عندما يتوفر تحديث لتطبيق مثبّت");
    snapshot(&mut h, "settings-ar-dark");
    let settings = h.state().as_ref().unwrap().settings.clone();
    assert_eq!(settings.language, "ar");
    let saved = serde_json::to_string(&settings).unwrap();
    let restored: Settings = serde_json::from_str(&saved).unwrap();
    assert_eq!(restored.language, "ar");
    h.get_by_label("تم").click_accesskit();
    h.run_steps(3);
    // egui's own parts read right to left too (scroll bars, menus).
    assert!(h.ctx.global_style().right_to_left);
    h.get_by_label("تحرير الصور");
    h.get_by_label_contains("آخر تحقق قبل 3 دقائق · المصدر: ArtCraft API");
    // A waiting update's Open half reads first: at the right of the update half.
    let open = h.get_by_label("فتح 0.4.1 دون تحديث. يبقى 0.5.0 متوفرًا.").rect();
    let update = h.get_by_label("تحديث إلى 0.5.0").rect();
    assert!(open.left() >= update.right(), "{open:?} {update:?}");
    // Mirrored: the settings button sits at the left, the first card at the right.
    let width = h.ctx.content_rect().width();
    assert!(h.get_by_label("الإعدادات").rect().center().x < width / 2.0);
    assert!(h.get_by_label("تفاصيل EffectCraft").rect().center().x > width / 2.0);
    snapshot(&mut h, "grid-ar-dark");
    // The app's page closes from its left, and the uninstall prompt reads right to left.
    h.get_by_label("تفاصيل PhotoCraft").click();
    h.run_steps(10);
    assert!(h.get_by_label("إغلاق (Esc)").rect().center().x < width / 2.0);
    snapshot(&mut h, "details-ar-dark");
    h.key_press(egui::Key::Escape);
    h.run_steps(10);
    h.get_by_label("مزيد من الإجراءات لـ LightCraft").click();
    h.run_steps(3);
    h.get_by_label("إلغاء التثبيت…").click();
    h.run_steps(3);
    snapshot(&mut h, "uninstall-ar-dark");
    h.get_by_label("إلغاء").click();
    h.run_steps(3);
    h.get_by_label("تحديث الكل (2)").click();
    h.run_steps(2);
    let installs: Vec<_> = sent.borrow().iter().filter_map(|c| if let Command::Install { app } = c { Some(app.clone()) } else { None }).collect();
    assert_eq!(installs, ["photocraft", "effectcraft"]);

    let (mut table, _) = harness_language(ViewMode::Table, egui::Theme::Light, true, &restored.language);
    table.run_steps(6);
    // The app column comes first, at the right.
    let app = table.get_by_label("التطبيق").rect().center().x;
    assert!(app > table.query_all_by_label("الإصدار").map(|n| n.rect().center().x).fold(0.0, f32::max));
    assert!(table.query_all_by_label_contains("أكتوبر 2026").count() >= 1);
    snapshot(&mut table, "table-ar-light");
    // A narrow window scrolls the table sideways, starting at its first column, at the right.
    table.set_size(egui::vec2(720.0, 900.0));
    table.run_steps(6);
    let app = table.get_by_label("التطبيق").rect();
    assert!(app.left() >= 0.0 && app.right() <= 720.0, "{app:?}");
    snapshot(&mut table, "table-ar-narrow");
    let (mut web, _) = harness_language(ViewMode::Grid, egui::Theme::Dark, false, &restored.language);
    web.run_steps(6);
    assert!(web.query_all_by_label("تنزيل لنظام macOS").count() >= 3);
}
