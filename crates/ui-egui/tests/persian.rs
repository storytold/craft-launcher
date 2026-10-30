//! The Persian UI, in its own test binary: the UI language is process-wide, so it can't share one
//! with the English UI tests. `ARTCRAFT_LAUNCHER_SNAPSHOTS=<dir>` also saves screenshots (run with
//! `-- --test-threads=1`, see `ui.rs`).

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
    fn send(&mut self, command: Command) {
        if let Command::FetchScreenshots { app } = command {
            self.events.extend(preview_pictures(&app));
        }
    }
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
        let installed = matches!(app.id, "photocraft" | "lightcraft").then(|| Installed {
            version: version::parse(if app.id == "photocraft" { "0.4.1" } else { "0.5.0" }),
            source: InstallSource::MacApp,
            location: format!("/Applications/{}.app", app.name),
            package: None,
        });
        ev.push(Event::Detected { app: app.id.into(), installed });
    }
    ev.push(Event::Progress { app: "filmcraft".into(), phase: JobPhase::Verifying });
    ev.push(Event::CheckFinished { at: NOW - 180, source: Source::Feed, rate_limited_until: None });

    ev
}

fn harness(view: ViewMode) -> Harness<'static, Option<LauncherUi>> {
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let mut backend = Some(Fake { events: events(), clock: Rc::new(Cell::new(NOW)) });
    let settings =
        Settings { view, notify: false, language: "fa".into(), theme: artcraft_launcher_ui_egui::theme::ThemeChoice::Dark, ..Settings::default() };
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

// Keep language isolated in this test binary.
#[test]
fn persian_content_preserves_chrome_and_interactions() {
    use artcraft_launcher_ui_egui::i18n::{Lang, lang_from_tag, tr};
    assert_eq!(lang_from_tag("fa_IR.UTF-8"), Some(Lang::FA));
    assert!(Lang::FA.is_rtl());
    assert_eq!(tr(Lang::FA, "Settings"), "تنظیمات");
    let mut h = harness(ViewMode::Grid);
    h.run_steps(8);
    h.get_by_label("تنظیمات");
    h.get_by_label("به‌روزرسانی به 0.5.0");
    let photo = h.get_by_label("PhotoCraft").rect();
    let light = h.get_by_label("VectorCraft").rect();
    assert!(photo.center().x > light.center().x, "content cards must start from the right");
    h.get_by_label("طراحی وکتور"); // Accessibility must receive logical, not display-order text.
    snapshot(&mut h, "grid-fa-dark");
    h.get_by_role(egui::accesskit::Role::TextInput).click();
    h.run_steps(2);
    h.get_by_role(egui::accesskit::Role::TextInput).type_text("وکتور");
    h.run_steps(5);
    h.get_by_label("VectorCraft");
    assert!(h.query_by_label("PhotoCraft").is_none(), "search matches the displayed Persian description");
    let input = h.get_by_role(egui::accesskit::Role::TextInput);
    input.focus();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.key_press(egui::Key::Backspace);
    h.run_steps(5);
    check_popups(&mut h);
    check_details(&mut h);
    h.get_by_label("تنظیمات").click();
    h.run_steps(4);
    snapshot(&mut h, "settings-fa-dark");
    h.get_by_label("بستن").click();
    h.run_steps(4);
    if let Some(l) = h.state_mut().as_mut() {
        l.settings.theme = artcraft_launcher_ui_egui::theme::ThemeChoice::Light;
    }
    h.run_steps(4);
    snapshot(&mut h, "grid-fa-light");
    let mut table = harness(ViewMode::Table);
    table.run_steps(8);
    let app = table.get_by_label("برنامه").rect();
    let status = table.get_by_label("وضعیت").rect();
    assert!(app.center().x < status.center().x, "app column must retain its English position");
    snapshot(&mut table, "table-fa-dark");
    h.set_size(egui::vec2(860.0, 800.0));
    h.run_steps(5);
    snapshot(&mut h, "grid-fa-narrow");
    h.set_size(egui::vec2(1180.0, 772.0));
    h.run_steps(5);
    h.get_by_label("تنظیمات").click();
    h.run_steps(5);
    let close_fa = h.get_by_label("بستن").rect();
    assert!(close_fa.bottom() < 772.0, "fixed settings footer stays inside the viewport");
    snapshot(&mut h, "settings-fa-772");
    h.get_by_label("بستن").click();
    h.run_steps(3);
    if let Some(l) = h.state_mut().as_mut() {
        l.settings.language = "en".into();
    }
    h.run_steps(5);
    h.get_by_label("Settings").click();
    h.run_steps(5);
    snapshot(&mut h, "settings-en-772");
    let close_en = h.get_by_label("Done").rect();
    assert!((close_fa.right() - close_en.right()).abs() < 2.0, "settings footer remains anchored like English");
    h.get_by_label("Done").click();
    h.run_steps(3);
    if let Some(l) = h.state_mut().as_mut() {
        l.settings.language = "fa".into();
    }
    h.set_size(egui::vec2(1180.0, 600.0));
    h.run_steps(5);
    h.get_by_label("تنظیمات").click();
    h.run_steps(5);
    assert!(h.get_by_label("بستن").rect().bottom() < 600.0, "compact windows retain the fixed footer");
    snapshot(&mut h, "settings-fa-600");
}

// Preview-only fixture loader. Production keeps its existing pinned, bounded image backend.
fn preview_pictures(app: &str) -> Vec<Event> {
    use artcraft_launcher_core::details::{AppDetails, MAX_SCREENSHOT_BYTES, looks_like_jpeg};
    let root = std::env::var_os("ARTCRAFT_PREVIEW_SHOTS").map(std::path::PathBuf::from);
    let Some(details) = AppDetails::by_id(app) else { return Vec::new() };
    details
        .shots
        .iter()
        .map(|shot| {
            let result = root.as_ref().ok_or_else(|| "Preview pictures unavailable".to_owned()).and_then(|root| {
                let data = std::fs::read(root.join(details.id).join(format!("{}.jpg", shot.key))).map_err(|e| e.to_string())?;
                if data.len() > MAX_SCREENSHOT_BYTES || !looks_like_jpeg(&data) {
                    return Err("Invalid preview JPEG".into());
                }
                Ok(data)
            });
            Event::Screenshot { app: app.to_owned(), key: shot.key.to_owned(), result }
        })
        .collect()
}

fn check_details(h: &mut Harness<'_, Option<LauncherUi>>) {
    use artcraft_launcher_ui_egui::i18n::{Lang, tr_ctx};
    // Editorial copy may fall back independently when an upstream description changes.
    let changed = "A newly changed upstream product description.";
    assert_eq!(tr_ctx(Lang::FA, "photocraft", changed), changed);
    h.get_by_label("جزئیات PhotoCraft").click();
    h.run_steps(12);
    h.get_all_by_label("ویرایشگر تصویری که کار با آن را از قبل بلدید.").last().unwrap();
    h.get_by_label("وب‌سایت ↗");
    h.get_by_label("منحنی‌ها و لایه‌های متن · 1/4");
    snapshot(h, "details-photo-fa-dark");
    if let Some(l) = h.state_mut().as_mut() {
        l.settings.theme = artcraft_launcher_ui_egui::theme::ThemeChoice::Light;
    }
    h.run_steps(6);
    snapshot(h, "details-photo-fa-light");
    if let Some(l) = h.state_mut().as_mut() {
        l.settings.theme = artcraft_launcher_ui_egui::theme::ThemeChoice::Dark;
    }
    h.run_steps(6);
    h.get_by_label("مدیریت حرفه‌ای رنگ").scroll_to_me();
    h.run_steps(8);
    snapshot(h, "details-photo-features-fa");
    h.get_by_label("تاریخ انتشار").scroll_to_me();
    h.run_steps(8);
    snapshot(h, "details-photo-release-fa");
    let version = h.get_by_label("نسخه").rect();
    let released = h.get_by_label("تاریخ انتشار").rect();
    assert!(version.center().x > released.center().x, "release facts read from right to left");
    h.key_press(egui::Key::Escape);
    h.run_steps(12);
    h.set_size(egui::vec2(860.0, 800.0));
    h.run_steps(6);
    h.get_by_label("جزئیات EffectCraft").scroll_to_me();
    h.run_steps(6);
    h.get_by_label("جزئیات EffectCraft").click();
    h.run_steps(12);
    h.get_all_by_label("Motion graphics and visual effects.").last().unwrap().scroll_to_me();
    h.run_steps(8);
    snapshot(h, "details-effect-fa-narrow");
    h.get_by_label("انیمیشن‌سازی با روشی آشنا").scroll_to_me();
    h.run_steps(8);
    snapshot(h, "details-effect-features-fa-narrow");
    h.key_press(egui::Key::Escape);
    h.run_steps(12);
    h.set_size(egui::vec2(1180.0, 900.0));
    h.run_steps(8);
}

fn check_popups(h: &mut Harness<'_, Option<LauncherUi>>) {
    use artcraft_launcher_ui_egui::settings::{CheckEvery, SortKey};
    // The first menu belongs to PhotoCraft, an installed app with a newer release.
    h.get_by_label("گزینه‌های بیشتر برای PhotoCraft").click();
    h.run_steps(6);
    let notes = h.get_by_label("یادداشت‌های انتشار").rect();
    let reveal = h.get_by_label("نمایش در Finder").rect();
    assert!((notes.right() - reveal.right()).abs() < 1.0, "popup rows share a right edge");
    assert!(notes.width() >= 200.0, "menu items keep a roomy click target");
    snapshot(h, "product-menu-fa-dark");
    if let Some(l) = h.state_mut().as_mut() {
        l.settings.theme = artcraft_launcher_ui_egui::theme::ThemeChoice::Light;
    }
    h.run_steps(5);
    snapshot(h, "product-menu-fa-light");
    if let Some(l) = h.state_mut().as_mut() {
        l.settings.theme = artcraft_launcher_ui_egui::theme::ThemeChoice::Dark;
    }
    h.run_steps(5);
    h.key_press(egui::Key::Escape);
    h.run_steps(6);
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "مرتب‌سازی").click();
    h.run_steps(6);
    snapshot(h, "sort-menu-fa-dark");
    h.set_size(egui::vec2(860.0, 600.0));
    h.run_steps(6);
    snapshot(h, "sort-menu-fa-narrow");
    h.set_size(egui::vec2(1180.0, 900.0));
    h.run_steps(6);
    h.get_by_label("نام (الف تا ی)").click();
    h.run_steps(6);
    assert_eq!(h.state().as_ref().unwrap().settings.sort, SortKey::Name);
    if let Some(l) = h.state_mut().as_mut() {
        l.settings.sort = SortKey::Suite;
    }
    h.run_steps(6);
    h.get_by_label("تنظیمات").click();
    h.run_steps(6);
    h.get_by_value("خودکار").click();
    h.run_steps(6);
    snapshot(h, "update-menu-fa-dark");
    h.get_by_label("فقط با درخواست من").click();
    h.run_steps(6);
    assert_eq!(h.state().as_ref().unwrap().settings.check_every, CheckEvery::Never);
    h.get_by_value("فقط با درخواست من"); // Native combo metadata stays logical too.
    h.get_by_value("فارسی").click();
    h.run_steps(6);
    snapshot(h, "language-menu-fa-dark");
    h.get_all_by_label("فارسی").last().unwrap().scroll_to_me();
    h.run_steps(8);
    snapshot(h, "language-menu-fa-scrolled");
    let option = h.get_all_by_label("فارسی").last().unwrap().rect();
    let scroll = h
        .get_all_by_role(egui::accesskit::Role::ScrollBar)
        .find(|node| {
            let r = node.rect();
            r.right() < option.left() && r.right() > option.left() - 40.0 && r.top() < option.bottom() && r.bottom() > option.top()
        })
        .expect("the language list has its own reserved scrollbar")
        .rect();
    assert!(option.left() - scroll.right() >= 10.0, "scrollbar does not overlap RTL menu rows");
    h.key_press(egui::Key::Escape);
    h.run_steps(6);
    h.get_by_label("بستن").click();
    h.run_steps(6);
}
