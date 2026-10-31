//! ArtCraft Launcher on Android — GameActivity + eframe/wgpu, reusing the desktop UI and backend.
//!
//! The launcher's egui interface ([`artcraft_launcher_ui_egui::LauncherUi`]) and its whole backend
//! ([`artcraft_launcher_native::NativeBackend`]: GitHub release lookups, download and checksum
//! verification) are the desktop ones, unchanged. Only the shell differs:
//!
//! | Desktop (`apps/artcraft-launcher`) | Android (this crate) |
//! |---|---|
//! | tray icon, "close to tray", `tray-icon` | no tray: closing the window quits |
//! | `notify-rust` notifications | logged (a NotificationManager bridge is a later step) |
//! | `rfd` folder picker | `None`: the app's own directory is the install location |
//! | `dirs` data/cache folders | [`AndroidApp::internal_data_path`], injected before the backend starts |
//! | login item, macOS Spaces, Linux theme portal | not applicable |
//! | `single_instance` (loopback TCP) | unchanged; it works inside the app sandbox |
//!
//! Two upstream changes this port needs, both kept as patches in
//! `ports/craft-launcher-android/upstream/`:
//!
//! * `Paths::set_root` — `dirs` answers nothing on Android and an app cannot write `/tmp`.
//! * Android root certificates read straight from `/system/etc/security/cacerts`, because
//!   `rustls-platform-verifier` needs an extra Kotlin component and Maven dependency to reach the
//!   system TrustManager.
//!
//! The download addresses are the port's own: see `catalog::RELEASE_OWNER`.
#![cfg(target_os = "android")]

use std::sync::{Arc, Mutex, PoisonError};

use artcraft_launcher_native::NativeBackend;
use artcraft_launcher_ui_egui::theme::Tokens;
use artcraft_launcher_ui_egui::{Host, LauncherUi, Settings, TitleBar};
use winit::platform::android::activity::AndroidApp;

/// eframe's storage key for the launcher's settings (the desktop shell's `SETTINGS_KEY`).
const SETTINGS_KEY: &str = "settings";

/// Wakes the window from a backend worker thread.
///
/// The backend reports progress from its own threads, so it needs a way to ask for a frame; the
/// context only exists once eframe has built the window, hence the cell.
#[derive(Default)]
struct Waker {
    ctx: Mutex<Option<egui::Context>>,
}

impl Waker {
    fn wake(&self) {
        if let Some(ctx) = self.ctx.lock().unwrap_or_else(PoisonError::into_inner).as_ref() {
            ctx.request_repaint();
        }
    }

    fn set(&self, ctx: egui::Context) {
        *self.ctx.lock().unwrap_or_else(PoisonError::into_inner) = Some(ctx);
    }
}

/// What the Android shell can do for the UI. Every default of [`Host`] already says "this platform
/// has no tray, no desktops and no login item", which is exactly right here.
#[derive(Default)]
struct AndroidHost {
    quit: bool,
}

impl Host for AndroidHost {
    /// The header is the title bar, as on the desktop's non-macOS builds.
    fn title_bar(&self) -> TitleBar {
        TitleBar::Custom
    }

    /// Android has a notification system, but reaching it needs a JNI bridge to
    /// `NotificationManager`. Until that exists the notification is recorded in logcat rather than
    /// silently dropped, so a missing update notice is diagnosable.
    fn notify(&mut self, title: &str, body: &str) {
        log::info!("notification: {title} — {body}");
    }

    fn quit(&mut self) {
        self.quit = true;
    }
}

/// The Android entry point. winit's `android-activity` glue looks this symbol up in the cdylib, so
/// the name and the signature are fixed — hence `unsafe(no_mangle)` and the crate-level
/// `unsafe_code = "deny"` rather than the workspace's `forbid`.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("ArtCraft"),
    );
    log::info!("artcraft-launcher-android: starting (ArtCraft Launcher {})", env!("CARGO_PKG_VERSION"));

    // Releases for this port live in RELEASE_REPO (tzhazuma/artcraft-android), and the upstream
    // feed only advertises desktop builds. Skipping it makes every app resolve against the port's
    // own GitHub releases instead of reporting "not available here".
    // set_var is unsafe in edition 2024 because another thread could read it concurrently; this
    // runs before any thread that reads the environment exists.
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("ARTCRAFT_LAUNCHER_NO_FEED", "1");
    }

    // Before anything builds a backend: `NativeBackend::new` calls `Paths::new`, and on Android
    // `dirs` answers nothing, which would leave the launcher's cache in an unwritable `/tmp`.
    match app.internal_data_path() {
        Some(dir) => {
            log::info!("artcraft-launcher-android: data root {}", dir.display());
            let _ = std::fs::create_dir_all(&dir);
            artcraft_launcher_native::paths::set_root(dir.clone());
            // The launch bridge writes its request into this same directory.
            artcraft_launcher_native::android::set_root(dir);
        }
        None => log::error!("artcraft-launcher-android: no internal data path; downloads cannot be kept"),
    }

    let waker = Arc::new(Waker::default());
    let backend_waker = waker.clone();
    let backend = Box::new(NativeBackend::new(move || backend_waker.wake()));
    // A context for the work that goes on without a window, as the desktop shell's `windowless`.
    // It must outlive `LauncherUi`, which borrows it to set up its repaint callback.
    let windowless = egui::Context::default();
    let ui = LauncherUi::new(&windowless, backend, Settings::default());

    let options = eframe::NativeOptions {
        android_app: Some(app),
        viewport: egui::ViewportBuilder::default()
            .with_title("ArtCraft Launcher")
            .with_app_id("ai.storyteller.artcraft-launcher")
            .with_inner_size([1120.0, 760.0])
            .with_min_inner_size([720.0, 500.0]),
        ..Default::default()
    };

    let shell_waker = waker.clone();
    let result = eframe::run_native(
        "ArtCraft Launcher",
        options,
        Box::new(move |cc| {
            shell_waker.set(cc.egui_ctx.clone());
            let mut ui = ui;
            // The settings eframe saved on the last run.
            if let Some(saved) = cc.storage.and_then(|s| eframe::get_value::<Settings>(s, SETTINGS_KEY)) {
                ui.settings = saved;
            }
            ui.attach(&cc.egui_ctx);
            Ok(Box::new(Shell { ui, host: AndroidHost::default(), waker: shell_waker.clone() }))
        }),
    );
    if let Err(e) = result {
        log::error!("artcraft-launcher-android: eframe exited: {e}");
    }
}

/// One window onto the launcher.
struct Shell {
    ui: LauncherUi,
    host: AndroidHost,
    #[allow(dead_code)]
    waker: Arc<Waker>,
}

impl eframe::App for Shell {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ui.logic(ctx, &mut self.host);
        if std::mem::take(&mut self.host.quit) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        // With no tray to retreat to, a close ends the process — which would kill a download or an
        // install half-way. Refuse the close while the launcher is busy, exactly as the desktop
        // shell keeps its window while a job runs.
        if ctx.input(|i| i.viewport().close_requested()) && self.ui.busy() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.ui.ui(ui, &mut self.host);
    }

    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        self.ui.raw_input(ctx, raw);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.ui.settings);
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        (if visuals.dark_mode { Tokens::DARK.bg } else { Tokens::LIGHT.bg }).to_normalized_gamma_f32()
    }
}
