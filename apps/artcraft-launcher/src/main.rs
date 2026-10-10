//! ArtCraft Launcher: install, update and open the Craft App Suite.
//!
//! The window can close to the tray (menu bar) and keep checking for updates, or quit entirely.

#![cfg_attr(windows, windows_subsystem = "windows")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod settings_file;
#[cfg(target_os = "macos")]
mod spaces;
mod tray;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use artcraft_launcher_core::protocol::Backend;
use artcraft_launcher_native::single_instance::{self, Instance};
use artcraft_launcher_native::{NativeBackend, login};
use artcraft_launcher_ui_egui::theme::Tokens;
use artcraft_launcher_ui_egui::{Host, LauncherUi, Settings, TitleBar};
use eframe::wgpu::rwh::{HasDisplayHandle, RawDisplayHandle};

const APP_ID: &str = "ai.storyteller.artcraft-launcher";
const SETTINGS_KEY: &str = "settings";

type Wake = Arc<OnceLock<Box<dyn Fn() + Send + Sync>>>;

fn main() -> ExitCode {
    // Started with administrator rights to install a batch of MSIs (see
    // `artcraft_launcher_native::elevated`): no window, no single-instance lock, then exit.
    #[cfg(windows)]
    {
        use artcraft_launcher_native::elevated;
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
        if let Some((first, rest)) = args.split_first()
            && first == elevated::FLAG
        {
            return ExitCode::from(elevated::helper_main(rest));
        }
    }
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    // Detect system language in the background so the first frame can use it.
    artcraft_launcher_ui_egui::i18n::detect_system_lang_in_background();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("artcraft-launcher {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let mut hidden = args.iter().any(|a| a == login::HIDDEN_FLAG);

    let wake: Wake = Arc::new(OnceLock::new());
    let show_requests = match single_instance::claim(wake.clone()) {
        Instance::Secondary => return ExitCode::SUCCESS,
        Instance::Primary(rx) => rx,
    };
    let (tray_loop, woken) = sync_channel(1);
    let waker = Arc::new(Waker { window: Mutex::new(None), tray_loop });
    {
        let waker = waker.clone();
        let _ = wake.set(Box::new(move || waker.wake()));
    }

    let backend_waker = waker.clone();
    let settings_path = settings_file::path(APP_ID);
    let mut launcher = Launcher::new(Box::new(NativeBackend::new(move || backend_waker.wake())), show_requests, settings_path.clone());
    // Started hidden at login where a window can't hide: start in the tray without mapping one,
    // so no window flashes up. Without a tray to come back from, the window opens after all.
    let mut window = true;
    if hidden && !window_can_hide_here(|name| std::env::var_os(name)) {
        let settings = settings_path.as_deref().and_then(|p| settings_file::load(p, SETTINGS_KEY));
        launcher.start(settings, &waker);
        window = launcher.tray.is_none();
    }
    // One pass per window: where a window can't hide, closing to the tray ends its pass and the
    // launcher carries on without one until the window is asked for again.
    loop {
        if window {
            let result = {
                let (waker, launcher) = (waker.clone(), &mut launcher);
                eframe::run_native(
                    "ArtCraft Launcher",
                    native_options(hidden),
                    Box::new(move |cc| {
                        launcher.open(cc, &waker);
                        Ok(Box::new(Shell::new(cc, launcher, hidden)))
                    }),
                )
            };
            waker.set_window(None);
            if let Err(e) = result {
                log::error!("{e}");
                return ExitCode::FAILURE;
            }
            if !std::mem::take(&mut launcher.closed_to_tray) {
                return ExitCode::SUCCESS;
            }
        }
        if !launcher.wait_in_tray(&woken) {
            return ExitCode::SUCCESS;
        }
        hidden = false;
        window = true;
    }
}

fn native_options(hidden: bool) -> eframe::NativeOptions {
    let icon =
        eframe::icon_data::from_png_bytes(include_bytes!("../../../assets/app-icon/hicolor/256x256/apps/ai.storyteller.artcraft-launcher.png")).ok();
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("ArtCraft Launcher")
        .with_app_id(APP_ID)
        .with_inner_size([1120.0, 760.0])
        .with_min_inner_size([720.0, 500.0])
        .with_visible(!hidden);
    // The header is the title bar (see `DesktopHost::title_bar`).
    viewport = if cfg!(target_os = "macos") {
        viewport.with_fullsize_content_view(true).with_titlebar_shown(false).with_title_shown(false)
    } else {
        viewport.with_decorations(false)
    };
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    eframe::NativeOptions { viewport, persist_window: true, wgpu_options: wgpu_options(), ..Default::default() }
}

/// wgpu's backend list. On Windows that is Direct3D 12 with GL as the fallback, not Vulkan:
/// Vulkan goes through vendor ICDs and third-party overlay layers, and on hybrid AMD laptops whose
/// integrated and discrete GPUs run different driver packages (common on OEM machines, where the
/// Adrenalin installer leaves the integrated GPU on the OEM driver) it crashes inside
/// `amdvlk64.dll` before the first frame. `WGPU_BACKEND` still overrides the default.
#[cfg(windows)]
fn wgpu_options() -> eframe::WgpuConfiguration {
    use eframe::wgpu::Backends;
    let mut config = eframe::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        setup.instance_descriptor.backends = Backends::from_env().unwrap_or(Backends::DX12 | Backends::GL);
    }
    config
}

#[cfg(not(windows))]
fn wgpu_options() -> eframe::WgpuConfiguration {
    eframe::WgpuConfiguration::default()
}

/// Wakes whatever is running the launcher: the window while there is one, the tray loop otherwise.
struct Waker {
    window: Mutex<Option<egui::Context>>,
    tray_loop: SyncSender<()>,
}

impl Waker {
    fn wake(&self) {
        if let Some(ctx) = self.window.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            ctx.request_repaint();
        }
        let _ = self.tray_loop.try_send(());
    }

    fn set_window(&self, ctx: Option<egui::Context>) {
        *self.window.lock().unwrap_or_else(PoisonError::into_inner) = ctx;
    }
}

/// Whether the window system can hide a window and show it again. Wayland can't.
fn window_can_hide(display: Option<RawDisplayHandle>) -> bool {
    !matches!(display, Some(RawDisplayHandle::Wayland(_)))
}

/// [`window_can_hide`] before there is a window, from the environment `env` reads: winit opens a
/// Wayland window wherever `WAYLAND_DISPLAY` or `WAYLAND_SOCKET` is set (Linux and the BSDs).
fn window_can_hide_here(env: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    let wayland = || ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"].iter().any(|name| env(name).is_some_and(|v| !v.is_empty()));
    !(cfg!(all(unix, not(target_os = "macos"))) && wayland())
}

/// What a close request does.
#[derive(Debug, PartialEq, Eq)]
enum OnClose {
    /// Keep the window and hide it.
    Hide,
    /// Let the window close and carry on in the tray without one.
    ToTray,
    Quit,
}

fn on_close(has_tray: bool, close_to_tray: bool, window_hides: bool) -> OnClose {
    if !(has_tray && close_to_tray) {
        OnClose::Quit
    } else if window_hides {
        OnClose::Hide
    } else {
        OnClose::ToTray
    }
}

/// What the user asked for, from the tray or by starting the launcher again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    Toggle,
    Show,
    Quit,
}

/// What the launcher does next while it has no window.
#[derive(Debug, PartialEq, Eq)]
enum TrayStep {
    Stay,
    OpenWindow,
    Quit,
}

fn tray_step(requests: &[Request]) -> TrayStep {
    if requests.contains(&Request::Quit) {
        TrayStep::Quit
    } else if requests.is_empty() {
        TrayStep::Stay
    } else {
        TrayStep::OpenWindow
    }
}

/// What `LauncherUi::logic` is told about its window while there is none: it isn't focused.
fn no_window() -> egui::RawInput {
    let viewport = egui::ViewportInfo { focused: Some(false), ..Default::default() };
    egui::RawInput { focused: false, viewports: std::iter::once((egui::ViewportId::ROOT, viewport)).collect(), ..Default::default() }
}

struct DesktopHost {
    tray: bool,
    quit: bool,
}

impl Host for DesktopHost {
    fn title_bar(&self) -> TitleBar {
        if cfg!(target_os = "macos") { TitleBar::MacOs } else { TitleBar::Custom }
    }

    fn notify(&mut self, title: &str, body: &str) {
        let (title, body) = (title.to_string(), body.to_string());
        // Some platforms block while the notification is delivered.
        std::thread::spawn(move || {
            let mut n = notify_rust::Notification::new();
            n.summary(&title).body(&body).appname("ArtCraft Launcher");
            #[cfg(windows)]
            n.app_id(APP_ID);
            #[cfg(all(unix, not(target_os = "macos")))]
            n.icon(APP_ID);
            if let Err(e) = n.show() {
                log::warn!("notification: {e}");
            }
        });
    }

    fn has_tray(&self) -> bool {
        self.tray
    }

    fn has_desktops(&self) -> bool {
        cfg!(target_os = "macos")
    }

    fn login_item(&self) -> Option<bool> {
        Some(login::is_enabled())
    }

    fn set_login_item(&mut self, on: bool) -> Result<(), String> {
        login::set_enabled(on)
    }

    fn quit(&mut self) {
        self.quit = true;
    }
}

/// Everything that outlives a window: the UI state and its backend, the tray icon, and the work
/// that goes on with or without a window.
struct Launcher {
    ui: LauncherUi,
    tray: Option<tray::Tray>,
    host: DesktopHost,
    show_requests: Receiver<()>,
    /// Stands in for a window's context while only the tray exists. A window can't reuse it: a
    /// new window's renderer has none of the textures a used context has already handed out.
    windowless: egui::Context,
    /// How soon `LauncherUi::logic` asked to run again, while there is no window.
    run_again: Receiver<Duration>,
    /// The saved settings are in and the tray icon is up.
    started: bool,
    /// The window closed to carry on in the tray, not to quit.
    closed_to_tray: bool,
    /// eframe's settings file, which the launcher writes itself while there is no window.
    settings_path: Option<PathBuf>,
}

impl Launcher {
    fn new(backend: Box<dyn Backend>, show_requests: Receiver<()>, settings_path: Option<PathBuf>) -> Self {
        let windowless = egui::Context::default();
        let (asked, run_again) = channel();
        windowless.set_request_repaint_callback(move |info| {
            let _ = asked.send(info.delay);
        });
        Self {
            ui: LauncherUi::new(&windowless, backend, Settings::default()),
            tray: None,
            host: DesktopHost { tray: false, quit: false },
            show_requests,
            windowless,
            run_again,
            started: false,
            closed_to_tray: false,
            settings_path,
        }
    }

    /// Takes in the saved settings and puts up the tray icon. On macOS the tray icon needs the
    /// event loop running, so this waits for the first window there.
    fn start(&mut self, settings: Option<Settings>, waker: &Arc<Waker>) {
        self.started = true;
        if let Some(settings) = settings {
            self.ui.settings = settings;
        }
        let waker = waker.clone();
        match tray::Tray::new(move || waker.wake()) {
            Ok(t) => self.tray = Some(t),
            Err(e) => log::warn!("no tray icon: {e}"),
        }
        self.host.tray = self.tray.is_some();
    }

    /// Readies the launcher for a new window. Unless the launcher started in the tray, the first
    /// one brings the saved settings and the tray icon.
    fn open(&mut self, cc: &eframe::CreationContext<'_>, waker: &Arc<Waker>) {
        waker.set_window(Some(cc.egui_ctx.clone()));
        if !self.started {
            self.start(cc.storage.and_then(|s| eframe::get_value(s, SETTINGS_KEY)), waker);
        }
        self.ui.attach(&cc.egui_ctx);
    }

    /// Saves the settings with no window, where eframe would on a window's behalf.
    fn save_settings(&self) {
        let Some(path) = self.settings_path.as_deref() else { return };
        if let Err(e) = settings_file::save(path, SETTINGS_KEY, &self.ui.settings) {
            log::warn!("couldn't save the settings: {e}");
        }
    }

    /// The work that goes on with or without a window. Returns what the user asked for.
    fn tick(&mut self, ctx: &egui::Context) -> Vec<Request> {
        self.ui.logic(ctx, &mut self.host);
        let mut requests = Vec::new();
        if self.show_requests.try_iter().count() > 0 {
            requests.push(Request::Show);
        }
        let actions = self.tray.as_ref().map(tray::Tray::actions).unwrap_or_default();
        for action in actions {
            match action {
                tray::TrayAction::Toggle => requests.push(Request::Toggle),
                tray::TrayAction::Show => requests.push(Request::Show),
                tray::TrayAction::Check => self.ui.check_now(),
                // Straight to the browser: works while the window is hidden in the tray.
                tray::TrayAction::Open(url) => {
                    if let Err(e) = artcraft_launcher_native::process::open(std::ffi::OsStr::new(url)) {
                        log::warn!("couldn't open {url}: {e}");
                    }
                }
                tray::TrayAction::Quit => requests.push(Request::Quit),
            }
        }
        if let Some(t) = self.tray.as_mut() {
            t.set_updates(self.ui.update_count());
        }
        if self.host.quit {
            self.host.quit = false;
            requests.push(Request::Quit);
        }
        requests
    }

    /// One [`Self::tick`] with no window.
    fn tick_in_tray(&mut self) -> Vec<Request> {
        let ctx = self.windowless.clone();
        // Only what this tick asks for says when the next one is due.
        self.run_again.try_iter().for_each(drop);
        let mut requests = Vec::new();
        let _ = ctx.run_logic(&no_window(), |ctx| requests = self.tick(ctx));
        requests
    }

    /// Carries on with no window until the user asks for it back (`true`) or quits (`false`).
    /// Settings that change meanwhile (the last check, updates announced, tray toggles) are saved
    /// as they change, as no window is there to save them.
    fn wait_in_tray(&mut self, woken: &Receiver<()>) -> bool {
        log::debug!("no window: carrying on in the tray");
        // What eframe saved as the window closed, or what was just read in at a start in the tray.
        let mut saved = self.ui.settings.clone();
        loop {
            let step = tray_step(&self.tick_in_tray());
            if self.ui.settings != saved {
                self.save_settings();
                saved = self.ui.settings.clone();
            }
            match step {
                TrayStep::OpenWindow => return true,
                TrayStep::Quit => return false,
                TrayStep::Stay => {}
            }
            // Sleep until woken, or until `LauncherUi::logic` wants its next scheduled look.
            match self.run_again.try_iter().min() {
                Some(wait) => {
                    if woken.recv_timeout(wait) == Err(RecvTimeoutError::Disconnected) {
                        std::thread::sleep(wait);
                    }
                }
                None => {
                    if woken.recv().is_err() {
                        return false;
                    }
                }
            }
        }
    }
}

/// One window onto the launcher.
struct Shell<'a> {
    launcher: &'a mut Launcher,
    window_hides: bool,
    visible: bool,
    quitting: bool,
    /// The "show on the current desktop" setting last applied to the window (macOS).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    applied_follow: Option<bool>,
}

impl<'a> Shell<'a> {
    fn new(cc: &eframe::CreationContext<'_>, launcher: &'a mut Launcher, hidden: bool) -> Self {
        let window_hides = window_can_hide(cc.display_handle().ok().map(|d| d.as_raw()));
        // Started hidden at login but there's no tray to come back from: show the window.
        let visible = !hidden || launcher.tray.is_none();
        let mut shell = Self { launcher, window_hides, visible: true, quitting: false, applied_follow: None };
        if !visible {
            shell.put_away(&cc.egui_ctx);
        } else if hidden {
            cc.egui_ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        }
        shell
    }

    fn show(&mut self, ctx: &egui::Context) {
        self.visible = true;
        // Before the window is ordered front, so it comes to this desktop rather than taking the
        // user to its own (macOS; the setting doesn't exist elsewhere).
        #[cfg(target_os = "macos")]
        {
            let on = self.launcher.ui.settings.show_on_current_desktop;
            spaces::follow_current_desktop(on);
            self.applied_follow = Some(on);
        }
        if self.window_hides {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        } else {
            // The window is already open and can't be raised from here: ask the desktop to point it out.
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
        }
    }

    /// Puts the window away in the tray: hidden where it can hide, closed where it can't.
    fn put_away(&mut self, ctx: &egui::Context) {
        if self.window_hides {
            self.visible = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        } else {
            self.launcher.closed_to_tray = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn quit(&mut self, ctx: &egui::Context) {
        self.quitting = true;
        self.launcher.closed_to_tray = false;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

impl eframe::App for Shell<'_> {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let requests = self.launcher.tick(ctx);
        #[cfg(target_os = "macos")]
        if self.applied_follow != Some(self.launcher.ui.settings.show_on_current_desktop) {
            let on = self.launcher.ui.settings.show_on_current_desktop;
            spaces::follow_current_desktop(on);
            self.applied_follow = Some(on);
        }
        for request in requests {
            match request {
                Request::Toggle if self.visible => self.put_away(ctx),
                Request::Toggle | Request::Show => self.show(ctx),
                Request::Quit => self.quit(ctx),
            }
        }
        // The close button puts the window away in the tray unless the user chose to quit. That is
        // the window manager's button or the header's own (no decorations), which asks to close
        // with `ViewportCommand::Close` and arrives here the same way.
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting && !self.launcher.closed_to_tray {
            match on_close(self.launcher.tray.is_some(), self.launcher.ui.settings.close_to_tray, self.window_hides) {
                OnClose::Hide => {
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    self.put_away(ctx);
                }
                OnClose::ToTray => self.launcher.closed_to_tray = true,
                OnClose::Quit => {}
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.launcher.ui.ui(ui, &mut self.launcher.host);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.launcher.ui.settings);
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        (if visuals.dark_mode { Tokens::DARK.bg } else { Tokens::LIGHT.bg }).to_normalized_gamma_f32()
    }
}

#[cfg(test)]
mod tests {
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
    use std::sync::mpsc::Sender;
    use std::time::Instant;

    use artcraft_launcher_core::protocol::{Capabilities, Command, Event, Source};
    use eframe::wgpu::rwh::{AppKitDisplayHandle, WaylandDisplayHandle, WindowsDisplayHandle, XcbDisplayHandle, XlibDisplayHandle};

    use super::*;

    const NOW: i64 = 1_791_500_000;

    /// What the tests can see of a [`Quiet`] backend, and its clock.
    #[derive(Default)]
    struct Seen {
        polls: AtomicUsize,
        detects: AtomicUsize,
        clock: AtomicI64,
        /// What the next poll hands out.
        news: Mutex<Vec<Event>>,
    }

    /// A backend with nothing to say unless the test gives it news.
    struct Quiet(Arc<Seen>);

    impl Backend for Quiet {
        fn send(&mut self, command: Command) {
            if command == Command::Detect {
                self.0.detects.fetch_add(1, Ordering::Relaxed);
            }
        }
        fn poll(&mut self) -> Vec<Event> {
            self.0.polls.fetch_add(1, Ordering::Relaxed);
            std::mem::take(&mut *self.0.news.lock().unwrap_or_else(PoisonError::into_inner))
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities { manage_apps: true, reveal_label: "Show in Files", self_update: false }
        }
        fn platform(&self) -> Option<artcraft_launcher_core::Platform> {
            None
        }
        fn now(&self) -> i64 {
            self.0.clock.load(Ordering::Relaxed)
        }
    }

    /// A launcher with no tray and no window, the sender of its show requests, and its backend's record.
    fn launcher() -> (Launcher, Sender<()>, Arc<Seen>) {
        let seen = Arc::new(Seen { clock: AtomicI64::new(NOW), ..Seen::default() });
        let (show, show_requests) = channel();
        (Launcher::new(Box::new(Quiet(seen.clone())), show_requests, None), show, seen)
    }

    #[test]
    fn a_wayland_window_cannot_hide() {
        assert!(!window_can_hide(Some(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(NonNull::dangling())))));
    }

    #[test]
    fn other_windows_hide() {
        for display in [
            RawDisplayHandle::Xlib(XlibDisplayHandle::new(None, 0)),
            RawDisplayHandle::Xcb(XcbDisplayHandle::new(None, 0)),
            RawDisplayHandle::Windows(WindowsDisplayHandle::new()),
            RawDisplayHandle::AppKit(AppKitDisplayHandle::new()),
        ] {
            assert!(window_can_hide(Some(display)), "{display:?}");
        }
        assert!(window_can_hide(None));
    }

    #[test]
    fn a_wayland_session_is_known_before_the_window_opens() {
        let env = |set: &'static [(&'static str, &'static str)]| move |name: &str| set.iter().find(|(n, _)| *n == name).map(|(_, v)| v.into());
        let wayland_elsewhere = !cfg!(all(unix, not(target_os = "macos")));
        assert_eq!(window_can_hide_here(env(&[("WAYLAND_DISPLAY", "wayland-0"), ("DISPLAY", ":0")])), wayland_elsewhere);
        assert_eq!(window_can_hide_here(env(&[("WAYLAND_SOCKET", "3")])), wayland_elsewhere);
        assert!(window_can_hide_here(env(&[("DISPLAY", ":0")])));
        assert!(window_can_hide_here(env(&[("WAYLAND_DISPLAY", ""), ("DISPLAY", ":0")])));
        assert!(window_can_hide_here(env(&[])));
    }

    #[test]
    fn closing_keeps_the_launcher_in_the_tray() {
        assert_eq!(on_close(true, true, true), OnClose::Hide);
        assert_eq!(on_close(true, true, false), OnClose::ToTray);
    }

    #[test]
    fn closing_quits_without_a_tray_or_when_the_user_turned_it_off() {
        for window_hides in [true, false] {
            assert_eq!(on_close(false, true, window_hides), OnClose::Quit);
            assert_eq!(on_close(true, false, window_hides), OnClose::Quit);
            assert_eq!(on_close(false, false, window_hides), OnClose::Quit);
        }
    }

    #[test]
    fn the_tray_alone_waits_until_asked() {
        assert_eq!(tray_step(&[]), TrayStep::Stay);
    }

    #[test]
    fn the_tray_alone_opens_the_window_when_asked() {
        assert_eq!(tray_step(&[Request::Toggle]), TrayStep::OpenWindow);
        assert_eq!(tray_step(&[Request::Show]), TrayStep::OpenWindow);
    }

    #[test]
    fn quit_wins_over_opening_the_window() {
        assert_eq!(tray_step(&[Request::Quit]), TrayStep::Quit);
        assert_eq!(tray_step(&[Request::Show, Request::Quit]), TrayStep::Quit);
        assert_eq!(tray_step(&[Request::Quit, Request::Toggle]), TrayStep::Quit);
    }

    #[test]
    fn with_no_window_a_show_request_opens_one() {
        let (mut launcher, show, _) = launcher();
        let (_wake, woken) = sync_channel(1);
        assert!(show.send(()).is_ok());
        assert!(launcher.wait_in_tray(&woken));
    }

    #[test]
    fn with_no_window_quit_wins_over_a_show_request() {
        let (mut launcher, show, _) = launcher();
        let (_wake, woken) = sync_channel(1);
        assert!(show.send(()).is_ok());
        launcher.host.quit();
        assert!(!launcher.wait_in_tray(&woken));
    }

    #[test]
    fn with_no_window_it_sleeps_until_woken() {
        let (mut launcher, show, seen) = launcher();
        let (wake, woken) = sync_channel(1);
        // Setting the UI up asked for one immediate run.
        assert!(launcher.tick_in_tray().is_empty());
        let pause = Duration::from_millis(200);
        let waking = std::thread::spawn(move || {
            std::thread::sleep(pause);
            let _ = show.send(());
            let _ = wake.try_send(());
        });
        let started = Instant::now();
        assert!(launcher.wait_in_tray(&woken));
        assert!(started.elapsed() >= pause);
        // After the first run: once on the way in and once when woken. It slept in between.
        assert_eq!(seen.polls.load(Ordering::Relaxed), 3);
        assert!(waking.join().is_ok());
    }

    #[test]
    fn with_no_window_the_ui_says_when_to_look_again() {
        let (mut launcher, _show, _) = launcher();
        // Setting the UI up asked for one immediate run.
        assert!(launcher.tick_in_tray().is_empty());
        assert_eq!(launcher.run_again.try_iter().min(), Some(Duration::ZERO));
        assert!(launcher.tick_in_tray().is_empty());
        // The UI's own interval, less the frame time egui takes off it.
        let wait = launcher.run_again.try_iter().min().unwrap_or_default();
        assert!(wait > Duration::from_secs(59) && wait <= Duration::from_secs(60), "{wait:?}");
    }

    #[test]
    fn with_no_window_nobody_came_back_to_it() {
        let (mut launcher, _show, seen) = launcher();
        // The window lost focus before it closed.
        let window = egui::Context::default();
        let _ = window.run_logic(&no_window(), |ctx| assert!(launcher.tick(ctx).is_empty()));
        // Long enough that coming back to a window would look again at what's installed.
        seen.clock.store(NOW + 60, Ordering::Relaxed);
        assert!(launcher.tick_in_tray().is_empty());
        assert_eq!(seen.detects.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn with_no_window_changed_settings_are_saved_before_quitting() {
        let dir = std::env::temp_dir().join(format!("artcraft-launcher-tray-save-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("app.ron");
        let seen = Arc::new(Seen { clock: AtomicI64::new(NOW), ..Seen::default() });
        let (_show, show_requests) = channel();
        let mut launcher = Launcher::new(Box::new(Quiet(seen.clone())), show_requests, Some(path.clone()));
        let (_wake, woken) = sync_channel(1);
        // A check finishes in the same tick as the user quits from the tray.
        seen.news.lock().unwrap_or_else(PoisonError::into_inner).push(Event::CheckFinished {
            at: NOW - 5,
            source: Source::Feed,
            rate_limited_until: None,
        });
        launcher.host.quit();
        assert!(!launcher.wait_in_tray(&woken));
        let saved: Option<Settings> = settings_file::load(&path, SETTINGS_KEY);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(saved.and_then(|s| s.last_check), Some(NOW - 5));
    }

    #[test]
    fn with_no_window_unchanged_settings_are_not_written() {
        let dir = std::env::temp_dir().join(format!("artcraft-launcher-tray-nosave-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (_, show_requests) = channel();
        let mut launcher = Launcher::new(Box::new(Quiet(Arc::new(Seen::default()))), show_requests, Some(dir.join("app.ron")));
        let (_wake, woken) = sync_channel(1);
        launcher.host.quit();
        assert!(!launcher.wait_in_tray(&woken));
        assert!(!dir.exists());
    }

    #[test]
    fn quitting_overrides_a_close_to_the_tray() {
        let (mut launcher, _show, _) = launcher();
        let ctx = launcher.windowless.clone();
        let mut shell = Shell { launcher: &mut launcher, window_hides: false, visible: true, quitting: false, applied_follow: None };
        shell.put_away(&ctx);
        assert!(shell.launcher.closed_to_tray);
        shell.quit(&ctx);
        assert!(!shell.launcher.closed_to_tray);
    }

    #[test]
    fn a_window_that_hides_is_not_closed_to_the_tray() {
        let (mut launcher, _show, _) = launcher();
        let ctx = launcher.windowless.clone();
        let mut shell = Shell { launcher: &mut launcher, window_hides: true, visible: true, quitting: false, applied_follow: None };
        shell.put_away(&ctx);
        assert!(!shell.visible);
        assert!(!shell.launcher.closed_to_tray);
    }
}
