//! Exercise the real Windows settings UI with synthetic releases, without installing apps.
use std::cell::RefCell;
use std::rc::Rc;

use artcraft_launcher_core::protocol::{AppPreferences, Backend, Capabilities, Command, Event, LatestRelease, Origin, ReleasePage};
use artcraft_launcher_core::{Arch, Asset, InstallSource, Installed, Os, PackageKind, Platform, Release, SUITE, version};
use artcraft_launcher_ui_egui::{Host, LauncherUi, Settings, settings::ViewMode};
use egui_kittest::{Harness, kittest::Queryable};

// The UI language is process-wide; multilingual harnesses must not draw concurrently.
static UI_LANGUAGE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

type Inbox = Rc<RefCell<Vec<Event>>>;
type Sent = Rc<RefCell<Vec<Command>>>;

struct Fake {
    inbox: Inbox,
    sent: Sent,
    fail_history: bool,
    launcher: Option<Installed>,
    self_update: bool,
}
impl Backend for Fake {
    fn send(&mut self, command: Command) {
        if let Command::LoadReleases { app, page, .. } = &command {
            self.inbox.borrow_mut().push(Event::Releases {
                app: app.clone(),
                page: *page,
                result: if self.fail_history {
                    Err(artcraft_launcher_core::protocol::ReleaseError { message: "GitHub answered HTTP 504".into(), retry_after: Some(120) })
                } else {
                    Ok(ReleasePage {
                        releases: if app == artcraft_launcher_core::catalog::LAUNCHER_ID {
                            vec![launcher_release("0.3.0"), launcher_release("0.2.1"), launcher_release("0.1.0")]
                        } else {
                            vec![release("0.7.0"), release("0.6.0"), release("0.5.0")]
                        },
                        more: false,
                    })
                },
            });
        }
        self.sent.borrow_mut().push(command);
    }
    fn poll(&mut self) -> Vec<Event> {
        std::mem::take(&mut *self.inbox.borrow_mut())
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities { manage_apps: true, reveal_label: "Show in Explorer", self_update: self.self_update }
    }
    fn launcher_installation(&self) -> Option<Installed> {
        self.launcher.clone()
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

fn release(v: &str) -> Release {
    Release {
        tag: format!("v{v}"),
        version: version::parse(v),
        name: format!("PhotoCraft {v}"),
        html_url: format!("https://github.com/storytold/photocraft/releases/tag/v{v}"),
        assets: vec![
            Asset {
                name: format!("photocraft-{v}-windows-x64-portable.zip"),
                url: format!("https://github.com/storytold/photocraft/releases/download/v{v}/photocraft-{v}-windows-x64-portable.zip"),
                size: 100,
                sha256: Some("a".repeat(64)),
            },
            Asset {
                name: format!("photocraft-{v}-windows-x64.msi"),
                url: format!("https://github.com/storytold/photocraft/releases/download/v{v}/photocraft-{v}-windows-x64.msi"),
                size: 100,
                sha256: Some("b".repeat(64)),
            },
        ],
        ..Release::default()
    }
}

fn launcher_release(v: &str) -> Release {
    let mut r = release(v);
    r.name = format!("ArtCraft Launcher {v}");
    r.html_url = format!("https://github.com/storytold/craft-launcher/releases/tag/v{v}");
    for asset in &mut r.assets {
        asset.name = asset.name.replace("photocraft", "artcraft-launcher");
        asset.url = asset.url.replace("storytold/photocraft", "storytold/craft-launcher").replace("/photocraft-", "/artcraft-launcher-");
    }
    r
}

fn setup_launcher(source: InstallSource, enabled: bool, settings: Settings) -> (Harness<'static, Option<LauncherUi>>, Sent, Inbox) {
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let inbox = Rc::new(RefCell::new(Vec::new()));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut backend = Some(Fake {
        inbox: inbox.clone(),
        sent: sent.clone(),
        fail_history: false,
        self_update: enabled,
        launcher: Some(Installed {
            version: version::parse("0.2.1"),
            source,
            location: "C:\\Tools\\ArtCraft Launcher\\artcraft-launcher.exe".into(),
            package: None,
        }),
    });
    let h = Harness::builder().with_size(egui::vec2(1180.0, 1000.0)).with_theme(egui::Theme::Dark).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), settings.clone()));
            }
            if let Some(l) = state {
                l.logic(&ui.ctx().clone(), &mut NoHost);
                l.ui(ui, &mut NoHost);
            }
        },
        None,
    );
    (h, sent, inbox)
}

#[test]
fn launcher_settings_choose_exact_release_without_installing_or_pinning_the_draft() {
    let _language = UI_LANGUAGE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (mut h, sent, inbox) = setup_launcher(InstallSource::Launcher, true, Settings { language: "en".into(), ..Settings::default() });
    h.run_steps(5);
    h.get_by_label("Settings").click();
    h.run_steps(3);
    snapshot(&mut h, "launcher-settings-entry-en");
    h.get_by_label("Choose launcher version…").click();
    h.run_steps(12);
    assert!(sent.borrow().contains(&Command::LoadReleases { app: artcraft_launcher_core::catalog::LAUNCHER_ID.into(), page: 1, force: false }));
    assert_eq!(h.query_all_by_label("Installation").count(), 0);
    h.get_by_value("v0.2.1").click();
    h.run_steps(3);
    snapshot(&mut h, "launcher-version-dropdown-en");
    h.get_by_label("v0.1.0").click();
    h.run_steps(3);
    assert!(h.state().as_ref().unwrap().settings.apps.is_empty());
    assert!(!sent.borrow().iter().any(|c| matches!(c, Command::InstallRelease { .. })));
    snapshot(&mut h, "launcher-version-portable-en");
    h.get_by_label("Switch to v0.1.0").click();
    h.run_steps(3);
    assert_eq!(sent.borrow().iter().filter(|c| matches!(c, Command::InstallRelease { app, tag, preferences, .. } if app == artcraft_launcher_core::catalog::LAUNCHER_ID && tag == "v0.1.0" && preferences.pinned_tag.as_deref() == Some("v0.1.0"))).count(), 1);
    assert!(h.state().as_ref().unwrap().settings.pending_launcher_switch.is_none());
    inbox.borrow_mut().push(Event::LauncherSwitchScheduled {
        tag: "v0.1.0".into(),
        preferences: AppPreferences { pinned_tag: Some("v0.1.0".into()), ..AppPreferences::default() },
    });
    h.run_steps(2);
    assert!(h.state().as_ref().unwrap().settings.pending_launcher_switch.is_some());
    assert!(h.state().as_ref().unwrap().settings.apps.is_empty(), "exit helper has not installed yet");
}

#[test]
fn launcher_msi_blocks_downgrades_but_can_select_upgrades_and_disable_pinning() {
    let _language = UI_LANGUAGE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (mut h, sent, _) = setup_launcher(InstallSource::Msi, true, Settings { language: "en".into(), ..Settings::default() });
    h.run_steps(5);
    h.get_by_label("Settings").click();
    h.run_steps(3);
    h.get_by_label("Choose launcher version…").click();
    h.run_steps(12);
    h.get_by_value("v0.2.1").click();
    h.run_steps(3);
    h.get_by_label("v0.1.0").click();
    h.run_steps(3);
    h.get_by_label("Switch to v0.1.0").click();
    h.run_steps(2);
    assert!(!sent.borrow().iter().any(|c| matches!(c, Command::InstallRelease { .. })));
    h.get_by_label("Launcher MSI downgrades require a portable copy. This installation can upgrade only.");
    h.get_by_value("v0.1.0").click();
    h.run_steps(3);
    h.get_by_label("v0.3.0").click();
    h.run_steps(3);
    h.get_by_label("Keep this version after switching").click();
    h.run_steps(2);
    snapshot(&mut h, "launcher-version-msi-en");
    h.get_by_label("Switch to v0.3.0").click();
    h.run_steps(3);
    assert!(sent.borrow().iter().any(|c| matches!(c, Command::InstallRelease { app, tag, preferences, .. } if app == artcraft_launcher_core::catalog::LAUNCHER_ID && tag == "v0.3.0" && preferences.pinned_tag.is_none())));
}

#[test]
fn launcher_development_instance_lists_releases_but_cannot_replace_itself() {
    let _language = UI_LANGUAGE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (mut h, sent, _) = setup_launcher(InstallSource::Launcher, false, Settings { language: "en".into(), ..Settings::default() });
    h.run_steps(5);
    h.get_by_label("Settings").click();
    h.run_steps(3);
    h.get_by_label("Choose launcher version…").click();
    h.run_steps(12);
    h.get_by_value("v0.2.1").click();
    h.run_steps(3);
    h.get_by_label("v0.3.0").click();
    h.run_steps(3);
    h.get_by_label("Switch to v0.3.0").click();
    h.run_steps(2);
    h.get_by_label("This development or externally managed launcher cannot switch versions in place.");
    assert!(!sent.borrow().iter().any(|c| matches!(c, Command::InstallRelease { .. })));
}

#[test]
fn launcher_pin_suppresses_auto_update_and_is_applied_only_after_actual_restart() {
    let _language = UI_LANGUAGE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let id = artcraft_launcher_core::catalog::LAUNCHER_ID;
    let pin = AppPreferences { pinned_tag: Some("v0.2.1".into()), ..AppPreferences::default() };
    for success in [false, true] {
        let settings = Settings {
            language: "en".into(),
            auto_update: true,
            pending_launcher_switch: Some((if success { "v0.2.1" } else { "v0.3.0" }.into(), pin.clone())),
            ..Settings::default()
        };
        let (mut h, sent, inbox) = setup_launcher(InstallSource::Msi, true, settings);
        h.run_steps(5);
        let saved = &h.state().as_ref().unwrap().settings;
        assert_eq!(saved.apps.contains_key(id), success);
        assert!(saved.pending_launcher_switch.is_none());
        inbox.borrow_mut().push(Event::LauncherUpdate(launcher_release("0.3.0")));
        h.run_steps(5);
        assert_eq!(sent.borrow().contains(&Command::UpdateLauncher), !success);
    }
}

fn setup(settings: Settings, installed: bool) -> (Harness<'static, Option<LauncherUi>>, Sent, Inbox) {
    setup_at_width(settings, installed, 1180.0)
}

fn setup_at_width(settings: Settings, installed: bool, width: f32) -> (Harness<'static, Option<LauncherUi>>, Sent, Inbox) {
    setup_history(settings, installed, width, false)
}

fn setup_history(settings: Settings, installed: bool, width: f32, fail_history: bool) -> (Harness<'static, Option<LauncherUi>>, Sent, Inbox) {
    // Each test sets its own language; these tests run serially when capturing screenshots.
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let mut events = vec![Event::Latest { app: "photocraft".into(), latest: LatestRelease::Found(release("0.7.0")), from: Origin::GitHub }];
    for app in &SUITE {
        events.push(Event::Detected {
            app: app.id.into(),
            installed: (installed && app.id == "photocraft").then(|| Installed {
                version: version::parse("0.6.0"),
                source: InstallSource::Launcher,
                location: "C:\\My Tools\\photocraft\\photocraft.exe".into(),
                package: None,
            }),
        });
    }
    let inbox = Rc::new(RefCell::new(events));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut backend = Some(Fake { inbox: inbox.clone(), sent: sent.clone(), fail_history, launcher: None, self_update: false });
    let h = Harness::builder().with_size(egui::vec2(width, 900.0)).with_theme(egui::Theme::Dark).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), settings.clone()));
            }
            if let Some(l) = state {
                l.logic(&ui.ctx().clone(), &mut NoHost);
                l.ui(ui, &mut NoHost);
            }
        },
        None,
    );
    (h, sent, inbox)
}

fn snapshot(h: &mut Harness<'_, Option<LauncherUi>>, name: &str) {
    if let Some(dir) = std::env::var_os("ARTCRAFT_LAUNCHER_SNAPSHOTS") {
        h.render().unwrap().save(std::path::Path::new(&dir).join(format!("{name}.png"))).unwrap();
    }
}

fn open(h: &mut Harness<'_, Option<LauncherUi>>, label: &str) {
    h.get_all_by_label("⋮").next().unwrap().click();
    h.run_steps(3);
    h.get_by_label(label).click();
    h.run_steps(5);
}

#[test]
fn release_history_retry_countdown_and_close_remain_visible_in_english_and_german() {
    let _language = UI_LANGUAGE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for (language, settings_label, countdown, close, filename) in [
        ("en", "App settings…", "Retrying automatically in 120s…", "Close", "releases-retry-en"),
        ("de", "App-Einstellungen…", "Automatischer neuer Versuch in 120s…", "Schließen", "releases-retry-de"),
    ] {
        let (mut h, sent, _) = setup_history(Settings { language: language.into(), notify: false, ..Settings::default() }, true, 1180.0, true);
        h.run_steps(5);
        open(&mut h, settings_label);
        assert!(h.get_by_label(countdown).rect().bottom() < 900.0);
        assert!(h.get_by_label(close).rect().bottom() < 900.0);
        assert_eq!(sent.borrow().iter().filter(|c| matches!(c, Command::LoadReleases { .. })).count(), 1);
        snapshot(&mut h, filename);
        h.get_by_label(close).click();
        h.run_steps(3);
        assert_eq!(h.query_all_by_label(countdown).count(), 0);
    }
}

#[test]
fn choosing_a_downgrade_waits_for_explicit_action_and_commits_only_after_success() {
    let _language = UI_LANGUAGE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for view in [ViewMode::Grid, ViewMode::Table] {
        let (mut h, sent, inbox) = setup(Settings { view, language: "en".into(), notify: false, ..Settings::default() }, true);
        h.run_steps(5);
        h.get_all_by_label("⋮").next().unwrap().click();
        h.run_steps(3);
        if view == ViewMode::Grid {
            snapshot(&mut h, "version-menu-en");
        }
        h.get_by_label("App settings…").click();
        h.run_steps(5);
        assert_eq!(sent.borrow().iter().filter(|c| matches!(c, Command::LoadReleases { .. })).count(), 1);
        sent.borrow_mut().clear();
        h.get_by_value("v0.6.0").click();
        h.run_steps(3);
        if view == ViewMode::Grid {
            snapshot(&mut h, "version-dropdown-en");
        }
        h.get_by_label("v0.5.0").click();
        h.run_steps(3);
        assert!(!sent.borrow().iter().any(|c| matches!(c, Command::InstallRelease { .. })));
        assert!(h.state().as_ref().unwrap().settings.apps.is_empty());
        if view == ViewMode::Grid {
            snapshot(&mut h, "version-settings-en");
        }
        h.get_by_label("Switch to v0.5.0").click();
        h.run_steps(3);
        assert!(sent.borrow().iter().any(|c| matches!(c, Command::InstallRelease { app, tag, preferences, .. }
            if app == "photocraft" && tag == "v0.5.0" && preferences.pinned_tag.as_deref() == Some("v0.5.0"))));
        inbox.borrow_mut().push(Event::JobFinished { app: "photocraft".into(), result: Err("disk full".into()) });
        h.run_steps(2);
        assert!(h.state().as_ref().unwrap().settings.apps.is_empty());
        inbox.borrow_mut().extend([
            Event::VersionInstalled {
                app: "photocraft".into(),
                preferences: AppPreferences { pinned_tag: Some("v0.5.0".into()), ..AppPreferences::default() },
            },
            Event::Detected {
                app: "photocraft".into(),
                installed: Some(Installed {
                    version: version::parse("0.5.0"),
                    source: InstallSource::Launcher,
                    location: "C:\\My Tools\\photocraft\\photocraft.exe".into(),
                    package: None,
                }),
            },
        ]);
        h.run_steps(3);
        assert_eq!(h.state().as_ref().unwrap().settings.apps["photocraft"].pinned_tag.as_deref(), Some("v0.5.0"));
        h.get_by_label("Installation").click();
        h.run_steps(3);
        if view == ViewMode::Grid {
            snapshot(&mut h, "installation-settings-en");
        }
        h.get_by_label("Open installation folder").click();
        h.run_steps(2);
        assert!(sent.borrow().contains(&Command::Reveal { app: "photocraft".into() }));
    }
}

#[test]
fn pin_survives_restart_and_blocks_update_all_and_automatic_updates() {
    let _language = UI_LANGUAGE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut settings = Settings { language: "en".into(), auto_update: true, notify: false, ..Settings::default() };
    settings.apps.insert("photocraft".into(), AppPreferences { pinned_tag: Some("v0.6.0".into()), ..AppPreferences::default() });
    let settings: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
    let (mut h, sent, inbox) = setup(settings, true);
    h.run_steps(5);
    assert!(!sent.borrow().contains(&Command::Install { app: "photocraft".into() }));
    assert_eq!(h.query_all_by_label("Update 1 app").count(), 0);
    assert_eq!(h.query_all_by_label("Version is kept").count(), 1);
    h.state_mut().as_mut().unwrap().settings.auto_update = false;
    h.state_mut().as_mut().unwrap().settings.apps.get_mut("photocraft").unwrap().pinned_tag = None;
    h.run_steps(3);
    h.get_by_label("Update 1 app").click();
    h.run_steps(2);
    assert!(sent.borrow().contains(&Command::Install { app: "photocraft".into() }));
    sent.borrow_mut().clear();
    h.state_mut().as_mut().unwrap().settings.auto_update = true;
    inbox.borrow_mut().push(Event::Latest { app: "photocraft".into(), latest: LatestRelease::Found(release("0.8.0")), from: Origin::GitHub });
    h.run_steps(3);
    assert!(sent.borrow().contains(&Command::Install { app: "photocraft".into() }));
    let legacy: Settings = serde_json::from_str(r#"{"language":"de","auto_update":false}"#).unwrap();
    assert!(legacy.apps.is_empty());
}

#[test]
fn german_version_settings_fit_and_closing_discards_the_draft() {
    let _language = UI_LANGUAGE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (mut h, sent, _) = setup(Settings { language: "de".into(), notify: false, ..Settings::default() }, true);
    h.run_steps(5);
    open(&mut h, "App-Einstellungen…");
    h.get_by_value("v0.6.0").click();
    h.run_steps(3);
    h.get_by_label("v0.5.0").click();
    h.run_steps(3);
    snapshot(&mut h, "version-settings-de");
    h.get_by_label("Schließen").click();
    h.run_steps(2);
    assert!(h.state().as_ref().unwrap().settings.apps.is_empty());
    assert!(!sent.borrow().iter().any(|c| matches!(c, Command::InstallRelease { .. })));
}

#[test]
fn a_new_portable_install_uses_its_text_folder_draft() {
    let _language = UI_LANGUAGE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let (mut h, sent, _) = setup(Settings { language: "en".into(), notify: false, ..Settings::default() }, false);
    h.run_steps(5);
    open(&mut h, "App settings…");
    h.get_by_label("Installation").click();
    h.run_steps(3);
    h.get_by_value("System default").click();
    h.run_steps(3);
    h.get_by_label("Portable (.zip)").click();
    h.run_steps(3);
    h.get_by_label("Choose a folder for a new installation").click();
    h.run_steps(2);
    h.get_all_by_role(egui::accesskit::Role::TextInput).last().unwrap().focus();
    h.get_all_by_role(egui::accesskit::Role::TextInput).last().unwrap().type_text("D:\\My Tools");
    h.run_steps(3);
    snapshot(&mut h, "installation-new-portable-en");
    h.get_by_label("Version").click();
    h.run_steps(3);
    h.get_by_label("Switch to v0.7.0").click();
    h.run_steps(2);
    assert!(sent.borrow().iter().any(|c| matches!(c, Command::InstallRelease { preferences, .. }
        if preferences.package == Some(PackageKind::PortableZip) && preferences.install_dir.as_deref() == Some("D:\\My Tools"))));
}

#[test]
fn table_keeps_the_vertical_menu_beside_open_in_narrow_windows() {
    let _language = UI_LANGUAGE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for width in [720.0, 960.0, 1120.0, 1180.0] {
        for language in ["en", "de"] {
            let mut settings = Settings { view: ViewMode::Table, language: language.into(), notify: false, ..Settings::default() };
            settings.apps.insert("photocraft".into(), AppPreferences { pinned_tag: Some("v0.6.0".into()), ..AppPreferences::default() });
            let (mut h, _, _) = setup_at_width(settings, true, width);
            h.run_steps(5);
            let open_label = if language == "de" { "Öffnen" } else { "Open" };
            let open = h.get_by_label(open_label).rect();
            let menu = h.get_all_by_label("⋮").find(|n| (n.rect().center().y - open.center().y).abs() < 10.0).unwrap();
            assert!(menu.rect().left() >= open.right(), "{language}/{width}: menu must follow Open");
            assert!(menu.rect().right() <= width - 16.0, "{language}/{width}: menu must remain inside the window: {:?}", menu.rect());
            if width == 1120.0 {
                snapshot(&mut h, &format!("table-actions-{language}"));
            }
            h.get_all_by_label("⋮").find(|n| (n.rect().center().y - open.center().y).abs() < 10.0).unwrap().click();
            h.run_steps(3);
            h.get_by_label(if language == "de" { "App-Einstellungen…" } else { "App settings…" }).click();
            h.run_steps(3);
            h.get_by_label("Version");
        }
    }
}

#[test]
fn an_msi_downgrade_is_enabled_but_requires_confirmation_and_supports_cancel() {
    let _language = UI_LANGUAGE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for language in ["en", "de"] {
        let (mut h, sent, inbox) = setup(Settings { language: language.into(), notify: false, ..Settings::default() }, true);
        inbox.borrow_mut().push(Event::Detected {
            app: "photocraft".into(),
            installed: Some(Installed {
                version: version::parse("0.7.0"),
                source: InstallSource::Msi,
                location: "C:\\Program Files\\PhotoCraft\\photocraft.exe".into(),
                package: Some("{10000000-0000-0000-0000-000000000000}".into()),
            }),
        });
        h.run_steps(5);
        open(&mut h, if language == "de" { "App-Einstellungen…" } else { "App settings…" });
        h.get_by_value("v0.7.0").click();
        h.run_steps(3);
        h.get_by_label("v0.6.0").click();
        h.run_steps(3);
        snapshot(&mut h, &format!("msi-version-settings-{language}"));
        let switch = if language == "de" { "Zu v0.6.0 wechseln" } else { "Switch to v0.6.0" };
        h.get_by_label(switch).click();
        h.run_steps(3);
        assert!(!sent.borrow().iter().any(|c| matches!(c, Command::InstallRelease { .. })));
        snapshot(&mut h, &format!("msi-downgrade-confirmation-{language}"));
        let pin = if language == "de" { "Version nach Wechsel festhalten" } else { "Keep this version after switching" };
        h.get_by_label(pin).click();
        h.run_steps(3);
        assert!(h.state().as_ref().unwrap().settings.apps.is_empty());
        h.get_by_label(if language == "de" { "Abbrechen" } else { "Cancel" }).click();
        h.run_steps(3);
        assert!(!sent.borrow().iter().any(|c| matches!(c, Command::InstallRelease { .. })));
        snapshot(&mut h, &format!("msi-cancelled-settings-{language}"));
        h.get_by_label(switch).click();
        h.run_steps(3);
        snapshot(&mut h, &format!("msi-second-confirmation-{language}"));
        h.get_by_label(if language == "de" {
            "Neuester Version gemäß globalen Update-Einstellungen folgen."
        } else {
            "Follow the latest release using the global update settings."
        });
        h.get_by_label(if language == "de" { "Deinstallieren und wechseln" } else { "Uninstall and switch" }).click();
        h.run_steps(3);
        assert!(
            sent.borrow().iter().any(
                |c| matches!(c, Command::InstallRelease { app, tag, preferences, confirmed_msi_downgrade: true } if app == "photocraft" && tag == "v0.6.0" && preferences.pinned_tag.is_none())
            )
        );
        assert!(h.state().as_ref().unwrap().settings.apps.is_empty());
    }
}

#[test]
fn switching_can_pin_or_follow_updates_for_both_upgrade_and_downgrade() {
    let _language = UI_LANGUAGE_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for tag in ["v0.7.0", "v0.5.0"] {
        for pin in [true, false] {
            let (mut h, sent, inbox) = setup(Settings { language: "en".into(), notify: false, ..Settings::default() }, true);
            h.run_steps(5);
            open(&mut h, "App settings…");
            h.get_by_value("v0.6.0").click();
            h.run_steps(3);
            h.get_by_label(if tag == "v0.7.0" { "v0.7.0 · Latest" } else { tag }).click();
            h.run_steps(3);
            if !pin {
                h.get_by_label("Keep this version after switching").click();
                h.run_steps(3);
            }
            h.get_by_label(&format!("Switch to {tag}")).click();
            h.run_steps(3);
            let preferences = sent
                .borrow()
                .iter()
                .find_map(|c| match c {
                    Command::InstallRelease { tag: actual, preferences, .. } if actual == tag => Some(preferences.clone()),
                    _ => None,
                })
                .unwrap();
            assert_eq!(preferences.pinned_tag.as_deref(), pin.then_some(tag));
            assert!(h.state().as_ref().unwrap().settings.apps.is_empty());
            inbox.borrow_mut().push(Event::VersionInstalled { app: "photocraft".into(), preferences });
            h.run_steps(3);
            assert_eq!(h.state().as_ref().unwrap().settings.apps["photocraft"].pinned_tag.as_deref(), pin.then_some(tag));
        }
    }
}
