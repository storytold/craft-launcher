//! The Fonts page: Craft Font Manager beside the apps in the toolbar's page toggle, and its
//! download switch, preview settings and sources in Settings. The page runs against the bundled Google Fonts
//! index, a font folder made per test (the launcher's own Inter, Archivo and Geist Mono, and
//! egui's Ubuntu Light, as installed fonts) and a fake font store.
//! `ARTCRAFT_LAUNCHER_SNAPSHOTS=<dir>` also saves screenshots; `ARTCRAFT_LAUNCHER_SNAPSHOT_FONTS=<dir>`
//! adds the fonts in that folder to the installed ones, and
//! `ARTCRAFT_LAUNCHER_SNAPSHOT_GOOGLE_FONTS=<google/fonts checkout>` has the fake store "download"
//! real files from it.
#![cfg(not(target_arch = "wasm32"))]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use artcraft_launcher_core::protocol::{Backend, Capabilities, Command, Event};
use artcraft_launcher_core::{Os, Platform};
use artcraft_launcher_ui_egui::{Host, LauncherUi, Page, Settings};
use craft_font_manager::craft_font_catalog::Os as FontOs;
use craft_font_manager::index::Index;
use craft_font_manager::store::FakeStore;
use craft_font_manager::{Catalog, FontEnv, Options, Runner};
use egui::accesskit::Role;
use egui_kittest::Harness;
use egui_kittest::kittest::{By, Queryable};

struct Quiet;

impl Backend for Quiet {
    fn send(&mut self, _: Command) {}
    fn poll(&mut self) -> Vec<Event> {
        Vec::new()
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { manage_apps: true, reveal_label: "Show in file manager", self_update: false, desktop_shortcuts: false }
    }
    fn platform(&self) -> Option<Platform> {
        Some(Platform { os: Os::Linux, arch: artcraft_launcher_core::Arch::X64 })
    }
    fn now(&self) -> i64 {
        1_760_000_000
    }
}

struct NoHost;
impl Host for NoHost {}

/// Fonts on "the system": the launcher's own faces, egui's Ubuntu Light, and any in
/// `ARTCRAFT_LAUNCHER_SNAPSHOT_FONTS`.
fn system_fonts(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts");
    for name in ["Inter-Variable.ttf", "Archivo-Variable.ttf", "GeistMono-Variable.ttf"] {
        std::fs::copy(assets.join(name), dir.join(name)).unwrap();
    }
    std::fs::write(dir.join("Ubuntu-Light.ttf"), epaint_default_fonts::UBUNTU_LIGHT).unwrap();
    if let Some(extra) = std::env::var_os("ARTCRAFT_LAUNCHER_SNAPSHOT_FONTS") {
        for entry in std::fs::read_dir(extra).into_iter().flatten().flatten() {
            let _ = std::fs::copy(entry.path(), dir.join(entry.file_name()));
        }
    }
}

const USER_INDEX: &str = "https://fonts.example.org/index.json";

/// An index a user could add as a source: its own Abel, at google/fonts' path for the file.
fn user_index() -> Vec<u8> {
    format!(
        r#"{{"schema":1,"version":"1.1","source":"examplefonts","commit":"{commit}",
        "source_info":{{"id":"examplefonts","name":"Example Fonts","curated":true,"file_urls":["https://fonts.example.org/files/{{path}}"]}},
        "languages":[{{"id":"en_Latn","name":"English","script":"Latn","population":1}}],
        "scripts":[{{"id":"Latn","name":"Latin"}}],
        "families":[{{"family":"Abel","source":"examplefonts","designer":"MADType","categories":["SANS_SERIF"],"license":"OFL","license_path":null,"languages":[0],"scripts":["Latn"],
          "files":[{{"path":"ofl/abel/Abel-Regular.ttf","size":35220,"sha256":"{sha}","style":"normal","weight":400,"axes":[],"postscript":[]}}]}}]}}"#,
        commit = "1".repeat(40),
        sha = "0a".repeat(32),
    )
    .into_bytes()
}

/// A store that "downloads" every file of Abel as egui's Ubuntu Light, and with
/// `ARTCRAFT_LAUNCHER_SNAPSHOT_GOOGLE_FONTS` any file of the index.
fn store(dir: &Path, index: &Index) -> Arc<FakeStore> {
    if let Some(checkout) = std::env::var_os("ARTCRAFT_LAUNCHER_SNAPSHOT_GOOGLE_FONTS") {
        return Arc::new(FakeStore::new(dir, HashMap::new()).serving(checkout).serving_index(USER_INDEX, user_index()));
    }
    let mut files = HashMap::new();
    for file in index.family("Abel").map(|f| f.files.clone()).unwrap_or_default() {
        files.insert(file.path, epaint_default_fonts::UBUNTU_LIGHT.to_vec());
    }
    Arc::new(FakeStore::new(dir, files).serving_index(USER_INDEX, user_index()))
}

fn launcher(name: &str, language: &str, runner: Runner) -> (Harness<'static, Option<LauncherUi>>, Arc<FakeStore>) {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fonts").join(name);
    let _ = std::fs::remove_dir_all(&root);
    system_fonts(&root.join("usr/share/fonts"));
    let store_dir = root.join("store");
    std::fs::create_dir_all(&store_dir).unwrap();
    let env = FontEnv { os: FontOs::Unix, root: root.clone(), font_store: Some(store_dir.clone()), ..FontEnv::default() };
    let catalog = Arc::new(Catalog::new(env).with_font_service(false));
    catalog.rescan();
    let index = craft_font_manager::index::bundled().unwrap();
    let store = store(&store_dir, &index);
    let index = craft_font_manager::store::Installer::indexes(&*store).map_err(|e| e.0);
    let mut options = Some(Options { catalog, installer: store.clone(), index, runner });
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let settings = Settings { notify: false, language: language.into(), ..Settings::default() };
    let harness = Harness::builder()
        .with_size(egui::vec2(1120.0, 760.0))
        .with_theme(egui::Theme::Dark)
        // The index's samples cover every script (Korean, Thai…), and the cards draw them before
        // their faces load; the test's fonts don't cover them all.
        .allow_missing_glyphs()
        .build_ui_state(
            move |ui, state: &mut Option<LauncherUi>| {
                if state.is_none() {
                    let mut s = settings.clone();
                    s.theme = artcraft_launcher_ui_egui::theme::ThemeChoice::Dark;
                    let mut launcher = LauncherUi::new(ui.ctx(), Box::new(Quiet), s);
                    if let Some(options) = options.take() {
                        launcher.use_font_options(options);
                    }
                    *state = Some(launcher);
                }
                if let Some(l) = state.as_mut() {
                    l.logic(&ui.ctx().clone(), &mut NoHost);
                    l.ui(ui, &mut NoHost);
                }
            },
            None,
        );
    (harness, store)
}

fn settle(h: &mut Harness<'_, Option<LauncherUi>>) {
    h.run_steps(6);
}

fn click(h: &mut Harness<'_, Option<LauncherUi>>, label: &str) {
    h.get_all_by_label(label).next().unwrap_or_else(|| panic!("no {label:?}")).click();
    settle(h);
}

fn fonts_page(h: &mut Harness<'_, Option<LauncherUi>>) {
    settle(h);
    h.get_by_role_and_label(Role::Button, "Fonts").click();
    settle(h);
    assert_eq!(h.state().as_ref().map(LauncherUi::page), Some(Page::Fonts));
}

fn results(h: &Harness<'_, Option<LauncherUi>>) -> Vec<String> {
    h.state().as_ref().and_then(|l| l.fonts_panel()).map(|p| p.results()).unwrap_or_default()
}

#[test]
fn the_page_toggle_switches_between_apps_and_fonts() {
    let (mut h, _) = launcher("page-toggle", "en", Runner::Inline);
    assert_eq!(h.state().as_ref().map(LauncherUi::page), Some(Page::Apps));
    assert!(h.state().as_ref().and_then(|l| l.fonts_panel()).is_none(), "the panel is built when the page first shows");
    assert!(h.query_by_label("Show apps as cards").is_some());
    fonts_page(&mut h);
    // The apps' controls give way to the panel's own.
    assert!(h.query_by_label("Show apps as cards").is_none());
    assert!(h.query_by_label("Check for updates").is_none());
    // The page starts on sans-serif families in the system language: choosing all categories adds families.
    let filtered = results(&h).len();
    assert!(filtered > 100);
    click(&mut h, "All categories");
    assert!(results(&h).len() > filtered, "{} vs {filtered}", results(&h).len());
    h.get_by_role_and_label(Role::TextInput, "Search").focus();
    h.step();
    h.get_by_role_and_label(Role::TextInput, "Search").type_text("noto sans jp");
    settle(&mut h);
    assert_eq!(results(&h), ["Noto Sans JP"]);

    h.get_by_role_and_label(Role::Button, "Apps").click();
    settle(&mut h);
    assert_eq!(h.state().as_ref().map(LauncherUi::page), Some(Page::Apps));
    assert!(h.query_by_label("Show apps as cards").is_some());
    assert!(h.query_by_label("Check for updates").is_some());
}

#[test]
fn font_downloads_are_switched_in_settings() {
    let (mut h, store) = launcher("settings", "en", Runner::Inline);
    settle(&mut h);
    click(&mut h, "Settings");
    let switch = || "Allow font downloads";
    assert!(!craft_font_manager::store::Installer::downloads_allowed(&*store));
    click(&mut h, switch());
    assert!(craft_font_manager::store::Installer::downloads_allowed(&*store));
    click(&mut h, switch());
    assert!(!craft_font_manager::store::Installer::downloads_allowed(&*store));
}

#[test]
fn sources_are_added_in_settings() {
    let (mut h, store) = launcher("sources", "en", Runner::Inline);
    craft_font_manager::store::Installer::allow_downloads(&*store, true).unwrap();
    fonts_page(&mut h);
    let before = results(&h).len();
    click(&mut h, "Settings");
    add_source(&mut h);
    assert_eq!(results(&h).len(), before + 1);
    assert!(h.query_all_by_label("Not curated").next().is_some());
}

#[test]
fn preview_settings_are_set_in_settings() {
    let (mut h, store) = launcher("preview-server", "en", Runner::Inline);
    let service = || craft_font_manager::store::Installer::preview_service(&*store);
    let refused = |h: &Harness<'_, Option<LauncherUi>>| {
        h.query_all(By::new().predicate(|n| n.value().or(n.label()).is_some_and(|l| l.contains("Couldn't use this server")))).next().is_some()
    };
    settle(&mut h);
    click(&mut h, "Settings");
    assert_eq!(service(), None, "the fake store has no default server");

    // The server field is collapsed under "Advanced".
    assert!(h.query_all_by_label("Font preview server").next().is_none(), "the server field starts collapsed");
    assert!(h.query_all_by_label("Show font previews").next().is_some(), "the previews switch stays visible");
    // The Fonts box can be below the dialog's fold, where a click wouldn't land.
    h.get_by_label("Advanced").click_accesskit();
    settle(&mut h);
    let field = "Font preview server";
    h.get_by_role_and_label(Role::TextInput, field).focus();
    h.step();
    h.get_by_role_and_label(Role::TextInput, field).type_text("http://192.0.2.1/");
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    assert_eq!(service(), None, "plain http only to this computer");
    assert!(refused(&h));

    h.get_by_role_and_label(Role::TextInput, field).focus();
    h.step();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_by_role_and_label(Role::TextInput, field).type_text("http://127.0.0.1:9/");
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    assert_eq!(service().as_deref(), Some("http://127.0.0.1:9"));
    assert!(!refused(&h));

    // Space toggles the focused switch: the Fonts box can be below the dialog's fold.
    let enabled = || craft_font_manager::store::Installer::previews_enabled(&*store);
    assert!(enabled(), "previews are on by default");
    let switch = "Show font previews";
    h.get_by_role_and_label(Role::CheckBox, switch).focus();
    h.step();
    h.key_press(egui::Key::Space);
    settle(&mut h);
    assert!(!enabled());
    h.key_press(egui::Key::Space);
    settle(&mut h);
    assert!(enabled());
}

/// Types the example source into Settings and adds it, then waits for its index.
fn add_source(h: &mut Harness<'_, Option<LauncherUi>>) {
    h.get_by_role_and_label(Role::TextInput, "Index address").focus();
    h.step();
    h.get_by_role_and_label(Role::TextInput, "Index address").type_text(USER_INDEX);
    h.step();
    // Enter adds it: the Fonts box can be below the dialog's fold, where a click wouldn't land.
    h.key_press(egui::Key::Enter);
    wait_for_sources(h);
}

fn wait_for_sources(h: &mut Harness<'_, Option<LauncherUi>>) {
    h.step();
    for _ in 0..400 {
        h.step();
        if h.state().as_ref().and_then(|l| l.fonts_panel()).is_some_and(|p| !p.fetching_sources()) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    settle(h);
}

fn snapshot(h: &mut Harness<'_, Option<LauncherUi>>, dir: &std::ffi::OsStr, name: &str) {
    h.run_steps(12);
    let image = h.render().unwrap();
    std::fs::create_dir_all(dir).unwrap();
    image.save(Path::new(dir).join(format!("{name}.png"))).unwrap();
}

fn allow(h: &mut Harness<'_, Option<LauncherUi>>, store: &FakeStore) {
    craft_font_manager::store::Installer::allow_downloads(store, true).unwrap();
    // The page reads the setting again on the next look; settle past the recheck.
    if let Some(l) = h.state_mut().as_mut() {
        l.set_page(Page::Apps);
    }
    settle(h);
    if let Some(l) = h.state_mut().as_mut() {
        l.set_page(Page::Fonts);
    }
    settle(h);
}

#[test]
fn screenshots() {
    let Some(dir) = std::env::var_os("ARTCRAFT_LAUNCHER_SNAPSHOTS") else { return };
    let shot = |name: &str, language: &str, f: &dyn Fn(&mut Harness<'_, Option<LauncherUi>>, &FakeStore)| {
        let (mut h, store) = launcher(&format!("shot-{name}"), language, Runner::Thread);
        if language == "en" {
            fonts_page(&mut h);
        } else {
            settle(&mut h);
            if let Some(l) = h.state_mut().as_mut() {
                l.set_page(Page::Fonts);
            }
            settle(&mut h);
        }
        f(&mut h, &store);
        snapshot(&mut h, &dir, name);
        store.release();
    };
    shot("apps", "en", &|h, _| {
        h.get_by_role_and_label(Role::Button, "Apps").click();
        settle(h);
    });
    shot("fonts-downloads-off", "en", &|_, _| {});
    shot("fonts-catalogue", "en", &|h, store| {
        allow(h, store);
    });
    shot("fonts-downloading", "en", &|h, store| {
        allow(h, store);
        store.hold();
        click(h, "Install Abel");
        h.run_steps(10);
    });
    shot("fonts-error", "en", &|h, store| {
        allow(h, store);
        store.fail_next("couldn't reach cdn.jsdelivr.net or raw.githubusercontent.com");
        click(h, "Install Abel");
        h.run_steps(30);
    });
    shot("fonts-empty-search", "en", &|h, _| {
        h.get_by_role_and_label(Role::TextInput, "Search").focus();
        h.step();
        h.get_by_role_and_label(Role::TextInput, "Search").type_text("Garamondish");
        settle(h);
    });
    shot("fonts-installed", "en", &|h, store| {
        allow(h, store);
        click(h, "Install Abel");
        h.run_steps(30);
        h.get_by_role_and_label(Role::Tab, "Installed").click();
        settle(h);
    });
    shot("fonts-details", "en", &|h, store| {
        allow(h, store);
        click(h, "ABeeZee");
        h.run_steps(20);
    });
    shot("fonts-details-installed", "en", &|h, _| {
        h.get_by_role_and_label(Role::TextInput, "Search").focus();
        h.step();
        h.get_by_role_and_label(Role::TextInput, "Search").type_text("inter");
        settle(h);
        click(h, "Inter");
        h.run_steps(20);
    });
    shot("fonts-details-not-installed", "en", &|h, _| {
        click(h, "ABeeZee");
        h.run_steps(10);
    });
    // Without a Japanese face the launcher stays in English and the Japanese labels don't exist.
    if artcraft_launcher_ui_egui::theme::cjk_font_available(artcraft_launcher_ui_egui::i18n::Lang::JA) {
        shot("fonts-japanese", "ja", &|h, store| {
            allow(h, store);
            h.get_by_role_and_label(Role::ComboBox, "言語").click();
            settle(h);
            h.get_by_role_and_label(Role::TextInput, "言語を検索").focus();
            h.step();
            h.get_by_role_and_label(Role::TextInput, "言語を検索").type_text("日本");
            settle(h);
            click(h, "Japanese · 日本語 (日本)");
            h.run_steps(40);
        });
    }
    shot("fonts-settings", "en", &|h, store| {
        allow(h, store);
        click(h, "Settings");
        add_source(h);
        // The Fonts box is below the dialog's fold: scroll to it.
        h.event(egui::Event::PointerMoved(egui::pos2(800.0, 500.0)));
        let wheel = egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -470.0),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
            source: egui::MouseWheelSource::Wheel,
        };
        h.event(wheel);
        h.run_steps(20);
        store.change_index(USER_INDEX);
        click(h, "Update sources");
        wait_for_sources(h);
    });
    shot("fonts-not-curated", "en", &|h, store| {
        allow(h, store);
        click(h, "Settings");
        add_source(h);
        click(h, "Done");
        h.get_by_role_and_label(Role::TextInput, "Search").focus();
        h.step();
        h.get_by_role_and_label(Role::TextInput, "Search").type_text("abel");
        settle(h);
        h.run_steps(30);
    });
    shot("fonts-sources-filter", "en", &|h, _| {
        click(h, "Noto");
        h.run_steps(20);
    });
}
