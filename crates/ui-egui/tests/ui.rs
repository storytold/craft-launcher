//! Drives the launcher UI against a fake backend: what's shown for each status, and which
//! commands the buttons send. `ARTCRAFT_LAUNCHER_SNAPSHOTS=<dir>` also saves screenshots; run that
//! with `-- --test-threads=1`, as rendering on several threads at once can crash Vulkan loaders
//! older than 1.4.350 (KhronosGroup/Vulkan-Loader#1863).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use artcraft_launcher_core::mcp::{AiTool, Entry, McpReport, McpServer};
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

struct Fake {
    events: Vec<Event>,
    sent: Rc<RefCell<Vec<Command>>>,
    manage: bool,
    clock: Rc<Cell<i64>>,
}

impl Backend for Fake {
    fn send(&mut self, command: Command) {
        if command == Command::McpScan {
            self.events.push(Event::Mcp(mcp_report()));
        }
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
    ev.push(Event::ReadyToInstall { app: "cadcraft".into(), ready: true });
    ev.push(Event::CheckFinished { at: NOW - 180, source: Source::GitHubApi, rate_limited_until: None });
    ev
}

/// Three installed apps can start their MCP server; WordCraft's command-line tool is missing.
/// The AI tools are a Windows machine's: Claude Code already starts PhotoCraft's server, Codex's
/// settings are broken, Zed and others aren't installed, and the Microsoft Store's Claude Desktop
/// keeps its settings down a very long path.
fn mcp_report() -> McpReport {
    let server = |id: &str| Ok(McpServer { command: format!("/usr/local/bin/{id}-cli"), args: vec!["mcp".into()] });
    let tool = |id: &str, name: &str, found: bool, path: &str, entries: &[(&str, Entry)], error: Option<&str>| AiTool {
        id: id.into(),
        name: name.into(),
        path: format!(r"C:\Users\Jeremy\{path}"),
        found,
        entries: entries.iter().map(|(a, e)| ((*a).to_string(), *e)).collect(),
        error: error.map(str::to_string),
    };
    McpReport {
        servers: [
            ("photocraft", server("photocraft")),
            ("lightcraft", server("lightcraft")),
            ("effectcraft", server("effectcraft")),
            (
                "wordcraft",
                Err("The command-line tool of WordCraft (wordcraft-cli) wasn't found next to the app. Reinstalling the app puts it back.".into()),
            ),
        ]
        .into_iter()
        .map(|(id, s)| (id.to_string(), s))
        .collect(),
        tools: vec![
            tool("zed", "Zed", false, r"AppData\Roaming\Zed\settings.json", &[], None),
            tool("claude-desktop", "Claude Desktop", true, CLAUDE_DESKTOP_PATH, &[], None),
            tool("claude-code", "Claude Code", true, ".claude.json", &[("photocraft", Entry::Current), ("vectorcraft", Entry::Different)], None),
            tool("cursor", "Cursor", true, r".cursor\mcp.json", &[("lightcraft", Entry::Foreign)], None),
            tool("codex", "Codex", true, r".codex\config.toml", &[], Some("TOML parse error at line 3, column 1")),
            tool("vscode", "VS Code", true, r"AppData\Roaming\Code\User\mcp.json", &[], None),
            tool("vscode-insiders", "VS Code Insiders", false, r"AppData\Roaming\Code - Insiders\User\mcp.json", &[], None),
            tool("windsurf", "Windsurf", false, r".codeium\windsurf\mcp_config.json", &[], None),
            tool("gemini-cli", "Gemini CLI", true, r".gemini\settings.json", &[], None),
            tool("copilot-cli", "GitHub Copilot CLI", true, r".copilot\mcp-config.json", &[], None),
            tool("antigravity", "Antigravity", true, r".gemini\config\mcp_config.json", &[], None),
            tool(
                "cline",
                "Cline",
                false,
                r"AppData\Roaming\Code\User\globalStorage\saoudrizwan.claude-dev\settings\cline_mcp_settings.json",
                &[],
                None,
            ),
            tool("kiro", "Kiro", false, r".kiro\settings\mcp.json", &[], None),
            tool("lm-studio", "LM Studio", true, r".lmstudio\mcp.json", &[], None),
            tool("opencode", "opencode", false, r".config\opencode\opencode.jsonc", &[], None),
        ],
    }
}

/// Where the Microsoft Store's Claude Desktop keeps its settings, under the home folder.
const CLAUDE_DESKTOP_PATH: &str = r"AppData\Local\Packages\Claude_pzs8sxrjxfjjc\LocalCache\Roaming\Claude\claude_desktop_config.json";

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
    harness_sized(events, view, theme, manage, scale, egui::vec2(1180.0, 900.0))
}

/// [`harness_with`] in a window of `size` points.
fn harness_sized(
    events: Vec<Event>,
    view: ViewMode,
    theme: egui::Theme,
    manage: bool,
    scale: f32,
    size: egui::Vec2,
) -> (Harness<'static, Option<LauncherUi>>, Rc<RefCell<Vec<Command>>>) {
    // Integration tests compile without cfg!(test), so system_lang() picks up the
    // developer's locale. Force English so labels match the assertions.
    artcraft_launcher_ui_egui::i18n::set_test_lang(artcraft_launcher_ui_egui::i18n::Lang::EN);
    let sent = Rc::new(RefCell::new(Vec::new()));
    let sent2 = sent.clone();
    let mut backend = Some(Fake { events, sent: sent2, manage, clock: Rc::new(Cell::new(NOW)) });
    let settings = Settings { view, notify: false, ..Settings::default() };
    // No `.wgpu()`: it gives every test its own GPU device, and those segfault on parallel test threads (see above).
    // The default renderer creates one only when a test renders.
    let harness = Harness::builder().with_size(size).with_pixels_per_point(scale).with_theme(theme).build_ui_state(
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
fn keeps_working_with_no_window_then_draws_in_a_new_one() {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let backend = Fake { events: events(), sent: sent.clone(), manage: true, clock: Rc::new(Cell::new(NOW)) };
    let windowless = egui::Context::default();
    let mut ui = LauncherUi::new(&windowless, Box::new(backend), Settings { notify: false, ..Settings::default() });
    // No window: the backend's news is still taken in.
    ui.logic(&windowless, &mut NoHost);
    assert!(sent.borrow().contains(&Command::CheckForUpdates { force: false }));
    assert_eq!(ui.update_count(), 2);
    assert_eq!(ui.settings.last_check, Some(NOW - 180));

    let mut attached = false;
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 900.0)).build_ui_state(
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
    h.run_steps(2);
    assert_eq!(*sent.borrow(), [Command::Install { app: "cadcraft".into() }]);
    sent.borrow_mut().clear();
    // The … menu beside it.
    let beside = |r: egui::Rect| row.y_range().contains(r.center().y) && (row.right()..row.right() + 60.0).contains(&r.left());
    h.get_all_by_label("…").find(|n| beside(n.rect())).unwrap().click();
    h.run_steps(3);
    snapshot(&mut h, "grid-retry-install-menu-dark");
    h.get_by_label("Download again").click();
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
    let row = h.get_all_by_label("WordCraft").next().unwrap().rect().center().y;
    h.get_all_by_label("…").find(|n| (n.rect().center().y - row).abs() < 25.0).unwrap().click();
    h.run_steps(3);
    h.get_by_label("Reinstall");
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

/// How far the middle of these lines of text is below the middle of the app icon beside them, in points.
fn below_icon_centre(h: &Harness<'_, Option<LauncherUi>>, lines: &[&str]) -> f32 {
    let text = lines.iter().map(|l| h.get_by_label(l).rect()).reduce(|a, b| a.union(b)).unwrap();
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
        let off = below_icon_centre(&h, &["PhotoCraft", "Image editing", "Like Adobe Photoshop®"]);
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
fn launcher_update_prompts_to_relaunch_with_updated_version() {
    let mut ev = events();
    ev.push(Event::LauncherUpdate(release("artcraft-launcher", "0.2.0", NOW)));
    let (mut h, _) = harness_with(ev, ViewMode::Grid, egui::Theme::Dark, true, 1.0);
    h.run_steps(6);
    // Dialog pops up on launch with the update prompt
    h.get_by_label_contains("ArtCraft Launcher 0.2.0 is available");
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
    let mut backend = Some(Fake { events: evs, sent, manage: true, clock: Rc::new(Cell::new(NOW)) });
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
fn automatic_updates_queue_installs_for_outdated_apps_and_launcher() {
    let mut ev = events();
    ev.push(Event::LauncherUpdate(release("artcraft-launcher", "0.2.0", NOW)));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let sent2 = sent.clone();
    let mut backend = Some(Fake { events: ev, sent: sent2, manage: true, clock: Rc::new(Cell::new(NOW)) });
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 900.0)).build_ui_state(
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
fn automatic_updates_are_disabled_by_default() {
    let mut ev = events();
    ev.push(Event::LauncherUpdate(release("artcraft-launcher", "0.2.0", NOW)));
    let sent = Rc::new(RefCell::new(Vec::new()));
    let sent2 = sent.clone();
    let mut backend = Some(Fake { events: ev, sent: sent2, manage: true, clock: Rc::new(Cell::new(NOW)) });
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 900.0)).build_ui_state(
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
fn the_footer_names_the_services_behind_what_is_on_screen() {
    let footer = |extra: Vec<Event>| {
        let mut evs = events();
        evs.extend(extra);
        let sent = Rc::new(RefCell::new(Vec::new()));
        let mut backend = Some(Fake { events: evs, sent, manage: true, clock: Rc::new(Cell::new(NOW)) });
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
fn custom_title_bar_has_window_buttons() {
    struct FramelessHost;
    impl Host for FramelessHost {
        fn title_bar(&self) -> artcraft_launcher_ui_egui::TitleBar {
            artcraft_launcher_ui_egui::TitleBar::Custom
        }
    }
    let mut backend = Some(Fake { events: events(), sent: Rc::new(RefCell::new(Vec::new())), manage: true, clock: Rc::new(Cell::new(NOW)) });
    let mut h = Harness::builder().with_size(egui::vec2(1180.0, 900.0)).build_ui_state(
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

#[test]
fn the_rate_limit_notice_never_runs_under_the_footer_links() {
    for width in [720.0, 960.0, 1180.0] {
        let mut evs = events();
        evs.push(Event::CheckFinished { at: NOW - 180, source: Source::GitHubApi, rate_limited_until: Some(NOW + 1800) });
        let mut backend = Some(Fake { events: evs, sent: Rc::new(RefCell::new(Vec::new())), manage: true, clock: Rc::new(Cell::new(NOW)) });
        let mut h = Harness::builder().with_size(egui::vec2(width, 900.0)).build_ui_state(
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
        // Everything on the footer's right: the three links, the social icons and (when there's
        // room) the platform.
        let mut right = Vec::new();
        for label in ["Website", "GitHub", "Discord", "X", "YouTube", "TikTok", "Instagram", "LinkedIn"] {
            right.extend(h.query_all_by_label(label).map(|n| n.rect()).filter(|r| (r.center().y - notice.center().y).abs() < 12.0));
        }
        assert_eq!(right.len(), 8, "{width}: {right:?}");
        let platform = h.query_by_label_contains("Installing for").map(|n| n.rect());
        assert_eq!(platform.is_some(), width > 720.0, "{width}: the platform label shows unless the window is narrow");
        right.extend(platform);
        for r in right {
            assert!(!r.intersects(notice), "{width}: notice {notice:?} overlaps {r:?}");
            assert!(!r.intersects(checked), "{width}: check time {checked:?} overlaps {r:?}");
        }
    }
}

/// The button labelled `label` on the same row as the text `row`.
fn on_row<'a>(h: &'a Harness<'_, Option<LauncherUi>>, label: &'a str, row: &str) -> egui_kittest::Node<'a> {
    let y = h.get_by_label(row).rect().center().y;
    h.get_all_by_label(label).find(|n| (n.rect().center().y - y).abs() < 20.0).unwrap()
}

#[test]
fn settings_connect_every_installed_app_to_an_ai_tool() {
    let (mut h, sent) = harness(ViewMode::Grid, egui::Theme::Dark, true);
    h.run_steps(6);
    h.get_by_label("Settings").click();
    h.run_steps(3);
    h.get_by_label("Connect to AI tools…").click();
    h.run_steps(4);
    assert!(sent.borrow().contains(&Command::McpScan));
    snapshot(&mut h, "ai-tools-dark");
    h.get_by_label("Apps");
    h.get_by_label_contains("WordCraft: The command-line tool of WordCraft");
    // Installed tools first, then the rest.
    let y = |label: &str| h.get_by_label(label).rect().center().y;
    assert!(y("Claude Code") < y("Not found on this computer") && y("Not found on this computer") < y("Zed"));
    // Claude Code starts PhotoCraft's server of the three that can start; VectorCraft's entry is
    // from an older install and can be removed.
    h.get_by_label("1 of 3 connected");
    h.get_by_label("Can't read");
    h.get_by_label("Its settings can't be read: TOML parse error at line 3, column 1");
    h.get_by_label("It has its own settings for LightCraft, which stay as they are.");
    on_row(&h, "Add", "Cursor").click();
    h.run_steps(2);
    let connect = Command::McpConnect { tool: "cursor".into(), apps: vec!["photocraft".into(), "effectcraft".into()], connect: true };
    assert!(sent.borrow().contains(&connect), "{:?}", sent.borrow());
    on_row(&h, "Remove", "Claude Code").click();
    h.run_steps(2);
    // Busy until the backend answers: one change at a time.
    assert!(!sent.borrow().iter().any(|c| matches!(c, Command::McpConnect { connect: false, .. })));
}

#[test]
fn an_apps_menu_connects_just_that_app() {
    let (mut h, sent) = harness(ViewMode::Grid, egui::Theme::Light, true);
    h.run_steps(6);
    h.get_all_by_label("…").next().unwrap().click(); // PhotoCraft
    h.run_steps(3);
    h.get_by_label("Connect to AI tools…").click();
    h.run_steps(4);
    snapshot(&mut h, "ai-tools-photocraft-light");
    h.get_by_label("Connected");
    assert!(h.query_by_label_contains("WordCraft: ").is_none(), "only PhotoCraft's notes");
    on_row(&h, "Remove", "Claude Code").click();
    h.run_steps(2);
    let remove = Command::McpConnect { tool: "claude-code".into(), apps: vec!["photocraft".into()], connect: false };
    assert!(sent.borrow().contains(&remove), "{:?}", sent.borrow());
    // Copying the configuration needs no AI tool at all.
    h.get_by_label("Copy configuration").click();
    let mut copied = None;
    for _ in 0..3 {
        h.step();
        copied = copied.or_else(|| {
            h.output().platform_output.commands.iter().find_map(|c| match c {
                egui::OutputCommand::CopyText(t) => Some(t.clone()),
                _ => None,
            })
        });
    }
    assert!(copied.is_some_and(|t| t.contains("\"photocraft\"") && t.contains("/usr/local/bin/photocraft-cli")), "nothing copied");
    h.get_by_label("Copied to the clipboard");
}

/// The right edge of the widget with `role` on the same row as the text `row`.
fn right_of_role(h: &Harness<'_, Option<LauncherUi>>, role: egui::accesskit::Role, row: &str) -> f32 {
    let y = h.get_by_label(row).rect().center().y;
    h.get_all_by_role(role).find(|n| (n.rect().center().y - y).abs() < 20.0).unwrap().rect().right()
}

/// Every row's buttons end where the apps box and Done end, a path too long for its row is cut
/// short of the buttons instead of running under them, and the dialog keeps its size: in a wide
/// window, a narrow one, and at 150 % scale.
#[test]
fn the_ai_tools_dialog_lines_up_and_long_paths_stay_clear_of_the_buttons() {
    for (name, size, scale) in
        [("wide", egui::vec2(1180.0, 900.0), 1.0), ("narrow", egui::vec2(735.0, 1028.0), 1.0), ("scaled", egui::vec2(900.0, 700.0), 1.5)]
    {
        let (mut h, _) = harness_sized(events(), ViewMode::Grid, egui::Theme::Dark, true, scale, size);
        h.run_steps(6);
        h.get_all_by_label("…").next().unwrap().click(); // PhotoCraft
        h.run_steps(3);
        h.get_by_label("Connect to AI tools…").click();
        h.run_steps(4);
        snapshot(&mut h, &format!("ai-tools-{name}"));
        let done = h.get_by_label("Done").rect();
        let edge = done.right();
        let near = |a: f32, b: f32| (a - b).abs() < 1.0;
        assert!(near(right_of_role(&h, egui::accesskit::Role::ComboBox, "Apps"), edge), "{name}: the apps box");
        // The first rows are in view: Claude Desktop (to add), Claude Code (connected, to remove).
        let add = on_row(&h, "Add", "Claude Desktop").rect();
        assert!(near(add.right(), edge), "{name}: Add ends at {}, Done at {edge}", add.right());
        assert!(near(on_row(&h, "Remove", "Claude Code").rect().right(), edge), "{name}: Remove");
        let path = h.get_by_label(&format!(r"C:\Users\Jeremy\{CLAUDE_DESKTOP_PATH}")).rect();
        assert!(path.right() <= add.left() - 8.0, "{name}: the path ends at {}, the button starts at {}", path.right(), add.left());
        assert!(near(path.left(), h.get_by_label("Claude Desktop").rect().left()), "{name}: name and path start together");
        let badge = on_row(&h, "Connected", "Claude Code").rect();
        assert!(badge.right() < on_row(&h, "Remove", "Claude Code").rect().left(), "{name}: the badge sits left of the button");
        // Nothing drifts: the dialog is the same a few frames on.
        h.run_steps(8);
        assert_eq!(h.get_by_label("Done").rect(), done, "{name}: the dialog moved or grew");
    }
}
