//! ArtCraft Launcher: install, update and open the Craft App Suite.
//!
//! The window can close to the tray (menu bar) and keep checking for updates, or quit entirely.

#![cfg_attr(windows, windows_subsystem = "windows")]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

mod logging;
mod motion;
mod settings_file;
#[cfg(target_os = "macos")]
mod spaces;
#[cfg(target_os = "linux")]
mod system_theme;
mod tray;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
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
    // First, so the panic report and every start-up warning are recorded (`logging`).
    let logs = logging::install();
    logging::install_panic_hook();
    // Detect system language in the background so the first frame can use it.
    artcraft_launcher_ui_egui::i18n::detect_system_lang_in_background();
    // Detect Discord protocol handler in the background without blocking startup.
    artcraft_launcher_native::process::ensure_discord_detection_started();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("artcraft-launcher {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    let mut hidden = args.iter().any(|a| a == login::HIDDEN_FLAG);

    let wake: Wake = Arc::new(OnceLock::new());
    let starts = match single_instance::claim(wake.clone()) {
        Instance::Secondary => return ExitCode::SUCCESS,
        Instance::Primary(rx) => rx,
    };
    // The log file lives in the launcher's data folder (beside the executable in portable mode);
    // opened after the arguments (so `--version` leaves no file behind) and once this is the only
    // launcher running, so a second start doesn't rotate away the running one's log. Records
    // logged until now are written to it first.
    if let Some(logs) = logs {
        match artcraft_launcher_native::paths::Paths::new() {
            Ok(paths) => match logs.attach_dir(&paths.logs()) {
                Ok(path) => log::info!("ArtCraft Launcher {}, log file {}", env!("CARGO_PKG_VERSION"), path.display()),
                Err(e) => {
                    logs.no_file();
                    log::warn!("no log file: {e}");
                }
            },
            Err(e) => {
                logs.no_file();
                log::warn!("no log file: {e}");
            }
        }
    }
    let (tray_loop, woken) = sync_channel(1);
    let waker = Arc::new(Waker { window: Mutex::new(None), tray_loop, quit_pending: AtomicBool::new(false) });
    {
        let waker = waker.clone();
        let _ = wake.set(Box::new(move || waker.wake()));
    }

    // The desktop's animation setting comes over D-Bus: ask now, so the answer is in by the first frame.
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = motion::reduced();

    let backend_waker = waker.clone();
    let settings_path = settings_file::path(APP_ID);
    let mut launcher = Launcher::new(Box::new(NativeBackend::new(move || backend_waker.wake())), starts, settings_path.clone());
    #[cfg(target_os = "linux")]
    {
        let theme_waker = waker.clone();
        launcher.system_theme = system_theme::Monitor::start(move || theme_waker.wake());
    }
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
    // On macOS the bundle's .icns is the icon. Any icon given to eframe (its egui logo when none
    // is) replaces it in the Dock at runtime with a raw bitmap, which skips the system's icon
    // treatment, so pass the empty icon, which eframe takes as "leave it alone".
    if cfg!(target_os = "macos") {
        viewport = viewport.with_icon(egui::IconData::default());
    } else if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    eframe::NativeOptions { viewport, persist_window: true, wgpu_options: wgpu_options(), ..Default::default() }
}

/// wgpu's backend list. On Windows that is Direct3D 12 with GL as the fallback, not Vulkan:
/// Vulkan goes through vendor ICDs and third-party overlay layers, and on hybrid AMD laptops whose
/// integrated and discrete GPUs run different driver packages (common on OEM machines, where the
/// Adrenalin installer leaves the integrated GPU on the OEM driver) it crashes inside
/// `amdvlk64.dll` before the first frame. `WGPU_BACKEND` still overrides the default, and then
/// wgpu picks the adapter itself. Direct3D 12 shaders compile with FXC ([`pin_dx12_compiler`]).
///
/// Direct3D 12 presents through DirectComposition rather than straight to the window. The
/// borderless window's drop shadow moves its client area down a pixel, and a swapchain made from
/// the window is then stretched over it vertically by the compositor, blurring every horizontal
/// edge and line of text. `WGPU_DX12_PRESENTATION_SYSTEM` still overrides it.
#[cfg(windows)]
fn wgpu_options() -> eframe::WgpuConfiguration {
    use eframe::wgpu::{Backends, Dx12Compiler, Dx12SwapchainKind, PowerPreference};
    let mut config = eframe::WgpuConfiguration::default();
    pin_dx12_compiler(&mut config, Dx12Compiler::from_env());
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        setup.instance_descriptor.backend_options.dx12.presentation_system =
            Dx12SwapchainKind::from_env().unwrap_or(Dx12SwapchainKind::DxgiFromVisual);
        if let Some(backends) = Backends::from_env() {
            setup.instance_descriptor.backends = backends;
        } else {
            setup.instance_descriptor.backends = Backends::DX12 | Backends::GL;
            let integrated = setup.power_preference == PowerPreference::LowPower;
            setup.native_adapter_selector = Some(Arc::new(move |adapters, surface| {
                adapters
                    .iter()
                    .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
                    .min_by_key(|a| {
                        let info = a.get_info();
                        adapter_rank(info.backend, info.device_type, integrated)
                    })
                    .cloned()
                    .ok_or_else(|| "no graphics adapter can draw to the window".to_owned())
            }));
        }
    }
    config
}

/// Compiles Direct3D 12 shaders with FXC (`d3dcompiler_47.dll`, part of Windows) unless
/// `env_choice` (normally `Dx12Compiler::from_env()`, i.e. `WGPU_DX12_COMPILER`) names another.
/// wgpu's default (`Auto`) loads whichever `dxcompiler.dll` the DLL search path finds. The launcher
/// ships none, and without a matching `dxil.dll` DXC warns that the DXIL is unsigned; wgpu takes
/// the warning for a compile error, its indirect-validation pipeline fails, the device is lost
/// and the launcher exits before the first frame (#126).
#[cfg(any(windows, test))]
fn pin_dx12_compiler(config: &mut eframe::WgpuConfiguration, env_choice: Option<eframe::wgpu::Dx12Compiler>) {
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        setup.instance_descriptor.backend_options.dx12.shader_compiler = env_choice.unwrap_or(eframe::wgpu::Dx12Compiler::Fxc);
    }
}

/// wgpu's own adapter order (by device type, then enumeration order), except that any Direct3D 12
/// adapter, the WARP software one included, comes before GL. wgpu ranks WARP last, so on GPUs with
/// no D3D12 driver (Intel HD 4000, Radeon TeraScale) it picked the GL one, whose legacy driver
/// failed `Surface::configure` and wgpu panicked before the window opened.
#[cfg(any(windows, test))]
fn adapter_rank(backend: eframe::wgpu::Backend, device_type: eframe::wgpu::DeviceType, integrated: bool) -> (bool, u8) {
    use eframe::wgpu::{Backend, DeviceType};
    let device = match device_type {
        DeviceType::DiscreteGpu => 1 + u8::from(integrated),
        DeviceType::IntegratedGpu => 2 - u8::from(integrated),
        DeviceType::Other => 3,
        DeviceType::VirtualGpu => 4,
        DeviceType::Cpu => 5,
    };
    (backend != Backend::Dx12, device)
}

/// wgpu's adapter on Linux and the BSDs: the GPU the desktop runs on. egui asks for the
/// high-performance GPU, which on a hybrid laptop in PRIME on-demand mode is the discrete one while
/// the display server runs on the integrated one: every frame then crosses between the GPUs, and
/// while the window is resized it shows stale or garbled frames and can stop drawing (issue #69).
/// With no preference wgpu takes the first adapter Vulkan lists, and Mesa's device-select layer
/// lists the display server's GPU first (or the one `DRI_PRIME` picks). Without that layer the
/// order follows the ICD files, where lavapipe (`lvp_icd`) can sort before the real driver, so a
/// software rasterizer only wins when nothing else can draw. `WGPU_POWER_PREF` (`low`, `high` or
/// `none`) still overrides it, and then wgpu picks the adapter itself.
#[cfg(not(any(windows, target_os = "macos")))]
fn wgpu_options() -> eframe::WgpuConfiguration {
    use eframe::wgpu::{DeviceType, PowerPreference};
    let mut config = eframe::WgpuConfiguration::default();
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
        if let Some(preference) = PowerPreference::from_env() {
            setup.power_preference = preference;
        } else {
            setup.power_preference = PowerPreference::None;
            setup.native_adapter_selector = Some(Arc::new(|adapters, surface| {
                adapters
                    .iter()
                    .filter(|a| surface.is_none_or(|s| a.is_surface_supported(s)))
                    .min_by_key(|a| a.get_info().device_type == DeviceType::Cpu)
                    .cloned()
                    .ok_or_else(|| "no graphics adapter can draw to the window".to_owned())
            }));
        }
    }
    config
}

#[cfg(target_os = "macos")]
fn wgpu_options() -> eframe::WgpuConfiguration {
    eframe::WgpuConfiguration::default()
}

/// Wakes whatever is running the launcher: the window while there is one, the tray loop otherwise.
struct Waker {
    window: Mutex<Option<egui::Context>>,
    tray_loop: SyncSender<()>,
    /// Quit was picked in the tray and no tick has taken it up yet.
    quit_pending: AtomicBool,
}

/// How long a tray Quit may wait for the window to take it up before the launcher exits anyway.
const QUIT_GRACE: Duration = Duration::from_secs(2);

impl Waker {
    fn wake(&self) {
        if let Some(ctx) = self.window.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            ctx.request_repaint();
        }
        let _ = self.tray_loop.try_send(());
    }

    /// Quit was picked in the tray. A minimized window may get no frames at all (Wayland sends none),
    /// so the quit would wait until the window is restored: if no tick has taken it up in time, exit.
    fn quit_from_tray(self: &Arc<Self>) {
        self.quit_pending.store(true, Ordering::SeqCst);
        let waker = self.clone();
        std::thread::spawn(move || {
            std::thread::sleep(QUIT_GRACE);
            if waker.quit_unseen() {
                log::warn!("the window did not answer a quit from the tray: exiting");
                std::process::exit(0);
            }
        });
    }

    /// A tick took up the quit.
    fn quit_seen(&self) {
        self.quit_pending.store(false, Ordering::SeqCst);
    }

    /// Whether a quit from the tray is still waiting (and no longer is).
    fn quit_unseen(&self) -> bool {
        self.quit_pending.swap(false, Ordering::SeqCst)
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

/// macOS sends notifications on behalf of a bundle id. When none is set, the library looks up an app named
/// "use_default" and macOS asks the user "Where is use_default?". Set ours before the first notification.
/// The library accepts only one call (even a failed one skips that lookup), so the first outcome is kept and logged once.
#[cfg(target_os = "macos")]
fn claim_notification_identity() -> &'static Result<(), String> {
    static CLAIMED: OnceLock<Result<(), String>> = OnceLock::new();
    CLAIMED.get_or_init(|| {
        let claimed = notify_rust::set_application(APP_ID).map_err(|e| e.to_string());
        if let Err(e) = &claimed {
            log::warn!("notification identity {APP_ID}: {e}");
        }
        claimed
    })
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
            #[cfg(target_os = "macos")]
            claim_notification_identity();
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

    fn pick_folder(&mut self, initial: Option<&str>) -> Option<String> {
        let mut dialog = rfd::FileDialog::new();
        if let Some(folder) = initial.filter(|p| std::path::Path::new(p).is_dir()) {
            dialog = dialog.set_directory(folder);
        }
        dialog.pick_folder().and_then(|p| p.to_str().map(str::to_string))
    }

    fn reduce_motion(&self) -> bool {
        motion::reduced()
    }

    fn discord_url(&self) -> &'static str {
        artcraft_launcher_native::process::discord_url()
    }

    fn quit(&mut self) {
        self.quit = true;
    }
}

/// Everything that outlives a window: the UI state and its backend, the tray icon, and the work
/// that goes on with or without a window.
struct Launcher {
    #[cfg(target_os = "linux")]
    system_theme: system_theme::Monitor,
    ui: LauncherUi,
    tray: Option<tray::Tray>,
    host: DesktopHost,
    /// Requests from later starts of the launcher.
    starts: Receiver<single_instance::Request>,
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
    waker: Option<Arc<Waker>>,
}

impl Launcher {
    fn new(backend: Box<dyn Backend>, starts: Receiver<single_instance::Request>, settings_path: Option<PathBuf>) -> Self {
        let windowless = egui::Context::default();
        let (asked, run_again) = channel();
        windowless.set_request_repaint_callback(move |info| {
            let _ = asked.send(info.delay);
        });
        Self {
            #[cfg(target_os = "linux")]
            system_theme: system_theme::Monitor::default(),
            ui: LauncherUi::new(&windowless, backend, Settings::default()),
            tray: None,
            host: DesktopHost { tray: false, quit: false },
            starts,
            windowless,
            run_again,
            started: false,
            closed_to_tray: false,
            settings_path,
            waker: None,
        }
    }

    /// Takes in the saved settings and puts up the tray icon. On macOS the tray icon needs the
    /// event loop running, so this waits for the first window there.
    fn start(&mut self, settings: Option<Settings>, waker: &Arc<Waker>) {
        self.started = true;
        if let Some(settings) = settings {
            self.ui.settings = settings;
        }
        self.waker = Some(waker.clone());
        let (wake, on_quit) = (waker.clone(), waker.clone());
        match tray::Tray::new(move || wake.wake(), move || on_quit.quit_from_tray()) {
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
        let starts: Vec<single_instance::Request> = self.starts.try_iter().collect();
        if starts.contains(&single_instance::Request::Replace) && !self.ui.busy() {
            // A newer launcher is starting and takes over once this one has quit.
            requests.push(Request::Quit);
        } else if !starts.is_empty() {
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
                    if url == artcraft_launcher_core::catalog::DISCORD_APP_URL {
                        artcraft_launcher_native::process::open_url_with_fallback(url, artcraft_launcher_core::catalog::DISCORD_URL);
                    } else if let Err(e) = artcraft_launcher_native::process::open(std::ffi::OsStr::new(url)) {
                        log::warn!("couldn't open {url}: {e}");
                    }
                }
                tray::TrayAction::Quit => {
                    if let Some(waker) = &self.waker {
                        waker.quit_seen();
                    }
                    requests.push(Request::Quit);
                }
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
        ui.ctx().output_mut(|o| {
            o.commands.retain(|cmd| {
                if let egui::OutputCommand::OpenUrl(open_url) = cmd {
                    if open_url.url == artcraft_launcher_core::catalog::DISCORD_APP_URL {
                        artcraft_launcher_native::process::open_url_with_fallback(&open_url.url, artcraft_launcher_core::catalog::DISCORD_URL);
                        return false;
                    }
                    if !open_url.url.starts_with("http://") && !open_url.url.starts_with("https://") {
                        if let Err(e) = artcraft_launcher_native::process::open(std::ffi::OsStr::new(&open_url.url)) {
                            log::warn!("couldn't open {}: {e}", open_url.url);
                        }
                        return false;
                    }
                }
                true
            });
        });
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        #[cfg(target_os = "linux")]
        self.launcher.system_theme.apply(raw_input);
        self.launcher.ui.raw_input(ctx, raw_input);
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

    /// The bundle id is set once: asking again gives the first outcome, not the library's "already set" error.
    /// (Setting it fails where the launcher is not installed, so the outcome itself is not asserted.)
    #[cfg(target_os = "macos")]
    #[test]
    fn notification_identity_is_claimed_once() {
        let first = claim_notification_identity();
        assert_eq!(claim_notification_identity(), first);
    }

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
    fn launcher() -> (Launcher, Sender<single_instance::Request>, Arc<Seen>) {
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
        assert!(show.send(single_instance::Request::Show).is_ok());
        assert!(launcher.wait_in_tray(&woken));
    }

    #[test]
    fn with_no_window_quit_wins_over_a_show_request() {
        let (mut launcher, show, _) = launcher();
        let (_wake, woken) = sync_channel(1);
        assert!(show.send(single_instance::Request::Show).is_ok());
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
            let _ = show.send(single_instance::Request::Show);
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
    fn a_tray_quit_nobody_took_up_ends_the_launcher() {
        let (tray_loop, _woken) = sync_channel(1);
        let waker = Waker { window: Mutex::new(None), tray_loop, quit_pending: AtomicBool::new(true) };
        assert!(waker.quit_unseen());
        assert!(!waker.quit_unseen());
        waker.quit_pending.store(true, Ordering::SeqCst);
        waker.quit_seen();
        assert!(!waker.quit_unseen());
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

    #[test]
    fn dx12_shaders_compile_with_fxc_unless_asked() {
        use eframe::egui_wgpu::WgpuSetup;
        use eframe::wgpu::Dx12Compiler;
        fn compiler(env_choice: Option<Dx12Compiler>) -> Dx12Compiler {
            let mut config = eframe::WgpuConfiguration::default();
            if let WgpuSetup::CreateNew(setup) = &mut config.wgpu_setup {
                // wgpu's default: whichever dxcompiler.dll the DLL search path finds (#126).
                setup.instance_descriptor.backend_options.dx12.shader_compiler = Dx12Compiler::Auto;
            }
            pin_dx12_compiler(&mut config, env_choice);
            match config.wgpu_setup {
                WgpuSetup::CreateNew(setup) => setup.instance_descriptor.backend_options.dx12.shader_compiler,
                WgpuSetup::Existing(_) => panic!("the default configuration creates its own instance"),
            }
        }
        assert!(matches!(compiler(None), Dx12Compiler::Fxc));
        assert!(matches!(compiler(Some(Dx12Compiler::Auto)), Dx12Compiler::Auto));
        assert!(matches!(compiler(Some(Dx12Compiler::default_dynamic_dxc())), Dx12Compiler::DynamicDxc { .. }));
    }

    #[test]
    fn warp_beats_a_gl_gpu_but_not_a_d3d12_gpu() {
        use eframe::wgpu::{Backend, DeviceType};
        // Intel HD 4000 + Radeon HD 7600M have no D3D12 driver; their GL one fails to configure.
        let warp = adapter_rank(Backend::Dx12, DeviceType::Cpu, false);
        assert!(warp < adapter_rank(Backend::Gl, DeviceType::IntegratedGpu, false));
        assert!(warp < adapter_rank(Backend::Gl, DeviceType::Other, false));
        // Hardware D3D12 still wins, in wgpu's order for the power preference.
        let (discrete, integrated) = (DeviceType::DiscreteGpu, DeviceType::IntegratedGpu);
        assert!(adapter_rank(Backend::Dx12, integrated, false) < warp);
        assert!(adapter_rank(Backend::Dx12, discrete, false) < adapter_rank(Backend::Dx12, integrated, false));
        assert!(adapter_rank(Backend::Dx12, integrated, true) < adapter_rank(Backend::Dx12, discrete, true));
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    #[test]
    fn linux_draws_on_the_desktops_gpu_unless_told_otherwise() {
        use eframe::wgpu::PowerPreference;
        let config = wgpu_options();
        let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &config.wgpu_setup else {
            panic!("the launcher creates its own wgpu instance");
        };
        // egui would ask for the discrete GPU: on a PRIME laptop that's not the desktop's (issue #69).
        let expected = PowerPreference::from_env().unwrap_or(PowerPreference::None);
        assert_eq!(setup.power_preference, expected);
    }
}
