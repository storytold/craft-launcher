//! The Vietnamese UI, in its own test binary: the UI language is process-wide, so it can't share one
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
    os: Os,
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
    // A backend message, translated through its `@msg` template where it's shown.
    ev.push(Event::JobFinished { app: "lightcraft".into(), result: Ok("LightCraft updated to 0.5.0".into()) });
    ev
}

fn harness(view: ViewMode) -> Harness<'static, Option<LauncherUi>> {
    harness_on(view, Os::MacOs)
}

fn harness_on(view: ViewMode, os: Os) -> Harness<'static, Option<LauncherUi>> {
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let mut backend = Some(Fake { events: events(), clock: Rc::new(Cell::new(NOW)), os });
    let settings =
        Settings { view, notify: false, language: "vi".into(), theme: artcraft_launcher_ui_egui::theme::ThemeChoice::Dark, ..Settings::default() };
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

fn snapshot(h: &mut Harness<'_, Option<LauncherUi>>, name: &str) {
    let Some(dir) = std::env::var_os("ARTCRAFT_LAUNCHER_SNAPSHOTS") else { return };
    let img = h.render().unwrap();
    img.save(std::path::Path::new(&dir).join(format!("{name}.png"))).unwrap();
}

// One test: the UI language is process-wide.
#[test]
fn vietnamese_ui_translates_labels_dates_and_messages() {
    let mut h = harness(ViewMode::Grid);
    h.run_steps(8);
    snapshot(&mut h, "grid-vi-dark");
    // Product names stay in English.
    assert!(h.query_by_label("Launcher").is_none());
    h.get_by_label("Craft App Suite");
    h.get_by_label("Chỉnh sửa ảnh");
    h.get_by_label("Cập nhật lên 0.5.0");
    h.get_by_label_contains("Đang kiểm tra mã kiểm…");
    h.get_by_label_contains("Đã kiểm tra 3 phút trước · qua ArtCraft API");
    h.get_by_label("Đã cập nhật LightCraft lên 0.5.0");

    let mut table = harness(ViewMode::Table);
    table.run_steps(8);
    snapshot(&mut table, "table-vi-dark");
    table.get_by_label("Phiên bản");
    assert!(table.query_all_by_label_contains("Thg 8, 2026").count() >= 1);
    check_settings_layout();
}

/// Checks the actual modal after resizing it while open, including Linux's extra controls.
fn check_settings_layout() {
    let mut h = harness_on(ViewMode::Grid, Os::Linux);
    h.run_steps(8);
    h.get_by_label("Tùy chọn").click();
    h.run_steps(8);
    for (width, height) in [(1180.0, 900.0), (720.0, 500.0), (480.0, 500.0), (1180.0, 900.0)] {
        h.set_size(egui::vec2(width, height));
        h.run_steps(8);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, height));
        let done = h.get_by_label("Xong").rect();
        let quit = h.get_by_label("Thoát ArtCraft Launcher").rect();
        assert!(screen.contains_rect(done), "{width}x{height}: Done is clipped: {done:?}");
        assert!(screen.contains_rect(quit), "{width}x{height}: Quit is clipped: {quit:?}");
        assert!(!done.intersects(quit));
        let title = h.get_all_by_label("Tùy chọn").last().unwrap().rect();
        assert!(screen.contains_rect(title), "{width}x{height}: title is clipped: {title:?}");
        h.get_by_label("Theo hệ thống").scroll_to_me();
        h.run_steps(8);
        let theme = h.get_by_label("Theo hệ thống").rect();
        assert!(theme.bottom() < done.top(), "appearance controls must scroll above the fixed actions");
        h.get_by_label("Tham gia Discord").scroll_to_me();
        h.run_steps(8);
        let discord = h.get_by_label("Tham gia Discord").rect();
        assert!(screen.contains_rect(discord), "community links must be reachable by scrolling");
        let mut links = vec![discord];
        for label in ["Trang web", "GitHub", "X", "Instagram", "LinkedIn", "YouTube", "TikTok"] {
            let candidates: Vec<_> = h
                .query_all_by_label(label)
                .map(|n| n.rect())
                .filter(|r| r.top() >= discord.top() - 1.0 && r.bottom() < done.top() && r.height() > 20.0)
                .collect();
            assert_eq!(candidates.len(), 1, "{width}x{height}: settings link {label}: {candidates:?}");
            links.extend(candidates);
        }
        for (i, r) in links.iter().enumerate() {
            assert!(screen.contains_rect(*r), "{width}x{height}: link is clipped: {r:?}");
            for other in links.iter().skip(i + 1) {
                assert!(!r.intersects(*other), "{width}x{height}: links overlap: {r:?}, {other:?}");
            }
        }
        snapshot(&mut h, &format!("settings-vi-{width}x{height}-bottom"));
        h.get_by_label("Ngôn ngữ").scroll_to_me();
        h.run_steps(8);
        let language = h.get_by_label("Ngôn ngữ").rect();
        assert!(screen.contains_rect(language), "the top of settings must remain reachable");
        snapshot(&mut h, &format!("settings-vi-{width}x{height}-top"));
    }
    h.get_by_label("Xong").click();
    h.run_steps(4);
    assert!(h.query_by_label("Xong").is_none(), "Done must still close the resized dialog");
}
