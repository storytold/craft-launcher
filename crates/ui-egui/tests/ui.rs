//! Drives the launcher UI against a fake backend: what's shown for each status, and which
//! commands the buttons send. `ARTCRAFT_LAUNCHER_SNAPSHOTS=<dir>` also saves screenshots.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use artcraft_launcher_core::protocol::{Backend, Capabilities, Command, Event, JobPhase, LatestRelease, PausedDownload, Source};
use artcraft_launcher_core::release::{Asset, Release};
use artcraft_launcher_core::status::InstallSource;
use artcraft_launcher_core::{Arch, Installed, Os, Platform, SUITE, version};
use artcraft_launcher_ui_egui::settings::ViewMode;
use artcraft_launcher_ui_egui::{Host, LauncherUi, Settings};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

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
        ev.push(Event::Latest { app: (*id).into(), latest: LatestRelease::Found(release(id, v, NOW - 86_400 - i as i64 * 600)) });
    }
    ev.push(Event::Latest { app: "soundcraft".into(), latest: LatestRelease::None });
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

fn harness(view: ViewMode, theme: egui::Theme, manage: bool) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let sent2 = sent.clone();
    let mut backend = Some(Fake { events: events(), sent: sent2, manage, clock: Rc::new(Cell::new(NOW)) });
    let settings = Settings { view, notify: false, ..Settings::default() };
    let harness = Harness::builder().with_size(egui::vec2(1180.0, 900.0)).with_theme(theme).wgpu().build_ui_state(
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

#[test]
fn grid_shows_every_status_and_installs_on_click() {
    let (mut h, sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(8);
    snapshot(&mut h, "grid-dark");
    // Statuses the user asked for: available, installed and up to date, update with version.
    assert!(h.query_all_by_label("Available to install").count() >= 3);
    assert!(h.query_all_by_label("Installed · up to date").count() >= 2);
    h.get_by_label("Update to 0.5.0");
    h.get_by_label("Update to 0.6.0");
    h.get_by_label_contains("0.4.1 installed → 0.5.0 released");
    h.get_by_label("Like Adobe Photoshop®");
    // The window has no title bar of its own here (the web build, and the tests).
    assert!(h.query_by_label("Close").is_none());
    assert!(h.query_all_by_label_contains("Discord").count() >= 2); // header link and footer link
    h.get_by_label_contains("Adobe, Photoshop, Illustrator");
    assert!(h.query_all_by_label("Coming soon").count() >= 1);
    h.get_by_label_contains("48 MB of 131 MB");
    h.get_by_label("Update all (2)").click();
    h.run_steps(2);
    let installs: Vec<_> = sent.borrow().iter().filter_map(|c| if let Command::Install { app } = c { Some(app.clone()) } else { None }).collect();
    assert_eq!(installs.len(), 2, "{installs:?}");
    assert!(installs.contains(&"photocraft".to_string()) && installs.contains(&"effectcraft".to_string()));
}

#[test]
fn downloads_can_be_paused_resumed_and_discarded() {
    let (mut h, sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(4);
    // Bring the paused and reconnecting apps on screen.
    if let Some(l) = h.state_mut().as_mut() {
        l.settings.filter = artcraft_launcher_ui_egui::settings::Filter::NotInstalled;
    }
    h.run_steps(6);
    snapshot(&mut h, "grid-downloads-dark");
    h.get_by_label("Download paused");
    h.get_by_label("Reconnecting…");
    h.get_by_label_contains("· 25%").click();
    h.run_steps(2);
    assert!(sent.borrow().contains(&Command::Install { app: "deckcraft".into() }));
    h.get_by_label("Discard download").click();
    h.run_steps(2);
    assert!(sent.borrow().contains(&Command::Cancel { app: "deckcraft".into() }));
    h.get_all_by_label("Pause download").next().unwrap().click();
    h.run_steps(2);
    assert!(sent.borrow().iter().any(|c| matches!(c, Command::Pause { .. })));
}

#[test]
fn table_view_and_light_theme() {
    let (mut h, sent) = harness(ViewMode::Table, egui::Theme::Light, true);
    h.run_steps(8);
    snapshot(&mut h, "table-light");
    h.get_by_label("Adobe Photoshop®");
    h.get_by_label("Microsoft Excel®");
    h.get_by_label_contains("Avid and Pro Tools are trademarks");
    h.get_by_label("Update to 0.5.0").click();
    h.run_steps(2);
    assert!(sent.borrow().contains(&Command::Install { app: "photocraft".into() }));
    h.get_all_by_label("Open").next().unwrap().click();
    h.run_steps(2);
    assert!(sent.borrow().iter().any(|c| matches!(c, Command::Launch { .. })));
}

#[test]
fn switching_views_and_filters() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    h.get_by_label_contains("Table").click();
    h.run_steps(4);
    assert_eq!(h.state().as_ref().unwrap().settings.view, ViewMode::Table);
    h.get_by_label_contains("Updates").click();
    h.run_steps(4);
    snapshot(&mut h, "table-updates-dark");
    assert!(h.query_by_label("WordCraft").is_none());
    h.get_by_label("PhotoCraft");
}

/// The app names in the order the table shows them.
fn order(h: &Harness<'_, Option<LauncherUi>>) -> Vec<String> {
    let names: Vec<&str> = SUITE.iter().map(|a| a.name).collect();
    let mut found: Vec<(f32, String)> = h
        .query_all(egui_kittest::kittest::by())
        .filter_map(|n| {
            let node = n.accesskit_node();
            let label = node.label().or_else(|| node.value())?;
            names.contains(&label.as_str()).then(|| (n.rect().top(), label))
        })
        .collect();
    found.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut names: Vec<String> = found.into_iter().map(|(_, l)| l).collect();
    // A label and its text can both carry the name.
    names.dedup();
    names
}

#[test]
fn every_table_column_sorts_both_ways() {
    let (mut h, _) = harness(ViewMode::Table, egui::Theme::Dark, true);
    h.run_steps(6);
    // Installed: highest first, apps that aren't installed last.
    h.get_by_label("Installed").click();
    h.run_steps(3);
    let o = order(&h);
    assert_eq!(&o[..4], ["EffectCraft", "PhotoCraft", "LightCraft", "WordCraft"], "{o:?}");
    // Reversed: lowest first, the empty cells still last.
    h.get_by_label("Installed").click();
    h.run_steps(3);
    let o = order(&h);
    assert_eq!(&o[..4], ["WordCraft", "LightCraft", "PhotoCraft", "EffectCraft"], "{o:?}");
    // Inspired by: A to Z.
    h.get_by_label("Inspired by").click();
    h.run_steps(3);
    let o = order(&h);
    assert_eq!(o.first().map(String::as_str), Some("PdfCraft"), "{o:?}"); // Adobe Acrobat
    // Latest: highest version first, SoundCraft (no release) last.
    h.get_by_label("Latest").click();
    h.run_steps(3);
    let o = order(&h);
    assert_eq!(o.first().map(String::as_str), Some("VectorCraft"), "{o:?}"); // 0.7.0
    snapshot(&mut h, "table-sorted-latest");
    // App: Z to A on the second click.
    h.get_by_label("App").click();
    h.run_steps(3);
    h.get_by_label("App").click();
    h.run_steps(3);
    let o = order(&h);
    assert_eq!(o.first().map(String::as_str), Some("WordCraft"), "{o:?}");
}

#[test]
fn settings_link_to_discord() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    h.get_by_label("Settings").click();
    h.run_steps(4);
    snapshot(&mut h, "settings-dark");
    h.get_by_label("Questions, ideas and news from the ArtCraft team");
    // The header button and the one in Settings.
    assert!(h.query_all_by_label_contains("Discord").count() >= 2);
}

#[test]
fn links_to_the_website_github_and_each_apps_pages() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    // Footer: website, GitHub and Discord.
    for label in ["Website", "GitHub", "Discord"] {
        assert!(h.query_all_by_label(label).count() >= 1, "{label}");
    }
    // Cards: every app links to its repo; only the seven classic apps have a website (plus the footer's).
    let shown_cards = h.query_all_by_label("GitHub").count() - 1;
    let shown_sites = h.query_all_by_label("Website").count() - 1;
    assert!(shown_cards >= 7, "{shown_cards}");
    assert_eq!(shown_sites, 7, "only classic apps have website pages");
    // The table's … menu has the app's GitHub repo and, for a new app, no website yet.
    let (mut h, _) = harness(ViewMode::Table, egui::Theme::Dark, true);
    h.run_steps(6);
    h.get_all_by_label("…").nth(7).unwrap().click(); // WordCraft
    h.run_steps(3);
    h.get_by_label("GitHub repository");
    h.get_by_label("Website (coming soon)");
}

/// Clicks the `n`th widget labelled `label` and returns the URL the app asked to open.
fn click_nth_opens(h: &mut Harness<'_, Option<LauncherUi>>, label: &str, n: usize) -> Option<String> {
    h.get_all_by_label(label).nth(n)?.click();
    for _ in 0..3 {
        h.step();
        let url = h.output().platform_output.commands.iter().find_map(|c| match c {
            egui::OutputCommand::OpenUrl(o) => Some(o.url.clone()),
            _ => None,
        });
        if url.is_some() {
            return url;
        }
    }
    None
}

fn click_opens(h: &mut Harness<'_, Option<LauncherUi>>, label: &str) -> Option<String> {
    click_nth_opens(h, label, 0)
}

/// Every URL the widgets labelled `label` open.
fn all_opened(h: &mut Harness<'_, Option<LauncherUi>>, label: &str) -> Vec<String> {
    let n = h.query_all_by_label(label).count();
    (0..n).filter_map(|i| click_nth_opens(h, label, i)).collect()
}

#[test]
fn links_open_the_right_pages() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    assert_eq!(click_opens(&mut h, "Discord").as_deref(), Some("https://discord.gg/artcraft"));
    // The footer's ArtCraft links and each card's own pages.
    let github = all_opened(&mut h, "GitHub");
    assert!(github.contains(&"https://github.com/storytold".to_string()), "{github:?}");
    assert!(github.contains(&"https://github.com/storytold/photocraft".to_string()), "{github:?}");
    let sites = all_opened(&mut h, "Website");
    assert!(sites.contains(&"https://getartcraft.com".to_string()), "{sites:?}");
    assert!(sites.contains(&"https://getartcraft.com/apps/photocraft".to_string()), "{sites:?}");
    // Social icons in the footer.
    assert_eq!(click_opens(&mut h, "X").as_deref(), Some("https://x.com/get_artcraft"));
    assert_eq!(click_opens(&mut h, "YouTube").as_deref(), Some("https://www.youtube.com/@OfficialArtcraftStudios"));
    assert_eq!(click_opens(&mut h, "TikTok").as_deref(), Some("https://www.tiktok.com/@artcraft.studios"));
    assert_eq!(click_opens(&mut h, "Instagram").as_deref(), Some("https://www.instagram.com/officialartcraftstudios"));
    assert_eq!(click_opens(&mut h, "LinkedIn").as_deref(), Some("https://www.linkedin.com/company/artcraft-ai"));
    // The … menu.
    h.get_all_by_label("…").next().unwrap().click();
    h.run_steps(3);
    assert_eq!(click_opens(&mut h, "All releases").as_deref(), Some("https://github.com/storytold/photocraft/releases"));
}

#[test]
fn looks_again_at_installed_apps_every_few_minutes_not_every_frame() {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let clock = Rc::new(Cell::new(NOW));
    let mut backend = Some(Fake { events: events(), sent: sent.clone(), manage: true, clock: clock.clone() });
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 900.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), Settings { notify: false, ..Settings::default() }));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut NoHost);
                l.ui(ui, &mut NoHost);
            }
        },
        None,
    );
    let detects = |sent: &Rc<RefCell<Vec<Command>>>| sent.borrow().iter().filter(|c| **c == Command::Detect).count();
    h.run_steps(10);
    assert_eq!(detects(&sent), 0, "the backend's own startup detection is enough");
    clock.set(NOW + 4 * 60);
    h.run_steps(10);
    assert_eq!(detects(&sent), 0, "not before five minutes");
    clock.set(NOW + 5 * 60);
    h.run_steps(10);
    assert_eq!(detects(&sent), 1, "once at five minutes, not once per frame");
}

#[test]
fn the_launcher_updates_itself_and_quits_to_restart() {
    struct QuitHost(Rc<Cell<bool>>);
    impl Host for QuitHost {
        fn quit(&mut self) {
            self.0.set(true);
        }
    }
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut events = events();
    events.push(Event::LauncherUpdate(release("artcraft-launcher", "0.2.0", NOW)));
    let shared_events = Rc::new(RefCell::new(Vec::new()));
    struct Scripted {
        inner: Fake,
        later: Rc<RefCell<Vec<Event>>>,
    }
    impl Backend for Scripted {
        fn send(&mut self, c: Command) {
            self.inner.send(c);
        }
        fn poll(&mut self) -> Vec<Event> {
            let mut out = self.inner.poll();
            out.extend(self.later.borrow_mut().drain(..));
            out
        }
        fn capabilities(&self) -> Capabilities {
            self.inner.capabilities()
        }
        fn platform(&self) -> Option<Platform> {
            self.inner.platform()
        }
        fn now(&self) -> i64 {
            self.inner.now()
        }
    }
    let quit = Rc::new(Cell::new(false));
    let mut host = QuitHost(quit.clone());
    let mut backend =
        Some(Scripted { inner: Fake { events, sent: sent.clone(), manage: true, clock: Rc::new(Cell::new(NOW)) }, later: shared_events.clone() });
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 900.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), Settings { notify: false, ..Settings::default() }));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut host);
                l.ui(ui, &mut host);
            }
        },
        None,
    );
    h.run_steps(6);
    h.get_by_label_contains("ArtCraft Launcher 0.2.0 is available");
    h.get_by_label("Update and restart").click();
    h.run_steps(2);
    assert!(sent.borrow().contains(&Command::UpdateLauncher));
    // Downloading: the banner shows progress instead of the button.
    shared_events
        .borrow_mut()
        .push(Event::Progress { app: "artcraft-launcher".into(), phase: JobPhase::Downloading { done: 5 << 20, total: 10 << 20 } });
    h.run_steps(3);
    assert!(h.query_by_label("Update and restart").is_none());
    assert!(!quit.get());
    // In place: the launcher quits so the helper can start the new one.
    shared_events.borrow_mut().push(Event::LauncherRestarting);
    h.run_steps(3);
    assert!(quit.get());
}

#[test]
fn the_current_desktop_setting_is_only_offered_where_the_os_has_desktops() {
    // NoHost has no desktops (Windows, Linux, web): no setting.
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    h.get_by_label("Settings").click();
    h.run_steps(4);
    assert!(h.query_by_label("Show the window on the current desktop").is_none());
    // On by default for everyone; macOS shows it in Settings (DesktopHost::has_desktops).
    assert!(Settings::default().show_on_current_desktop);
}

#[test]
fn web_build_offers_downloads() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, false);
    h.run_steps(8);
    snapshot(&mut h, "web-grid-dark");
    assert!(h.query_all_by_label("Download for macOS").count() >= 8);
    assert!(h.query_by_label("Install").is_none());
}

#[test]
fn custom_title_bar_has_window_buttons() {
    struct FramelessHost;
    impl Host for FramelessHost {
        fn title_bar(&self) -> artcraft_launcher_ui_egui::TitleBar {
            artcraft_launcher_ui_egui::TitleBar::Custom
        }
    }
    let mut backend = Some(Fake { events: events(), sent: Rc::new(RefCell::new(Vec::new())), manage: true, clock: Rc::new(Cell::new(NOW)) });
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 900.0)).wgpu().build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), Settings { notify: false, ..Settings::default() }));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut FramelessHost);
                l.ui(ui, &mut FramelessHost);
            }
        },
        None,
    );
    h.run_steps(6);
    snapshot(&mut h, "grid-title-bar-dark");
    h.get_by_label("Minimize");
    h.get_by_label("Maximize");
    h.get_by_label("Close").click();
    h.step();
    let closes = h.output().viewport_output.values().any(|v| v.commands.contains(&egui::ViewportCommand::Close));
    assert!(closes, "the close button asks the window to close (the shell decides whether that hides it to the tray)");
}
