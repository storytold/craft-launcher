//! Drives the launcher UI against a fake backend: what's shown for each status, and which
//! commands the buttons send. `ARTCRAFT_LAUNCHER_SNAPSHOTS=<dir>` also saves screenshots; run that
//! with `-- --test-threads=1`, as rendering on several threads at once can crash Vulkan loaders
//! older than 1.4.350 (KhronosGroup/Vulkan-Loader#1863).

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::OnceLock;

use artcraft_launcher_core::details::AppDetails;
use artcraft_launcher_core::protocol::{
    Backend, CANCELLED, Capabilities, Command, Event, INSTALL_CANCELLED, JobPhase, LatestRelease, Origin, PausedDownload, Source,
};
use artcraft_launcher_core::release::{Asset, Release};
use artcraft_launcher_core::status::InstallSource;
use artcraft_launcher_core::{Arch, Installed, Os, Platform, SUITE, version};
use artcraft_launcher_ui_egui::settings::ViewMode;
use artcraft_launcher_ui_egui::{Host, LauncherUi, Settings};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};

const NOW: i64 = 1_791_500_000;

/// How the fake backend answers a request for an app's details pictures, whenever it is polled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Pictures {
    /// Every picture arrives.
    #[default]
    Online,
    /// Every picture fails, as with no connection.
    Offline,
    /// Nothing is answered yet: the requests wait until the mode changes.
    Silent,
    /// The second picture of each page fails (not published), the others arrive.
    SecondFails,
}

/// A small JPEG, made here so that no picture is kept in the repository for the tests.
fn test_jpeg() -> Vec<u8> {
    static JPEG: OnceLock<Vec<u8>> = OnceLock::new();
    JPEG.get_or_init(|| {
        let (w, h) = (320u32, 200u32);
        let rgb: Vec<u8> = (0..h).flat_map(|y| (0..w).flat_map(move |x| [(x * 255 / w) as u8, (y * 255 / h) as u8, 160])).collect();
        let mut out = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80).encode(&rgb, w, h, image::ExtendedColorType::Rgb8).unwrap();
        out
    })
    .clone()
}

struct Fake {
    events: Vec<Event>,
    sent: Rc<RefCell<Vec<Command>>>,
    manage: bool,
    pictures: Rc<Cell<Pictures>>,
    /// Apps whose pictures were asked for and not answered yet.
    asked: Vec<String>,
    clock: Rc<Cell<i64>>,
}

impl Backend for Fake {
    fn send(&mut self, command: Command) {
        if let Command::FetchScreenshots { app } = &command {
            self.asked.push(app.clone());
        }
        self.sent.borrow_mut().push(command);
    }
    fn poll(&mut self) -> Vec<Event> {
        let mut out = std::mem::take(&mut self.events);
        if self.pictures.get() != Pictures::Silent {
            for app in std::mem::take(&mut self.asked) {
                let Some(page) = AppDetails::by_id(&app) else { continue };
                for (i, shot) in page.shots.iter().enumerate() {
                    let result = match self.pictures.get() {
                        Pictures::Offline => Err("couldn't reach raw.githubusercontent.com (no connection)".to_string()),
                        Pictures::SecondFails if i == 1 => Err("this picture isn't published".to_string()),
                        _ => Ok(test_jpeg()),
                    };
                    out.push(Event::Screenshot { app: app.clone(), key: shot.key.to_string(), result });
                }
            }
        }
        out
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

struct PlatformFake {
    inner: Fake,
    platform: Platform,
}
impl Backend for PlatformFake {
    fn send(&mut self, command: Command) {
        self.inner.send(command);
    }
    fn poll(&mut self) -> Vec<Event> {
        self.inner.poll()
    }
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    fn platform(&self) -> Option<Platform> {
        Some(self.platform)
    }
    fn now(&self) -> i64 {
        self.inner.now()
    }
}

struct NoHost;
impl Host for NoHost {}

/// A harness that has the launcher's fonts, theme and image loaders before its first pass, and
/// never paints a synthetic cursor into saved images.
fn test_harness_builder<State>(theme: egui::Theme) -> egui_kittest::HarnessBuilder<State> {
    let choice = if theme == egui::Theme::Dark {
        artcraft_launcher_ui_egui::theme::ThemeChoice::Dark
    } else {
        artcraft_launcher_ui_egui::theme::ThemeChoice::Light
    };
    egui_kittest::HarnessBuilder::default()
        // 0.37 defaults: strict accessible names and a panic on a missing glyph. Neither is
        // relaxed here; fixed controls are painter-drawn and dynamic notes/feed text uses the
        // attributed monochrome emoji fonts.
        .with_accessibility_check(true)
        .with_render_cursor(false)
        .with_context_setup(move |ctx| {
            artcraft_launcher_ui_egui::theme::install_fonts(ctx, None);
            egui_extras::install_image_loaders(ctx);
            artcraft_launcher_ui_egui::theme::apply(ctx, choice);
        })
}

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
    ev.push(Event::ReadyToInstall { app: "cadcraft".into(), ready: true });
    ev.push(Event::CheckFinished { at: NOW - 180, source: Source::GitHubApi, rate_limited_until: None });
    ev
}

fn harness(view: ViewMode, theme: egui::Theme, manage: bool) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    harness_at(view, theme, manage, 1.0)
}

fn harness_at(view: ViewMode, theme: egui::Theme, manage: bool, scale: f32) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    harness_with(events(), view, theme, manage, scale)
}

fn harness_with(
    events: Vec<Event>,
    view: ViewMode,
    theme: egui::Theme,
    manage: bool,
    scale: f32,
) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    harness_full(events, view, theme, manage, scale, artcraft_launcher_ui_egui::TitleBar::Native, Rc::default(), None, None)
}

/// Like `harness_with`, with the details pictures answered as `pictures` says (changeable during the test).
fn harness_pictures(
    events: Vec<Event>,
    view: ViewMode,
    theme: egui::Theme,
    manage: bool,
    scale: f32,
    pictures: Rc<Cell<Pictures>>,
) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    harness_full(events, view, theme, manage, scale, artcraft_launcher_ui_egui::TitleBar::Native, pictures, None, None)
}

fn harness_with_title_bar(
    events: Vec<Event>,
    view: ViewMode,
    theme: egui::Theme,
    manage: bool,
    scale: f32,
    bar: artcraft_launcher_ui_egui::TitleBar,
) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    harness_full(events, view, theme, manage, scale, bar, Rc::default(), None, None)
}

fn harness_with_discord(discord_url: Option<&'static str>) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    harness_full(
        events(),
        ViewMode::Grid,
        egui::Theme::Dark,
        true,
        1.0,
        artcraft_launcher_ui_egui::TitleBar::Native,
        Rc::default(),
        None,
        discord_url,
    )
}

#[allow(clippy::too_many_arguments)] // Test fixture inputs vary independently across existing cases.
/// Builds a harness with the full fixture matrix (events, view, theme, manage mode, scale, title
/// bar, pictures, platform and Discord URL), forces English labels, and returns the recorded
/// commands.
fn harness_full(
    events: Vec<Event>,
    view: ViewMode,
    theme: egui::Theme,
    manage: bool,
    scale: f32,
    bar: artcraft_launcher_ui_egui::TitleBar,
    pictures: Rc<Cell<Pictures>>,
    platform: Option<Platform>,
    discord_url: Option<&'static str>,
) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    struct HeaderHost(artcraft_launcher_ui_egui::TitleBar, Option<&'static str>);
    impl Host for HeaderHost {
        fn title_bar(&self) -> artcraft_launcher_ui_egui::TitleBar {
            self.0
        }
        fn discord_url(&self) -> &'static str {
            self.1.unwrap_or(artcraft_launcher_core::catalog::DISCORD_URL)
        }
    }
    // Integration tests compile without cfg!(test), so system_lang() picks up the
    // developer's locale. Force English so labels match the assertions.
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let sent = Rc::new(RefCell::new(Vec::new()));
    let sent2 = sent.clone();
    let fake = Fake { events, sent: sent2, manage, pictures, asked: Vec::new(), clock: Rc::new(Cell::new(NOW)) };
    let mut backend: Option<Box<dyn Backend>> = Some(match platform {
        Some(platform) => Box::new(PlatformFake { inner: fake, platform }),
        None => Box::new(fake),
    });
    let settings = Settings { view, notify: false, ..Settings::default() };
    // No `.wgpu()`: it gives every test its own GPU device, and those segfault on parallel test threads (see above).
    // The default renderer creates one only when a test renders.
    let harness = test_harness_builder(theme).with_size(egui::vec2(1180.0, 1200.0)).with_pixels_per_point(scale).with_theme(theme).build_ui_state(
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
                *state = Some(LauncherUi::new(ui.ctx(), b, s));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut HeaderHost(bar, discord_url));
                l.ui(ui, &mut HeaderHost(bar, discord_url));
            }
        },
        None,
    );
    (harness, sent)
}

/// Save a PNG under `ARTCRAFT_LAUNCHER_SNAPSHOTS` when that is set. `name` is a test-owned file
/// stem (the test names and sizes make each one unique), never a path: a separator is rejected so
/// no caller can write outside the evidence directory.
fn snapshot(h: &mut Harness<'_, Option<LauncherUi>>, name: &str) {
    let Some(dir) = std::env::var_os("ARTCRAFT_LAUNCHER_SNAPSHOTS") else { return };
    assert!(
        !name.is_empty() && !name.contains(['/', '\\']) && name != "." && name != "..",
        "snapshot name must be one pathless file stem, got {name:?}"
    );
    let img = h.render().unwrap();
    img.save(std::path::Path::new(&dir).join(format!("{name}.png"))).unwrap();
}

/// A managing macOS fixture with a fresh PhotoCraft release and a scripted folder picker, for
/// the install-folder flow.
fn folder_harness(picked: Rc<RefCell<Option<String>>>) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    struct Picker(Rc<RefCell<Option<String>>>);
    impl Host for Picker {
        fn pick_folder(&mut self, _initial: Option<&str>) -> Option<String> {
            self.0.borrow_mut().take()
        }
    }
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut backend = Some(Fake {
        events: vec![
            Event::Latest { app: "photocraft".into(), latest: LatestRelease::Found(release("photocraft", "0.6.0", NOW)), from: Origin::GitHub },
            Event::Detected { app: "photocraft".into(), installed: None },
        ],
        sent: sent.clone(),
        manage: true,
        pictures: Rc::default(),
        asked: vec![],
        clock: Rc::new(Cell::new(NOW)),
    });
    let mut host = Picker(picked);
    let harness = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 900.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if let Some(backend) = backend.take() {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(backend), Settings { notify: false, ..Default::default() }));
            }
            if let Some(launcher) = state {
                launcher.logic(ui.ctx(), &mut host);
                launcher.ui(ui, &mut host);
            }
        },
        None,
    );
    (harness, sent)
}

fn install_commands(sent: &RefCell<Vec<Command>>) -> Vec<Command> {
    sent.borrow().iter().filter(|c| !matches!(c, Command::CheckForUpdates { .. })).cloned().collect()
}

#[test]
/// A custom-folder install opens a confirmation dialog with no command until Install; the
/// chosen root and per-app folder are persisted, Configure precedes Install, and Cancel keeps
/// no override.
fn a_fresh_install_confirms_the_folder_and_configures_before_installing() {
    let picked = Rc::new(RefCell::new(None));
    let (mut h, sent) = folder_harness(picked.clone());
    h.run_steps(8);
    h.get_by_label("Settings").click();
    h.run_steps(4);
    h.get_by_label("Browse…").scroll_to_me();
    h.run_steps(4);
    *picked.borrow_mut() = Some("/Volumes/Craft Apps".into());
    h.get_by_label("Browse…").click();
    h.run_steps(3);
    assert_eq!(h.state().as_ref().unwrap().settings.install_locations.root.as_deref(), Some("/Volumes/Craft Apps"));
    h.get_by_label("Done").click();
    h.run_steps(3);
    sent.borrow_mut().clear();
    h.get_by_label("Install").scroll_to_me();
    h.run_steps(3);
    h.get_by_label("Install").click();
    h.run_steps(4);
    assert!(!sent.borrow().iter().any(|c| matches!(c, Command::Install { .. })));
    h.get_by_label("Apps are placed inside this folder.");
    h.check_accessibility();
    snapshot(&mut h, "install-folder-confirmation");
    *picked.borrow_mut() = Some("/Volumes/Photo".into());
    h.get_by_label("Browse…").click();
    h.run_steps(3);
    assert!(h.state().as_ref().unwrap().settings.install_locations.per_app.is_empty());
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(h.state().as_ref().unwrap().settings.install_locations.per_app.is_empty());
    h.get_by_label("Install").scroll_to_me();
    h.run_steps(3);
    h.get_by_label("Install").click();
    h.run_steps(3);
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Install folder").click();
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Install folder").type_text("/Volumes/Typed Photo");
    h.run_steps(3);
    h.get_by_label("Browse…").click();
    h.run_steps(3);
    assert_eq!(h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Install folder").value().as_deref(), Some("/Volumes/Typed Photo"));
    sent.borrow_mut().clear();
    h.get_all_by_role_and_label(egui::accesskit::Role::Button, "Install").last().unwrap().click();
    h.run_steps(2);
    let commands = install_commands(&sent);
    assert!(matches!(commands.first(), Some(Command::Configure(options))
        if options.install_locations.root.as_deref() == Some("/Volumes/Craft Apps")
        && options.install_locations.folder("photocraft") == Some("/Volumes/Typed Photo")));
    assert_eq!(commands.get(1), Some(&Command::Install { app: "photocraft".into() }));
}

#[test]
fn use_default_removes_an_app_override_and_settings_can_reset_the_root() {
    let picked = Rc::new(RefCell::new(Some("/Volumes/Photo".into())));
    let (mut h, sent) = folder_harness(picked);
    h.run_steps(8);
    h.state_mut().as_mut().unwrap().settings.install_locations.root = Some("/Volumes/Craft Apps".into());
    h.state_mut().as_mut().unwrap().settings.install_locations.per_app.insert("photocraft".into(), "/Volumes/Old Photo".into());
    h.run_steps(2);
    h.get_by_label("Install").scroll_to_me();
    h.run_steps(3);
    h.get_by_label("Install").click();
    h.run_steps(3);
    h.get_by_label("Use default").click();
    h.run_steps(2);
    sent.borrow_mut().clear();
    h.get_all_by_role_and_label(egui::accesskit::Role::Button, "Install").last().unwrap().click();
    h.run_steps(2);
    assert!(h.state().as_ref().unwrap().settings.install_locations.per_app.is_empty());
    assert!(
        matches!(install_commands(&sent).first(), Some(Command::Configure(options)) if options.install_locations.folder("photocraft") == Some("/Volumes/Craft Apps"))
    );
    h.get_by_label("Settings").click();
    h.run_steps(3);
    h.get_by_label("Use default").scroll_to_me();
    h.run_steps(3);
    h.get_by_label("Use default").click();
    h.run_steps(2);
    assert_eq!(h.state().as_ref().unwrap().settings.install_locations.root, None);
}

/// The install-folder fields' default comes from the host, so a person sees the real folder a
/// fresh install uses (here `/Applications`) instead of only "System default", and a saved root
/// override replaces it. The default is the same value the empty field's hint shows.
#[test]
fn the_install_folder_fields_default_to_the_hosts_answer() {
    struct DefaultHost {
        picked: Rc<RefCell<Option<String>>>,
        initial: Rc<RefCell<Option<Option<String>>>>,
    }
    impl Host for DefaultHost {
        fn pick_folder(&mut self, initial: Option<&str>) -> Option<String> {
            *self.initial.borrow_mut() = Some(initial.map(str::to_string));
            self.picked.borrow_mut().take()
        }
        fn default_install_folder(&mut self, _kind: artcraft_launcher_core::PackageKind, _arch: Option<Arch>) -> Option<String> {
            Some("/Applications".into())
        }
    }
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut backend = Some(Fake {
        events: vec![
            Event::Latest { app: "photocraft".into(), latest: LatestRelease::Found(release("photocraft", "0.6.0", NOW)), from: Origin::GitHub },
            Event::Detected { app: "photocraft".into(), installed: None },
        ],
        sent: sent.clone(),
        manage: true,
        pictures: Rc::default(),
        asked: vec![],
        clock: Rc::new(Cell::new(NOW)),
    });
    let picked = Rc::new(RefCell::new(None));
    let initial = Rc::new(RefCell::new(None));
    let mut host = DefaultHost { picked: picked.clone(), initial: initial.clone() };
    let harness = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 900.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if let Some(backend) = backend.take() {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(backend), Settings { notify: false, ..Default::default() }));
            }
            if let Some(launcher) = state {
                launcher.logic(ui.ctx(), &mut host);
                launcher.ui(ui, &mut host);
            }
        },
        None,
    );
    let mut h = harness;
    h.run_steps(8);
    // Settings' install folder starts at the host's default.
    h.get_by_label("Settings").click();
    h.run_steps(4);
    h.get_by_label("Browse…").scroll_to_me();
    h.run_steps(4);
    h.get_by_label("Browse…").click();
    h.run_steps(3);
    assert_eq!(initial.borrow().as_ref(), Some(&Some("/Applications".into())));
    h.get_by_label("Done").click();
    h.run_steps(3);
    // So does the per-app install dialog's folder, and a saved root override replaces it.
    h.get_by_label("Install").scroll_to_me();
    h.run_steps(3);
    h.get_by_label("Install").click();
    h.run_steps(3);
    h.get_by_label("Browse…").scroll_to_me();
    h.run_steps(3);
    h.get_by_label("Browse…").click();
    h.run_steps(3);
    assert_eq!(initial.borrow().as_ref(), Some(&Some("/Applications".into())));
    h.state_mut().as_mut().unwrap().settings.install_locations.root = Some("/Volumes/Craft Apps".into());
    h.run_steps(3);
    h.get_by_label("Browse…").click();
    h.run_steps(3);
    assert_eq!(initial.borrow().as_ref(), Some(&Some("/Volumes/Craft Apps".into())));
    h.check_accessibility();
}

/// A host whose default folder names the package it was asked about (`Msi X86`), and answers
/// nothing when the architecture is unknown, as the desktop host does for an MSI.
/// What Browse was last opened at: `None` until it is clicked.
type BrowsedFrom = Rc<RefCell<Option<Option<String>>>>;

struct PackageHost {
    initial: BrowsedFrom,
}
impl Host for PackageHost {
    fn pick_folder(&mut self, initial: Option<&str>) -> Option<String> {
        *self.initial.borrow_mut() = Some(initial.map(str::to_string));
        None
    }
    fn default_install_folder(&mut self, kind: artcraft_launcher_core::PackageKind, arch: Option<Arch>) -> Option<String> {
        Some(format!("{kind:?} {:?}", arch?))
    }
}

/// A Windows launcher whose apps publish only the named assets, with `preferred` formats saved.
/// Returns the harness and what Browse was last opened at.
fn windows_package_harness(
    apps: &[(&str, &[&str])],
    preferred: Vec<artcraft_launcher_core::PackageKind>,
) -> (Harness<'static, Option<LauncherUi>>, BrowsedFrom) {
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let mut events = Vec::new();
    for (id, assets) in apps {
        let mut latest = release(id, "0.6.0", NOW);
        latest.assets = assets.iter().map(|name| Asset { name: (*name).into(), size: 1, url: format!("https://x/{name}"), sha256: None }).collect();
        events.push(Event::Latest { app: (*id).into(), latest: LatestRelease::Found(latest), from: Origin::GitHub });
        events.push(Event::Detected { app: (*id).into(), installed: None });
    }
    let fake = Fake { events, sent: Rc::default(), manage: true, pictures: Rc::default(), asked: vec![], clock: Rc::new(Cell::new(NOW)) };
    let mut backend: Option<Box<dyn Backend>> = Some(Box::new(PlatformFake { inner: fake, platform: Platform { os: Os::Windows, arch: Arch::X64 } }));
    let initial = Rc::new(RefCell::new(None));
    let mut host = PackageHost { initial: initial.clone() };
    let harness = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 900.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if let Some(backend) = backend.take() {
                let settings = Settings { notify: false, preferred_kinds: preferred.clone(), ..Default::default() };
                *state = Some(LauncherUi::new(ui.ctx(), backend, settings));
            }
            if let Some(launcher) = state {
                launcher.logic(ui.ctx(), &mut host);
                launcher.ui(ui, &mut host);
            }
        },
        None,
    );
    (harness, initial)
}

/// The install dialog's default follows the package the backend will really install: Portable
/// ZIP is preferred, but this release ships only a 32-bit MSI, so the app goes into the 32-bit
/// Program Files and not the launcher's `apps` folder.
#[test]
fn the_install_folder_default_follows_the_package_the_backend_falls_back_to() {
    let (mut h, initial) =
        windows_package_harness(&[("photocraft", &["photocraft-0.6.0-windows-x86.msi"])], vec![artcraft_launcher_core::PackageKind::PortableZip]);
    h.run_steps(8);
    h.get_by_label("Install").scroll_to_me();
    h.run_steps(3);
    h.get_by_label("Install").click();
    h.run_steps(3);
    h.get_by_label("Browse…").click();
    h.run_steps(3);
    assert_eq!(initial.borrow().as_ref(), Some(&Some("Msi X86".into())));
}

/// Settings covers every app, so it shows a folder only when every app's package shares one: a
/// 32-bit-only app beside a 64-bit one leaves it at "System default".
#[test]
fn settings_shows_a_default_folder_only_when_every_app_shares_it() {
    let open_settings_browse = |apps: &[(&str, &[&str])]| {
        let (mut h, initial) = windows_package_harness(apps, vec![]);
        h.run_steps(8);
        h.get_by_label("Settings").click();
        h.run_steps(4);
        h.get_by_label("Browse…").scroll_to_me();
        h.run_steps(4);
        h.get_by_label("Browse…").click();
        h.run_steps(3);
        initial.take()
    };
    let x64 = ["photocraft-0.6.0-windows-x64.msi", "photocraft-0.6.0-windows-x86.msi"];
    let shared = open_settings_browse(&[("photocraft", &x64), ("vectorcraft", &["vectorcraft-0.6.0-windows-x64.msi"])]);
    assert_eq!(shared, Some(Some("Msi X64".into())));
    let mixed = open_settings_browse(&[("photocraft", &x64), ("vectorcraft", &["vectorcraft-0.6.0-windows-x86.msi"])]);
    assert_eq!(mixed, Some(None));
}

#[cfg(windows)]
#[test]
fn windows_package_choice_can_select_portable_and_configures_the_backend() {
    let (mut h, sent) = harness_full(
        events(),
        ViewMode::Grid,
        egui::Theme::Light,
        true,
        1.0,
        artcraft_launcher_ui_egui::TitleBar::Native,
        Rc::default(),
        Some(Platform { os: Os::Windows, arch: Arch::X64 }),
        None,
    );
    h.run_steps(8);
    h.get_by_label("Settings").click();
    h.run_steps(4);
    h.get_by_value("Installer (.msi)").click();
    h.run_steps(2);
    h.get_by_label("Portable (.zip)").click();
    h.run_steps(3);
    assert_eq!(h.state().as_ref().unwrap().settings.preferred_kinds, [artcraft_launcher_core::PackageKind::PortableZip]);
    assert!(
        sent.borrow()
            .iter()
            .any(|command| matches!(command, Command::Configure(options) if options.kinds == [artcraft_launcher_core::PackageKind::PortableZip]))
    );
}

/// The narrow 1000 x 700 native window that first showed it: a ComboBox grows to the width its
/// layout offers, so an unconstrained one started at the label's last glyph and painted over
/// "Install new apps as". The test reads the real widget rects, not a snapshot.
#[test]
fn a_narrow_settings_row_keeps_its_control_clear_of_its_label() {
    let (mut h, _sent) = harness_full(
        events(),
        ViewMode::Grid,
        egui::Theme::Light,
        true,
        1.0,
        artcraft_launcher_ui_egui::TitleBar::Native,
        Rc::default(),
        Some(Platform { os: Os::Windows, arch: Arch::X64 }),
        None,
    );
    h.set_size(egui::vec2(1000.0, 700.0));
    h.run_steps(6);
    h.get_by_label("Settings").click();
    h.run_steps(6);
    h.get_by_role_and_label(egui::accesskit::Role::Label, "Install new apps as").scroll_to_me();
    h.run_steps(3);
    let label = h.get_by_role_and_label(egui::accesskit::Role::Label, "Install new apps as").rect();
    // Several combo boxes are on the settings page; the format chooser is the one nearest the
    // label, whichever column the dialog's size puts it in.
    let combo = h
        .query_all_by_role(egui::accesskit::Role::ComboBox)
        .min_by_key(|node| {
            let r = node.rect();
            ((r.center().x - label.center().x).abs() + (r.center().y - label.center().y).abs()) as i32
        })
        .expect("the install-format chooser is on screen")
        .rect();
    assert!(!label.intersects(combo), "the label {label:?} overlaps its combo box {combo:?}");
    assert!(
        (label.center().y - combo.center().y).abs() < 2.0 && combo.left() >= label.right() + 4.0 || combo.top() >= label.bottom() + 4.0,
        "the label {label:?} and its combo box {combo:?} must keep a clear gap, on one row or stacked"
    );
}

/// Drives the real launcher lifecycle (`logic` then `ui`) in one `Context::run_ui` pass, optionally
/// asking egui for a discarded second pass over the same input.
fn drive_launcher(launcher: &mut LauncherUi, ctx: &egui::Context, events: Vec<egui::Event>, time: f64, two_passes: bool) {
    let mut first = two_passes;
    let input = egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1180.0, 1200.0))),
        time: Some(time),
        events,
        ..Default::default()
    };
    let _ = ctx.run_ui(input, |root| {
        if first {
            first = false;
            root.ctx().request_discard("test: force a second pass");
        }
        launcher.logic(&root.ctx().clone(), &mut NoHost);
        launcher.ui(root, &mut NoHost);
    });
}

/// Drives one pointer gesture as move, press and release over separate frames, optionally
/// asking for a discarded second pass on release.
fn click_steps(launcher: &mut LauncherUi, ctx: &egui::Context, at: egui::Pos2, time: f64, two_passes_on_release: bool) {
    let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
    drive_launcher(launcher, ctx, vec![egui::Event::PointerMoved(at)], time, false);
    drive_launcher(launcher, ctx, vec![button(true)], time + 0.1, false);
    drive_launcher(launcher, ctx, vec![button(false)], time + 0.2, two_passes_on_release);
}

/// Counts the `Install` commands among the recorded backend commands.
fn install_count(sent: &RefCell<Vec<Command>>) -> usize {
    sent.borrow().iter().filter(|c| matches!(c, Command::Install { .. })).count()
}

/// The AccessKit target of a visible widget, as an assistive technology would address it.
fn accesskit_target(h: &Harness<'_, Option<LauncherUi>>, label: &str) -> (egui::accesskit::NodeId, egui::accesskit::TreeId) {
    h.get_by_label(label).accesskit_node().locate()
}

/// The semantic click a screen reader emits for `target`, addressed exactly as the tree did.
fn accesskit_click(target: (egui::accesskit::NodeId, egui::accesskit::TreeId)) -> egui::Event {
    egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
        target_node: target.0,
        target_tree: target.1,
        action: egui::accesskit::Action::Click,
        data: None,
    })
}

/// Runs one visible frame with a semantic (AccessKit) click delivered on every egui pass, or over
/// one pass only when `two_passes` is false. This is the actual UI path: delivering the same click
/// on a discarded pass lets a button's own `clicked()` handler queue its action twice, so a call
/// site that bypasses `act_command`/`act` is caught instead of being hidden by egui's
/// first-pass-only pointer events.
///
/// An AccessKit request belongs to the `RawInput` of a pass, exactly as kittest queues it;
/// inserting it with `Context::input_mut` after `begin_pass` is too late in 0.37. `run_ui` also
/// spends the raw events on its first pass and re-runs the second pass without them, so the passes
/// are driven explicitly. They share one `cumulative_frame_nr` (the action boundary's visible-frame
/// identity): `run_ui` advances that counter once per visible frame, while `begin_pass`/`end_pass`
/// do not. The empty priming frame starts a fresh frame without drawing anything.
fn drive_accesskit_click(
    launcher: &mut LauncherUi,
    ctx: &egui::Context,
    target: (egui::accesskit::NodeId, egui::accesskit::TreeId),
    time: f64,
    two_passes: bool,
) {
    let screen_rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1180.0, 1200.0));
    let _ = ctx.run_ui(egui::RawInput { screen_rect: Some(screen_rect), time: Some(time), ..Default::default() }, |_| {});
    let passes = if two_passes { 2 } else { 1 };
    for pass in 0..passes {
        ctx.begin_pass(egui::RawInput {
            screen_rect: Some(screen_rect),
            time: Some(time),
            events: vec![accesskit_click(target)],
            ..Default::default()
        });
        ctx.root_ui(|root| {
            if two_passes && pass == 0 {
                root.ctx().request_discard("test: repeat the semantic click in this frame");
            }
            // Match Harness::build_ui_state's scope and frame: auto widget IDs include both.
            // Drawing directly in root_ui addresses different nodes than the captured tree.
            root.scope_builder(egui::UiBuilder::new(), |ui| {
                egui::Frame::central_panel(ui.style()).outer_margin(8.0).inner_margin(0.0).show(ui, |ui| {
                    launcher.logic(&ui.ctx().clone(), &mut NoHost);
                    launcher.ui(ui, &mut NoHost);
                });
            });
        });
        let _ = ctx.end_pass();
    }
}

/// The end-to-end shape of one pointer gesture per frame. A discarded second pass has no pointer
/// events (egui clears them after the first pass), so this alone cannot catch a call site that
/// bypasses the action boundary. The AccessKit-click tests below re-deliver the same semantic click
/// on every pass and therefore exercise the actual UI call sites twice in one visible frame.
#[test]
fn the_same_download_is_sent_once_across_discarded_passes_but_again_next_frame() {
    let (mut h, sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(8);
    let at = h.get_by_label("Update to 0.5.0").rect().center();
    let ctx = h.ctx.clone();
    let mut launcher = h.state_mut().take().expect("the harness built the launcher");

    sent.borrow_mut().clear();
    click_steps(&mut launcher, &ctx, at, 100.0, true);
    assert_eq!(install_count(&sent), 1, "one visible frame must send one install, not one per pass: {:?}", sent.borrow());

    // A new frame is a new user gesture: the guard from the previous frame must not swallow it.
    sent.borrow_mut().clear();
    click_steps(&mut launcher, &ctx, at, 200.0, false);
    assert_eq!(install_count(&sent), 1, "the next frame must accept the same action again: {:?}", sent.borrow());
}

#[test]
/// Clicking Open across a discarded second pass increments the launch counter once for the
/// visible frame.
fn opening_an_app_counts_once_across_discarded_passes() {
    let (mut h, _sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(8);
    let at = h.get_all_by_label("Open").next().expect("an up-to-date app has an Open button").rect().center();
    let ctx = h.ctx.clone();
    let mut launcher = h.state_mut().take().expect("the harness built the launcher");
    let before = launcher.settings.launches.values().copied().sum::<u32>();
    click_steps(&mut launcher, &ctx, at, 100.0, true);
    let after = launcher.settings.launches.values().copied().sum::<u32>();
    assert_eq!(after, before + 1, "one launch gesture increments the count once");
}

#[test]
/// The real Update button, clicked through AccessKit on every pass, crosses the action boundary
/// once per frame and again on the next frame.
fn the_actual_update_button_queues_once_across_a_discarded_pass() {
    let (mut h, sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(8);
    let target = accesskit_target(&h, "Update to 0.5.0");
    let ctx = h.ctx.clone();
    let mut launcher = h.state_mut().take().expect("the harness built the launcher");
    sent.borrow_mut().clear();
    drive_accesskit_click(&mut launcher, &ctx, target, 100.0, true);
    assert_eq!(install_count(&sent), 1, "the real Update button must cross the action boundary once: {:?}", sent.borrow());
    // The next visible frame is a new gesture, not a repeated pass.
    sent.borrow_mut().clear();
    drive_accesskit_click(&mut launcher, &ctx, target, 200.0, false);
    assert_eq!(install_count(&sent), 1, "the next frame must accept the same action again: {:?}", sent.borrow());
}

#[test]
/// The real Open button, clicked through AccessKit on every pass, delivers one `Launch` and
/// increments the count once.
fn the_actual_open_button_launches_once_across_a_discarded_pass() {
    let (mut h, sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(8);
    let target = h.get_all_by_label("Open").next().expect("an up-to-date app has an Open button").accesskit_node().locate();
    let ctx = h.ctx.clone();
    let mut launcher = h.state_mut().take().expect("the harness built the launcher");
    let before = launcher.settings.launches.values().copied().sum::<u32>();
    sent.borrow_mut().clear();
    drive_accesskit_click(&mut launcher, &ctx, target, 100.0, true);
    let launches = sent.borrow().iter().filter(|c| matches!(c, Command::Launch { .. })).count();
    assert_eq!(launches, 1, "the real Open button must deliver one launch, not one per pass: {:?}", sent.borrow());
    assert_eq!(launcher.settings.launches.values().copied().sum::<u32>(), before + 1, "the launch count increments once");
}

#[test]
/// A toast created by logic while no window ran is shown once on the first drawing pass and
/// expires relative to that pass.
fn a_job_finished_while_hidden_shows_one_fresh_toast_on_the_next_frame() {
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let sent = Rc::new(RefCell::new(Vec::new()));
    let backend = Fake {
        events: vec![Event::JobFinished { app: "photocraft".into(), result: Ok("PhotoCraft updated to 0.5.0".into()) }],
        sent: sent.clone(),
        manage: true,
        pictures: Rc::default(),
        asked: Vec::new(),
        clock: Rc::new(Cell::new(NOW)),
    };
    // Build the harness with no launcher first: its construction pass runs at time 0, before a test
    // can set `RawInput::time`, and would otherwise consume the hidden message.
    let mut attached = false;
    // Ask for a discarded second pass on the first drawing frame: the toast path must be
    // idempotent across passes, not just across steps.
    let mut discard_once = true;
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if let Some(l) = state {
                if !attached {
                    attached = true;
                    l.attach(ui.ctx());
                }
                if discard_once {
                    discard_once = false;
                    ui.ctx().request_discard("test: discard the first drawing pass");
                }
                l.logic(&ui.ctx().clone(), &mut NoHost);
                l.ui(ui, &mut NoHost);
            }
        },
        None,
    );
    // The tray path: `logic` runs without any UI pass, with the input time of the last shown frame.
    let windowless = egui::Context::default();
    let mut launcher = LauncherUi::new(&windowless, Box::new(backend), Settings { notify: false, language: "en".into(), ..Settings::default() });
    launcher.logic(&windowless, &mut NoHost);
    *h.state_mut() = Some(launcher);
    // The visible window's clock is far ahead of the logic-only tick's stale input time. The toast
    // must be fresh on the first drawing pass and must expire relative to it.
    h.input_mut().time = Some(1000.0);
    h.run_steps(2);
    assert_eq!(h.query_all_by_label("PhotoCraft updated to 0.5.0").count(), 1, "the hidden result is shown once");
    h.run_steps(2);
    assert_eq!(h.query_all_by_label("PhotoCraft updated to 0.5.0").count(), 1, "a discarded/next pass must not duplicate it");
    // A stale expiry (old logic time + 4.5 s) would already have dropped it; at 1004.6 the real
    // 1000.0 + 4.5 deadline has passed, so the toast goes.
    h.input_mut().time = Some(1004.6);
    h.run_steps(2);
    assert!(h.query_by_label("PhotoCraft updated to 0.5.0").is_none(), "the toast expires relative to the drawing pass");
}

#[test]
/// At 560 px the toolbar's search, refresh, settings, sort and per-app menu keep their
/// accessible names, with no unnamed input widgets and a strict accessibility pass.
fn a_narrow_toolbar_names_its_controls_and_passes_strict_accessibility() {
    let (mut h, _sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.set_size(egui::vec2(560.0, 900.0));
    h.run_steps(6);
    h.get_by_role_and_label(egui::accesskit::Role::TextInput, "Search apps");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Check for updates");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "Settings");
    h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Sort");
    h.get_by_role_and_label(egui::accesskit::Role::Button, "More actions for PhotoCraft");
    let unnamed = h.unnamed_widgets();
    assert!(unnamed.is_empty(), "narrow toolbar has unnamed input widgets: {unnamed:?}");
    h.check_accessibility();
}

#[test]
fn install_locations_survive_settings_save_and_older_settings_use_system_default() {
    let old: Settings = serde_json::from_str(r#"{"auto_update":true}"#).unwrap();
    assert!(old.auto_update);
    assert_eq!(old.install_locations, Default::default());
    let mut settings = old;
    settings.install_locations.root = Some("/Volumes/Craft Apps".into());
    settings.install_locations.per_app.insert("photocraft".into(), "/Volumes/Photo".into());
    let restored: Settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
    assert_eq!(restored.install_locations.folder("photocraft"), Some("/Volumes/Photo"));
    assert_eq!(restored.install_locations.folder("filmcraft"), Some("/Volumes/Craft Apps"));
}

#[test]
/// Logic without a window still consumes backend events; attaching to a new window then draws
/// the update cards.
fn keeps_working_with_no_window_then_draws_in_a_new_one() {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let backend =
        Fake { events: events(), sent: sent.clone(), manage: true, pictures: Rc::default(), asked: Vec::new(), clock: Rc::new(Cell::new(NOW)) };
    let windowless = egui::Context::default();
    let mut ui = LauncherUi::new(&windowless, Box::new(backend), Settings { notify: false, ..Settings::default() });
    // No window: the backend's news is still taken in.
    ui.logic(&windowless, &mut NoHost);
    assert!(sent.borrow().contains(&Command::CheckForUpdates { force: false }));
    assert_eq!(ui.update_count(), 2);
    assert_eq!(ui.settings.last_check, Some(NOW - 180));

    let mut attached = false;
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
        move |egui_ui, state: &mut LauncherUi| {
            if !attached {
                attached = true;
                state.attach(egui_ui.ctx());
            }
            state.logic(&egui_ui.ctx().clone(), &mut NoHost);
            state.ui(egui_ui, &mut NoHost);
        },
        ui,
    );
    h.run_steps(4);
    h.get_by_label("Update to 0.5.0");
    h.get_by_label("Update to 0.6.0");
}

#[test]
fn grid_shows_every_status_and_installs_on_click() {
    let (mut h, sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(8);
    snapshot(&mut h, "grid-dark");
    // Statuses the user asked for: available, installed and up to date, update with version.
    assert!(h.query_all_by_label("Available to install").count() >= 3);
    assert_eq!(h.query_all_by_label("Installed · up to date").count(), 0, "an installed app carries no badge");
    h.get_by_label("Update to 0.5.0");
    h.get_by_label("Update to 0.6.0");
    // The version sits in the card's top strip, and the date under the description.
    h.get_by_label("0.4.1 → 0.5.0");
    assert!(h.query_all_by_label("v0.4.0").count() >= 2);
    // Written like every other date ("Oct 7, 2026"); the day itself depends on the zone the test runs in.
    let updated: Vec<String> = h.query_all_by_label_contains("Last updated: ").filter_map(|n| n.accesskit_node().value()).collect();
    assert!(updated.len() >= 2, "{updated:?}");
    assert!(updated.iter().all(|l| l.starts_with("Last updated: Oct ") && l.ends_with(", 2026")), "{updated:?}");
    // The window has no title bar of its own here (the web build, and the tests).
    assert!(h.query_by_label("Close").is_none());
    assert!(h.query_all_by_label_contains("Discord").count() >= 1); // the footer icon (the header has none)
    assert!(h.query_by_label("Join the ArtCraft community on Discord").is_none(), "no Discord link in the header");
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
    h.run_steps(3);
    h.get_all_by_role_and_label(egui::accesskit::Role::Button, "Install").last().unwrap().click();
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
/// A kept download offers Retry install (which only opens install) and menu Download again
/// (Cancel then Install); apps without a kept download do not offer it.
fn a_kept_download_offers_retry_install_and_download_again() {
    let (mut h, sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(4);
    if let Some(l) = h.state_mut().as_mut() {
        l.settings.filter = artcraft_launcher_ui_egui::settings::Filter::NotInstalled;
    }
    h.run_steps(6);
    // Below the fold in the restyled grid: bring the card on screen first.
    h.get_by_label("Retry install").scroll_to_me();
    h.run_steps(4);
    snapshot(&mut h, "grid-retry-install-dark");
    sent.borrow_mut().clear();
    let retry = h.get_by_label("Retry install");
    let row = retry.rect();
    retry.click();
    h.run_steps(3);
    assert!(sent.borrow().is_empty());
    h.get_all_by_role_and_label(egui::accesskit::Role::Button, "Install").last().unwrap().click();
    h.run_steps(2);
    assert_eq!(*sent.borrow(), [Command::Install { app: "cadcraft".into() }]);
    sent.borrow_mut().clear();
    // The … menu beside it.
    let beside = |r: egui::Rect| row.y_range().contains(r.center().y) && (row.right()..row.right() + 60.0).contains(&r.left());
    h.get_all_by_label_contains("More actions").find(|n| beside(n.rect())).unwrap().click();
    h.run_steps(3);
    snapshot(&mut h, "grid-retry-install-menu-dark");
    h.get_by_label("Download again").click();
    h.run_steps(3);
    assert_eq!(*sent.borrow(), [Command::Cancel { app: "cadcraft".into() }]);
    h.get_all_by_role_and_label(egui::accesskit::Role::Button, "Install").last().unwrap().click();
    h.run_steps(2);
    assert_eq!(*sent.borrow(), [Command::Cancel { app: "cadcraft".into() }, Command::Install { app: "cadcraft".into() }]);
    // Apps without a kept download don't offer it.
    assert_eq!(h.query_all_by_label("Retry install").count(), 1);
}

#[test]
fn a_dismissed_install_prompt_is_not_called_a_cancelled_download() {
    let finished = |app: &str, why: &str| Event::JobFinished { app: app.into(), result: Err(why.into()) };
    // The password prompt was dismissed with the download kept (cadcraft) and without (pdfcraft);
    // a download was cancelled (vectorcraft).
    let mut ev = events();
    ev.push(finished("cadcraft", INSTALL_CANCELLED));
    ev.push(finished("pdfcraft", INSTALL_CANCELLED));
    ev.push(finished("vectorcraft", CANCELLED));
    let (mut h, _) = harness_with(ev, ViewMode::Grid, egui::Theme::Dark, true, 1.0);
    h.run_steps(6);
    snapshot(&mut h, "grid-install-cancelled-dark");
    for toast in ["Install cancelled. The download is kept for the next try.", "Install cancelled", "Download cancelled"] {
        assert_eq!(h.query_all_by_label(toast).count(), 1, "{toast}");
    }
}

#[test]
/// A ready update names its version and installs on Retry, while an up-to-date app offers
/// Reinstall instead of Download again.
fn a_kept_update_names_its_version_and_an_up_to_date_app_is_not_downloaded_again() {
    let mut ev = events();
    for app in ["effectcraft", "wordcraft"] {
        ev.push(Event::ReadyToInstall { app: app.into(), ready: true });
    }
    let (mut h, sent) = harness_with(ev, ViewMode::Table, egui::Theme::Dark, true, 1.0);
    h.run_steps(8);
    snapshot(&mut h, "table-retry-update-dark");
    sent.borrow_mut().clear();
    assert!(h.query_by_label("Update to 0.6.0").is_none());
    h.get_by_label("Retry update to 0.6.0").click();
    h.run_steps(2);
    assert_eq!(*sent.borrow(), [Command::Install { app: "effectcraft".into() }]);
    // WordCraft is up to date: its menu offers Reinstall, which uses the kept file.
    let row = h.get_by_role_and_label(egui::accesskit::Role::Label, "WordCraft").rect().center().y;
    h.get_all_by_label_contains("More actions").find(|n| (n.rect().center().y - row).abs() < 25.0).unwrap().click();
    h.run_steps(3);
    h.get_by_label("Reinstall");
    h.check_accessibility();
    assert!(h.query_by_label("Download again").is_none());
}

#[test]
fn an_install_waiting_for_another_says_so_and_can_be_cancelled() {
    let mut events = events();
    // Two updates reached the system's installer together: one runs, the other waits its turn.
    events.push(Event::Progress { app: "photocraft".into(), phase: JobPhase::Installing });
    events.push(Event::Progress { app: "effectcraft".into(), phase: JobPhase::Waiting });
    let (mut h, sent) = harness_with(events, ViewMode::Grid, egui::Theme::Dark, true, 1.0);
    h.run_steps(6);
    snapshot(&mut h, "grid-waiting-dark");
    h.get_by_label("Installing…");
    h.get_by_label("Waiting for another app…");
    // Only the waiting one: the install that is running can't be called off.
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    let cancels: Vec<_> = sent.borrow().iter().filter(|c| matches!(c, Command::Cancel { .. })).cloned().collect();
    assert_eq!(cancels, [Command::Cancel { app: "effectcraft".into() }]);
}

#[test]
fn the_launcher_update_waiting_for_other_installs_can_be_cancelled() {
    let mut ev = events();
    ev.push(Event::LauncherUpdate(release("artcraft-launcher", "0.2.0", NOW)));
    ev.push(Event::Progress { app: "artcraft-launcher".into(), phase: JobPhase::Waiting });
    let (mut h, sent) = harness_with(ev, ViewMode::Grid, egui::Theme::Dark, true, 1.0);
    h.run_steps(6);
    snapshot(&mut h, "grid-launcher-waiting-dark");
    h.get_by_label("Waiting for other installs…");
    sent.borrow_mut().clear();
    h.get_by_label("Cancel update").click();
    h.run_steps(2);
    assert_eq!(*sent.borrow(), [Command::Cancel { app: "artcraft-launcher".into() }]);
}

#[test]
/// The launcher banner's Cancel crosses the action boundary once per visible frame.
fn the_launcher_banner_cancel_is_sent_once_across_discarded_passes() {
    let mut ev = events();
    ev.push(Event::LauncherUpdate(release("artcraft-launcher", "0.2.0", NOW)));
    ev.push(Event::Progress { app: "artcraft-launcher".into(), phase: JobPhase::Waiting });
    let (mut h, sent) = harness_with(ev, ViewMode::Grid, egui::Theme::Dark, true, 1.0);
    h.run_steps(6);
    let at = h.get_by_label("Cancel update").rect().center();
    let ctx = h.ctx.clone();
    let mut launcher = h.state_mut().take().expect("the harness built the launcher");
    sent.borrow_mut().clear();
    click_steps(&mut launcher, &ctx, at, 100.0, true);
    let cancels = sent.borrow().iter().filter(|c| matches!(c, Command::Cancel { .. })).count();
    assert_eq!(cancels, 1, "the banner's Cancel must cross the action boundary once: {:?}", sent.borrow());
}

#[test]
fn each_cancellation_says_what_it_called_off() {
    let finished = |app: &str, why: &str| Event::JobFinished { app: app.into(), result: Err(why.into()) };
    let mut ev = events();
    // An uninstall called off while it waited for the installer.
    ev.push(Event::Progress { app: "lightcraft".into(), phase: JobPhase::Removing });
    ev.push(Event::Progress { app: "lightcraft".into(), phase: JobPhase::Waiting });
    ev.push(finished("lightcraft", CANCELLED));
    // The launcher's own update, cancelled while it waited.
    ev.push(Event::LauncherUpdate(release("artcraft-launcher", "0.2.0", NOW)));
    ev.push(Event::Progress { app: "artcraft-launcher".into(), phase: JobPhase::Waiting });
    ev.push(finished("artcraft-launcher", CANCELLED));
    // A download cancelled mid-way.
    ev.push(finished("filmcraft", CANCELLED));
    let (mut h, _) = harness_with(ev, ViewMode::Grid, egui::Theme::Dark, true, 1.0);
    h.run_steps(6);
    for toast in ["Uninstall cancelled", "Update cancelled", "Download cancelled"] {
        assert_eq!(h.query_all_by_label(toast).count(), 1, "{toast}");
    }
}

#[test]
fn table_view_and_light_theme() {
    let (mut h, sent) = harness(ViewMode::Table, egui::Theme::Light, true);
    h.run_steps(8);
    snapshot(&mut h, "table-light");
    h.get_by_label("Update to 0.5.0").click();
    h.run_steps(2);
    assert!(sent.borrow().contains(&Command::Install { app: "photocraft".into() }));
    h.get_all_by_label("Open").next().unwrap().click();
    h.run_steps(2);
    assert!(sent.borrow().iter().any(|c| matches!(c, Command::Launch { .. })));
}

#[test]
/// Hide removes an app from All into Hidden Apps; Unhide restores it, clears the hidden set and
/// returns the filter to All.
fn hiding_an_app_removes_it_until_it_is_unhidden() {
    let (mut h, _) = harness(ViewMode::Table, egui::Theme::Dark, true);
    h.run_steps(6);
    let row = h.get_by_role_and_label(egui::accesskit::Role::Label, "PhotoCraft").rect().center().y;
    h.get_all_by_label_contains("More actions").find(|n| (n.rect().center().y - row).abs() < 25.0).unwrap().click();
    h.run_steps(3);
    h.get_by_label("Hide App").click();
    h.run_steps(4);
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Label, "PhotoCraft").is_none());
    assert!(h.state().as_ref().unwrap().settings.hidden.contains("photocraft"));
    h.get_by_label("Hidden Apps 1").click();
    h.run_steps(4);
    h.get_by_role_and_label(egui::accesskit::Role::Label, "PhotoCraft");
    assert!(h.query_by_role_and_label(egui::accesskit::Role::Label, "WordCraft").is_none());
    let row = h.get_by_role_and_label(egui::accesskit::Role::Label, "PhotoCraft").rect().center().y;
    h.get_all_by_label_contains("More actions").find(|n| (n.rect().center().y - row).abs() < 25.0).unwrap().click();
    h.run_steps(3);
    h.get_by_label("Unhide App").click();
    h.run_steps(4);
    h.get_by_role_and_label(egui::accesskit::Role::Label, "PhotoCraft");
    assert!(h.query_by_label("Hidden Apps 1").is_none());
    assert!(h.state().as_ref().unwrap().settings.hidden.is_empty());
    assert_eq!(h.state().as_ref().unwrap().settings.filter, artcraft_launcher_ui_egui::settings::Filter::All);
}

#[test]
/// The view switch stores Table, and the table names each app once in its wordmark rather than
/// again on the decorative icon.
fn switching_views() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    h.get_by_label("Show apps as a table").click();
    h.run_steps(4);
    assert_eq!(h.state().as_ref().unwrap().settings.view, ViewMode::Table);
    h.get_by_role_and_label(egui::accesskit::Role::Label, "PhotoCraft");
    h.get_by_role_and_label(egui::accesskit::Role::Label, "WordCraft");
    // The row icon is decorative; the wordmark carries the name exactly once.
    assert_eq!(h.query_all_by_role_and_label(egui::accesskit::Role::Image, "PhotoCraft").count(), 0, "the table icon must not repeat the wordmark");
    assert_eq!(
        h.query_all_by_role_and_label(egui::accesskit::Role::Label, "PhotoCraft").count(),
        1,
        "the table wordmark names PhotoCraft exactly once"
    );
}

/// How far the middle of these lines of text is below the middle of the app icon beside them, in points.
fn below_icon_centre(h: &Harness<'_, Option<LauncherUi>>, lines: &[&str]) -> f32 {
    let text = lines.iter().map(|l| h.get_by_role_and_label(egui::accesskit::Role::Label, l).rect()).reduce(|a, b| a.union(b)).unwrap();
    let icon = h
        .query_all(egui_kittest::kittest::by().role(egui::accesskit::Role::Image))
        .map(|n| n.rect())
        .filter(|r| r.right() <= text.left() && r.top() < text.bottom() && r.bottom() > text.top())
        .max_by(|a, b| a.right().total_cmp(&b.right()))
        .unwrap();
    assert!(text.left() - icon.right() < 24.0, "no icon beside {lines:?}");
    text.center().y - icon.center().y
}

#[test]
fn name_and_tagline_are_centred_on_the_icon() {
    for (scale, suffix) in [(1.0, "-1x"), (1.5, "-1.5x"), (2.0, "-2x")] {
        let (mut h, _) = harness_at(ViewMode::Grid, egui::Theme::Dark, true, scale);
        h.run_steps(8);
        snapshot(&mut h, &format!("grid-dark{suffix}"));
        let off = below_icon_centre(&h, &["PhotoCraft", "Image editing"]);
        assert!(off.abs() < 0.25, "grid at {scale}x: {off}");
        let (mut h, _) = harness_at(ViewMode::Table, egui::Theme::Light, true, scale);
        h.run_steps(8);
        snapshot(&mut h, &format!("table-light{suffix}"));
        let off = below_icon_centre(&h, &["PhotoCraft", "Image editing"]);
        assert!(off.abs() < 0.25, "table at {scale}x: {off}");
    }
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
    // Status: apps with an update first (suite order breaks the tie), then the up to date ones.
    h.get_by_label("Status").click();
    h.run_steps(3);
    let o = order(&h);
    assert_eq!(&o[..4], ["PhotoCraft", "EffectCraft", "LightCraft", "WordCraft"], "{o:?}");
    // Reversed: the apps not installed first, then up to date, then the updates last. The table
    // keeps its column's order; only the grid puts installed apps above the rest.
    h.get_by_label("Status").click();
    h.run_steps(3);
    let o = order(&h);
    assert_eq!(&o[o.len().saturating_sub(4)..], ["LightCraft", "WordCraft", "PhotoCraft", "EffectCraft"], "{o:?}");
    // The old Installed and Latest columns are gone; Version shows the number, and from and to for an update.
    assert!(h.query_by_label("Latest").is_none());
    h.get_by_label("Version");
    assert!(h.query_all_by_label("0.4.0").count() >= 2);
    h.get_by_label("0.4.1 → 0.5.0");
    h.get_by_label("0.5.0 → 0.6.0");
    snapshot(&mut h, "table-sorted-status");
    // App: Z to A on the second click.
    h.get_by_label("App").click();
    h.run_steps(3);
    h.get_by_label("App").click();
    h.run_steps(3);
    let o = order(&h);
    assert_eq!(o.first().map(String::as_str), Some("WordCraft"), "{o:?}");
}

/// The first close to the tray is explained once, by a system notification (#45, #215).
#[test]
fn the_first_close_to_the_tray_is_explained_once() {
    struct Notices(Vec<(String, String)>);
    impl Host for Notices {
        fn notify(&mut self, title: &str, body: &str) {
            self.0.push((title.to_string(), body.to_string()));
        }
    }
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    let mut host = Notices(Vec::new());
    let l = h.state_mut().as_mut().unwrap();
    assert!(!l.settings.tray_hint_shown);
    l.hidden_to_tray(&mut host);
    l.hidden_to_tray(&mut host);
    assert!(l.settings.tray_hint_shown);
    assert_eq!(host.0.len(), 1, "{:?}", host.0);
    assert_eq!(host.0[0].0, "ArtCraft Launcher is still running");
    assert!(host.0[0].1.contains("tray icon"), "{}", host.0[0].1);
}

#[test]
/// The Settings dialog announces its heading once and shows the community section's Discord
/// entry alongside the footer's.
fn settings_link_to_discord() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    h.get_by_label("Settings").click();
    h.run_steps(4);
    h.check_accessibility();
    assert_eq!(
        h.query_all_by_role_and_label(egui::accesskit::Role::Label, "Settings").count(),
        1,
        "the Settings hero icon must not repeat its heading"
    );
    snapshot(&mut h, "settings-dark");
    h.get_by_label("Questions, ideas and news from the ArtCraft team");
    // The footer icon and the button in Settings.
    assert!(h.query_all_by_label_contains("Discord").count() >= 2);
}

#[test]
/// After the taller Settings dialog, the uninstall prompt keeps its own content-sized modal with
/// its buttons close under the text (#88), and Cancel closes it.
fn the_uninstall_prompt_keeps_its_own_size_after_a_taller_dialog() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    // Settings is the tallest dialog: open it and close it again.
    h.get_by_label("Settings").click();
    h.run_steps(4);
    h.get_by_label("Done").click();
    h.run_steps(3);
    // PhotoCraft is installed, so its … menu offers Uninstall.
    h.get_by_role_and_label(egui::accesskit::Role::Button, "More actions for PhotoCraft").click();
    h.run_steps(3);
    h.get_by_label("Uninstall…").click();
    h.run_steps(4);
    h.check_accessibility();
    h.get_by_role_and_label(egui::accesskit::Role::Label, "Uninstall PhotoCraft?");
    assert_eq!(h.query_all_by_label_contains("Uninstall PhotoCraft?").count(), 1, "the title icon is decorative, not a second copy of the heading");
    snapshot(&mut h, "uninstall-prompt-dark");
    // The buttons follow the text closely, not somewhere down the height Settings had (#88).
    let text = h.get_by_label("The app will be moved to the Trash. Your documents and settings stay.").rect();
    let button = h.get_by_label("Uninstall").rect();
    let gap = button.top() - text.bottom();
    assert!((0.0..48.0).contains(&gap), "the buttons sit {gap} px below the text");
    h.get_by_label("Cancel").click();
    h.run_steps(3);
    assert!(h.query_by_label("Uninstall").is_none());
}

#[test]
/// The footer links website and GitHub, cards link only GitHub, and the table menu adds the
/// app's website and repository.
fn links_to_the_website_github_and_each_apps_pages() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    // Footer: website, GitHub and Discord.
    for label in ["Website", "GitHub", "Discord"] {
        assert!(h.query_all_by_label(label).count() >= 1, "{label}");
    }
    // Cards: every app links to its repo and to its own page on getartcraft.com (plus the footer's).
    let shown_cards = h.query_all_by_label("GitHub").count() - 1;
    let shown_sites = h.query_all_by_label("Website").count() - 1;
    assert!(shown_cards >= 13, "{shown_cards}");
    assert_eq!(shown_sites, 0, "the cards only link to GitHub; the website is the footer's icon");
    // The table's … menu has the app's website and GitHub repo.
    let (mut h, _) = harness(ViewMode::Table, egui::Theme::Dark, true);
    h.run_steps(6);
    h.get_all_by_label_contains("More actions").nth(8).unwrap().click(); // WordCraft
    h.run_steps(3);
    h.get_by_label("GitHub repository");
    // The footer's Website link and WordCraft's now-live one in the menu.
    assert!(h.query_all_by_label_contains("Website").count() >= 2);
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
/// Footer, card and social links open their expected pages; one card menu's All releases action
/// opens a repository releases page.
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
    assert_eq!(sites, vec!["https://getartcraft.com".to_string()], "{sites:?}");
    // Social icons in the footer.
    assert_eq!(click_opens(&mut h, "X").as_deref(), Some("https://x.com/get_artcraft"));
    assert_eq!(click_opens(&mut h, "YouTube").as_deref(), Some("https://www.youtube.com/@OfficialArtcraftStudios"));
    assert_eq!(click_opens(&mut h, "TikTok").as_deref(), Some("https://www.tiktok.com/@artcraft.studios"));
    assert_eq!(click_opens(&mut h, "Instagram").as_deref(), Some("https://www.instagram.com/officialartcraftstudios"));
    assert_eq!(click_opens(&mut h, "LinkedIn").as_deref(), Some("https://www.linkedin.com/company/artcraft-ai"));
    // The … menu.
    h.get_all_by_label_contains("More actions").next().unwrap().click();
    h.run_steps(3);
    // The first card is the first installed app by name.
    let url = click_opens(&mut h, "All releases").unwrap_or_default();
    assert!(url.starts_with("https://github.com/storytold/") && url.ends_with("/releases"), "{url}");
}

/// An asset URL can come straight from GitHub's release JSON; it must not reach the opener when
/// it carries an opener metacharacter, whether it is shown on a card or in the launcher dialog.
#[test]
fn hostile_release_asset_urls_never_reach_the_opener() {
    const HOSTILE: &str = "https://github.com/storytold/photocraft/releases/download/v0.5.0/photocraft-0.5.0-macos-universal.dmg|calc.exe";

    let mut ev = events();
    for event in &mut ev {
        if let Event::Latest { app, latest: LatestRelease::Found(release), .. } = event
            && app.as_str() == "photocraft"
            && let Some(asset) = release.assets.first_mut()
        {
            asset.url = HOSTILE.to_string();
        }
    }
    let (mut h, _) = harness_with(ev, ViewMode::Grid, egui::Theme::Dark, false, 1.0);
    h.run_steps(8);
    assert!(h.query_all_by_label("Download for macOS").count() >= 8, "the cards must offer their asset downloads");
    let opened = all_opened(&mut h, "Download for macOS");
    assert!(!opened.iter().any(|url| url.as_str() == HOSTILE), "a hostile card asset URL reached the opener: {opened:?}");

    let mut ev = events();
    let mut launcher_release = release("artcraft-launcher", "0.2.0", NOW);
    launcher_release.assets[0].url = HOSTILE.to_string();
    ev.push(Event::LauncherUpdate(launcher_release));
    let (mut h, _) = harness_with(ev, ViewMode::Grid, egui::Theme::Dark, false, 1.0);
    h.run_steps(6);
    assert_eq!(h.query_all_by_label("Download").count(), 1, "the launcher update dialog must be open");
    let opened = all_opened(&mut h, "Download");
    assert!(opened.is_empty(), "a hostile launcher-dialog asset URL reached the opener: {opened:?}");
}

#[test]
fn discord_link_opens_app_when_discord_is_installed() {
    let (mut h, _) = harness_with_discord(Some(artcraft_launcher_core::catalog::DISCORD_APP_URL));
    h.run_steps(6);
    assert_eq!(click_opens(&mut h, "Discord").as_deref(), Some("discord://-/invite/artcraft"));
}

#[test]
/// Background detection waits five minutes: it is not sent at startup or on every frame, and is
/// sent once at the five-minute mark.
fn looks_again_at_installed_apps_every_few_minutes_not_every_frame() {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let clock = Rc::new(Cell::new(NOW));
    let mut backend =
        Some(Fake { events: events(), sent: sent.clone(), manage: true, pictures: Rc::default(), asked: Vec::new(), clock: clock.clone() });
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
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
/// Update and restart queues `UpdateLauncher`, progress hides the button, and
/// `LauncherRestarting` asks the host to quit; the update title is announced once.
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
    let mut backend = Some(Scripted {
        inner: Fake { events, sent: sent.clone(), manage: true, pictures: Rc::default(), asked: Vec::new(), clock: Rc::new(Cell::new(NOW)) },
        later: shared_events.clone(),
    });
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
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
    h.get_by_role_and_label(egui::accesskit::Role::Label, "ArtCraft Launcher 0.2.0 is available.");
    assert_eq!(
        h.query_all_by_label_contains("ArtCraft Launcher 0.2.0 is available").count(),
        1,
        "the update title is announced once, not once by the icon and once by the heading"
    );
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
/// A found launcher update opens the relaunch prompt once; Later dismisses it and leaves the
/// toolbar banner.
fn launcher_update_prompts_to_relaunch_with_updated_version() {
    let mut ev = events();
    ev.push(Event::LauncherUpdate(release("artcraft-launcher", "0.2.0", NOW)));
    let (mut h, _) = harness_with(ev, ViewMode::Grid, egui::Theme::Dark, true, 1.0);
    h.run_steps(6);
    // Dialog pops up on launch with the update prompt
    h.get_by_role_and_label(egui::accesskit::Role::Label, "ArtCraft Launcher 0.2.0 is available.");
    assert_eq!(
        h.query_all_by_label_contains("ArtCraft Launcher 0.2.0 is available").count(),
        1,
        "the update title is announced once, not once by the icon and once by the heading"
    );
    h.get_by_label("A new version is available. Would you like to relaunch with the updated version?");
    h.get_by_label("What's new");
    // Clicking "Later" dismisses the modal dialog
    h.get_by_label("Later").click();
    h.run_steps(3);
    assert!(h.query_by_label("A new version is available. Would you like to relaunch with the updated version?").is_none());
    // Toolbar update banner is now visible with Update and restart
    h.get_by_label("Update and restart");
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
/// A feed alert is acknowledged only by OK (persisted), while an inline announcement stays until
/// its own Dismiss is clicked.
fn feed_announcements_and_alerts_show_until_dismissed() {
    use artcraft_launcher_core::feed::Notice;
    let notice = |id: &str, title: &str, link: Option<&str>| Notice {
        id: id.into(),
        title: title.into(),
        body: format!("{title} body"),
        link: link.map(str::to_string),
        created_at: None,
    };
    let sent = Rc::new(RefCell::new(Vec::new()));
    let mut evs = events();
    evs.push(Event::Notices {
        announcements: vec![notice("a1", "CADCraft 1.0 is out", Some("https://getartcraft.com/news"))],
        alerts: vec![notice("x1", "Security update", Some("https://getartcraft.com/security"))],
    });
    let mut backend = Some(Fake { events: evs, sent, manage: true, pictures: Rc::default(), asked: Vec::new(), clock: Rc::new(Cell::new(NOW)) });
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
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
    h.run_steps(6);
    // The alert pops up first; OK acknowledges it for good.
    h.get_by_label("Security update");
    h.get_by_label("Open link");
    h.get_by_label("OK").click();
    h.run_steps(4);
    assert!(h.query_by_label("Security update").is_none());
    assert!(h.state().as_ref().unwrap().settings.dismissed_notices.contains("x1"));
    // The announcement stays inline until dismissed.
    h.get_by_label("CADCraft 1.0 is out");
    h.get_by_label("Learn more");
    h.get_all_by_label("Dismiss").next().unwrap().click();
    h.run_steps(4);
    assert!(h.query_by_label("CADCraft 1.0 is out").is_none());
    assert!(h.state().as_ref().unwrap().settings.dismissed_notices.contains("a1"));
}

#[test]
fn automatic_checks_run_every_five_minutes() {
    use artcraft_launcher_ui_egui::settings::CheckEvery;
    assert_eq!(Settings::default().check_every.seconds(), Some(300));
    // Older saved choices still load, and mean "automatically" now.
    assert_eq!(CheckEvery::Day.seconds(), Some(300));
    assert_eq!(CheckEvery::Never.seconds(), None);
}

#[test]
/// With auto-update on, outdated apps are queued for Install and the launcher for
/// `UpdateLauncher`.
fn automatic_updates_queue_installs_for_outdated_apps_and_launcher() {
    let mut ev = events();
    ev.push(Event::LauncherUpdate(release("artcraft-launcher", "0.2.0", NOW)));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let sent2 = sent.clone();
    let mut backend =
        Some(Fake { events: ev, sent: sent2, manage: true, pictures: Rc::default(), asked: Vec::new(), clock: Rc::new(Cell::new(NOW)) });
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), Settings { auto_update: true, notify: false, ..Settings::default() }));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut NoHost);
                l.ui(ui, &mut NoHost);
            }
        },
        None,
    );
    h.run_steps(4);
    let commands = sent.borrow().clone();
    assert!(commands.contains(&Command::Install { app: "photocraft".into() }), "outdated photocraft should be queued for install: {commands:?}");
    assert!(commands.contains(&Command::Install { app: "effectcraft".into() }), "outdated effectcraft should be queued for install: {commands:?}");
    assert!(commands.contains(&Command::UpdateLauncher), "launcher update should be queued: {commands:?}");
}

#[test]
/// With auto-update off, no app install or launcher update is queued.
fn automatic_updates_are_disabled_by_default() {
    let mut ev = events();
    ev.push(Event::LauncherUpdate(release("artcraft-launcher", "0.2.0", NOW)));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let sent2 = sent.clone();
    let mut backend =
        Some(Fake { events: ev, sent: sent2, manage: true, pictures: Rc::default(), asked: Vec::new(), clock: Rc::new(Cell::new(NOW)) });
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
        move |ui, state: &mut Option<LauncherUi>| {
            if state.is_none()
                && let Some(b) = backend.take()
            {
                *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), Settings { auto_update: false, notify: false, ..Settings::default() }));
            }
            if let Some(l) = state.as_mut() {
                l.logic(&ui.ctx().clone(), &mut NoHost);
                l.ui(ui, &mut NoHost);
            }
        },
        None,
    );
    h.run_steps(4);
    let commands = sent.borrow().clone();
    assert!(!commands.contains(&Command::Install { app: "photocraft".into() }), "photocraft should not be auto-installed when auto_update is false");
    assert!(!commands.contains(&Command::UpdateLauncher), "launcher should not be auto-updated when auto_update is false");
}

#[test]
/// The footer names the release origins actually in use, adding GitHub only when some release
/// came from there.
fn the_footer_names_the_services_behind_what_is_on_screen() {
    let footer = |extra: Vec<Event>| {
        let mut evs = events();
        evs.extend(extra);
        let sent = Rc::new(RefCell::new(Vec::new()));
        let mut backend = Some(Fake { events: evs, sent, manage: true, pictures: Rc::default(), asked: Vec::new(), clock: Rc::new(Cell::new(NOW)) });
        let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
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
        h.run_steps(6);
        let label =
            h.get_by_label_contains("Checked ").accesskit_node().label().or_else(|| h.get_by_label_contains("Checked ").accesskit_node().value());
        label.unwrap_or_default()
    };
    assert_eq!(footer(Vec::new()), "Checked 3 min ago · using ArtCraft API");
    let from_github =
        vec![Event::Latest { app: "wordcraft".into(), latest: LatestRelease::Found(release("wordcraft", "0.3.1", NOW)), from: Origin::GitHub }];
    assert_eq!(footer(from_github), "Checked 3 min ago · using ArtCraft API and GitHub");
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
/// A custom title bar shows Minimize, Maximize and Close, and Close requests the viewport close
/// command.
fn custom_title_bar_has_window_buttons() {
    struct FramelessHost;
    impl Host for FramelessHost {
        fn title_bar(&self) -> artcraft_launcher_ui_egui::TitleBar {
            artcraft_launcher_ui_egui::TitleBar::Custom
        }
    }
    let mut backend = Some(Fake {
        events: events(),
        sent: Rc::new(RefCell::new(Vec::new())),
        manage: true,
        pictures: Rc::default(),
        asked: Vec::new(),
        clock: Rc::new(Cell::new(NOW)),
    });
    let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 1200.0)).build_ui_state(
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

/// Maximized, the window's top-right pixel is the screen's corner: there is no resize band there,
/// so it belongs to the close button, as it does with Windows' own caption buttons (#374).
#[test]
fn maximized_the_close_button_reaches_the_screens_corner() {
    let (mut h, _) = harness_with_title_bar(events(), ViewMode::Grid, egui::Theme::Dark, true, 1.0, artcraft_launcher_ui_egui::TitleBar::Custom);
    h.run_steps(6);
    let set_maximized = |h: &mut Harness<'_, Option<LauncherUi>>, maximized: bool| {
        h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().maximized = Some(maximized);
        h.run_steps(2);
    };
    // Presses and releases at `pos`, and returns what the window was asked to do.
    let click = |h: &mut Harness<'_, Option<LauncherUi>>, pos: egui::Pos2| -> Vec<egui::ViewportCommand> {
        let mut asked = Vec::new();
        for pressed in [true, false] {
            let input = h.input_mut();
            input.events.push(egui::Event::PointerMoved(pos));
            input.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
            h.step();
            asked.extend(h.output().viewport_output.values().flat_map(|v| v.commands.iter().cloned()));
        }
        asked
    };
    // Windowed, the clickable area stops short of the button's top-right corner by the resize
    // band; maximized, it reaches it.
    let windowed = h.get_by_label("Close").rect();
    set_maximized(&mut h, true);
    let maximized = h.get_by_label("Close").rect();
    assert!(maximized.top() < windowed.top() && maximized.right() > windowed.right(), "windowed {windowed:?}, maximized {maximized:?}");
    // The button's own corner stands in for the screen's: the harness keeps the app 8 px in from
    // the window. Windowed, it is beyond the band and egui's click tolerance; maximized, it closes.
    let corner = egui::pos2(maximized.right() - 0.5, maximized.top() + 0.5);
    let asked = click(&mut h, corner);
    assert!(asked.contains(&egui::ViewportCommand::Close), "maximized: {asked:?}");
    set_maximized(&mut h, false);
    let asked = click(&mut h, corner);
    assert!(!asked.contains(&egui::ViewportCommand::Close), "windowed: {asked:?}");
    // And the window's own corner is still for resizing.
    let asked = click(&mut h, egui::pos2(1180.0 - 0.5, 0.5));
    assert!(asked.iter().any(|c| matches!(c, egui::ViewportCommand::BeginResize(_))), "{asked:?}");
}

#[test]
/// Clicking a card opens its details drawer with the page links, wrapping picture arrows and
/// Esc-to-close.
fn clicking_a_card_opens_its_details_drawer() {
    let (mut h, _sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(8);
    // Every app has a page: a card of each generation opens one.
    h.get_by_label("VectorCraft details");
    h.get_by_label("WordCraft details");
    h.get_by_label("PhotoCraft details").click();
    h.run_steps(10);
    h.check_accessibility();
    assert_eq!(
        h.query_all_by_role_and_label(egui::accesskit::Role::Image, "PhotoCraft").count(),
        0,
        "the hero icon is decorative; the wordmark carries the app name"
    );
    assert_eq!(
        h.query_all_by_role_and_label(egui::accesskit::Role::Label, "PhotoCraft").count(),
        2,
        "exactly the grid card's wordmark and the details drawer's wordmark carry the app name"
    );
    snapshot(&mut h, "details-dark");
    h.get_all_by_label_contains("The image editor you already know how to use").last().unwrap();
    h.get_by_label_contains("Curves and type layers · 1/4");
    // The links the page offers as buttons of their own.
    h.get_by_label("Website ↗");
    h.get_by_label("GitHub ↗");
    h.get_by_label("Releases ↗");
    // The picture arrows wrap around, and the thumbnails jump.
    h.get_by_label("Previous picture (←)").click();
    h.run_steps(3);
    h.get_by_label_contains("Selections and masked adjustments · 4/4");
    h.get_by_label("Next picture (→)").click();
    h.run_steps(3);
    h.get_by_label_contains("Curves and type layers · 1/4");
    // Esc closes it and the page is gone once it has slid out.
    h.key_press(egui::Key::Escape);
    h.run_steps(10);
    // Only the card's own line is left once the page has slid out.
    assert_eq!(h.query_all_by_label_contains("The image editor you already know").count(), 1);
}

#[test]
fn details_drawer_in_the_light_theme() {
    let (mut h, _sent) = harness(ViewMode::Grid, egui::Theme::Light, true);
    h.run_steps(8);
    h.get_by_label("PhotoCraft details").click();
    h.run_steps(10);
    snapshot(&mut h, "details-light");
    h.get_all_by_label_contains("The image editor you already know how to use").last().unwrap();
}

fn fetches(sent: &Rc<RefCell<Vec<Command>>>, app: &str) -> usize {
    sent.borrow().iter().filter(|c| matches!(c, Command::FetchScreenshots { app: a } if a == app)).count()
}

#[test]
fn opening_a_page_asks_for_its_pictures_once_and_only_while_it_lacks_some() {
    let (mut h, sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(8);
    assert_eq!(fetches(&sent, "photocraft"), 0, "nothing is fetched before a page is opened");
    h.get_by_label("PhotoCraft details").click();
    h.run_steps(10);
    assert_eq!(fetches(&sent, "photocraft"), 1);
    h.get_by_label("Enlarge picture");
    // Closed and opened again: the pictures are in hand, so nothing is asked.
    h.key_press(egui::Key::Escape);
    h.run_steps(10);
    h.get_by_label("PhotoCraft details").click();
    h.run_steps(10);
    assert_eq!(fetches(&sent, "photocraft"), 1);
    h.get_by_label("Enlarge picture");
    // Another page asks for its own, and a page without pictures asks for nothing.
    h.key_press(egui::Key::Escape);
    h.run_steps(10);
    h.get_by_label("VectorCraft details").scroll_to_me();
    h.run_steps(3);
    h.get_by_label("VectorCraft details").click();
    h.run_steps(10);
    assert_eq!(fetches(&sent, "vectorcraft"), 1);
    h.key_press(egui::Key::Escape);
    h.run_steps(10);
    h.get_by_label("PdfCraft details").scroll_to_me();
    h.run_steps(3);
    h.get_by_label("PdfCraft details").click();
    h.run_steps(10);
    assert_eq!(fetches(&sent, "pdfcraft"), 0);
}

#[test]
fn a_frame_stands_in_for_the_pictures_until_they_arrive() {
    let pictures = Rc::new(Cell::new(Pictures::Silent));
    let (mut h, sent) = harness_pictures(events(), ViewMode::Grid, egui::Theme::Dark, true, 1.0, pictures.clone());
    h.run_steps(8);
    h.get_by_label("PhotoCraft details").click();
    h.run_steps(10);
    snapshot(&mut h, "details-loading-dark");
    h.get_by_label("Loading pictures…");
    assert!(h.query_by_label("Enlarge picture").is_none(), "there is nothing to enlarge yet");
    // The rest of the page is there, and the arrows work on the frames.
    h.get_all_by_label_contains("The image editor you already know how to use").last().unwrap();
    h.get_by_label("Next picture (→)").click();
    h.run_steps(3);
    h.get_by_label_contains("Levels with a live histogram · 2/4");
    // Asking again meanwhile is not done: the answer is on its way.
    assert_eq!(fetches(&sent, "photocraft"), 1);
    pictures.set(Pictures::Online);
    h.run_steps(6);
    assert!(h.query_by_label("Loading pictures…").is_none());
    h.get_by_label("Enlarge picture");
}

#[test]
fn offline_the_frame_says_so_and_opening_the_page_again_retries() {
    let pictures = Rc::new(Cell::new(Pictures::Offline));
    let (mut h, sent) = harness_pictures(events(), ViewMode::Grid, egui::Theme::Light, true, 1.0, pictures.clone());
    h.run_steps(8);
    h.get_by_label("PhotoCraft details").click();
    h.run_steps(10);
    snapshot(&mut h, "details-offline-light");
    h.get_by_label("Pictures couldn't be loaded. Reopen this page to try again.");
    assert!(h.query_by_label("Loading pictures…").is_none());
    assert!(h.query_by_label("Enlarge picture").is_none());
    h.get_all_by_label_contains("The image editor you already know how to use").last().unwrap();
    // Back online: the page is closed and opened again, which asks again.
    pictures.set(Pictures::Online);
    h.key_press(egui::Key::Escape);
    h.run_steps(10);
    h.get_by_label("PhotoCraft details").click();
    h.run_steps(10);
    assert_eq!(fetches(&sent, "photocraft"), 2);
    h.get_by_label("Enlarge picture");
    assert!(h.query_by_label("Pictures couldn't be loaded. Reopen this page to try again.").is_none());
}

#[test]
fn the_enlarged_view_shows_a_frame_for_a_picture_still_missing() {
    let pictures = Rc::new(Cell::new(Pictures::SecondFails));
    let (mut h, _sent) = harness_pictures(events(), ViewMode::Grid, egui::Theme::Dark, true, 1.0, pictures);
    h.run_steps(8);
    h.get_by_label("PhotoCraft details").click();
    h.run_steps(10);
    h.get_by_label("Enlarge picture").click();
    h.run_steps(3);
    // The second picture is missing: flipping to it keeps the view open, with a frame; the others still show.
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(3);
    // Once in the drawer behind (it shows the same picture) and once in the enlarged view.
    assert_eq!(h.query_all_by_label("Pictures couldn't be loaded. Reopen this page to try again.").count(), 2);
    assert_eq!(h.query_all_by_label("Close (Esc)").count(), 2, "the enlarged view is still open");
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(3);
    assert!(h.query_by_label("Pictures couldn't be loaded. Reopen this page to try again.").is_none(), "the third picture is fine");
}

#[test]
fn a_page_without_pictures_has_no_gallery() {
    let (mut h, _sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(8);
    // PdfCraft's screenshots still show its old name (PrintCraft), so its page has none for now.
    h.get_by_label("PdfCraft details").scroll_to_me();
    h.run_steps(3);
    h.get_by_label("PdfCraft details").click();
    h.run_steps(10);
    snapshot(&mut h, "details-no-pictures-dark");
    h.get_all_by_label_contains("The open-source PDF workbench.").last().unwrap();
    h.get_by_label_contains("Search as you type");
    assert!(h.query_by_label("Enlarge picture").is_none());
    assert!(h.query_by_label("Next picture (→)").is_none());
}

#[test]
/// Enlarge adds a second, keyboard-navigable view over the drawer; Esc closes the enlarged
/// picture before the drawer.
fn the_lead_picture_can_be_enlarged() {
    let (mut h, _sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(8);
    h.get_by_label("PhotoCraft details").click();
    h.run_steps(10);
    // The drawer has one "Close (Esc)"; the enlarged view adds its own.
    assert_eq!(h.query_all_by_label("Close (Esc)").count(), 1);
    h.get_by_label("Enlarge picture").click();
    h.run_steps(4);
    h.check_accessibility();
    snapshot(&mut h, "details-zoom-dark");
    assert_eq!(h.query_all_by_label("Close (Esc)").count(), 2);
    // The keyboard flips through the pictures there too.
    h.key_press(egui::Key::ArrowRight);
    h.run_steps(3);
    h.get_by_label_contains("Levels with a live histogram · 2/4");
    // Esc closes the enlarged picture first, and the drawer only the second time.
    h.key_press(egui::Key::Escape);
    h.run_steps(4);
    assert_eq!(h.query_all_by_label("Close (Esc)").count(), 1);
    h.get_all_by_label_contains("The image editor you already know how to use").last().unwrap();
    h.key_press(egui::Key::Escape);
    h.run_steps(10);
    // Only the card's own line is left once the page has slid out.
    assert_eq!(h.query_all_by_label_contains("The image editor you already know").count(), 1);
}

/// Every page, whatever it holds: no pictures (PdfCraft), one (DesignCraft) or several. The arrow keys are
/// pressed more often than there are pictures, both ways, to wrap around the list and to do nothing on a
/// page that has one or none; the enlarged view is opened wherever there is a picture to enlarge.
#[test]
fn every_page_opens_and_pages_through_its_pictures() {
    let (mut h, _sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    // Tall enough to show all thirteen cards at once.
    h.set_size(egui::vec2(1180.0, 3000.0));
    h.run_steps(8);
    for d in &artcraft_launcher_core::details::DETAILS {
        let name = SUITE.iter().find(|a| a.id == d.id).map(|a| a.name).unwrap_or(d.id);
        h.get_by_label(&format!("{name} details")).scroll_to_me();
        h.run_steps(3);
        h.get_by_label(&format!("{name} details")).click();
        h.run_steps(8);
        h.get_all_by_label_contains(d.headline).last().unwrap();
        let presses = d.shots.len() + 2;
        for key in [egui::Key::ArrowRight, egui::Key::ArrowLeft] {
            for _ in 0..presses {
                h.key_press(key);
                h.run_steps(1);
            }
        }
        // The picture can be enlarged exactly when the page has one.
        assert_eq!(h.query_by_label("Enlarge picture").is_some(), !d.shots.is_empty(), "{}", d.id);
        if !d.shots.is_empty() {
            h.get_by_label("Enlarge picture").click();
            h.run_steps(3);
            for key in [egui::Key::ArrowRight, egui::Key::ArrowLeft] {
                for _ in 0..presses {
                    h.key_press(key);
                    h.run_steps(1);
                }
            }
            // A full lap in each direction brings it back to the first picture. Its caption is in the drawer
            // and in the enlarged view, and has a counter only when there are several pictures.
            let first = &d.shots[0];
            let caption = if d.shots.len() > 1 { format!("{} · 1/{}", first.caption, d.shots.len()) } else { first.caption.to_string() };
            assert!(h.query_all_by_label_contains(&caption).count() >= 1, "{}: {caption}", d.id);
            h.key_press(egui::Key::Escape);
            h.run_steps(3);
        }
        h.key_press(egui::Key::Escape);
        h.run_steps(10);
        // The card keeps its own copy of the line; the page's is gone.
        assert!(h.query_all_by_label_contains(d.headline).count() <= 1, "{} did not close", d.id);
    }
}

/// The drawer and the enlarged picture at sizes far below what the desktop window allows (its minimum is
/// 720 x 500, but the web build has no such floor) and far above, resized while they are open.
#[test]
fn the_drawer_and_the_enlarged_picture_survive_any_window_size() {
    let (mut h, _sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(8);
    h.get_by_label("PhotoCraft details").click();
    h.run_steps(10);
    let sizes = [(1180.0, 900.0), (720.0, 500.0), (480.0, 320.0), (320.0, 200.0), (120.0, 90.0), (40.0, 30.0), (4000.0, 2400.0)];
    for zoom in [false, true] {
        if zoom {
            h.get_by_label("Enlarge picture").click();
            h.run_steps(3);
        }
        for (w, ht) in sizes {
            h.set_size(egui::vec2(w, ht));
            h.run_steps(3);
        }
        // Back to a normal size, still working.
        h.set_size(egui::vec2(1180.0, 900.0));
        h.run_steps(4);
        if zoom {
            h.key_press(egui::Key::Escape);
            h.run_steps(3);
        }
        h.get_all_by_label_contains("The image editor you already know how to use").last().unwrap();
    }
}

#[test]
/// At 720–1180 px the rate-limit notice stays readable and clear of the footer's checked time
/// and eight right-side links.
fn the_rate_limit_notice_never_runs_under_the_footer_links() {
    for width in [720.0, 960.0, 1180.0] {
        let mut evs = events();
        evs.push(Event::CheckFinished { at: NOW - 180, source: Source::GitHubApi, rate_limited_until: Some(NOW + 1800) });
        let mut backend = Some(Fake {
            events: evs,
            sent: Rc::new(RefCell::new(Vec::new())),
            manage: true,
            pictures: Rc::default(),
            asked: Vec::new(),
            clock: Rc::new(Cell::new(NOW)),
        });
        let mut h = test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(width, 900.0)).build_ui_state(
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
        h.run_steps(6);
        snapshot(&mut h, &format!("footer-rate-limited-{width}"));
        // Full or short, the notice says when GitHub can be asked again.
        let notice = h.get_by_label_contains("in 30 min").rect();
        let checked = h.get_by_label_contains("Checked ").rect();
        // Cut short in a narrow window, but enough of it left to read.
        assert!(notice.width() >= 120.0 && notice.right() <= width, "{width}: notice {notice:?}");
        // Everything on the footer's right: the website, GitHub and Discord icons and the social icons.
        let mut right = Vec::new();
        for label in ["Website", "GitHub", "Discord", "X", "YouTube", "TikTok", "Instagram", "LinkedIn"] {
            right.extend(h.query_all_by_label(label).map(|n| n.rect()).filter(|r| (r.center().y - notice.center().y).abs() < 12.0));
        }
        assert_eq!(right.len(), 8, "{width}: {right:?}");
        assert!(h.query_by_label_contains("Installing for").is_none(), "{width}: the footer no longer names the platform");
        for r in right {
            assert!(!r.intersects(notice), "{width}: notice {notice:?} overlaps {r:?}");
            assert!(!r.intersects(checked), "{width}: check time {checked:?} overlaps {r:?}");
        }
    }
}

#[test]
/// At 720–1180 px refresh, search, sort and view share one unclipped toolbar row without
/// overlaps.
fn toolbar_keeps_navigation_refresh_search_and_view_on_one_unclipped_row() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    for width in [720.0, 800.0, 1180.0] {
        h.set_size(egui::vec2(width, 900.0));
        h.run_steps(6);
        let refresh = h.get_by_label("Check for updates").rect();
        let search = h.query_all_by_role(egui::accesskit::Role::TextInput).next().unwrap().rect();
        let view = h.get_by_label("Show apps as cards").rect();
        let sort = h.get_by_role_and_label(egui::accesskit::Role::ComboBox, "Sort").rect();
        assert!(refresh.top() > 48.0, "{width}: refresh belongs in the toolbar, not the header: {refresh:?}");
        assert!((refresh.center().y - search.center().y).abs() < 2.0, "{width}: refresh and search must share a row");
        assert!(refresh.right() <= search.left() + 1.0, "{width}: refresh overlaps search: {refresh:?}, {search:?}");
        assert!(refresh.right() <= sort.left() + 1.0, "{width}: sort overlaps refresh: {refresh:?}, {sort:?}");
        assert!(sort.right() <= search.left() + 1.0, "{width}: sort overlaps search: {sort:?}, {search:?}");
        assert!(search.right() <= view.left() + 1.0 && view.right() <= width, "{width}: search overlaps view or view is clipped");
        for (label, rect) in [("refresh", refresh), ("search", search), ("sort", sort), ("view", view)] {
            assert!(rect.left() >= 0.0 && rect.right() <= width, "{width}: {label} is outside the window: {rect:?}");
        }
        assert!(!sort.intersects(search) && !view.intersects(search), "{width}: toolbar controls overlap");
        h.check_accessibility();
    }
}

#[test]
fn the_toolbar_has_no_filter_dropdown() {
    use artcraft_launcher_ui_egui::settings::Filter;
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    for label in ["All 13", "Installed 4", "Updates 2", "Not installed 9"] {
        assert!(h.query_by_value(label).is_none() && h.query_by_label(label).is_none(), "{label}");
    }
    // Only the sort dropdown is left.
    assert_eq!(h.query_all_by_role(egui::accesskit::Role::ComboBox).count(), 1);
    assert_eq!(h.state().as_ref().unwrap().settings.filter, Filter::All);
}

#[test]
fn saved_filters_are_migrated_on_ui_construction() {
    use artcraft_launcher_ui_egui::settings::Filter;

    for (saved, expected) in [
        (r#"{"filter":"All"}"#, Filter::All),
        (r#"{"filter":"Installed"}"#, Filter::All),
        (r#"{"filter":"Updates"}"#, Filter::All),
        (r#"{"filter":"NotInstalled"}"#, Filter::All),
        (r#"{"filter":"Hidden"}"#, Filter::Hidden),
    ] {
        let mut settings: Settings = serde_json::from_str(saved).unwrap();
        settings.language = "en".into(); // Avoid the global system-locale probe in parallel tests.
        settings.last_check = Some(NOW);
        let backend = Fake {
            events: Vec::new(),
            sent: Rc::default(),
            manage: true,
            pictures: Rc::default(),
            asked: Vec::new(),
            clock: Rc::new(Cell::new(NOW)),
        };
        let ui = LauncherUi::new(&egui::Context::default(), Box::new(backend), settings);
        assert_eq!(ui.settings.filter, expected, "{saved}");
        assert_eq!(ui.settings.last_check, Some(NOW));
    }
}

/// Checks that, at one window width, every header control stays inside the window and clear of
/// the brand text, that a custom title bar's buttons are there and inside, and snapshots the
/// three widths that matter visually.
fn assert_header_at_width(h: &mut Harness<'_, Option<LauncherUi>>, bar: artcraft_launcher_ui_egui::TitleBar, theme: egui::Theme, width: f32) {
    for label in ["Show apps as cards", "Show apps as a table", "Settings", "Check for updates"] {
        let rect = h.get_by_label(label).rect();
        assert!(rect.left() >= 0.0 && rect.right() <= width, "{bar:?} {width}: {label} {rect:?}");
        if let Some(text) = h.query_by_label("Craft App Suite") {
            assert!(!rect.intersects(text.rect()), "{bar:?} {width}: Craft App Suite overlaps {label} {rect:?}");
        }
    }
    if bar == artcraft_launcher_ui_egui::TitleBar::Custom {
        for label in ["Minimize", "Maximize", "Close"] {
            let rect = h.get_by_label(label).rect();
            assert!(rect.left() >= 0.0 && rect.right() <= width, "{width}: {label} {rect:?}");
        }
    }
    if width == 1180.0 {
        assert!(h.query_by_label("Launcher").is_none());
        h.get_by_label("Craft App Suite");
    }
    if [720.0, 800.0, 1180.0].contains(&width) {
        snapshot(h, &format!("header-{theme:?}-{bar:?}-{width}"));
    }
}

#[test]
fn narrow_header_keeps_the_view_switch_clear_of_brand_text() {
    use artcraft_launcher_ui_egui::TitleBar;
    for theme in [egui::Theme::Dark, egui::Theme::Light] {
        for bar in [TitleBar::Native, TitleBar::MacOs, TitleBar::Custom] {
            let (mut h, _) = harness_with_title_bar(events(), ViewMode::Grid, theme, true, 1.0, bar);
            for width in (720..=1180).step_by(10).chain([720, 1180]).map(|w| w as f32) {
                h.set_size(egui::vec2(width, 900.0));
                h.run_steps(6);
                assert_header_at_width(&mut h, bar, theme, width);
            }
            h.get_by_label("Show apps as a table").click();
            h.run_steps(2);
            assert_eq!(h.state().as_ref().unwrap().settings.view, ViewMode::Table);
        }
    }
}

/// A shell on an OS whose animation setting the test switches.
struct OsMotion(Rc<Cell<bool>>);

impl Host for OsMotion {
    fn reduce_motion(&self) -> bool {
        self.0.get()
    }
}

/// The grid in a window too short for it, at 60 frames a second, on an OS asking for fewer
/// animations (`reduce`) or not. Nothing is downloading, so the window is still unless something
/// animates.
fn short_window(reduce: Rc<Cell<bool>>) -> Harness<'static, Option<LauncherUi>> {
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let events = events().into_iter().filter(|e| !matches!(e, Event::Progress { .. })).collect();
    let mut backend =
        Some(Fake { events, sent: Rc::default(), manage: true, pictures: Rc::default(), asked: Vec::new(), clock: Rc::new(Cell::new(NOW)) });
    let mut host = OsMotion(reduce);
    let mut h =
        test_harness_builder(egui::Theme::Dark).with_size(egui::vec2(1180.0, 500.0)).with_step_dt(1.0 / 60.0).with_max_steps(120).build_ui_state(
            move |ui, state: &mut Option<LauncherUi>| {
                if state.is_none()
                    && let Some(b) = backend.take()
                {
                    let settings = Settings { view: ViewMode::Grid, notify: false, ..Settings::default() };
                    *state = Some(LauncherUi::new(ui.ctx(), Box::new(b), settings));
                }
                if let Some(l) = state.as_mut() {
                    l.logic(&ui.ctx().clone(), &mut host);
                    l.ui(ui, &mut host);
                }
            },
            None,
        );
    h.run_steps(8);
    h
}

/// How far down the window the grid's first card is.
fn grid_top(h: &Harness<'_, Option<LauncherUi>>) -> f32 {
    h.get_by_label("Image editing").rect().top()
}

/// Turns the mouse wheel `lines` down over the grid, by way of the shell's `raw_input_hook`. The
/// next frame takes it in.
fn turn_wheel(h: &mut Harness<'_, Option<LauncherUi>>, lines: f32) {
    h.hover_at(egui::pos2(590.0, 300.0));
    h.run_steps(2);
    let wheel = egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, -lines),
        phase: egui::TouchPhase::Move,
        modifiers: egui::Modifiers::NONE,
        source: egui::MouseWheelSource::Wheel,
    };
    let mut raw = egui::RawInput { events: vec![wheel], ..Default::default() };
    if let Some(l) = h.state() {
        l.raw_input(&h.ctx, &mut raw);
    }
    for event in raw.events {
        h.event(event);
    }
}

#[test]
fn the_os_animation_setting_is_followed_as_it_changes() {
    let reduce = Rc::new(Cell::new(true));
    let mut h = short_window(reduce.clone());
    assert_eq!(h.ctx.global_style().animation_time, 0.0);
    // Switched back on while the launcher runs: noticed within a couple of seconds.
    reduce.set(false);
    h.run_steps(150);
    assert_eq!(h.ctx.global_style().animation_time, egui::Style::default().animation_time);
    reduce.set(true);
    h.run_steps(150);
    assert_eq!(h.ctx.global_style().animation_time, 0.0);
}

#[test]
fn with_animations_off_the_wheel_scrolls_at_once() {
    let mut h = short_window(Rc::new(Cell::new(true)));
    let notch = 3.0 * h.ctx.options(|o| o.input_options.line_scroll_speed);
    let start = grid_top(&h);
    turn_wheel(&mut h, 3.0);
    // The frame the wheel turned in, and the one that draws where it scrolled to.
    h.run_steps(2);
    let moved = start - grid_top(&h);
    assert!((moved - notch).abs() < 0.5, "moved {moved} of {notch}");
    // Nothing is left to glide.
    h.run_steps(30);
    assert_eq!(start - grid_top(&h), moved);
}

#[test]
fn with_animations_on_the_wheel_glides() {
    let mut h = short_window(Rc::new(Cell::new(false)));
    let notch = 3.0 * h.ctx.options(|o| o.input_options.line_scroll_speed);
    let start = grid_top(&h);
    turn_wheel(&mut h, 3.0);
    h.run_steps(2);
    let moved = start - grid_top(&h);
    assert!(moved > 0.0 && moved < notch / 2.0, "moved {moved} of {notch}");
    // And is there within the second.
    h.run_steps(60);
    let moved = start - grid_top(&h);
    assert!((moved - notch).abs() < 0.5, "moved {moved} of {notch}");
}

/// Moves the pointer from one card to the next. Returns how many frames are drawn before the
/// window is still again.
fn frames_to_hover_the_next_card(h: &mut Harness<'_, Option<LauncherUi>>) -> u64 {
    let cards: Vec<egui::Pos2> = ["Image editing", "Vector illustration"].map(|t| h.get_by_label(t).rect().center()).to_vec();
    // Still to begin with: the icons are in, the first card is lit and the scroll bar is showing.
    h.hover_at(cards[0]);
    h.run();
    h.hover_at(cards[1]);
    h.run()
}

#[test]
fn with_animations_off_hovering_a_card_shows_at_once() {
    let mut h = short_window(Rc::new(Cell::new(true)));
    let frames = frames_to_hover_the_next_card(&mut h);
    assert!(frames <= 5, "{frames} frames");
}

#[test]
fn with_animations_on_hovering_a_card_animates() {
    let mut h = short_window(Rc::new(Cell::new(false)));
    // The rule wipes in and the icon rises over 0.3 s, at 60 frames a second.
    let frames = frames_to_hover_the_next_card(&mut h);
    assert!(frames >= 10, "{frames} frames");
}

#[test]
/// Command-plus-wheel over the main UI raises the global zoom factor.
fn control_wheel_zooms_the_main_ui() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(4);
    let before = h.ctx.zoom_factor();
    h.input_mut().events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        phase: egui::TouchPhase::Move,
        delta: egui::vec2(0.0, 100.0),
        modifiers: egui::Modifiers::COMMAND,
        source: egui::MouseWheelSource::Unknown,
    });
    h.step();
    h.run_steps(2);
    assert!(h.ctx.zoom_factor() > before);
}

#[test]
/// With release notes open, keyboard and wheel zoom change only the notes text (reset works)
/// while the global zoom stays put and the release heading appears once.
fn changelog_shortcuts_zoom_only_the_notes_text() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(4);
    // The grid's default order is name A-Z, so pick the app explicitly: the first card is not
    // PhotoCraft, and the heading assertion below is about this release's own page.
    h.get_by_label("More actions for PhotoCraft").click();
    h.run_steps(2);
    h.get_by_label("Release notes").click();
    h.run_steps(4);
    assert_eq!(
        h.query_all_by_role_and_label(egui::accesskit::Role::Image, "PhotoCraft 0.5.0").count(),
        0,
        "the Notes hero icon is decorative and must not repeat the release heading"
    );
    assert_eq!(
        h.query_all_by_role_and_label(egui::accesskit::Role::Label, "PhotoCraft 0.5.0").count(),
        1,
        "the release heading appears exactly once"
    );
    let before = h.get_by_label("What's Changed").rect().height();
    let global = h.ctx.zoom_factor();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Plus);
    h.run_steps(3);
    assert_eq!(h.ctx.zoom_factor(), global, "zooming notes must not resize the main UI");
    assert!(h.get_by_label("What's Changed").rect().height() > before);

    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num0);
    h.run_steps(3);
    assert!((h.get_by_label("What's Changed").rect().height() - before).abs() < 0.5);
    h.input_mut().events.push(egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        phase: egui::TouchPhase::Move,
        delta: egui::vec2(0.0, 100.0),
        modifiers: egui::Modifiers::COMMAND,
        source: egui::MouseWheelSource::Unknown,
    });
    h.step();
    h.run_steps(2);
    assert_eq!(h.ctx.zoom_factor(), global);
    let larger = h.get_by_label("What's Changed").rect().height();
    assert!(larger > before);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Minus);
    h.run_steps(3);
    assert!(h.get_by_label("What's Changed").rect().height() < larger);
    snapshot(&mut h, "notes-zoomed");
    h.get_by_label("Close").click();
    h.run_steps(3);
    assert_eq!(h.ctx.zoom_factor(), global);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Equals);
    h.run_steps(3);
    assert!(h.ctx.zoom_factor() > global, "main-UI keyboard zoom should still work after closing notes");
}

#[test]
fn command_m_minimizes_the_mac_window_only() {
    use artcraft_launcher_ui_egui::TitleBar;
    for bar in [TitleBar::MacOs, TitleBar::Native, TitleBar::Custom] {
        let (mut h, _) = harness_with_title_bar(events(), ViewMode::Grid, egui::Theme::Dark, true, 1.0, bar);
        h.run_steps(4);
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::M,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::MAC_CMD,
        });
        h.step();
        let minimized = h.output().viewport_output.values().any(|v| v.commands.iter().any(|c| matches!(c, egui::ViewportCommand::Minimized(true))));
        assert_eq!(minimized, bar == TitleBar::MacOs, "{bar:?}");
    }
}

#[test]
fn an_occupied_folder_asks_before_replacing_it() {
    let path = "/Volumes/Craft Apps/PhotoCraft.app";
    let mut ev = events();
    ev.push(Event::Occupied { app: "photocraft".into(), path: path.into() });
    ev.push(Event::JobFinished { app: "photocraft".into(), result: Err(format!("{path} already exists")) });
    let (mut h, sent) = harness_with(ev, ViewMode::Grid, egui::Theme::Dark, true, 1.0);
    h.run_steps(6);
    h.get_by_label(&format!("{path} already exists. Installing PhotoCraft will overwrite it."));
    assert!(h.query_by_label(&format!("{path} already exists")).is_none());
    sent.borrow_mut().clear();
    h.get_by_label("Replace").click();
    h.run_steps(2);
    assert_eq!(*sent.borrow(), [Command::Replace { app: "photocraft".into() }]);
    assert!(h.query_by_label("Replace").is_none());
}

#[test]
fn a_not_installed_card_is_grey_until_the_pointer_is_over_it() {
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.set_size(egui::vec2(1180.0, 1100.0));
    h.run_steps(8);
    let icon = h.get_by_label("CADCraft icon").rect();
    // The most colourful pixel in the icon: how far apart its red, green and blue are.
    let colour = |h: &mut Harness<'_, Option<LauncherUi>>| {
        let image = h.render().unwrap();
        let mut most = 0_i32;
        for y in icon.top() as u32..icon.bottom() as u32 {
            for x in icon.left() as u32..icon.right() as u32 {
                let p = image.get_pixel(x, y).0;
                let (hi, lo) = (p[0].max(p[1]).max(p[2]), p[0].min(p[1]).min(p[2]));
                most = most.max(i32::from(hi) - i32::from(lo));
            }
        }
        most
    };
    assert!(colour(&mut h) <= 8, "a not installed app's icon is grey");
    h.hover_at(egui::pos2(icon.center().x + 120.0, icon.center().y + 60.0));
    // The colour picture decodes on a background thread: give it real time, as a slow machine
    // (CI's Windows runner) needs more than a few frames.
    let mut shown = 0;
    for _ in 0..100 {
        h.run_steps(2);
        shown = colour(&mut h);
        if shown > 40 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(shown > 40, "the icon shows its colour under the pointer");
    h.hover_at(egui::pos2(5.0, 400.0));
    h.run_steps(8);
    assert!(colour(&mut h) <= 8, "the colour goes again when the pointer leaves");
}

#[test]
fn the_sort_offers_name_both_ways_recently_updated_and_frequently_used() {
    use artcraft_launcher_ui_egui::settings::SortKey;
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    h.get_by_value("Name (A–Z)").click();
    h.run_steps(3);
    for label in ["Name (A–Z)", "Name (Z–A)", "Recently updated", "Frequently used"] {
        h.get_by_label(label);
    }
    h.get_by_label("Name (Z–A)").click();
    h.run_steps(4);
    let s = &h.state().as_ref().unwrap().settings;
    assert_eq!((s.sort, s.sort_descending), (SortKey::Name, true));
    h.get_by_value("Name (Z–A)").click();
    h.run_steps(3);
    h.get_by_label("Frequently used").click();
    h.run_steps(4);
    assert_eq!(h.state().as_ref().unwrap().settings.sort, SortKey::Frequent);
}

#[test]
fn opening_an_app_counts_and_frequently_used_puts_the_most_opened_first() {
    use artcraft_launcher_ui_egui::settings::SortKey;
    let (mut h, sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    h.state_mut().as_mut().unwrap().settings.sort = SortKey::Frequent;
    h.run_steps(4);
    // Installed apps in name order: Effect (updates), Light (Open), Photo (updates), Word (Open).
    let opens: Vec<_> = h.get_all_by_label("Open").collect();
    assert_eq!(opens.len(), 2);
    opens[1].click(); // WordCraft
    h.run_steps(3);
    assert!(sent.borrow().contains(&Command::Launch { app: "wordcraft".into() }));
    assert_eq!(h.state().as_ref().unwrap().settings.launches.get("wordcraft"), Some(&1));
    h.run_steps(4);
    let word = h.get_by_label("WordCraft details").rect();
    let light = h.get_by_label("LightCraft details").rect();
    assert!(word.min.y < light.min.y || (word.min.y == light.min.y && word.min.x < light.min.x), "{word:?} {light:?}");
}

#[test]
fn an_apps_page_is_a_card_in_the_middle_with_next_and_previous_arrows_and_closes_from_outside() {
    use artcraft_launcher_core::details::AppDetails;
    let (mut h, _) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    let headline = |id: &str| AppDetails::by_id(id).unwrap().headline;
    let showing = |h: &Harness<'_, Option<LauncherUi>>, id: &str| h.query_all_by_label_contains(headline(id)).count() >= 2;
    h.get_by_label("PhotoCraft details").click();
    h.run_steps(12);
    assert!(showing(&h, "photocraft"));
    // A card with a margin of window all round, not a panel against one edge.
    let close = h.get_by_label("Close (Esc)").rect();
    assert!(close.right() < 1180.0 - 24.0 && close.left() > 24.0, "{close:?}");
    // Next goes to the following app in the grid's order (Word after Photo), and back again.
    h.get_by_label("Next app").click();
    h.run_steps(12);
    assert!(showing(&h, "wordcraft"));
    h.get_by_label("Previous app").click();
    h.run_steps(12);
    assert!(showing(&h, "photocraft"));
    // A click on the card's own margin keeps it open (the card is 1116 wide, centred in 1180).
    let margin = egui::pos2(36.0, 450.0);
    h.hover_at(margin);
    h.run_steps(2);
    h.drag_at(margin);
    h.run_steps(2);
    h.drop_at(margin);
    h.run_steps(12);
    assert!(showing(&h, "photocraft"), "a click on the card closed it");
    // A click on the dimmed window outside the card closes it.
    h.hover_at(egui::pos2(10.0, 450.0));
    h.run_steps(2);
    h.drag_at(egui::pos2(10.0, 450.0));
    h.run_steps(2);
    h.drop_at(egui::pos2(10.0, 450.0));
    h.run_steps(30);
    assert!(!showing(&h, "photocraft"), "a click outside did not close it");
}
